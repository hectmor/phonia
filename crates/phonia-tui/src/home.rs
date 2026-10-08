//! The Home section: a pure view over data the TUI already holds elsewhere (`state.status`,
//! `state.queue`, and later `state.library`), so it keeps no data of its own -- there is nothing
//! here to fetch that Queue or Library do not already ask for on their own. See #139.

use crate::app::State;
use phonia_ipc::{ItemId, Request};

/// What the "Continue" row offers right now, and what Enter on it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Continue {
    /// Something is playing, paused, loading or seeking: resumes exactly where it is.
    Resume { text: String },
    /// Stopped, but the queue still remembers what it was on (a real Stop drops the engine's own
    /// position, but never touches the *queue's* `current`): plays it again from the start.
    Replay { id: ItemId, text: String },
    /// Nothing has played yet this session, but the queue has something in it: starts it.
    Start { text: String },
    /// Nothing at all to continue: an empty queue.
    Empty,
}

impl Continue {
    /// The line this row shows.
    pub fn text(&self) -> &str {
        match self {
            Continue::Resume { text }
            | Continue::Replay { text, .. }
            | Continue::Start { text } => text,
            Continue::Empty => "Nothing to continue yet.",
        }
    }

    /// The request Enter on this row sends, if any.
    pub fn request(&self) -> Option<Request> {
        match self {
            Continue::Resume { .. } => Some(Request::Resume),
            Continue::Replay { id, .. } => Some(Request::Play { item: Some(*id) }),
            Continue::Start { .. } => Some(Request::Play { item: None }),
            Continue::Empty => None,
        }
    }
}

/// What the "Continue" row offers. The *engine's* own state, not the queue's, is what tells
/// "paused mid-track" apart from "stopped, but still sitting on something": a real Stop clears
/// `status.track` (and the position with it), but leaves the queue's own `current` exactly where
/// it was (it is only ever cleared by removing or clearing entries) -- so `status.track` being
/// absent, with `queue.current` still present, is specifically the "stopped, replay it" case, not
/// "nothing has ever played".
pub fn continuation(state: &State) -> Continue {
    if let Some(track) = state
        .status
        .as_ref()
        .and_then(|status| status.track.as_ref())
    {
        let name = phonia_ipc::fmt::track_name(
            track.title.as_deref(),
            track.artist.as_deref(),
            track.source.as_deref(),
        );
        let status = state.status.as_ref().expect("just matched its track");
        let when = match status.state {
            phonia_ipc::State::Paused => "paused",
            phonia_ipc::State::Seeking => "seeking",
            phonia_ipc::State::Loading => "loading",
            phonia_ipc::State::Playing | phonia_ipc::State::Stopped => "playing",
        };
        let position = phonia_ipc::fmt::ms(status.position_ms);
        let text = match status.duration_ms {
            Some(duration) => format!(
                "Continue: {name}  ({when} at {position}/{})",
                phonia_ipc::fmt::ms(duration)
            ),
            None => format!("Continue: {name}  ({when} at {position})"),
        };
        return Continue::Resume { text };
    }
    let Some(queue) = &state.queue else {
        return Continue::Empty;
    };
    if let Some(id) = queue.current {
        let item = queue.items.iter().find(|item| item.id == id);
        let name = item
            .map(|item| {
                phonia_ipc::fmt::track_name(
                    item.title.as_deref(),
                    item.artist.as_deref(),
                    Some(&item.source),
                )
            })
            .unwrap_or_else(|| "?".to_string());
        return Continue::Replay {
            id,
            text: format!("Play again: {name}"),
        };
    }
    if queue.items.is_empty() {
        Continue::Empty
    } else {
        let count = queue.items.len();
        let noun = if count == 1 { "track" } else { "tracks" };
        Continue::Start {
            text: format!("Start the queue ({count} {noun})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests_support::{queue, status};
    use phonia_ipc::{ItemId, QueueItem, Repeat, Track};

    fn state_with(status_: Option<phonia_ipc::Status>, queue_: Option<phonia_ipc::Queue>) -> State {
        State {
            status: status_,
            queue: queue_,
            ..State::default()
        }
    }

    fn item(id: u64, source: &str, title: Option<&str>) -> QueueItem {
        QueueItem {
            id: ItemId(id),
            source: source.to_string(),
            title: title.map(str::to_string),
            artist: None,
            duration_ms: None,
            cover: None,
        }
    }

    #[test]
    fn nothing_playing_and_an_empty_queue_has_nothing_to_continue() {
        let state = state_with(None, None);
        assert_eq!(continuation(&state), Continue::Empty);
        assert_eq!(continuation(&state).request(), None);

        let mut empty_queue = queue();
        empty_queue.items = Vec::new();
        let state = state_with(None, Some(empty_queue));
        assert_eq!(continuation(&state), Continue::Empty);
    }

    #[test]
    fn a_paused_track_resumes_at_its_own_position() {
        let mut playing = status();
        playing.track = Some(Track {
            item_id: Some(ItemId(1)),
            source: Some("tidal:1".into()),
            title: Some("Song".into()),
            artist: Some("Artist".into()),
            duration_ms: Some(200_000),
            quality: None,
            cover: None,
            replay_gain: None,
        });
        playing.state = phonia_ipc::State::Paused;
        playing.position_ms = 83_000;
        playing.duration_ms = Some(200_000);
        let state = state_with(Some(playing), None);
        let Continue::Resume { text } = continuation(&state) else {
            panic!("expected to resume")
        };
        assert_eq!(text, "Continue: Artist - Song  (paused at 1:23/3:20)");
        assert_eq!(continuation(&state).request(), Some(Request::Resume));
    }

    #[test]
    fn stopped_but_the_queue_remembers_its_current_entry_replays_it_from_the_start() {
        let mut q = queue();
        q.items = vec![item(1, "tidal:1", Some("Song"))];
        q.order = vec![ItemId(1)];
        q.current = Some(ItemId(1));
        let state = state_with(None, Some(q));
        let Continue::Replay { id, text } = continuation(&state) else {
            panic!("expected to replay")
        };
        assert_eq!(id, ItemId(1));
        assert_eq!(text, "Play again: Song");
        assert_eq!(
            continuation(&state).request(),
            Some(Request::Play {
                item: Some(ItemId(1))
            })
        );
    }

    #[test]
    fn a_queue_with_nothing_current_yet_starts_from_the_top() {
        let mut q = queue();
        q.items = vec![item(1, "tidal:1", None), item(2, "tidal:2", None)];
        q.order = vec![ItemId(1), ItemId(2)];
        q.current = None;
        let state = state_with(None, Some(q));
        let Continue::Start { text } = continuation(&state) else {
            panic!("expected to start")
        };
        assert_eq!(text, "Start the queue (2 tracks)");
        assert_eq!(
            continuation(&state).request(),
            Some(Request::Play { item: None })
        );
    }

    #[test]
    fn repeat_and_shuffle_do_not_change_what_continue_offers() {
        // Sanity: continuation only cares about status/queue.current, not these.
        let mut q = queue();
        q.items = vec![item(1, "tidal:1", Some("Song"))];
        q.current = Some(ItemId(1));
        q.repeat = Repeat::All;
        q.shuffle = true;
        let state = state_with(None, Some(q));
        assert!(matches!(continuation(&state), Continue::Replay { .. }));
    }
}
