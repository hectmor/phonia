//! Which output the daemon plays on, and how to move it to another.
//!
//! The ids are the ones `phonia-core`'s catalog hands out (`exclusive:hw:DS2,0`, `shared:default`,
//! `shared:<output>`); how a sink is built for an id, and how outputs are listed, are functions
//! given in, so a test can use fakes.

use futures_util::future::BoxFuture;
use phonia_core::config::OutputSpec;
use phonia_core::output::catalog::{self, Entry};
use phonia_core::output::{SinkFactory, Volume, VolumeHandler};
use phonia_ipc as ipc;
use std::sync::{Arc, Mutex};

/// Builds the sink factory for an output.
pub type Build = Arc<dyn Fn(&OutputSpec) -> Arc<dyn SinkFactory> + Send + Sync>;
/// Lists the outputs there are now.
pub type Lister = Arc<dyn Fn() -> BoxFuture<'static, Vec<Entry>> + Send + Sync>;

/// Why the volume could not be set.
#[derive(Debug, PartialEq, Eq)]
pub enum VolumeError {
    /// The output has no volume of its own to set: an exclusive card with no hardware mixer
    /// control of its own.
    Unsupported,
    Failed(String),
}

pub struct Outputs {
    current: Mutex<(OutputSpec, String)>,
    build: Build,
    lister: Lister,
    /// The factory of the output the daemon plays on, for its volume.
    factory: Mutex<Option<Arc<dyn SinkFactory>>>,
    /// The volume asked for, kept across outputs: switching to another output starts it at the
    /// same loudness.
    level: Mutex<Volume>,
    /// Told when the desktop's mixer changes the volume.
    on_volume: Mutex<Option<VolumeHandler>>,
}

impl Outputs {
    /// Playing on `initial`, which is what the daemon was started with.
    pub fn new(initial: OutputSpec, build: Build) -> Self {
        let description = initial.describe();
        Self {
            current: Mutex::new((initial, description)),
            build,
            lister: Arc::new(|| Box::pin(catalog::list())),
            factory: Mutex::new(None),
            level: Mutex::new(Volume::default()),
            on_volume: Mutex::new(None),
        }
    }

    /// Calls `handler` when the volume changes from outside (the desktop's mixer).
    pub fn on_volume_change(&self, handler: VolumeHandler) {
        *self.on_volume.lock().unwrap() = Some(handler);
    }

    /// `factory` is the output the daemon plays on now. With `carry_volume`, the level last asked
    /// for is written into it (shared outputs only ever want this coming from another shared
    /// output: see [`Outputs::switched_to`]); either way, a hardware control is never written
    /// except by an explicit [`Outputs::set_volume`] — it belongs to the card, not to phonia.
    pub fn attach(&self, factory: Arc<dyn SinkFactory>, carry_volume: bool) {
        if let Some(control) = factory.volume() {
            if carry_volume {
                let _ = control.set(*self.level.lock().unwrap());
            }
            if let Some(handler) = self.on_volume.lock().unwrap().clone() {
                control.on_change(handler);
            }
        }
        *self.factory.lock().unwrap() = Some(factory);
    }

