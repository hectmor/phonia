//! The playback queue: an ordered list of tracks with shuffle and repeat, which the engine asks
//! for the next track to play.
//!
//! The queue answers [`TrackSupplier::advance`] (what comes next) and delegates
//! [`TrackSupplier::open`] to a [`TrackOpener`], which knows how to get audio for one track
//! (a file, TIDAL). Every entry has an [`ItemId`]; the [`TrackRef`] the engine sees encodes that
//! id rather than the source, so a track queued twice is two distinct entries and "play this
//! entry" is unambiguous.

mod inner;

use crate::engine::{Advance, LoadedTrack, Peek, TrackMeta, TrackOpener, TrackRef, TrackSupplier};
use anyhow::{Result, anyhow};
use futures_util::future::BoxFuture;
use inner::{Inner, Peeked};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::watch;

/// Identifies one entry of a queue for as long as the queue lives. Never reused, even after the
/// entry is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(pub u64);

impl ItemId {
    /// The reference the engine is given for this entry.
    pub fn track_ref(self) -> TrackRef {
        TrackRef(self.0.to_string())
    }

    /// The entry a reference from [`ItemId::track_ref`] stands for.
    pub fn from_ref(track: &TrackRef) -> Option<ItemId> {
        track.0.parse().ok().map(ItemId)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    /// Repeat the entry that just played, until the user skips.
    One,
    /// Start over after the last entry.
    All,
}

/// What a queue entry plays: a reference only the opener understands, plus what is already known
/// about it (more is filled in once it has been opened).
#[derive(Debug, Clone, PartialEq)]
pub struct QueueTrack {
    pub source: TrackRef,
    pub title: Option<String>,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueItem {
    pub id: ItemId,
    pub track: QueueTrack,
}

/// The whole queue at one moment, for a list to render.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueSnapshot {
    /// Increases with every change.
    pub version: u64,
    /// The entries in queue order.
    pub items: Vec<QueueItem>,
    /// The order they will be played in: the same ids as `items`, shuffled when `shuffle` is on.
    pub order: Vec<ItemId>,
    /// The entry that is playing, or was last.
    pub current: Option<ItemId>,
    pub shuffle: bool,
    pub repeat: Repeat,
}

/// A playback queue, shared between whoever edits it and the engine that plays from it.
///
/// One lock guards all the state and is never held across an `.await`: everything the engine asks
/// of the queue is quick CPU work. Every change publishes a fresh [`QueueSnapshot`] for
/// [`Queue::subscribe`]rs, so a list can render the queue without ever taking the lock.
pub struct Queue {
    inner: Mutex<Inner>,
    opener: Arc<dyn TrackOpener>,
    snapshots: watch::Sender<Arc<QueueSnapshot>>,
    /// Lets the futures returned by `open` report what they learn back to the queue.
    me: Weak<Queue>,
}

impl Queue {
    /// An empty queue whose shuffles are random.
    pub fn new(opener: Arc<dyn TrackOpener>) -> Arc<Self> {
        Self::with_seed(opener, rand::random())
    }

    /// An empty queue whose shuffles are reproducible from `seed`.
    pub fn with_seed(opener: Arc<dyn TrackOpener>, seed: u64) -> Arc<Self> {
        let inner = Inner::new(seed);
        let (snapshots, _) = watch::channel(Arc::new(inner.snapshot()));
        Arc::new_cyclic(|me| Queue {
            inner: Mutex::new(inner),
            opener,
            snapshots,
            me: me.clone(),
        })
    }

    /// Runs `change` on the state and publishes the result. Publishing while the lock is held
    /// (it never waits) keeps snapshots in the order the changes happened.
    fn mutate<R>(&self, change: impl FnOnce(&mut Inner) -> R) -> R {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.snapshot().version;
        let result = change(&mut inner);
        if inner.snapshot().version != before {
            self.snapshots.send_replace(Arc::new(inner.snapshot()));
        }
        result
    }

    /// Appends tracks to the end, returning their ids.
    pub fn add(&self, tracks: impl IntoIterator<Item = QueueTrack>) -> Vec<ItemId> {
        self.mutate(|inner| inner.add(tracks))
    }

    /// Inserts tracks at `at` (clamped) in queue order.
    pub fn insert(&self, at: usize, tracks: impl IntoIterator<Item = QueueTrack>) -> Vec<ItemId> {
        self.mutate(|inner| inner.insert(at, tracks))
    }

