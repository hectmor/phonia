//! The library: favorite tracks, favorite albums, and the playlists the user created themselves.
//!
//! Unlike a search, there is nothing to type: the moment its section is first shown, the app asks
//! the daemon for it (see `maybe_load_library` in `app.rs`), once, the same request every time —
//! there is no "nothing yet" state to sit in beforehand the way a search waits for a query, so a
//! [`LibraryState`] only exists from the instant that request is sent ([`app::State::library`] is
//! `None` until then).

use crate::browse::Phase;
use crate::cursor::Cursor;
use crate::list::Found;
use phonia_ipc::{AlbumSummary, Payload, PlaylistSummary, Request, TrackSummary};

/// How many of each list one page asks for.
pub const PAGE_SIZE: u32 = 50;

/// The three lists the library has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LibraryTab {
    #[default]
    FavoriteTracks,
    FavoriteAlbums,
    Playlists,
}

impl LibraryTab {
    pub const ALL: [LibraryTab; 3] = [
        LibraryTab::FavoriteTracks,
        LibraryTab::FavoriteAlbums,
        LibraryTab::Playlists,
    ];

    pub fn title(self) -> &'static str {
        match self {
            LibraryTab::FavoriteTracks => "Favorite tracks",
            LibraryTab::FavoriteAlbums => "Favorite albums",
            LibraryTab::Playlists => "Your playlists",
        }
    }

    /// The next tab to the right, stopping at the last.
    pub fn next(self) -> LibraryTab {
        let index = LibraryTab::ALL
            .iter()
            .position(|tab| *tab == self)
            .unwrap_or(0);
        LibraryTab::ALL[(index + 1).min(LibraryTab::ALL.len() - 1)]
    }

    /// The next tab to the left, stopping at the first.
    pub fn previous(self) -> LibraryTab {
        let index = LibraryTab::ALL
            .iter()
            .position(|tab| *tab == self)
            .unwrap_or(0);
        LibraryTab::ALL[index.saturating_sub(1)]
    }
}

