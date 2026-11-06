//! A sink factory that drops to shared for a track the exclusive device cannot play natively,
//! and climbs back to it for the next track that fits (#228). Built only for `mode = "auto"`
//! (`phoniad`'s `main.rs`) -- a pinned exclusive output asked for bit-perfect or nothing, the same
//! line #157's own DAC-lost fallback already draws.

use super::caps::{Missing, Unsupported};
use super::{AudioSink, ReleaseHandler, SinkFactory, VolumeControl};
use crate::config::OutputSpec;
use crate::decode::SourceSpec;
use anyhow::Result;
use std::sync::{Arc, Mutex};

/// Told `(from, to)` when the side actually open changes. Called on the audio thread: must not
/// block, the same contract `phoniad`'s own sink-report channel already has.
pub type SwitchHandler = Arc<dyn Fn(OutputSpec, OutputSpec) + Send + Sync>;

pub struct FallbackSinkFactory {
    exclusive: (OutputSpec, Arc<dyn SinkFactory>),
    shared: (OutputSpec, Arc<dyn SinkFactory>),
    on_shared: Mutex<bool>,
    on_switch: SwitchHandler,
}

impl FallbackSinkFactory {
    pub fn new(
        exclusive: (OutputSpec, Arc<dyn SinkFactory>),
        shared: (OutputSpec, Arc<dyn SinkFactory>),
        on_switch: SwitchHandler,
    ) -> Arc<Self> {
        Arc::new(Self {
            exclusive,
            shared,
            on_shared: Mutex::new(false),
            on_switch,
        })
    }

    /// Notifies and remembers a side change, unless it is the one already in effect.
    fn moved_to(&self, now_shared: bool) {
        let mut on_shared = self.on_shared.lock().unwrap();
        if *on_shared == now_shared {
            return;
        }
        let (from, to) = if now_shared {
            (&self.exclusive.0, &self.shared.0)
        } else {
            (&self.shared.0, &self.exclusive.0)
        };
        *on_shared = now_shared;
        (self.on_switch)(from.clone(), to.clone());
    }

    fn open_side(&self) -> &Arc<dyn SinkFactory> {
        if *self.on_shared.lock().unwrap() {
            &self.shared.1
        } else {
            &self.exclusive.1
        }
    }
}

impl SinkFactory for FallbackSinkFactory {
    /// Tries the exclusive side first, every time -- never stays on shared "just in case": a
    /// track whose spec fits the DAC always lands back on it, right when the engine next opens a
    /// sink for one (the same moment it would reopen for any other format change).
    fn open(&self, spec: SourceSpec) -> Result<Box<dyn AudioSink>> {
        match self.exclusive.1.open(spec) {
            Ok(sink) => {
                self.moved_to(false);
                Ok(sink)
            }
            Err(error) => {
                let falls_back = error
                    .downcast_ref::<Unsupported>()
                    .is_some_and(|unsupported| !matches!(unsupported.missing, Missing::Depth));
                if !falls_back {
                    return Err(error);
                }
                // The reservation is per open attempt (see `output::reserve`), and a failed open
                // never releases it on its own -- it has to be given back explicitly here, or
                // the sound server can't drive the same card for the resampled stream.
                self.exclusive.1.release();
                let sink = self.shared.1.open(spec)?;
                self.moved_to(true);
                Ok(sink)
            }
        }
    }

    fn release(&self) {
        self.exclusive.1.release();
        self.shared.1.release();
    }

    fn volume(&self) -> Option<Arc<dyn VolumeControl>> {
        self.open_side().volume()
    }

