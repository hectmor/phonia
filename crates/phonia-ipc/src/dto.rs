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
    /// The track's own title; no longer includes the artist (since 1.9 -- a 1.8 client shows the
    /// title without the artist, which still reads fine on its own).
    pub title: Option<String>,
    /// The performing artist(s), joined `", "`; absent for a local file, or a TIDAL track with
    /// none credited (since 1.9).
    #[serde(default)]
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
    /// What TIDAL delivered, for a track that is streamed from TIDAL (since 1.5).
    #[serde(default)]
    pub quality: Option<StreamQuality>,
    /// The track's album's cover id, to turn into a URL with [`crate::image::url`]; absent for a
    /// local file or a TIDAL track with no album (since 1.6).
    #[serde(default)]
    pub cover: Option<String>,
    /// The gain ReplayGain decided for this track, if `replaygain` is not `off` and TIDAL has
    /// loudness data for it; absent otherwise. This is decided the same way whatever the output,
    /// but only actually applied to the audio in shared mode: a client shows it only when
    /// `route.mode` is `shared` too (since 1.8).
    #[serde(default)]
    pub replay_gain: Option<ReplayGain>,
}

/// The gain [`crate::fmt::replay_gain`] decided on, as millibels (hundredths of a dB) rather than
/// a float, so this type (and everything that contains it) can derive `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayGain {
    pub kind: GainKind,
    pub millibels: i32,
}

/// Which of a track's two possible gains was actually used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GainKind {
    Track,
    Album,
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
    /// The verdict for the sink that is open right now, if one has been reported: absent until
    /// the first write after an output opens, and cleared once it closes (since 1.7).
    #[serde(default)]
    pub sink_report: Option<SinkReport>,
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
    /// The track's own title; no longer includes the artist (since 1.9).
    pub title: Option<String>,
    /// The performing artist(s), joined `", "`; absent for a local file, or a TIDAL track with
    /// none credited (since 1.9).
    #[serde(default)]
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
    /// The track's album's cover id, to turn into a URL with [`crate::image::url`]; absent for a
    /// local file or a TIDAL track with no album (since 1.6).
    #[serde(default)]
    pub cover: Option<String>,
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
    /// Whether the queue running dry fetches more tracks from TIDAL on its own (since 1.13;
    /// `#[serde(default)]` so an older daemon's snapshot still parses as `false`).
    #[serde(default)]
    pub autoplay: bool,
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
    /// The route id (`exclusive:hw:DS2,0`, `shared:default`) of the output whose sink produced
    /// this report, when the daemon knows it (since 1.7).
    #[serde(default)]
    pub output: Option<String>,
}

impl SinkReport {
    /// Whether this report still describes what `status` says is playing now: the same audio
    /// format, and -- when both sides know it -- the same output. A report never outlives the
    /// sink it was taken from, but nothing announces that a sink closed or changed on its own, so
    /// a stale report has to be recognized this way instead of cleared directly.
    pub fn applies_to(&self, status: &Status) -> bool {
        if status.spec != Some(self.source) {
            return false;
        }
        match (&self.output, &status.route) {
            (Some(report_output), Some(route)) => *report_output == route.id,
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Completed,
    Interrupted,
    Failed,
}

// --- The catalog (since 1.6) ---------------------------------------------------------------------

/// The kinds of thing TIDAL's catalog has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogKind {
    Tracks,
    Albums,
    Artists,
    Playlists,
    /// A kind a newer daemon has and this version does not know.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistRef {
    /// TIDAL's id, as text.
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumRef {
    pub id: String,
    pub title: String,
    /// TIDAL's cover id (a UUID), to turn into an image URL with [`crate::image::url`] (since 1.6,
    /// with covers).
    #[serde(default)]
    pub cover: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackSummary {
    pub id: String,
    pub title: String,
    /// "Remastered", "Live"...
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub album: Option<AlbumRef>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default)]
    pub track_number: Option<u32>,
    /// Which disc of the album the track is on, when there is more than one.
    #[serde(default)]
    pub volume_number: Option<u32>,
    /// The best tier TIDAL has the track in.
    #[serde(default)]
    pub quality: Option<Quality>,
    /// Whether it can be played where the daemon is.
    #[serde(default = "yes")]
    pub streamable: bool,
}

/// What kind of release an album is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlbumKind {
    Album,
    Ep,
    Single,
    /// A kind a newer daemon has and this version does not know.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumSummary {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    /// `2011-05-31`.
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub track_count: Option<u32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default)]
    pub quality: Option<Quality>,
    /// An album, an EP or a single (since 1.6, with the album and artist views).
    #[serde(default)]
    pub kind: Option<AlbumKind>,
    #[serde(default)]
    pub copyright: Option<String>,
    /// TIDAL's cover id (a UUID), to turn into an image URL with [`crate::image::url`] (since 1.6,
    /// with covers).
    #[serde(default)]
    pub cover: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistSummary {
    pub id: String,
    pub name: String,
    /// TIDAL's picture id (a UUID), the same shape as an album's cover id (since 1.6, with
    /// covers); many artists have none.
    #[serde(default)]
    pub picture: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistSummary {
    /// A UUID.
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub creator: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub track_count: Option<u32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// TIDAL's square image id (a UUID), the same shape as an album's cover id (since 1.6, with
    /// covers).
    #[serde(default)]
    pub cover: Option<String>,
}

/// A track's lyrics (since 1.10). `lines` is empty when there is no time-synced version; `plain`
/// is still `Some` in that case, so a client can fall back to showing it unsynced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lyrics {
    #[serde(default)]
    pub lines: Vec<LyricLine>,
    #[serde(default)]
    pub plain: Option<String>,
    #[serde(default)]
    pub right_to_left: bool,
    #[serde(default)]
    pub provider: Option<String>,
}

