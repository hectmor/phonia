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

/// Where to move within the current track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekTarget {
    Absolute(Duration),
    /// Ahead of what has been heard so far.
    Forward(Duration),
    /// Behind what has been heard so far, stopping at the start of the track.
    Backward(Duration),
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
    /// Moves within the current track. Reported with [`Event::Seeked`], or
    /// [`Event::SeekRejected`] if the track can't do it. Pausing is unaffected: a seek while
    /// paused stays paused.
    Seek(SeekTarget),
    /// Pauses (if playing) and gives the audio device back to the desktop, so another program can
    /// use it. The track and the exact position are kept; [`Command::Resume`] takes the device
    /// again and carries on. Does nothing while stopped.
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    /// Waiting for the supplier to resolve a track. Nothing is audible.
    Loading,
    Playing,
    /// A track is loaded and its position is kept, but the audio is silent.
    Paused,
    /// The current track is being reopened at a new position (a stream that can't rewind is
    /// opened again where the seek points). Silent, like [`State::Loading`], but the track and
    /// the position it will resume from are known.
    Seeking,
}

/// A snapshot of the engine, always up to date on [`super::Engine::status`].
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub state: State,
    /// The current track; `None` unless `state` is [`State::Playing`], [`State::Paused`] or
    /// [`State::Seeking`].
    pub track: Option<TrackMeta>,
    pub spec: Option<SourceSpec>,
    /// What the listener has heard of the current track, as of the last [`Event::Position`].
    pub position: Duration,
    pub duration: Option<Duration>,
    pub output: OutputState,
}

/// What the engine is doing with the audio device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputState {
    /// Not holding the device: nothing has been played yet, or playback stopped.
    Closed,
    Open,
    /// A track is loaded but the device was handed back (see [`Command::Release`]); `by` is the
    /// program that asked for it, if one did. Resuming takes the device again.
    Released { by: Option<String> },
}

/// Why the engine gave the audio device back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseReason {
    /// It had been paused for longer than the configured time.
    Idle,
    /// [`Command::Release`].
    Command,
    /// Another program asked for the device.
    Requested,
    /// The output went away underneath the engine (a Bluetooth speaker switched off, the sound
    /// server stopped). An [`Event::Error`] says which.
    Lost,
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
    /// A seek was applied: the position playback will continue from. Sent as soon as the seek is
    /// accepted, so for a track that has to be reopened it precedes the audio actually resuming
    /// (see [`State::Seeking`]).
    Seeked { position: Duration },
    /// A seek could not be done, with why. Playback carries on as if it had not been requested.
    SeekRejected { reason: String },
    /// The supplier has no further track to offer.
    QueueExhausted,
    /// The engine paused and gave the audio device back; the track and position are kept.
    OutputReleased { by: Option<String>, reason: ReleaseReason },
    /// It took the device again, on resume or for a new track.
    OutputAcquired,
    /// Something went wrong; the engine has stopped, but stays usable.
    Error { message: String },
}
