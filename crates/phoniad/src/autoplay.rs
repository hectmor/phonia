//! Autoplay's own bookkeeping: when the queue is about to run dry, fetch more tracks from the
//! seed track's TIDAL radio and append them, picking up gapless through the engine's own
//! existing prefetch (see `docs/DECISIONS.md`'s #33 part 5 entry for why no engine change was
//! needed). This module is pure, synchronous state -- no locks, no async, no network -- so it is
//! unit-tested directly; `daemon.rs` holds one of these behind a `std::sync::Mutex` and does the
//! actual fetching around it.

use phonia_core::queue::{ItemId, QueueSnapshot, QueueTrack};

/// How many tracks one refill adds: enough that the next refill (seeded by the newest of them)
/// has time to land before the batch itself runs out.
pub(crate) const BATCH: usize = 10;

/// How long a radio fetch is given before autoplay gives up on it for this entry. Playback is
/// never blocked waiting for this: it only bounds how long a stale in-flight fetch can outlive
/// its own relevance.
pub(crate) const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// One fetch in flight at a time, keyed by generation so a superseded or cancelled one is told
/// apart from the one that matters: `finish` only acts on the answer if its generation is still
/// current, which is bumped by every `begin` for a different entry and by every `cancel`.
#[derive(Debug, Default)]
pub(crate) struct Autoplay {
    generation: u64,
    /// The entry the current (or last) fetch was seeded for.
    entry: Option<ItemId>,
    fetching: bool,
    /// Set if the queue ran dry (`Event::QueueExhausted`) while a fetch for its own entry was
    /// still in flight: the daemon should resume playback once that fetch lands, onto whatever
    /// it added.
    resume: bool,
    /// The most recent TIDAL track to actually start, for seeding a refill when the entry that
    /// just became last is a local file (it has nothing of its own to seed with).
    last_tidal: Option<String>,
}

impl Autoplay {
    /// A TIDAL track started: remembered as the seed a later, local-file-seeded refill can fall
    /// back to. A local file (`None`) changes nothing -- the last TIDAL track is still the last
    /// one, whatever has played since.
    pub fn note_started(&mut self, tidal_id: Option<String>) {
        if let Some(id) = tidal_id {
            self.last_tidal = Some(id);
        }
    }

    /// Starts a fetch for `entry`, unless one is already running for it. Returns the generation
    /// to pass to `finish` once it resolves, or `None` if nothing should be fetched (already
    /// fetching for this exact entry). A fetch already running for a *different* entry is made
    /// stale by this call (its own `finish` will see a generation that is no longer current).
    pub fn begin(&mut self, entry: ItemId) -> Option<u64> {
        if self.fetching && self.entry == Some(entry) {
            return None;
        }
        self.generation += 1;
        self.entry = Some(entry);
        self.fetching = true;
        self.resume = false;
        Some(self.generation)
    }

    /// The queue ran dry: if a fetch for the entry that just ended is still in flight, resuming
    /// playback once it lands is now this fetch's job.
    pub fn queue_exhausted(&mut self, current: Option<ItemId>) {
        if self.fetching && self.entry == current {
            self.resume = true;
        }
    }

    /// A fetch for `generation` resolved. `None` if it is no longer the one in flight (a newer
    /// entry became due, or the fetch was cancelled) -- its answer must not be acted on either
    /// way. Otherwise `Some(resume)`: whether the queue ran dry while it was in flight, so its
    /// answer, if it adds anything, should resume playback.
    pub fn finish(&mut self, generation: u64) -> Option<bool> {
        if !self.fetching || generation != self.generation {
            return None;
        }
        self.fetching = false;
        Some(std::mem::take(&mut self.resume))
    }

    /// An explicit Stop or a cleared queue: any fetch in flight becomes stale, and is not acted
    /// on, and must not resume playback once it lands.
    pub fn cancel(&mut self) {
        self.generation += 1;
        self.entry = None;
        self.fetching = false;
        self.resume = false;
    }

    /// The id to seed `entry`'s radio from: its own TIDAL id if it has one, else the last TIDAL
    /// track that actually started this session. `None` when neither exists (nothing TIDAL has
    /// played yet) -- there is nothing to fetch a radio from.
    pub fn seed(&self, queue: &QueueSnapshot, entry: ItemId) -> Option<String> {
        let own = queue
            .items
            .iter()
            .find(|item| item.id == entry)
            .and_then(|item| phonia_core::openers::Source::parse(&item.track.source.0).ok())
            .and_then(|source| match source {
                phonia_core::openers::Source::Tidal(id) => Some(id),
                phonia_core::openers::Source::File(_) => None,
            });
        own.or_else(|| self.last_tidal.clone())
    }
}