    /// The volume, when the output has one phonia can set: always the control's own current
    /// value, never a cached one. A hardware volume can change at any moment (another program, or
    /// someone turning the DAC's own knob), and the control itself is the only truth; this also
    /// means a freshly attached output needs no separate "read the starting level" step.
    pub fn volume(&self) -> Option<Volume> {
        self.factory
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|factory| factory.volume())
            .map(|control| control.get())
    }

    /// Changes the volume with `change`, based on the control's own current value. Returns the
    /// volume now.
    pub fn set_volume(&self, change: impl FnOnce(&mut Volume)) -> Result<Volume, VolumeError> {
        let control = self
            .factory
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|factory| factory.volume())
            .ok_or(VolumeError::Unsupported)?;
        let mut level = control.get();
        change(&mut level);
        level.percent = level.percent.min(100);
        control
            .set(level)
            .map_err(|error| VolumeError::Failed(format!("{error:#}")))?;
        *self.level.lock().unwrap() = level;
        Ok(level)
    }

    /// The desktop's mixer changed the volume. Remembered only for a future shared output to
    /// carry forward (see [`Outputs::switched_to`]); a hardware control is always read fresh
    /// instead (see [`Outputs::volume`]).
    pub fn volume_changed_outside(&self, volume: Volume) {
        *self.level.lock().unwrap() = volume;
    }

    /// Lists outputs with `lister` instead of asking the system.
    pub fn with_lister(mut self, lister: Lister) -> Self {
        self.lister = lister;
        self
    }

    pub async fn list(&self) -> Vec<Entry> {
        (self.lister)().await
    }

    /// Where the sound is going now.
    pub fn route(&self) -> ipc::Route {
        let (spec, description) = &*self.current.lock().unwrap();
        ipc::Route {
            id: spec.id(),
            mode: match spec {
                OutputSpec::Exclusive { .. } => ipc::OutputMode::Exclusive,
                OutputSpec::Shared { .. } => ipc::OutputMode::Shared,
            },
            description: description.clone(),
        }
    }

    /// The output an id names, and a factory for it.
    pub fn prepare(&self, id: &str) -> anyhow::Result<(OutputSpec, Arc<dyn SinkFactory>)> {
        let spec = OutputSpec::from_id(id)?;
        let factory = (self.build)(&spec);
        Ok((spec, factory))
    }

    /// Names the current output the way the list does (`Fosi Audio DS2`, not `device hw:DS2,0`).
    pub async fn refresh(&self) {
        let entries = self.list().await;
        let spec = self.current.lock().unwrap().0.clone();
        let description = catalog::describe(&spec, &entries);
        self.current.lock().unwrap().1 = description;
    }

    /// The daemon plays on `spec` now. `entries` name it for people. The level last asked for
    /// carries over only between two shared outputs: carrying it into a hardware control could
    /// push a DAC to a very different loudness than whatever it was already at, and carrying a
    /// hardware level into a fresh shared stream would double it with the sound server's own
    /// mixing (shared mode may already be driving that same control on its own).
    pub fn switched_to(&self, spec: OutputSpec, entries: &[Entry], factory: Arc<dyn SinkFactory>) {
        let description = catalog::describe(&spec, entries);
        let both_shared = matches!(self.current.lock().unwrap().0, OutputSpec::Shared { .. })
            && matches!(spec, OutputSpec::Shared { .. });
        *self.current.lock().unwrap() = (spec, description);
        self.attach(factory, both_shared);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phonia_core::output::VolumeControl as _;
    use phonia_core::output::fake::FakeSinkFactory;

    fn exclusive() -> OutputSpec {
        OutputSpec::Exclusive {
            device: "hw:fake,0".into(),
        }
    }

    fn shared() -> OutputSpec {
        OutputSpec::Shared { sink: None }
    }

    fn outputs(initial: OutputSpec) -> Outputs {
        Outputs::new(initial, Arc::new(|_| FakeSinkFactory::blocking()))
    }

    #[test]
    fn attaching_without_carrying_reads_the_controls_own_starting_value() {
        let factory = FakeSinkFactory::blocking().with_volume();
        factory.fake_volume().unwrap().change_from_outside(Volume {
            percent: 68,
            muted: false,
        });
        let outputs = outputs(exclusive());
        outputs.attach(factory, false);
        assert_eq!(
            outputs.volume(),
            Some(Volume {
                percent: 68,
                muted: false
            }),
            "read the control's real value instead of a seeded default"
        );
    }

    #[test]
    fn switching_to_an_exclusive_output_never_carries_a_level_in() {
        let outputs = outputs(shared());
        outputs.attach(FakeSinkFactory::blocking().with_volume(), false);
        outputs.set_volume(|v| v.percent = 35).unwrap();

        let next = FakeSinkFactory::blocking().with_volume();
        next.fake_volume().unwrap().change_from_outside(Volume {
            percent: 68,
            muted: false,
        });
        outputs.switched_to(exclusive(), &[], next);

        assert_eq!(
            outputs.volume().unwrap().percent,
            68,
            "kept the new control's own value, never the old shared level"
        );
    }

    #[test]
    fn switching_from_exclusive_to_shared_does_not_carry_the_hardware_level_in() {
        let outputs = outputs(exclusive());
        let first = FakeSinkFactory::blocking().with_volume();
        first.fake_volume().unwrap().change_from_outside(Volume {
            percent: 20,
            muted: false,
        });
        outputs.attach(first, false);
        assert_eq!(outputs.volume().unwrap().percent, 20);

        let next = FakeSinkFactory::blocking().with_volume();
        outputs.switched_to(shared(), &[], next);
        assert_eq!(
            outputs.volume(),
            Some(Volume::default()),
            "the new shared stream starts at its own default, not the old hardware level"
        );
    }

    #[test]
    fn switching_between_two_shared_outputs_still_carries_the_level() {
        let outputs = outputs(shared());
        outputs.attach(FakeSinkFactory::blocking().with_volume(), false);
        outputs.set_volume(|v| v.percent = 35).unwrap();

        let next = FakeSinkFactory::blocking().with_volume();
        outputs.switched_to(shared(), &[], next.clone());

        assert_eq!(
            next.fake_volume().unwrap().get(),
            Volume {
                percent: 35,
                muted: false
            }
        );
    }

    #[test]
    fn attaching_to_a_control_less_output_leaves_volume_unavailable() {
        let outputs = outputs(exclusive());
        outputs.attach(FakeSinkFactory::blocking(), false);
        assert_eq!(outputs.volume(), None);
        assert_eq!(
            outputs.set_volume(|v| v.percent = 10).unwrap_err(),
            VolumeError::Unsupported
        );
    }
}
