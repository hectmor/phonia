//! Audio output sinks. The playback engine talks to the hardware only through [`AudioSink`], so
//! the bit-perfect ALSA sink and the in-memory [`fake::FakeSink`] used by tests are
//! interchangeable.

use crate::decode::SourceSpec;
use anyhow::Result;
use std::sync::Arc;

pub mod alsa;
pub mod device;
pub mod fake;
pub mod reserve;

/// A destination for decoded audio. Samples are interleaved, left-justified `i32` (see
/// `crate::decode`), exactly as the decoder produces them.
///
/// Implementations are driven from a single dedicated audio thread and are deliberately not
/// required to be `Send` (`alsa::PCM` isn't).
pub trait AudioSink {
    fn spec(&self) -> SourceSpec;

    /// How many frames the device can hold queued at most. The engine keeps that much of what it
    /// wrote, so that audio the listener has not heard yet survives closing the device.
    fn capacity_frames(&self) -> u64;

    /// Accepts up to one period of frames from `samples`, blocking until the device has room, and
    /// returns how many frames were taken (at least one if `samples` holds a whole frame). Callers
    /// loop until everything is written. Keeping each call short is what makes pause, seek and
    /// shutdown responsive.
    ///
    /// Must not be called while paused.
    fn write(&mut self, samples: &[i32]) -> Result<usize>;

    /// Frames accepted by the device that haven't been played yet, so that
    /// `frames_written - delay_frames()` is what the listener has actually heard.
    fn delay_frames(&mut self) -> Result<u64>;

    /// Pauses playback without losing audio: [`AudioSink::resume`] continues from the exact
    /// sample where playback stopped. Does nothing if already paused.
    fn pause(&mut self) -> Result<()>;

    /// Undoes [`AudioSink::pause`]. Does nothing if not paused.
    fn resume(&mut self) -> Result<()>;

    /// Discards everything queued in the device, so it is never heard, and leaves the sink ready
    /// for new audio (also when paused).
    fn flush(&mut self) -> Result<()>;

    /// Blocks until everything queued has been played. The sink can be written to again
    /// afterwards: the engine reuses it for the next track of the same format.
    fn drain(&mut self) -> Result<()>;
}

/// Opens sinks for the engine, which decides when (a new track with a different format needs a
/// new device configuration). Called on the audio thread, so the sink it returns never has to be
/// `Send`.
pub trait SinkFactory: Send + Sync {
    fn open(&self, spec: SourceSpec) -> Result<Box<dyn AudioSink>>;

    /// Gives the sound card back to the desktop (drops the reservation, if any). Called once the
    /// engine has closed its sink for good: on stop, on failure, and when it lets go of the DAC.
    /// Not called between two tracks of different formats, where the card stays reserved.
    fn release(&self) {}

    /// Installs the function to call when another program asks for the card. It blocks until the
    /// engine has answered, and returns whether the card was given up. Factories that don't reserve
    /// anything ignore it.
    fn on_release_request(&self, _handler: ReleaseHandler) {}
}

/// Another program asking for the card the engine is playing on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseRequest {
    /// Who is asking, if known (a process name).
    pub by: Option<String>,
    /// The asker's priority, as in `org.freedesktop.ReserveDevice1`.
    pub priority: i32,
}

pub type ReleaseHandler = Arc<dyn Fn(ReleaseRequest) -> bool + Send + Sync>;
