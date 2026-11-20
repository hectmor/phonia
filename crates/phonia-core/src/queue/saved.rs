//! The queue as saved across a daemon restart (see #37): plain data, no lock, no ids.

use super::{QueueSnapshot, QueueTrack, Repeat};
use crate::engine::TrackRef;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The file the queue is saved to, inside `recent::state_dir`.
pub const STATE_FILE: &str = "queue.json";

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTrack {
    pub source: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
    pub cover: Option<String>,
    pub album_id: Option<String>,
    pub artist_id: Option<String>,
    pub album: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedQueue {
    pub tracks: Vec<SavedTrack>,
    /// The play order, as indices into `tracks`.
    pub order: Vec<usize>,
    /// The index into `tracks` of the entry that was playing, or was last.
    pub current: Option<usize>,
    pub position_ms: u64,
    pub shuffle: bool,
    /// "off", "one" or "all"; anything else loads as off.
    pub repeat: String,
}

impl SavedQueue {
    pub fn from_snapshot(snapshot: &QueueSnapshot, position: Duration) -> Self {
        let index = |id| snapshot.items.iter().position(|item| item.id == id);
        Self {
            tracks: snapshot
                .items
                .iter()
                .map(|item| SavedTrack::from(&item.track))
                .collect(),
            order: snapshot.order.iter().filter_map(|id| index(*id)).collect(),
            current: snapshot.current.and_then(index),
            position_ms: position.as_millis() as u64,
            shuffle: snapshot.shuffle,
            repeat: match snapshot.repeat {
                Repeat::Off => "off",
                Repeat::One => "one",
                Repeat::All => "all",
            }
            .to_string(),
        }
    }

    pub fn position(&self) -> Duration {
        Duration::from_millis(self.position_ms)
    }

    pub(super) fn repeat(&self) -> Repeat {
        match self.repeat.as_str() {
            "one" => Repeat::One,
            "all" => Repeat::All,
            _ => Repeat::Off,
        }
    }

    /// The play order as indices, or queue order when the saved one is not a permutation of the
    /// tracks (a hand-edited or truncated file).
    pub(super) fn valid_order(&self) -> Vec<usize> {
        let mut seen = vec![false; self.tracks.len()];
        let permutation = self.order.len() == self.tracks.len()
            && self
                .order
                .iter()
                .all(|&i| seen.get_mut(i).is_some_and(|s| !std::mem::replace(s, true)));
        if permutation {
            self.order.clone()
        } else {
            (0..self.tracks.len()).collect()
        }
    }
}

impl From<&QueueTrack> for SavedTrack {
    fn from(track: &QueueTrack) -> Self {
        Self {
            source: track.source.0.clone(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            duration_ms: track.duration.map(|d| d.as_millis() as u64),
            cover: track.cover.clone(),
            album_id: track.album_id.clone(),
            artist_id: track.artist_id.clone(),
            album: track.album.clone(),
        }
    }
}

impl From<&SavedTrack> for QueueTrack {
    fn from(saved: &SavedTrack) -> Self {
        Self {
            source: TrackRef(saved.source.clone()),
            title: saved.title.clone(),
            artist: saved.artist.clone(),
            duration: saved.duration_ms.map(Duration::from_millis),
            cover: saved.cover.clone(),
            album_id: saved.album_id.clone(),
            artist_id: saved.artist_id.clone(),
            album: saved.album.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::inner::Inner;
    use super::*;

    fn track(name: &str) -> QueueTrack {
        QueueTrack {
            source: TrackRef(name.to_string()),
            title: Some(name.to_string()),
            artist: None,
            duration: Some(Duration::from_secs(10)),
            cover: None,
            album_id: None,
            artist_id: None,
            album: None,
        }
    }

    fn played_order(inner: &Inner) -> Vec<String> {
        let snapshot = inner.snapshot();
        snapshot
            .order
            .iter()
            .map(|id| inner.item(*id).unwrap().track.source.0.clone())
            .collect()
    }

    #[test]
    fn a_shuffled_queue_round_trips_with_its_current_and_position() {
        let mut source = Inner::new(7);
        let ids = source.add(["a", "b", "c", "d"].map(track));
        source.set_shuffle(true);
        source.set_repeat(Repeat::All);
        assert!(source.commit(ids[2]));
        let saved = SavedQueue::from_snapshot(&source.snapshot(), Duration::from_millis(1500));
        let saved: SavedQueue =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();

        let mut restored = Inner::new(99);
        let current = restored.restore(&saved).unwrap();
        let snapshot = restored.snapshot();
        assert_eq!(played_order(&restored), played_order(&source));
        assert_eq!(snapshot.current, Some(current));
        assert_eq!(restored.item(current).unwrap().track.source.0, "c");
        assert!(snapshot.shuffle);
        assert_eq!(snapshot.repeat, Repeat::All);
        assert_eq!(saved.position(), Duration::from_millis(1500));
    }

    #[test]
    fn an_order_that_is_not_a_permutation_falls_back_to_queue_order() {
        for order in [vec![0, 0, 1], vec![0, 1], vec![0, 1, 9]] {
            let saved = SavedQueue {
                tracks: ["a", "b", "c"]
                    .map(|n| SavedTrack::from(&track(n)))
                    .to_vec(),
                order,
                ..Default::default()
            };
            let mut inner = Inner::new(1);
            inner.restore(&saved);
            assert_eq!(played_order(&inner), ["a", "b", "c"]);
        }
    }

    #[test]
    fn a_current_out_of_range_restores_as_none() {
        let saved = SavedQueue {
            tracks: vec![SavedTrack::from(&track("a"))],
            current: Some(5),
            ..Default::default()
        };
        let mut inner = Inner::new(1);
        assert_eq!(inner.restore(&saved), None);
        assert_eq!(inner.snapshot().current, None);
        assert_eq!(inner.snapshot().items.len(), 1);
    }

    #[test]
    fn a_partial_file_loads_with_defaults() {
        let saved: SavedQueue = serde_json::from_str(r#"{"shuffle":true}"#).unwrap();
        assert!(saved.shuffle && saved.tracks.is_empty());
        assert_eq!(saved.repeat(), Repeat::Off);
    }
}