/// One line of synced lyrics, timed from the start of the track (since 1.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    pub at_ms: u64,
    pub text: String,
}

/// One page of a list that may be longer: `total` is how many there are in all, `offset` where
/// this page starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub offset: u64,
}

/// Something in the catalog that stands for a list of tracks, to be added to the queue as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CatalogRef {
    Album {
        id: String,
    },
    Playlist {
        id: String,
    },
    /// An artist's most listened to tracks.
    ArtistTopTracks {
        id: String,
    },
    /// The logged-in user's favorite tracks (since 1.6, with the library).
    FavoriteTracks,
    /// TIDAL's own "radio" for a track: tracks picked to follow it, seeded by it, never
    /// including the seed itself (since 1.12).
    TrackRadio {
        id: String,
    },
    /// Something a newer daemon can add and this version does not know.
    #[serde(other)]
    Unknown,
}

/// A list of albums that can be asked for page by page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlbumListRef {
    /// An artist's albums.
    ArtistAlbums { id: String },
    /// An artist's EPs and singles.
    ArtistSingles { id: String },
    /// The logged-in user's favorite albums (since 1.6, with the library).
    FavoriteAlbums,
    /// A list a newer daemon has and this version does not know.
    #[serde(other)]
    Unknown,
}

/// A list of playlists that can be asked for page by page (since 1.6, with the library).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlaylistListRef {
    /// The playlists the logged-in user created themselves, not the ones they only follow.
    Mine,
    /// A list a newer daemon has and this version does not know.
    #[serde(other)]
    Unknown,
}

/// One entry of a playlist folder (since 1.11): a sub-folder, or a playlist. Unlike
/// [`PlaylistListRef::Mine`], a playlist here may be one the user only follows, not one they
/// created — TIDAL's own folders mix both, and hiding the followed ones would make a folder look
/// wrong next to what TIDAL's own app shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FolderEntry {
    Folder {
        /// A UUID; pass this back as `Request::PlaylistFolder`'s own `folder` to open it.
        id: String,
        name: String,
        /// How many entries (playlists and sub-folders alike) are directly inside it.
        item_count: u32,
    },
    Playlist(PlaylistSummary),
    /// A kind of entry a newer daemon has and this version does not know.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_96K: Spec = Spec {
        sample_rate: 96_000,
        channels: 2,
        bits_per_sample: 24,
    };
    const SPEC_48K: Spec = Spec {
        sample_rate: 48_000,
        channels: 2,
        bits_per_sample: 16,
    };

    fn report(source: Spec, output: Option<&str>) -> SinkReport {
        SinkReport {
            device: "hw:1,0".into(),
            source,
            negotiated_format: "S24_3LE".into(),
            bit_perfect: true,
            problem: None,
            hw_params: None,
            mode: Some(OutputMode::Exclusive),
            resampled_to: None,
            codec: None,
            lossy: false,
            output: output.map(str::to_string),
        }
    }

    fn status_with(spec: Option<Spec>, route: Option<&str>) -> Status {
        Status {
            state: State::Playing,
            track: None,
            spec,
            position_ms: 0,
            duration_ms: None,
            output: Output::Open,
            route: route.map(|id| Route {
                id: id.to_string(),
                mode: OutputMode::Exclusive,
                description: "Fosi Audio DS2".into(),
            }),
            volume: None,
            quality_range: None,
            sink_report: None,
        }
    }

    #[test]
    fn a_report_applies_when_the_format_matches_and_neither_side_names_an_output() {
        let report = report(SPEC_96K, None);
        assert!(report.applies_to(&status_with(Some(SPEC_96K), None)));
    }

    #[test]
    fn a_report_does_not_apply_once_the_format_has_changed() {
        let report = report(SPEC_96K, None);
        assert!(!report.applies_to(&status_with(Some(SPEC_48K), None)));
        assert!(!report.applies_to(&status_with(None, None)));
    }

    #[test]
    fn a_report_does_not_apply_to_a_different_output() {
        let report = report(SPEC_96K, Some("exclusive:hw:1,0"));
        assert!(report.applies_to(&status_with(Some(SPEC_96K), Some("exclusive:hw:1,0"))));
        assert!(!report.applies_to(&status_with(Some(SPEC_96K), Some("shared:default"))));
    }

    #[test]
    fn the_output_check_is_skipped_when_either_side_does_not_know_it() {
        // An older daemon's report, or a status with no route yet: the format match is all there
        // is to go on, so it is not refused just because the output can't be compared.
        let unlabelled = report(SPEC_96K, None);
        assert!(unlabelled.applies_to(&status_with(Some(SPEC_96K), Some("exclusive:hw:1,0"))));

        let labelled = report(SPEC_96K, Some("exclusive:hw:1,0"));
        assert!(labelled.applies_to(&status_with(Some(SPEC_96K), None)));
    }
}
