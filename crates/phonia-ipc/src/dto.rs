//! The data that travels: what the daemon reports about playback and the queue.
//!
//! Times are milliseconds (`*_ms`), so a person or a script reading the JSON needs no knowledge of
//! how Rust serializes durations.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

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
    /// What TIDAL delivered, for a track that is streamed from TIDAL (since 1.5).
    #[serde(default)]
    pub quality: Option<StreamQuality>,
}

/// A TIDAL quality tier, worst to best: `low < high < lossless < hires`. `high` and `low` are
/// lossy (AAC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Low,
    High,
    Lossless,
    Hires,
    #[serde(other)]
    Unknown,
}

impl fmt::Display for Quality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Quality::Hires => "hires",
            Quality::Lossless => "lossless",
            Quality::High => "high",
            Quality::Low => "low",
            Quality::Unknown => "unknown",
        })
    }
}

impl FromStr for Quality {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "hires" => Ok(Quality::Hires),
            "lossless" => Ok(Quality::Lossless),
            "high" => Ok(Quality::High),
            "low" => Ok(Quality::Low),
            other => Err(format!(
                "unknown quality {other:?}: expected hires, lossless, high or low"
            )),
        }
    }
}

/// The tier a streamed track was asked for and the one TIDAL gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamQuality {
    pub requested: Quality,
    pub delivered: Quality,
}

impl StreamQuality {
    /// Whether TIDAL gave a lower tier than was asked for.
    pub fn fell_back(&self) -> bool {
        self.delivered != Quality::Unknown
            && self.requested != Quality::Unknown
            && self.delivered < self.requested
    }
}

/// The best tier the daemon asks TIDAL for, and the worst it will play.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityRange {
    pub max: Quality,
    pub min: Quality,
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
    /// Where the sound goes, when the daemon knows (since 1.2).
    #[serde(default)]
    pub route: Option<Route>,
    /// The volume, when the output has one phonia can set: a shared output does, an exclusive
    /// card does not (since 1.3).
    #[serde(default)]
    pub volume: Option<Volume>,
    /// The tiers the daemon asks TIDAL for (since 1.5).
    #[serde(default)]
    pub quality_range: Option<QualityRange>,
}

/// How loud, as the desktop's mixers show it: 100 is unity gain, and the scale is cubic in
/// amplitude, so 50 is about -18 dB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    pub percent: u8,
    pub muted: bool,
}

/// How the daemon reaches an output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// A sound card of the daemon's own: bit-perfect.
    Exclusive,
    /// Through the desktop's sound server: mixed and resampled, not bit-perfect.
    Shared,
    #[serde(other)]
    Unknown,
}

/// Where the sound is going.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    /// The id to give back to `set_output`: `exclusive:hw:DS2,0`, `shared:default`, `shared:<sink>`.
    pub id: String,
    pub mode: OutputMode,
    /// For people: the card or the sound server's name for the output.
    pub description: String,
}

/// An output the daemon can play on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputInfo {
    pub id: String,
    pub mode: OutputMode,
    /// The output's name.
    pub name: String,
    /// What kind of output it is (`USB`, `Bluetooth`, `card 2`...), when known.
    pub detail: Option<String>,
    /// Whether playing there is bit-perfect: only an exclusive card is.
    pub bit_perfect: bool,
    /// Whether the way to the speaker loses information (a Bluetooth link does).
    pub lossy: bool,
    /// The Bluetooth codec in use.
    pub codec: Option<String>,
    /// For the sound server's entry that follows the desktop's default output.
    pub is_default: bool,
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
    Released {
        by: Option<String>,
    },
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
    /// The output went away (a Bluetooth speaker switched off). Since 1.2.
    Lost,
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
    /// How the sound got there (since 1.2). Absent from older daemons, which only had cards.
    #[serde(default)]
    pub mode: Option<OutputMode>,
    /// For shared mode: the rate the output runs at, when the server resamples to it.
    #[serde(default)]
    pub resampled_to: Option<u32>,
    /// For shared mode: the Bluetooth codec in use.
    #[serde(default)]
    pub codec: Option<String>,
    /// For shared mode: whether the way to the speaker loses information.
    #[serde(default)]
    pub lossy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Completed,
    Interrupted,
    Failed,
}
