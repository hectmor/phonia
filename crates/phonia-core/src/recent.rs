//! A bounded log of recently played tracks, kept by the daemon and saved across its own restarts
//! (see #144). Unlike `play_log`, which reports a finished play to TIDAL, nothing here ever
//! leaves the machine: this is purely local, and covers local files too, not just TIDAL tracks.

use crate::engine::EndReason;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The most entries kept at once; the oldest is dropped once a new one would exceed it.
pub const MAX_RECENT: usize = 50;

/// The file the log is saved to, inside [`state_dir`].
pub const STATE_FILE: &str = "recently_played.json";

/// Where the log is kept across a `phoniad` restart: state, not configuration (`config::dir`) or
/// a credential (`auth::store`) -- it is neither a setting nor a secret, just a convenience that
/// happens to be worth keeping. `None` on a platform with no such directory (`dirs::state_dir`'s
/// own limit, not phonia's): persistence is then simply skipped, never a startup failure.
pub fn state_dir() -> Option<PathBuf> {
    dirs::state_dir().map(|dir| dir.join("phonia"))
}

/// A track counts as played once it has been heard this long -- the same threshold TIDAL itself
/// uses for its own Recently Played (see `crate::play_log::MIN_HEARD`), reused here as a simple
/// position check: unlike `play_log::SessionTracker`, this does not account for pauses or seeks
/// precisely, since `engine::Event::Position` already stalls while paused. A seek forward past
/// the threshold counts; that is accepted as simple and good enough for a local list, not
/// something reported anywhere.
pub const MIN_HEARD_MS: u64 = 30_000;

/// One entry of the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayedTrack {
    pub source: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
    pub cover: Option<String>,
    /// When this play started, milliseconds since the epoch.
    pub played_at_ms: u64,
}

/// The log itself: most recent first, deduplicated by source (playing something again moves it
/// back to the front with a fresh timestamp, rather than listing it twice).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentlyPlayed {
    items: Vec<PlayedTrack>,
}

impl RecentlyPlayed {
    pub fn items(&self) -> &[PlayedTrack] {
        &self.items
    }

    fn record(&mut self, track: PlayedTrack) {
        self.items.retain(|item| item.source != track.source);
        self.items.insert(0, track);
        self.items.truncate(MAX_RECENT);
    }
}

/// Loads the log from `path`. A missing file is simply an empty log (the ordinary case: a first
/// run, or a platform with no [`state_dir`]); a corrupt one is also treated as empty, after a
/// warning on stderr -- this is a convenience log, never worth failing a daemon's startup over.
pub fn load(path: &Path) -> RecentlyPlayed {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RecentlyPlayed::default();
        }
        Err(error) => {
            eprintln!(
                "phonia: reading {path:?}: {error} -- starting with an empty recently played log"
            );
            return RecentlyPlayed::default();
        }
    };
    match serde_json::from_str::<RecentlyPlayed>(&text) {
        Ok(recent) => recent,
        Err(error) => {
            eprintln!(
                "phonia: {path:?} is not a valid recently played log ({error}) -- starting empty"
            );
            RecentlyPlayed::default()
        }
    }
}

/// Saves the log to `path`, replacing it in one step (a temporary file, then a rename), so a
/// crash never leaves it half written -- the same precaution `auth::store`'s own session file
/// takes. Creates `path`'s parent directory if it does not exist yet.
pub fn save(path: &Path, recent: &RecentlyPlayed) -> std::io::Result<()> {
    let json = serde_json::to_string(recent).expect("a recently played log always serializes");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}

/// What is heard of the current track so far, until it is recorded or displaced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    entry: PlayedTrack,
    recorded: bool,
}

/// Turns the engine's own playback events into entries of a [`RecentlyPlayed`] log: a pure state
/// machine, the same shape as `play_log::SessionTracker`, but far simpler, since this only needs
/// to answer one question ("has this been heard long enough yet") rather than build a full
/// session TIDAL would accept.
#[derive(Debug, Default)]
pub struct Tracker {
    recent: RecentlyPlayed,
    current: Option<Pending>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts already knowing `recent` -- the log loaded from disk at startup, if there was one.
    pub fn seeded(recent: RecentlyPlayed) -> Self {
        Self {
            recent,
            current: None,
        }
    }

    pub fn recent(&self) -> &RecentlyPlayed {
        &self.recent
    }

    /// A track started playing. `entry` is `None` when it could not be resolved at all (should
    /// not normally happen); whatever was pending for the track before this one is simply
    /// dropped if it was never heard long enough to be recorded -- the same "below the threshold,
    /// never happened" rule `play_log` itself follows.
    pub fn started(&mut self, entry: Option<PlayedTrack>) {
        self.current = entry.map(|entry| Pending {
            entry,
            recorded: false,
        });
    }

    /// The current track's position advanced. Records it the moment it crosses the threshold;
    /// returns whether this call is what did so (the caller only needs to announce a change).
    pub fn position(&mut self, position_ms: u64) -> bool {
        let Some(pending) = &mut self.current else {
            return false;
        };
        if pending.recorded || position_ms < MIN_HEARD_MS {
            return false;
        }
        self.recent.record(pending.entry.clone());
        pending.recorded = true;
        true
    }

