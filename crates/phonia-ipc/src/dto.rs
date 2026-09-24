//! The data that travels: what the daemon reports about playback and the queue.
//!
//! Times are milliseconds (`*_ms`), so a person or a script reading the JSON needs no knowledge of
//! how Rust serializes durations.

use serde::{Deserialize, Serialize};

/// Identifies one entry of the queue for as long as the daemon runs. Never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Stopped,
    Loading,
    Playing,
    Paused,
    Seeking,
}

/// The format of the audio being played.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: u32,
}

/// The track being played.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub item_id: Option<ItemId>,
    /// `file:/abs/path` or `tidal:<id>` (see [`crate::source`]).
    pub source: Option<String>,
    pub title: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    pub track: Option<Track>,
    pub spec: Option<Spec>,
    /// What has been heard of the current track.
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    /// What the daemon is doing with the audio device. Absent from daemons older than 1.1.
    #[serde(default)]
    pub output: Output,
}

/// The daemon's hold on the audio device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Output {
    /// Not holding it: nothing played yet, or playback stopped.
    #[default]
    Closed,
    Open,
    /// Handed back to the desktop while a track stays loaded; resuming takes it again. `by` names
    /// the program that asked for it, if one did.
    Released { by: Option<String> },
}

/// Why the daemon handed the audio device back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    /// Paused for longer than the configured time.
    Idle,
    /// A client asked (`release`).
    Command,
    /// Another program asked for the device.
    Requested,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: ItemId,
    pub source: String,
    pub title: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    #[default]
    Off,
    One,
    All,
}

/// The whole queue at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queue {
    /// Increases with every change, so a stale snapshot can be recognized.
    pub version: u64,
    /// The entries in queue order.
    pub items: Vec<QueueItem>,
    /// The order they will play in (the same ids; shuffled when `shuffle` is on).
    pub order: Vec<ItemId>,
    pub current: Option<ItemId>,
    pub shuffle: bool,
    pub repeat: Repeat,
}

/// Whether playback is bit-perfect, and the evidence, reported when a track starts on a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkReport {
    pub device: String,
    pub source: Spec,
    /// The sample format negotiated with the device, e.g. `S24_3LE`.
    pub negotiated_format: String,
    pub bit_perfect: bool,
    /// Why it is not known to be bit-perfect; absent when it is.
    pub problem: Option<String>,
    /// What the kernel reports for the running stream (`hw_params`), when readable.
    pub hw_params: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Completed,
    Interrupted,
    Failed,
}