    /// Inserts tracks right after the one playing.
    pub fn play_next(&self, tracks: impl IntoIterator<Item = QueueTrack>) -> Vec<ItemId> {
        self.mutate(|inner| inner.play_next(tracks))
    }

    /// Removes entries, returning how many existed. The queue does not stop or skip a track
    /// that is playing; whoever drives the engine decides what to do about that.
    pub fn remove(&self, ids: &[ItemId]) -> usize {
        self.mutate(|inner| inner.remove(ids))
    }

    /// Moves an entry to position `to` in queue order (clamped). `false` if there is no such entry.
    pub fn move_to(&self, id: ItemId, to: usize) -> bool {
        self.mutate(|inner| inner.move_to(id, to))
    }

    pub fn clear(&self) {
        self.mutate(Inner::clear);
    }

    pub fn set_shuffle(&self, shuffle: bool) {
        self.mutate(|inner| inner.set_shuffle(shuffle));
    }

    pub fn set_repeat(&self, repeat: Repeat) {
        self.mutate(|inner| inner.set_repeat(repeat));
    }

    pub fn snapshot(&self) -> Arc<QueueSnapshot> {
        self.snapshots.borrow().clone()
    }

    /// The queue as it changes; the latest state is always there to read.
    pub fn subscribe(&self) -> watch::Receiver<Arc<QueueSnapshot>> {
        self.snapshots.subscribe()
    }

    /// Fills in the title and length of an entry that didn't have them, from what opening it
    /// revealed.
    pub fn record_meta(&self, id: ItemId, meta: &TrackMeta) {
        self.mutate(|inner| inner.record_meta(id, meta.title.as_deref(), meta.duration));
    }
}

impl TrackSupplier for Queue {
    fn advance(&self, how: Advance) -> Option<TrackRef> {
        self.mutate(|inner| inner.advance(how))
            .map(ItemId::track_ref)
    }

    fn open(&self, track: TrackRef, at: Duration) -> BoxFuture<'static, Result<LoadedTrack>> {
        // The entry becomes the current one as it is opened, not when it was offered: an offer
        // can be abandoned before anything is opened.
        self.open_entry(track, at, true)
    }

    fn peek(&self, how: Advance) -> Peek {
        match self.inner.lock().unwrap().peek(how) {
            Peeked::Next(id) => Peek::Next(id.track_ref()),
            Peeked::End => Peek::End,
            Peeked::Unknown => Peek::Unknown,
        }
    }

    fn open_ahead(&self, track: TrackRef) -> BoxFuture<'static, Result<LoadedTrack>> {
        // Opened, but not yet the current one: it may never be played (the queue can change
        // before its turn comes).
        self.open_entry(track, Duration::ZERO, false)
    }

    fn started(&self, track: &TrackRef) -> bool {
        match ItemId::from_ref(track) {
            Some(id) => self.mutate(|inner| inner.commit(id)),
            None => false,
        }
    }
}

impl Queue {
    /// Opens an entry. With `commit` it becomes the current one as it is opened.
    fn open_entry(
        &self,
        track: TrackRef,
        at: Duration,
        commit: bool,
    ) -> BoxFuture<'static, Result<LoadedTrack>> {
        let Some(id) = ItemId::from_ref(&track) else {
            return Box::pin(async move { Err(anyhow!("{:?} is not a queue entry", track.0)) });
        };
        let Some(item) = self.mutate(|inner| {
            if commit && !inner.commit(id) {
                return None;
            }
            inner.item(id).cloned()
        }) else {
            return Box::pin(
                async move { Err(anyhow!("the queue entry {} no longer exists", id.0)) },
            );
        };

        let opening = self.opener.open(item.track.source.clone(), at);
        let me = self.me.clone();
        Box::pin(async move {
            let mut loaded = opening.await?;
            // The engine identifies the track by this reference, and reopens it by it when it
            // seeks in a stream that can't rewind: it has to be one the queue understands.
            loaded.meta.track = id.track_ref();
            loaded.meta.title = loaded.meta.title.or(item.track.title);
            loaded.meta.duration = loaded.meta.duration.or(item.track.duration);
            if let Some(queue) = me.upgrade() {
                queue.record_meta(id, &loaded.meta);
            }
            Ok(loaded)
        })
    }
}

#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod tests;
