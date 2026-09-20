//! The queue's sequencing state machine: pure data, no locks, no async, so every rule can be
//! tested deterministically.

use super::{ItemId, QueueItem, QueueSnapshot, QueueTrack, Repeat};
use crate::engine::Advance;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;

/// How many played entries `Previous` can walk back through.
const HISTORY_LIMIT: usize = 200;

/// Where `advance` measures from.
enum Base {
    /// An entry that is (or is about to be) playing, and its place in the play order.
    At(usize),
    /// The playing entry was removed: this is the play-order slot it occupied, which now holds
    /// whatever followed it.
    Vacated(usize),
    /// Nothing has played yet.
    Nothing,
}

pub(super) struct Inner {
    next_id: u64,
    /// The queue as a list shows it.
    items: Vec<QueueItem>,
    /// The order entries are played in: the same as `items` unless shuffled.
    order: Vec<ItemId>,
    /// The entry the engine last opened.
    current: Option<ItemId>,
    /// What `advance` last answered, until the engine actually opens something. Lets two
    /// advances in a row chain, and keeps `advance` from moving the cursor for a load the engine
    /// may abandon.
    pending: Option<ItemId>,
    /// Whether `pending` was answered to `Previous`: opening it is then a step back, and the
    /// entry being left must not be recorded as "played before" it.
    pending_backward: bool,
    /// Entries played before `current`, most recent last, for `Previous`.
    history: Vec<ItemId>,
    /// Set when the current entry was removed; see [`Base::Vacated`].
    vacated: Option<usize>,
    repeat: Repeat,
    shuffle: bool,
    rng: StdRng,
    version: u64,
}

impl Inner {
    pub(super) fn new(seed: u64) -> Self {
        Self {
            next_id: 1,
            items: Vec::new(),
            order: Vec::new(),
            current: None,
            pending: None,
            pending_backward: false,
            history: Vec::new(),
            vacated: None,
            repeat: Repeat::Off,
            shuffle: false,
            rng: StdRng::seed_from_u64(seed),
            version: 0,
        }
    }