/// Up to `n` of `candidates`, in order, skipping any whose source is already in `queue` or
/// earlier in `candidates` itself (TIDAL's own radio can repeat a track across pages, and a
/// refill must not re-add something still sitting in the queue from an earlier one).
pub(crate) fn pick(
    candidates: Vec<QueueTrack>,
    queue: &QueueSnapshot,
    n: usize,
) -> Vec<QueueTrack> {
    let mut seen: std::collections::HashSet<String> = queue
        .items
        .iter()
        .map(|item| item.track.source.0.clone())
        .collect();
    let mut picked = Vec::with_capacity(n.min(candidates.len()));
    for track in candidates {
        if picked.len() >= n {
            break;
        }
        if seen.insert(track.source.0.clone()) {
            picked.push(track);
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(source: &str) -> QueueTrack {
        QueueTrack {
            source: phonia_core::engine::TrackRef(source.to_string()),
            title: None,
            artist: None,
            duration: None,
            cover: None,
            album_id: None,
        }
    }

    fn snapshot_with(ids_and_sources: &[(u64, &str)]) -> QueueSnapshot {
        QueueSnapshot {
            version: 1,
            items: ids_and_sources
                .iter()
                .map(|(id, source)| phonia_core::queue::QueueItem {
                    id: ItemId(*id),
                    track: track(source),
                })
                .collect(),
            order: ids_and_sources.iter().map(|(id, _)| ItemId(*id)).collect(),
            current: ids_and_sources.last().map(|(id, _)| ItemId(*id)),
            shuffle: false,
            repeat: phonia_core::queue::Repeat::Off,
            autoplay: true,
        }
    }

    #[test]
    fn begin_refuses_a_second_fetch_for_the_same_entry_but_not_for_a_different_one() {
        let mut state = Autoplay::default();
        let first = state.begin(ItemId(1)).unwrap();
        assert_eq!(state.begin(ItemId(1)), None, "already fetching for entry 1");

        let second = state.begin(ItemId(2)).unwrap();
        assert_ne!(first, second, "a different entry gets a fresh generation");
        // The first fetch is now stale: its own generation no longer matches.
        assert_eq!(state.finish(first), None);
        assert_eq!(state.finish(second), Some(false));
    }

    #[test]
    fn cancel_makes_the_in_flight_fetch_stale_and_drops_any_pending_resume() {
        let mut state = Autoplay::default();
        let generation = state.begin(ItemId(1)).unwrap();
        state.queue_exhausted(Some(ItemId(1)));
        state.cancel();
        assert_eq!(state.finish(generation), None, "cancelled: not acted on");

        // A fresh begin for the same entry works again, with no leftover resume.
        let generation = state.begin(ItemId(1)).unwrap();
        assert_eq!(state.finish(generation), Some(false));
    }

    #[test]
    fn queue_exhausted_sets_resume_only_for_the_entry_actually_being_fetched_for() {
        let mut state = Autoplay::default();
        let generation = state.begin(ItemId(1)).unwrap();

        state.queue_exhausted(Some(ItemId(2)));
        assert_eq!(
            state.finish(generation),
            Some(false),
            "exhausted for a different entry: not this fetch's concern"
        );

        let generation = state.begin(ItemId(1)).unwrap();
        state.queue_exhausted(Some(ItemId(1)));
        assert_eq!(state.finish(generation), Some(true));
    }

    #[test]
    fn finish_without_a_matching_begin_is_a_no_op() {
        let mut state = Autoplay::default();
        assert_eq!(state.finish(1), None, "nothing was ever begun");
    }

    #[test]
    fn seed_prefers_the_entrys_own_tidal_id_falls_back_to_the_last_one_and_then_to_none() {
        let mut state = Autoplay::default();
        let queue = snapshot_with(&[(1, "tidal:100"), (2, "file:/a.flac")]);

        assert_eq!(state.seed(&queue, ItemId(1)), Some("100".to_string()));
        assert_eq!(
            state.seed(&queue, ItemId(2)),
            None,
            "a local file, and nothing TIDAL has played yet"
        );

        state.note_started(Some("100".to_string()));
        assert_eq!(
            state.seed(&queue, ItemId(2)),
            Some("100".to_string()),
            "falls back to the last TIDAL track that actually started"
        );

        state.note_started(None);
        assert_eq!(
            state.seed(&queue, ItemId(2)),
            Some("100".to_string()),
            "a local file starting does not erase the last TIDAL id"
        );
    }

    #[test]
    fn pick_skips_sources_already_queued_and_duplicates_within_the_batch_and_caps_at_n() {
        let queue = snapshot_with(&[(1, "tidal:1"), (2, "tidal:2")]);
        let candidates = vec![
            track("tidal:2"), // already in the queue
            track("tidal:3"),
            track("tidal:3"), // repeated by TIDAL's own radio
            track("tidal:4"),
            track("tidal:5"),
        ];
        let picked = pick(candidates, &queue, 2);
        assert_eq!(
            picked
                .iter()
                .map(|t| t.source.0.as_str())
                .collect::<Vec<_>>(),
            ["tidal:3", "tidal:4"]
        );
    }

    #[test]
    fn pick_from_fewer_candidates_than_n_returns_all_of_them() {
        let queue = snapshot_with(&[]);
        let candidates = vec![track("tidal:1"), track("tidal:2")];
        assert_eq!(pick(candidates, &queue, 10).len(), 2);
    }
}