/// The row under the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selected<'a> {
    Track(&'a TrackSummary),
    Album(&'a AlbumSummary),
    Playlist(&'a PlaylistSummary),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryState {
    /// Counts the one request that loads it, and guards a late answer to it after the connection
    /// was lost meanwhile (the same way a search's generation does).
    pub generation: u64,
    pub phase: Phase,
    pub tab: LibraryTab,
    pub favorite_tracks: Found<TrackSummary>,
    pub favorite_albums: Found<AlbumSummary>,
    pub playlists: Found<PlaylistSummary>,
}

impl LibraryState {
    /// A library about to be loaded, and the request that loads it.
    pub fn new() -> (Self, Request) {
        (
            Self {
                generation: 0,
                phase: Phase::Loading,
                tab: LibraryTab::default(),
                favorite_tracks: Found::default(),
                favorite_albums: Found::default(),
                playlists: Found::default(),
            },
            Request::Library {
                limit: Some(PAGE_SIZE),
            },
        )
    }

    /// Where the cursor of each list is, to tell whether a key moved one.
    pub fn list_cursors(&self) -> [Cursor; 3] {
        [
            self.favorite_tracks.cursor,
            self.favorite_albums.cursor,
            self.playlists.cursor,
        ]
    }

    /// The cursor of a tab's list and how many rows the list has.
    pub fn list_of(&mut self, tab: LibraryTab) -> (&mut Cursor, usize) {
        match tab {
            LibraryTab::FavoriteTracks => (
                &mut self.favorite_tracks.cursor,
                self.favorite_tracks.items.len(),
            ),
            LibraryTab::FavoriteAlbums => (
                &mut self.favorite_albums.cursor,
                self.favorite_albums.items.len(),
            ),
            LibraryTab::Playlists => (&mut self.playlists.cursor, self.playlists.items.len()),
        }
    }

    /// Takes the daemon's answer to the request that loads it. `false` if it was something else
    /// (which is then a failure).
    pub fn finish(&mut self, payload: Payload) -> bool {
        let Payload::Library {
            favorite_tracks,
            favorite_albums,
            my_playlists,
        } = payload
        else {
            self.fail("the daemon answered something other than a library".to_string());
            return false;
        };
        self.favorite_tracks = Found::from_page(favorite_tracks);
        self.favorite_albums = Found::from_page(favorite_albums);
        self.playlists = Found::from_page(my_playlists);
        self.phase = Phase::Done;
        true
    }

    pub fn fail(&mut self, reason: String) {
        self.phase = Phase::Failed(reason);
    }

    /// The next page of the tab shown, if the cursor is close to its end and there is more: the
    /// request for it, with the list marked as waiting for it.
    pub fn next_page(&mut self, tab: LibraryTab) -> Option<Request> {
        if self.phase != Phase::Done {
            return None;
        }
        Some(match tab {
            LibraryTab::FavoriteTracks => Request::Tracks {
                from: phonia_ipc::CatalogRef::FavoriteTracks,
                offset: self.favorite_tracks.next_offset()?,
                limit: Some(PAGE_SIZE),
            },
            LibraryTab::FavoriteAlbums => Request::Albums {
                from: phonia_ipc::AlbumListRef::FavoriteAlbums,
                offset: self.favorite_albums.next_offset()?,
                limit: Some(PAGE_SIZE),
            },
            LibraryTab::Playlists => Request::Playlists {
                from: phonia_ipc::PlaylistListRef::Mine,
                offset: self.playlists.next_offset()?,
                limit: Some(PAGE_SIZE),
            },
        })
    }

    /// Adds the page that came for `tab`.
    pub fn add_page(&mut self, tab: LibraryTab, payload: Payload) {
        match (tab, payload) {
            (LibraryTab::FavoriteTracks, Payload::Tracks { page, .. }) => {
                self.favorite_tracks.append(Some(page))
            }
            (LibraryTab::FavoriteAlbums, Payload::Albums { page, .. }) => {
                self.favorite_albums.append(Some(page))
            }
            (LibraryTab::Playlists, Payload::Playlists { page, .. }) => {
                self.playlists.append(Some(page))
            }
            _ => self.page_failed(tab),
        }
    }

    /// The page asked for did not come: it can be asked for again.
    pub fn page_failed(&mut self, tab: LibraryTab) {
        match tab {
            LibraryTab::FavoriteTracks => self.favorite_tracks.loading = false,
            LibraryTab::FavoriteAlbums => self.favorite_albums.loading = false,
            LibraryTab::Playlists => self.playlists.loading = false,
        }
    }

    /// The row under the cursor in the list shown, if there is one.
    pub fn selected(&self) -> Option<Selected<'_>> {
        match self.tab {
            LibraryTab::FavoriteTracks => self
                .favorite_tracks
                .items
                .get(self.favorite_tracks.cursor.selected())
                .map(Selected::Track),
            LibraryTab::FavoriteAlbums => self
                .favorite_albums
                .items
                .get(self.favorite_albums.cursor.selected())
                .map(Selected::Album),
            LibraryTab::Playlists => self
                .playlists
                .items
                .get(self.playlists.cursor.selected())
                .map(Selected::Playlist),
        }
    }

    /// The connection went away: if it was still loading, this will never be answered, and if it
    /// were, late, the answer must not count.
    pub fn connection_lost(&mut self) {
        if self.phase == Phase::Loading {
            self.generation += 1;
            self.phase = Phase::Failed("the connection to the daemon was lost".to_string());
        }
    }

    /// How many of a list there are in all.
    pub fn total(&self, tab: LibraryTab) -> u64 {
        match tab {
            LibraryTab::FavoriteTracks => self.favorite_tracks.total,
            LibraryTab::FavoriteAlbums => self.favorite_albums.total,
            LibraryTab::Playlists => self.playlists.total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phonia_ipc::Page;

    fn page<T>(items: Vec<T>, total: u64) -> Page<T> {
        Page {
            items,
            total,
            offset: 0,
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

    fn library_payload(tracks: Vec<TrackSummary>, total: u64) -> Payload {
        Payload::Library {
            favorite_tracks: page(tracks, total),
            favorite_albums: page(vec![], 0),
            my_playlists: page(vec![], 0),
        }
    }

    #[test]
    fn a_new_library_is_loading_and_the_request_asks_for_all_three() {
        let (library, request) = LibraryState::new();
        assert_eq!(library.phase, Phase::Loading);
        assert_eq!(library.generation, 0);
        assert_eq!(
            request,
            Request::Library {
                limit: Some(PAGE_SIZE)
            }
        );
    }

    #[test]
    fn an_answer_fills_the_three_lists_with_their_totals() {
        let (mut library, _) = LibraryState::new();
        assert!(library.finish(library_payload(vec![track("1"), track("2")], 5)));
        assert_eq!(library.phase, Phase::Done);
        assert_eq!(library.favorite_tracks.items.len(), 2);
        assert_eq!(library.total(LibraryTab::FavoriteTracks), 5);
        assert_eq!(library.total(LibraryTab::FavoriteAlbums), 0);
        assert_eq!(library.total(LibraryTab::Playlists), 0);
    }

    #[test]
    fn an_answer_that_is_not_a_library_is_a_failure() {
        let (mut library, _) = LibraryState::new();
        assert!(!library.finish(Payload::Ack));
        assert!(matches!(library.phase, Phase::Failed(_)));
    }

    #[test]
    fn losing_the_connection_while_loading_ends_it_and_drops_a_late_answer() {
        let (mut library, _) = LibraryState::new();
        let waiting_for = library.generation;
        library.connection_lost();
        assert!(matches!(library.phase, Phase::Failed(_)));
        assert_ne!(
            library.generation, waiting_for,
            "the late answer will not match"
        );

        // One already loaded is not touched by a later disconnect.
        let (mut done, _) = LibraryState::new();
        done.finish(library_payload(vec![track("1")], 1));
        let generation = done.generation;
        done.connection_lost();
        assert_eq!(done.phase, Phase::Done);
        assert_eq!(done.generation, generation);
    }

    #[test]
    fn the_tabs_step_left_and_right_and_stop_at_the_ends() {
        assert_eq!(LibraryTab::default(), LibraryTab::FavoriteTracks);
        assert_eq!(
            LibraryTab::FavoriteTracks.next(),
            LibraryTab::FavoriteAlbums
        );
        assert_eq!(LibraryTab::FavoriteAlbums.next(), LibraryTab::Playlists);
        assert_eq!(LibraryTab::Playlists.next(), LibraryTab::Playlists);
        assert_eq!(LibraryTab::Playlists.previous(), LibraryTab::FavoriteAlbums);
        assert_eq!(
            LibraryTab::FavoriteTracks.previous(),
            LibraryTab::FavoriteTracks
        );
        assert_eq!(LibraryTab::Playlists.title(), "Your playlists");
    }

    #[test]
    fn the_next_page_of_each_tab_asks_the_right_request() {
        let (mut library, _) = LibraryState::new();
        library.finish(library_payload(vec![track("1")], 1));
        // Nothing more of a one-item, fully loaded list.
        assert_eq!(library.next_page(LibraryTab::FavoriteTracks), None);

        let mut long = LibraryState::new().0;
        long.finish(Payload::Library {
            favorite_tracks: page((0..50).map(|n| track(&n.to_string())).collect(), 120),
            favorite_albums: page(vec![], 0),
            my_playlists: page(vec![], 0),
        });
        long.favorite_tracks.cursor.last(50);
        assert_eq!(
            long.next_page(LibraryTab::FavoriteTracks),
            Some(Request::Tracks {
                from: phonia_ipc::CatalogRef::FavoriteTracks,
                offset: 50,
                limit: Some(PAGE_SIZE),
            })
        );
        assert!(long.favorite_tracks.loading);
    }

    #[test]
    fn a_page_that_comes_is_added_and_a_page_that_fails_can_be_asked_for_again() {
        let (mut library, _) = LibraryState::new();
        library.finish(Payload::Library {
            favorite_tracks: page((0..50).map(|n| track(&n.to_string())).collect(), 120),
            favorite_albums: page(vec![], 0),
            my_playlists: page(vec![], 0),
        });
        library.favorite_tracks.cursor.last(50);
        library.next_page(LibraryTab::FavoriteTracks);
        library.add_page(
            LibraryTab::FavoriteTracks,
            Payload::Tracks {
                from: phonia_ipc::CatalogRef::FavoriteTracks,
                page: page((50..100).map(|n| track(&n.to_string())).collect(), 120),
            },
        );
        assert_eq!(library.favorite_tracks.items.len(), 100);
        assert!(!library.favorite_tracks.loading);

        library.next_page(LibraryTab::FavoriteTracks);
        library.page_failed(LibraryTab::FavoriteTracks);
        assert!(!library.favorite_tracks.loading);
    }

    #[test]
    fn the_row_under_the_cursor_follows_the_tab() {
        let (mut library, _) = LibraryState::new();
        library.finish(library_payload(vec![track("1")], 1));
        assert!(matches!(library.selected(), Some(Selected::Track(_))));
        library.tab = LibraryTab::FavoriteAlbums;
        assert_eq!(library.selected(), None, "no favorite albums");
    }
}