    pub(super) fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            version: self.version,
            items: self.items.clone(),
            order: self.order.clone(),
            current: self.current,
            shuffle: self.shuffle,
            repeat: self.repeat,
        }
    }

    #[cfg(test)]
    pub(super) fn history_len(&self) -> usize {
        self.history.len()
    }

    pub(super) fn item(&self, id: ItemId) -> Option<&QueueItem> {
        self.items.iter().find(|item| item.id == id)
    }

    pub(super) fn record_meta(&mut self, id: ItemId, title: Option<&str>, duration: Option<std::time::Duration>) {
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else { return };
        let mut changed = false;
        if item.track.title.is_none() && title.is_some() {
            item.track.title = title.map(str::to_string);
            changed = true;
        }
        if item.track.duration.is_none() && duration.is_some() {
            item.track.duration = duration;
            changed = true;
        }
        if changed {
            self.version += 1;
        }
    }

    fn new_items(&mut self, tracks: impl IntoIterator<Item = QueueTrack>) -> Vec<QueueItem> {
        tracks
            .into_iter()
            .map(|track| {
                let id = ItemId(self.next_id);
                self.next_id += 1;
                QueueItem { id, track }
            })
            .collect()
    }

    fn position(&self, id: ItemId) -> Option<usize> {
        self.order.iter().position(|candidate| *candidate == id)
    }

    // ---- editing -------------------------------------------------------------------------

    /// Appends to the end of the queue and of the play order.
    pub(super) fn add(&mut self, tracks: impl IntoIterator<Item = QueueTrack>) -> Vec<ItemId> {
        let at = self.items.len();
        self.insert(at, tracks)
    }

    /// Inserts at `at` in queue order (clamped). While shuffled the entries still go to the end
    /// of the play order: they are played once this cycle, and where they land is predictable.
    pub(super) fn insert(&mut self, at: usize, tracks: impl IntoIterator<Item = QueueTrack>) -> Vec<ItemId> {
        let new = self.new_items(tracks);
        let ids: Vec<ItemId> = new.iter().map(|item| item.id).collect();
        let at = at.min(self.items.len());
        let order_at = if self.shuffle { self.order.len() } else { at };
        self.items.splice(at..at, new);
        self.insert_in_order(order_at, &ids);
        self.version += 1;
        ids
    }

    /// Inserts right after the entry that is playing (or first, if nothing is), in both the
    /// queue and the play order.
    pub(super) fn play_next(&mut self, tracks: impl IntoIterator<Item = QueueTrack>) -> Vec<ItemId> {
        let new = self.new_items(tracks);
        let ids: Vec<ItemId> = new.iter().map(|item| item.id).collect();

        let base = self.current;
        let items_at = base
            .and_then(|id| self.items.iter().position(|item| item.id == id))
            .map_or_else(|| self.vacated.unwrap_or(0).min(self.items.len()), |index| index + 1);
        let order_at = base
            .and_then(|id| self.position(id))
            .map_or_else(|| self.vacated.unwrap_or(0).min(self.order.len()), |index| index + 1);

        self.items.splice(items_at..items_at, new);
        self.insert_in_order(order_at, &ids);
        self.version += 1;
        ids
    }

    fn insert_in_order(&mut self, at: usize, ids: &[ItemId]) {
        let at = at.min(self.order.len());
        self.order.splice(at..at, ids.iter().copied());
        // Entries added before the slot a removed entry left push it along; one added exactly
        // at the slot takes it over, which is what "next" should then be.
        if let Some(vacated) = &mut self.vacated
            && at < *vacated
        {
            *vacated += ids.len();
        }
    }

    /// Removes entries, returning how many existed. Removing the current entry leaves playback
    /// to the caller (the queue only answers what comes next).
    pub(super) fn remove(&mut self, ids: &[ItemId]) -> usize {
        let mut removed = 0;
        for &id in ids {
            let Some(index) = self.items.iter().position(|item| item.id == id) else { continue };
            self.items.remove(index);
            removed += 1;

            let slot = self.position(id);
            if let Some(slot) = slot {
                self.order.remove(slot);
            }
            if self.current == Some(id) {
                self.current = None;
                self.vacated = slot;
            } else if let (Some(vacated), Some(slot)) = (&mut self.vacated, slot)
                && slot < *vacated
            {
                *vacated -= 1;
            }
            if self.pending == Some(id) {
                self.pending = None;
            }
            self.history.retain(|played| *played != id);
        }
        if removed > 0 {
            self.version += 1;
        }
        removed
    }

    /// Moves an entry to position `to` in queue order (clamped). The cursor stays on the entry
    /// it was on, since it is an id. While shuffled the play order is left alone.
    pub(super) fn move_to(&mut self, id: ItemId, to: usize) -> bool {
        let Some(from) = self.items.iter().position(|item| item.id == id) else { return false };
        let item = self.items.remove(from);
        self.items.insert(to.min(self.items.len()), item);
        if !self.shuffle {
            self.order = self.items.iter().map(|item| item.id).collect();
        }
        self.version += 1;
        true
    }

    pub(super) fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.current = None;
        self.pending = None;
        self.pending_backward = false;
        self.history.clear();
        self.vacated = None;
        self.version += 1;
    }

    pub(super) fn set_repeat(&mut self, repeat: Repeat) {
        if self.repeat != repeat {
            self.repeat = repeat;
            self.version += 1;
        }
    }

    /// Turning shuffle on keeps the current entry current, at the head of a shuffled order of
    /// the rest; turning it off returns to queue order without moving the cursor.
    pub(super) fn set_shuffle(&mut self, shuffle: bool) {
        if self.shuffle == shuffle {
            return;
        }
        self.shuffle = shuffle;
        if shuffle {
            let current = self.current;
            let mut others: Vec<ItemId> = self.items.iter().map(|item| item.id).filter(|id| Some(*id) != current).collect();
            others.shuffle(&mut self.rng);
            self.order = current.into_iter().chain(others).collect();
            // The entry that took the place of a removed one is no longer meaningful.
            self.vacated = None;
        } else {
            self.order = self.items.iter().map(|item| item.id).collect();
            self.vacated = None;
        }
        self.version += 1;
    }

    // ---- sequencing ----------------------------------------------------------------------

    /// The entry `advance` measures from: whatever it last offered, or else what is playing.
    fn base_id(&self) -> Option<ItemId> {
        self.pending.filter(|id| self.position(*id).is_some()).or(self.current)
    }

    fn base(&self) -> Base {
        match self.base_id().and_then(|id| self.position(id)) {
            Some(position) => Base::At(position),
            None => self.vacated.map_or(Base::Nothing, Base::Vacated),
        }
    }

    /// What the engine should play for `how`. Records the answer as `pending`, and nothing else
    /// about the cursor: it moves when the engine opens the entry (see [`Inner::commit`]).
    pub(super) fn advance(&mut self, how: Advance) -> Option<ItemId> {
        let target = match how {
            Advance::Restart => self.base_id(),
            Advance::Auto => match self.repeat {
                Repeat::One => self.base_id().or_else(|| self.order.first().copied()),
                Repeat::Off => self.forward(false),
                Repeat::All => self.forward(true),
            },
            Advance::Next => self.forward(self.repeat != Repeat::Off),
            Advance::Previous => self.backward(self.repeat == Repeat::All),
        };
        self.pending = target;
        self.pending_backward = how == Advance::Previous && target.is_some();
        target
    }

    fn forward(&mut self, wrap: bool) -> Option<ItemId> {
        let next = match self.base() {
            Base::At(position) => position + 1,
            Base::Vacated(slot) => slot,
            Base::Nothing => 0,
        };
        if let Some(id) = self.order.get(next) {
            return Some(*id);
        }
        // Past the end: start over, or stop.
        (wrap && !self.order.is_empty()).then(|| self.wrap_around())
    }

    fn backward(&mut self, wrap: bool) -> Option<ItemId> {
        // What was actually played beats what the order says, so `Previous` after a shuffle or a
        // jump goes back to where the listener was.
        while let Some(&played) = self.history.last() {
            if self.position(played).is_some() {
                return Some(played);
            }
            self.history.pop();
        }
        match self.base() {
            Base::At(0) => wrap.then(|| self.order.last().copied()).flatten(),
            Base::At(position) => self.order.get(position - 1).copied(),
            Base::Vacated(slot) => slot.checked_sub(1).and_then(|previous| self.order.get(previous).copied()),
            Base::Nothing => self.order.first().copied(),
        }
    }

    /// Starts a new cycle of the play order and returns its first entry. A shuffled order is
    /// reshuffled, taking care not to open the new cycle with the entry that closed the last one.
    fn wrap_around(&mut self) -> ItemId {
        if self.shuffle {
            let last = self.base_id();
            self.order.shuffle(&mut self.rng);
            if self.order.len() >= 2 && Some(self.order[0]) == last {
                self.order.swap(0, 1);
            }
            self.version += 1;
        }
        self.order[0]
    }

    /// The engine opened `id`: it is now the current entry. Returns whether the entry exists.
    /// Opening the current entry again (a restart, a repeat, a seek that reopens the stream)
    /// changes nothing.
    pub(super) fn commit(&mut self, id: ItemId) -> bool {
        if self.item(id).is_none() {
            return false;
        }
        let stepping_back = self.pending == Some(id) && self.pending_backward;
        self.pending = None;
        self.pending_backward = false;
        self.vacated = None;
        if self.current == Some(id) {
            return true;
        }
        if self.history.last() == Some(&id) {
            // Going back through history: what was current is not "before" what we return to.
            self.history.pop();
        } else if stepping_back {
            // Going back through the order: likewise, or `Previous` would bounce between two
            // entries once the history runs out.
        } else if let Some(previous) = self.current {
            self.history.push(previous);
            if self.history.len() > HISTORY_LIMIT {
                self.history.remove(0);
            }
        }
        self.current = Some(id);
        self.version += 1;
        true
    }
}
