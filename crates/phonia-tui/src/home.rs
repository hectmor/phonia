//! The Home section: a pure view over data the TUI already holds elsewhere (`state.status`,
//! `state.queue`, and `state.library`), so it keeps no data of its own -- there is nothing here to
//! fetch that Queue or Library do not already ask for on their own. See #139.

use crate::app::State;
use crate::browse::Phase;
use crate::library::LibraryTab;
use phonia_ipc::{AlbumSummary, FolderEntry, ItemId, Request, TrackSummary};

/// How many of a block's items show before its own "See all" row.
const BLOCK_SIZE: usize = 6;

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

/// One row of Home, in display order. `Header` and `Spacer` are never under the cursor -- see
/// [`Row::selectable`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row<'a> {
    Continue(Continue),
    /// A blank line set before a block, so it does not run straight into whatever came before.
    Spacer,
    /// A block's own title: the matching Library tab's, so the two agree on what to call it.
    Header(LibraryTab),
    Album(&'a AlbumSummary),
    Entry(&'a FolderEntry),
    Track(&'a TrackSummary),
    /// Closes a block: jumps into Library on `tab`, showing all `total` of it, not just the
    /// [`BLOCK_SIZE`] shown here.
    SeeAll {
        tab: LibraryTab,
        total: u64,
    },
}

impl Row<'_> {
    /// Whether this row is ever under the cursor: a header or a spacer is not a thing to act on.
    pub fn selectable(&self) -> bool {
        !matches!(self, Row::Header(_) | Row::Spacer)
    }
}

/// Every row Home shows right now: the Continue row, always, then one block per non-empty list
/// the library already holds (favorite albums, playlist folders, favorite tracks, in that order),
/// each capped at [`BLOCK_SIZE`] with a "See all" row into the rest. A list not loaded yet, or
/// loaded and empty, contributes no block at all -- there is nothing to show two ways (not ready,
/// or ready and empty), so neither needs its own row.
pub fn rows(state: &State) -> Vec<Row<'_>> {
    let mut rows = vec![Row::Continue(continuation(state))];
    let Some(library) = &state.library else {
        return rows;
    };
    if library.phase == Phase::Done {
        push_block(
            &mut rows,
            LibraryTab::FavoriteAlbums,
            &library.favorite_albums.items,
            library.favorite_albums.total,
            Row::Album,
        );
    }
    if library.playlists_phase == Phase::Done {
        push_block(
            &mut rows,
            LibraryTab::Playlists,
            &library.playlists.items,
            library.playlists.total,
            Row::Entry,
        );
    }
    if library.phase == Phase::Done {
        push_block(
            &mut rows,
            LibraryTab::FavoriteTracks,
            &library.favorite_tracks.items,
            library.favorite_tracks.total,
            Row::Track,
        );
    }
    rows
}

fn push_block<'a, T>(
    rows: &mut Vec<Row<'a>>,
    tab: LibraryTab,
    items: &'a [T],
    total: u64,
    row: impl Fn(&'a T) -> Row<'a>,
) {
    if items.is_empty() {
        return;
    }
    rows.push(Row::Spacer);
    rows.push(Row::Header(tab));
    rows.extend(items.iter().take(BLOCK_SIZE).map(row));
    rows.push(Row::SeeAll { tab, total });
}

/// The display index (into [`rows`]'s own list, headers and spacers included) of the `selected`-th
/// selectable row, if there is one that many.
pub fn display_index_of(rows: &[Row], selected: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.selectable())
        .nth(selected)
        .map(|(index, _)| index)
}

/// How many rows the cursor can move between.
pub fn selectable_count(rows: &[Row]) -> usize {
    rows.iter().filter(|row| row.selectable()).count()
}

