//! The engine's public vocabulary: what it is told to do, and what it reports back.

use crate::decode::SourceSpec;
use std::time::Duration;

/// An opaque reference to a track, meaningful only to the [`super::TrackSupplier`] that hands it
/// out (a TIDAL track id, a file path, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRef(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct TrackMeta {
    pub track: TrackRef,
    pub title: Option<String>,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Plays the given track, replacing whatever is playing. `None` starts from whatever the
    /// supplier considers next, and does nothing if the engine is already loading or playing.
    Play(Option<TrackRef>),
    /// Stops playback and releases the audio device.
    Stop,
    /// Skips to the supplier's next track.
    Next,
    /// Goes back to the previous track, or starts the current one over if it has been playing
    /// for [`super::PREVIOUS_RESTART_AFTER`] or more.
    Previous,
    /// Silences playback without losing the position. Pausing while a track is still loading
    /// makes it start paused.
    Pause,
    /// Continues after a [`Command::Pause`].
    Resume,
    /// `Pause` if playing (or loading), `Resume` if paused.
    TogglePause,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    /// Waiting for the supplier to resolve a track. Nothing is audible.
    Loading,
    Playing,
    /// A track is loaded and its position is kept, but the audio is silent.
    Paused,
}

/// A snapshot of the engine, always up to date on [`super::Engine::status`].
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub state: State,
    /// The current track; `None` unless `state` is [`State::Playing`] or [`State::Paused`].
    pub track: Option<TrackMeta>,
    pub spec: Option<SourceSpec>,
    /// What the listener has heard of the current track, as of the last [`Event::Position`].
    pub position: Duration,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// The track played to its end.
    Completed,
    /// It was cut short by a `Play`, `Next`, `Stop` or shutdown.
    Interrupted,
    /// Decoding or the audio device failed; an [`Event::Error`] precedes this.
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    StateChanged(State),
    /// Sent just before `StateChanged(Playing)`.
    TrackStarted { meta: TrackMeta, spec: SourceSpec },
    TrackEnded { meta: TrackMeta, reason: EndReason },
    /// How much of the current track has actually been heard (not merely handed to the device),
    /// sent about four times a second while playing, and also when a track starts, pauses or
    /// ends. The last one of a completed track equals its length.
    Position { position: Duration, duration: Option<Duration> },
    /// The supplier has no further track to offer.
    QueueExhausted,
    /// Something went wrong; the engine has stopped, but stays usable.
    Error { message: String },
}
