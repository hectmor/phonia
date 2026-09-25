//! Which output the daemon plays on, and how to move it to another.
//!
//! The ids are the ones `phonia-core`'s catalog hands out (`exclusive:hw:DS2,0`, `shared:default`,
//! `shared:<output>`); how a sink is built for an id, and how outputs are listed, are functions
//! given in, so a test can use fakes.

use futures_util::future::BoxFuture;
use phonia_core::config::OutputSpec;
use phonia_core::output::{SinkFactory, Volume, VolumeHandler};
use phonia_core::output::catalog::{self, Entry};
use phonia_ipc as ipc;
use std::sync::{Arc, Mutex};

/// Builds the sink factory for an output.
pub type Build = Arc<dyn Fn(&OutputSpec) -> Arc<dyn SinkFactory> + Send + Sync>;
/// Lists the outputs there are now.
pub type Lister = Arc<dyn Fn() -> BoxFuture<'static, Vec<Entry>> + Send + Sync>;

/// Why the volume could not be set.
#[derive(Debug, PartialEq, Eq)]
pub enum VolumeError {
    /// The output has no volume of its own to set (an exclusive card).
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

    /// `factory` is the output the daemon plays on now. The volume asked for carries over to it, and
    /// changes made outside are followed.
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

    /// The volume, when the output has one phonia can set.
    pub fn volume(&self) -> Option<Volume> {
        let has_volume = self.factory.lock().unwrap().as_ref().is_some_and(|factory| factory.volume().is_some());
        has_volume.then(|| *self.level.lock().unwrap())
    }

    /// Changes the volume with `change`. Returns the volume now.
    pub fn set_volume(&self, change: impl FnOnce(&mut Volume)) -> Result<Volume, VolumeError> {
        let control = self
            .factory
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|factory| factory.volume())
            .ok_or(VolumeError::Unsupported)?;
        let mut level = *self.level.lock().unwrap();
        change(&mut level);
        level.percent = level.percent.min(100);
        control.set(level).map_err(|error| VolumeError::Failed(format!("{error:#}")))?;
        *self.level.lock().unwrap() = level;
        Ok(level)
    }

    /// The desktop's mixer changed the volume.
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

    /// The daemon plays on `spec` now. `entries` name it for people.
    pub fn switched_to(&self, spec: OutputSpec, entries: &[Entry], factory: Arc<dyn SinkFactory>) {
        let description = catalog::describe(&spec, entries);
        *self.current.lock().unwrap() = (spec, description);
        self.attach(factory, true);
    }
}