    /// The current track ended. A track shorter than the threshold only ever gets recorded this
    /// way, and only if it played to the end -- the same "`Completed` saves a short track" rule
    /// decided for #144. Returns whether this call recorded it.
    pub fn ended(&mut self, reason: EndReason) -> bool {
        let Some(pending) = self.current.take() else {
            return false;
        };
        if pending.recorded || reason != EndReason::Completed {
            return false;
        }
        self.recent.record(pending.entry);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(source: &str, played_at_ms: u64) -> PlayedTrack {
        PlayedTrack {
            source: source.to_string(),
            title: Some(format!("Song {source}")),
            artist: None,
            duration_ms: Some(200_000),
            cover: None,
            played_at_ms,
        }
    }

    #[test]
    fn a_track_heard_past_the_threshold_is_recorded_once_not_on_every_later_position() {
        let mut tracker = Tracker::new();
        tracker.started(Some(entry("tidal:1", 1_000)));
        assert!(!tracker.position(10_000));
        assert_eq!(tracker.recent().items(), &[]);
        assert!(tracker.position(MIN_HEARD_MS));
        assert_eq!(tracker.recent().items(), &[entry("tidal:1", 1_000)]);
        // Further positions do not record it again.
        assert!(!tracker.position(MIN_HEARD_MS + 5_000));
        assert_eq!(tracker.recent().items().len(), 1);
    }

    #[test]
    fn a_short_track_is_recorded_only_if_it_completes() {
        let mut tracker = Tracker::new();
        tracker.started(Some(entry("tidal:1", 1_000)));
        assert!(!tracker.position(10_000));
        assert!(!tracker.ended(EndReason::Interrupted), "never heard enough");
        assert_eq!(tracker.recent().items(), &[]);

        tracker.started(Some(entry("tidal:2", 2_000)));
        tracker.position(10_000);
        assert!(tracker.ended(EndReason::Completed));
        assert_eq!(tracker.recent().items(), &[entry("tidal:2", 2_000)]);
    }

    #[test]
    fn ending_a_track_already_recorded_by_position_does_not_record_it_twice() {
        let mut tracker = Tracker::new();
        tracker.started(Some(entry("tidal:1", 1_000)));
        tracker.position(MIN_HEARD_MS);
        assert!(!tracker.ended(EndReason::Completed), "already recorded");
        assert_eq!(tracker.recent().items().len(), 1);
    }

    #[test]
    fn starting_a_new_track_drops_whatever_was_pending_and_not_recorded() {
        let mut tracker = Tracker::new();
        tracker.started(Some(entry("tidal:1", 1_000)));
        tracker.position(5_000); // below the threshold
        tracker.started(Some(entry("tidal:2", 2_000)));
        assert!(!tracker.ended(EndReason::Interrupted));
        assert_eq!(tracker.recent().items(), &[]);
    }

    #[test]
    fn playing_the_same_track_again_moves_it_to_the_front_with_a_fresh_time_not_twice() {
        let mut tracker = Tracker::new();
        tracker.started(Some(entry("tidal:1", 1_000)));
        tracker.position(MIN_HEARD_MS);
        tracker.started(Some(entry("tidal:2", 2_000)));
        tracker.position(MIN_HEARD_MS);
        tracker.started(Some(entry("tidal:1", 3_000)));
        tracker.position(MIN_HEARD_MS);
        assert_eq!(
            tracker.recent().items(),
            &[entry("tidal:1", 3_000), entry("tidal:2", 2_000)]
        );
    }

    #[test]
    fn nothing_resolved_at_all_is_simply_not_tracked() {
        let mut tracker = Tracker::new();
        tracker.started(None);
        assert!(!tracker.position(MIN_HEARD_MS));
        assert!(!tracker.ended(EndReason::Completed));
        assert_eq!(tracker.recent().items(), &[]);
    }

    #[test]
    fn the_log_keeps_only_the_most_recent_up_to_the_cap() {
        let mut recent = RecentlyPlayed::default();
        for n in 0..MAX_RECENT + 5 {
            recent.record(entry(&n.to_string(), n as u64));
        }
        assert_eq!(recent.items().len(), MAX_RECENT);
        assert_eq!(recent.items()[0].source, (MAX_RECENT + 4).to_string());
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phonia-recent-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_missing_file_loads_as_an_empty_log_not_an_error() {
        let path = temp_dir("missing").join(STATE_FILE);
        assert_eq!(load(&path), RecentlyPlayed::default());
    }

    #[test]
    fn a_corrupt_file_also_loads_as_empty_after_a_warning() {
        let dir = temp_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(STATE_FILE);
        fs::write(&path, b"not json at all").unwrap();
        assert_eq!(load(&path), RecentlyPlayed::default());
    }

    #[test]
    fn saving_then_loading_round_trips_exactly() {
        let dir = temp_dir("round-trip");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(STATE_FILE);
        let mut recent = RecentlyPlayed::default();
        recent.record(entry("tidal:1", 1_000));
        recent.record(entry("tidal:2", 2_000));
        save(&path, &recent).unwrap();
        assert_eq!(load(&path), recent);
    }

    #[test]
    fn saving_creates_the_parent_directory_and_replaces_an_older_save() {
        // The directory does not exist yet at all: `save` must create it.
        let path = temp_dir("nested").join("sub").join(STATE_FILE);
        let mut first = RecentlyPlayed::default();
        first.record(entry("tidal:1", 1_000));
        save(&path, &first).unwrap();
        assert_eq!(load(&path), first);

        let mut second = RecentlyPlayed::default();
        second.record(entry("tidal:2", 2_000));
        save(&path, &second).unwrap();
        assert_eq!(load(&path), second);
    }
}