/// What Enter on the `selected`-th selectable row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Nothing to do: a header, a spacer, past the end, or a row not wired up yet (opening an
    /// album, a playlist or a folder from Home nested in its own stack is #139's last part).
    Nothing,
    /// Sent as is.
    Request(Request),
    /// Plays just this one track, the same as Enter on a favorite track in the Library section.
    PlayTrack(TrackSummary),
    /// Jumps into the Library section, on this tab, to see the rest of a block.
    JumpTo(LibraryTab),
}

pub fn intent(state: &State, selected: usize) -> Intent {
    let rows = rows(state);
    match rows.iter().filter(|row| row.selectable()).nth(selected) {
        Some(Row::Continue(continue_)) => continue_
            .request()
            .map(Intent::Request)
            .unwrap_or(Intent::Nothing),
        Some(Row::Track(track)) => Intent::PlayTrack((*track).clone()),
        Some(Row::SeeAll { tab, .. }) => Intent::JumpTo(*tab),
        Some(Row::Album(_)) | Some(Row::Entry(_)) | Some(Row::Header(_)) | Some(Row::Spacer)
        | None => Intent::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests_support::{queue, status};
    use crate::browse::Phase;
    use crate::library::LibraryState;
    use phonia_ipc::{FolderEntry, ItemId, Page, Payload, QueueItem, Repeat, Track};

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

    fn page<T>(items: Vec<T>, total: u64) -> Page<T> {
        Page {
            items,
            total,
            offset: 0,
        }
    }

    fn album(id: &str) -> AlbumSummary {
        AlbumSummary {
            id: id.into(),
            title: format!("Album {id}"),
            version: None,
            artists: vec![],
            release_date: None,
            track_count: None,
            duration_ms: None,
            explicit: false,
            quality: None,
            kind: None,
            copyright: None,
            cover: None,
        }
    }

    fn track(id: &str) -> TrackSummary {
        TrackSummary {
            id: id.into(),
            title: format!("Song {id}"),
            version: None,
            artists: vec![],
            album: None,
            duration_ms: None,
            explicit: false,
            track_number: None,
            volume_number: None,
            quality: None,
            streamable: true,
        }
    }

    fn with_library(mut build: impl FnMut(&mut LibraryState)) -> State {
        let (mut library, _) = LibraryState::new();
        library.phase = Phase::Done;
        library.playlists_phase = Phase::Done;
        build(&mut library);
        State {
            library: Some(library),
            ..State::default()
        }
    }

    #[test]
    fn with_no_library_loaded_yet_there_is_only_the_continue_row() {
        let state = State::default();
        let rows = rows(&state);
        assert_eq!(rows, vec![Row::Continue(Continue::Empty)]);
        assert_eq!(selectable_count(&rows), 1);
    }

    #[test]
    fn an_empty_library_adds_no_blocks() {
        let state = with_library(|_| {});
        assert_eq!(rows(&state), vec![Row::Continue(Continue::Empty)]);
    }

    #[test]
    fn a_favorite_albums_block_ends_in_a_see_all_row_with_the_real_total() {
        let state = with_library(|library| {
            library.favorite_albums =
                crate::list::Found::from_page(page(vec![album("1"), album("2")], 9));
        });
        let rows = rows(&state);
        assert_eq!(
            rows,
            vec![
                Row::Continue(Continue::Empty),
                Row::Spacer,
                Row::Header(LibraryTab::FavoriteAlbums),
                Row::Album(&album("1")),
                Row::Album(&album("2")),
                Row::SeeAll {
                    tab: LibraryTab::FavoriteAlbums,
                    total: 9
                },
            ]
        );
        assert_eq!(selectable_count(&rows), 4, "continue, 2 albums, see all");
    }

    #[test]
    fn a_block_only_shows_the_first_six_even_with_more_loaded() {
        let state = with_library(|library| {
            library.favorite_tracks = crate::list::Found::from_page(page(
                (0..10).map(|n| track(&n.to_string())).collect(),
                10,
            ));
        });
        let rows = rows(&state);
        let tracks = rows
            .iter()
            .filter(|row| matches!(row, Row::Track(_)))
            .count();
        assert_eq!(tracks, BLOCK_SIZE);
        assert!(rows.contains(&Row::SeeAll {
            tab: LibraryTab::FavoriteTracks,
            total: 10
        }));
    }

    #[test]
    fn all_three_blocks_show_in_order_when_every_list_has_something() {
        let state = with_library(|library| {
            library.favorite_albums = crate::list::Found::from_page(page(vec![album("a")], 1));
            library.playlists = crate::list::Found::from_page(page(
                vec![FolderEntry::Folder {
                    id: "f".into(),
                    name: "Moods".into(),
                    item_count: 1,
                }],
                1,
            ));
            library.favorite_tracks = crate::list::Found::from_page(page(vec![track("t")], 1));
        });
        let rows = rows(&state);
        let headers: Vec<LibraryTab> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Header(tab) => Some(*tab),
                _ => None,
            })
            .collect();
        assert_eq!(
            headers,
            vec![
                LibraryTab::FavoriteAlbums,
                LibraryTab::Playlists,
                LibraryTab::FavoriteTracks,
            ]
        );
    }

    #[test]
    fn a_block_not_loaded_yet_contributes_nothing() {
        let (mut library, _) = LibraryState::new();
        // Only the plain library (tracks/albums) finished; the playlists request is still out.
        library.finish(Payload::Library {
            favorite_tracks: page(vec![track("1")], 1),
            favorite_albums: page(vec![], 0),
            my_playlists: page(vec![], 0),
        });
        let state = State {
            library: Some(library),
            ..State::default()
        };
        let rows = rows(&state);
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, Row::Header(LibraryTab::Playlists)))
        );
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::Header(LibraryTab::FavoriteTracks)))
        );
    }

    #[test]
    fn display_index_of_skips_headers_and_spacers() {
        let state = with_library(|library| {
            library.favorite_albums = crate::list::Found::from_page(page(vec![album("1")], 1));
        });
        let rows = rows(&state);
        // 0: Continue, 1: Spacer, 2: Header, 3: Album (selectable index 1), 4: SeeAll (index 2).
        assert_eq!(display_index_of(&rows, 0), Some(0));
        assert_eq!(display_index_of(&rows, 1), Some(3));
        assert_eq!(display_index_of(&rows, 2), Some(4));
        assert_eq!(display_index_of(&rows, 3), None);
        assert_eq!(selectable_count(&rows), 3);
    }

    #[test]
    fn intent_on_the_continue_row_is_its_own_request() {
        let mut q = queue();
        q.items = vec![item(1, "tidal:1", Some("Song"))];
        q.current = Some(ItemId(1));
        let state = state_with(None, Some(q));
        assert_eq!(
            intent(&state, 0),
            Intent::Request(Request::Play {
                item: Some(ItemId(1))
            })
        );
    }

    #[test]
    fn intent_on_a_favorite_track_plays_just_that_track() {
        let state = with_library(|library| {
            library.favorite_tracks = crate::list::Found::from_page(page(vec![track("1")], 1));
        });
        assert_eq!(intent(&state, 1), Intent::PlayTrack(track("1")));
    }

    #[test]
    fn intent_on_see_all_jumps_to_the_matching_tab() {
        let state = with_library(|library| {
            library.favorite_albums = crate::list::Found::from_page(page(vec![album("1")], 1));
        });
        assert_eq!(
            intent(&state, 2),
            Intent::JumpTo(LibraryTab::FavoriteAlbums)
        );
    }

    #[test]
    fn intent_on_an_album_or_a_folder_row_does_nothing_yet() {
        let state = with_library(|library| {
            library.favorite_albums = crate::list::Found::from_page(page(vec![album("1")], 1));
        });
        assert_eq!(intent(&state, 1), Intent::Nothing);
    }

    #[test]
    fn intent_past_the_end_does_nothing() {
        let state = State::default();
        assert_eq!(intent(&state, 5), Intent::Nothing);
    }
}