    fn on_release_request(&self, handler: ReleaseHandler) {
        self.exclusive.1.on_release_request(handler.clone());
        self.shared.1.on_release_request(handler);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::caps::Capabilities;
    use crate::output::fake::FakeSinkFactory;
    use std::collections::BTreeMap;
    use std::sync::Mutex as StdMutex;

    fn spec(rate: u32) -> SourceSpec {
        SourceSpec {
            sample_rate: rate,
            channels: 2,
            bits_per_sample: 24,
        }
    }

    fn unsupported(missing: Missing) -> anyhow::Error {
        anyhow::Error::new(Unsupported {
            device: "hw:fake,0".into(),
            source: spec(22_050),
            missing,
            caps: Capabilities {
                channels: (2, 2),
                rates: BTreeMap::new(),
            },
        })
    }

    fn exclusive() -> OutputSpec {
        OutputSpec::Exclusive {
            device: "hw:fake,0".into(),
        }
    }

    fn shared() -> OutputSpec {
        OutputSpec::Shared { sink: None }
    }

    type Switches = Arc<StdMutex<Vec<(OutputSpec, OutputSpec)>>>;

    fn recorder() -> (SwitchHandler, Switches) {
        let switches = Arc::new(StdMutex::new(Vec::new()));
        let recorded = switches.clone();
        let handler: SwitchHandler = Arc::new(move |from, to| {
            recorded.lock().unwrap().push((from, to));
        });
        (handler, switches)
    }

    #[test]
    fn a_rate_the_dac_refuses_falls_back_to_shared_and_notifies_once() {
        let dac = FakeSinkFactory::blocking();
        dac.fail_next_open_with(unsupported(Missing::Rate));
        let pulse = FakeSinkFactory::blocking();
        let (on_switch, switches) = recorder();
        let fallback = FallbackSinkFactory::new(
            (exclusive(), dac.clone()),
            (shared(), pulse.clone()),
            on_switch,
        );

        let sink = fallback.open(spec(22_050));
        assert!(
            sink.is_ok(),
            "the track plays through shared instead of failing"
        );
        assert_eq!(
            dac.release_count(),
            1,
            "the failed exclusive open is released explicitly"
        );
        assert_eq!(
            switches.lock().unwrap().as_slice(),
            [(exclusive(), shared())]
        );
    }

    #[test]
    fn a_fitting_track_after_a_fallback_climbs_back_to_the_dac() {
        let dac = FakeSinkFactory::blocking();
        dac.fail_next_open_with(unsupported(Missing::Rate));
        let pulse = FakeSinkFactory::blocking();
        let (on_switch, switches) = recorder();
        let fallback = FallbackSinkFactory::new(
            (exclusive(), dac.clone()),
            (shared(), pulse.clone()),
            on_switch,
        );

        fallback.open(spec(22_050)).unwrap();
        fallback.open(spec(96_000)).unwrap();

        assert_eq!(
            switches.lock().unwrap().as_slice(),
            [(exclusive(), shared()), (shared(), exclusive())],
        );
    }

    #[test]
    fn staying_on_the_same_side_does_not_notify_again() {
        let dac = FakeSinkFactory::blocking();
        let (on_switch, switches) = recorder();
        let fallback = FallbackSinkFactory::new(
            (exclusive(), dac.clone()),
            (shared(), FakeSinkFactory::blocking()),
            on_switch,
        );

        fallback.open(spec(96_000)).unwrap();
        fallback.open(spec(44_100)).unwrap();

        assert!(switches.lock().unwrap().is_empty());
    }

    #[test]
    fn a_depth_the_source_itself_cannot_offer_is_not_a_fallback_reason() {
        let dac = FakeSinkFactory::blocking();
        dac.fail_next_open_with(unsupported(Missing::Depth));
        let (on_switch, _switches) = recorder();
        let fallback = FallbackSinkFactory::new(
            (exclusive(), dac),
            (shared(), FakeSinkFactory::blocking()),
            on_switch,
        );

        let Err(error) = fallback.open(spec(22_050)) else {
            panic!("a depth no device could play must still fail")
        };
        assert!(error.downcast_ref::<Unsupported>().is_some());
    }

    #[test]
    fn an_unrelated_open_error_is_not_a_fallback_reason_either() {
        let dac = FakeSinkFactory::blocking();
        dac.fail_next_open("the card is busy");
        let (on_switch, switches) = recorder();
        let fallback = FallbackSinkFactory::new(
            (exclusive(), dac),
            (shared(), FakeSinkFactory::blocking()),
            on_switch,
        );

        let Err(error) = fallback.open(spec(96_000)) else {
            panic!("an unrelated open error must still fail")
        };
        assert_eq!(error.to_string(), "the card is busy");
        assert!(switches.lock().unwrap().is_empty());
    }

    #[test]
    fn volume_follows_whichever_side_is_open_now() {
        let dac = FakeSinkFactory::blocking().with_volume();
        dac.fail_next_open_with(unsupported(Missing::Rate));
        let pulse = FakeSinkFactory::blocking().with_volume();
        let (on_switch, _switches) = recorder();
        let expected = pulse.volume().unwrap();
        let fallback = FallbackSinkFactory::new(
            (exclusive(), dac.clone() as Arc<dyn SinkFactory>),
            (shared(), pulse.clone() as Arc<dyn SinkFactory>),
            on_switch,
        );

        fallback.open(spec(22_050)).unwrap();
        assert!(
            fallback
                .volume()
                .is_some_and(|v| Arc::ptr_eq(&v, &expected))
        );
    }

    #[test]
    fn release_gives_up_both_sides() {
        let dac = FakeSinkFactory::blocking();
        let pulse = FakeSinkFactory::blocking();
        let (on_switch, _switches) = recorder();
        let fallback = FallbackSinkFactory::new(
            (exclusive(), dac.clone()),
            (shared(), pulse.clone()),
            on_switch,
        );

        fallback.release();
        assert_eq!(dac.release_count(), 1);
        assert_eq!(pulse.release_count(), 1);
    }
}
