//! The library: favorite tracks, favorite albums, and the root of TIDAL's own playlist folder
//! tree (the Playlists tab; see `phonia_core::catalog::FolderEntry`'s own doc for why a folder's
//! playlists are not filtered down to owned-only the way the other two lists are).
//!
//! Unlike a search, there is nothing to type: the moment its section is first shown, the app asks
//! the daemon for it (see `maybe_load_library` in `app.rs`), once, the same request every time —
//! there is no "nothing yet" state to sit in beforehand the way a search waits for a query, so a
//! [`LibraryState`] only exists from the instant that request is sent ([`app::State::library`] is
//! `None` until then). The root folder is a second, separate request (`playlist_request`), fired
//! at the same moment but completing on its own.

use crate::browse::Phase;
use crate::cursor::Cursor;
use crate::list::Found;
use phonia_ipc::{AlbumSummary, FolderEntry, Payload, Request, TrackSummary};

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
    /// A row of the Playlists tab, now the root of TIDAL's own folder tree: either a sub-folder
    /// or a playlist (which may be one only followed, not owned -- see
    /// `phonia_core::catalog::FolderEntry`'s own doc for why).
    Entry(&'a FolderEntry),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryState {
    /// Counts the one request that loads favorite tracks and favorite albums (`Payload::Library`),
    /// and guards a late answer to it after the connection was lost meanwhile (the same way a
    /// search's generation does). Also guards the root folder's own initial fetch below, fired at
    /// the same moment: both are invalidated together on a reconnect.
    pub generation: u64,
    pub phase: Phase,
    pub tab: LibraryTab,
    pub favorite_tracks: Found<TrackSummary>,
    pub favorite_albums: Found<AlbumSummary>,
    /// The root of "My Collection" (the Playlists tab): sub-folders and playlists, in the order
    /// TIDAL's own API already sorts them. Fetched by its own request (`playlist_request`), not
    /// `Payload::Library`'s own `my_playlists` field -- that field is folder-unaware (a flat,
    /// owned-only list), so it stays on the wire for an older client but is no longer read here.
    pub playlists: Found<FolderEntry>,
    /// Whether the root folder's own first page has arrived: independent of `phase`, since it is
    /// a separate request that can succeed or fail on its own.
    pub playlists_phase: Phase,
}

impl LibraryState {
    /// A library about to be loaded, and the request that loads favorite tracks and favorite
    /// albums. The root folder (the Playlists tab) needs a second, separate request: see
    /// `playlist_request`.
    pub fn new() -> (Self, Request) {
        (
            Self {
                generation: 0,
                phase: Phase::Loading,
                tab: LibraryTab::default(),
                favorite_tracks: Found::default(),
                favorite_albums: Found::default(),
                playlists: Found::default(),
                playlists_phase: Phase::Loading,
            },
            Request::Library {
                limit: Some(PAGE_SIZE),
            },
        )
    }

    /// The request that loads the first page of the root of "My Collection", fired at the same
    /// moment as `new`'s own request.
    pub fn playlist_request() -> Request {
        Request::PlaylistFolder {
            folder: None,
            offset: 0,
            limit: Some(PAGE_SIZE),
        }
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

    /// Takes the daemon's answer to the request that loads favorite tracks and favorite albums.
    /// `false` if it was something else (which is then a failure). `my_playlists` is deliberately
    /// not read: see the field's own doc.
    pub fn finish(&mut self, payload: Payload) -> bool {
        let Payload::Library {
            favorite_tracks,
            favorite_albums,
            my_playlists: _,
        } = payload
        else {
            self.fail("the daemon answered something other than a library".to_string());
            return false;
        };
        self.favorite_tracks = Found::from_page(favorite_tracks);
        self.favorite_albums = Found::from_page(favorite_albums);
        self.phase = Phase::Done;
        true
    }

    pub fn fail(&mut self, reason: String) {
        self.phase = Phase::Failed(reason);
    }

    /// Takes the daemon's answer to `playlist_request`: the root folder's first page. `false` if
    /// it was something else.
    pub fn finish_playlists(&mut self, payload: Payload) -> bool {
        let Payload::PlaylistFolder { page, .. } = payload else {
            self.fail_playlists(
                "the daemon answered something other than a playlist folder".to_string(),
            );
            return false;
        };
        self.playlists = Found::from_page(page);
        self.playlists_phase = Phase::Done;
        true
    }

    pub fn fail_playlists(&mut self, reason: String) {
        self.playlists_phase = Phase::Failed(reason);
    }

    /// Whether the tab shown has finished its own (possibly separate) initial load.
    fn tab_ready(&self, tab: LibraryTab) -> bool {
        match tab {
            LibraryTab::Playlists => self.playlists_phase == Phase::Done,
            LibraryTab::FavoriteTracks | LibraryTab::FavoriteAlbums => self.phase == Phase::Done,
        }
    }

    /// The next page of the tab shown, if the cursor is close to its end and there is more: the
    /// request for it, with the list marked as waiting for it.
    pub fn next_page(&mut self, tab: LibraryTab) -> Option<Request> {
        if !self.tab_ready(tab) {
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
            LibraryTab::Playlists => Request::PlaylistFolder {
                folder: None,
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
            (LibraryTab::Playlists, Payload::PlaylistFolder { page, .. }) => {
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
                .map(Selected::Entry),
        }
    }

    /// The connection went away: if it was still loading, this (and the root folder's own
    /// request) will never be answered, and if either were, late, the answer must not count.
    pub fn connection_lost(&mut self) {
        let was_loading = self.phase == Phase::Loading || self.playlists_phase == Phase::Loading;
        if self.phase == Phase::Loading {
            self.phase = Phase::Failed("the connection to the daemon was lost".to_string());
        }
        if self.playlists_phase == Phase::Loading {
            self.playlists_phase =
                Phase::Failed("the connection to the daemon was lost".to_string());
        }
        if was_loading {
            self.generation += 1;
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

        // One already loaded (both the library and the root folder) is not touched by a later
        // disconnect.
        let (mut done, _) = LibraryState::new();
        done.finish(library_payload(vec![track("1")], 1));
        done.finish_playlists(Payload::PlaylistFolder {
            folder: None,
            page: page(vec![], 0),
        });
        let generation = done.generation;
        done.connection_lost();
        assert_eq!(done.phase, Phase::Done);
        assert_eq!(done.playlists_phase, Phase::Done);
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

    fn folder_entry(id: &str, name: &str) -> FolderEntry {
        FolderEntry::Folder {
            id: id.into(),
            name: name.into(),
            item_count: 1,
        }
    }

    fn playlist_entry(id: &str, title: &str) -> FolderEntry {
        FolderEntry::Playlist(phonia_ipc::PlaylistSummary {
            id: id.into(),
            title: title.into(),
            creator: None,
            description: None,
            track_count: None,
            duration_ms: None,
            cover: None,
        })
    }

    #[test]
    fn a_new_library_also_carries_the_root_folders_own_request() {
        let (library, _) = LibraryState::new();
        assert_eq!(library.playlists_phase, Phase::Loading);
        assert_eq!(
            LibraryState::playlist_request(),
            Request::PlaylistFolder {
                folder: None,
                offset: 0,
                limit: Some(PAGE_SIZE),
            }
        );
    }

    #[test]
    fn the_root_folders_own_answer_fills_the_playlists_tab_independently_of_the_rest() {
        let (mut library, _) = LibraryState::new();
        assert!(library.finish_playlists(Payload::PlaylistFolder {
            folder: None,
            page: page(
                vec![
                    folder_entry("f1", "Moods"),
                    playlist_entry("p1", "Road trip")
                ],
                2
            ),
        }));
        assert_eq!(library.playlists_phase, Phase::Done);
        assert_eq!(library.playlists.items.len(), 2);
        // The rest of the library is untouched -- these are two separate requests.
        assert_eq!(library.phase, Phase::Loading);
    }

    #[test]
    fn an_answer_that_is_not_a_playlist_folder_fails_only_the_playlists_tab() {
        let (mut library, _) = LibraryState::new();
        assert!(!library.finish_playlists(Payload::Ack));
        assert!(matches!(library.playlists_phase, Phase::Failed(_)));
        assert_eq!(library.phase, Phase::Loading, "unrelated to the rest");
    }

    #[test]
    fn a_folder_or_a_playlist_row_is_told_apart_as_the_selected_entry() {
        let (mut library, _) = LibraryState::new();
        library.finish_playlists(Payload::PlaylistFolder {
            folder: None,
            page: page(
                vec![
                    folder_entry("f1", "Moods"),
                    playlist_entry("p1", "Road trip"),
                ],
                2,
            ),
        });
        library.tab = LibraryTab::Playlists;
        let Some(Selected::Entry(FolderEntry::Folder { name, .. })) = library.selected() else {
            panic!("expected the folder at the top");
        };
        assert_eq!(name, "Moods");
        library.playlists.cursor.down(2);
        assert!(matches!(
            library.selected(),
            Some(Selected::Entry(FolderEntry::Playlist(_)))
        ));
    }

    #[test]
    fn the_playlists_tab_pages_through_the_root_folder_not_the_flat_list() {
        let (mut library, _) = LibraryState::new();
        library.finish_playlists(Payload::PlaylistFolder {
            folder: None,
            page: page(
                (0..50)
                    .map(|n| playlist_entry(&n.to_string(), "x"))
                    .collect(),
                120,
            ),
        });
        library.playlists.cursor.last(50);
        assert_eq!(
            library.next_page(LibraryTab::Playlists),
            Some(Request::PlaylistFolder {
                folder: None,
                offset: 50,
                limit: Some(PAGE_SIZE),
            })
        );
        library.add_page(
            LibraryTab::Playlists,
            Payload::PlaylistFolder {
                folder: None,
                page: page(
                    (50..100)
                        .map(|n| playlist_entry(&n.to_string(), "x"))
                        .collect(),
                    120,
                ),
            },
        );
        assert_eq!(library.playlists.items.len(), 100);
    }

    #[test]
    fn next_page_of_the_playlists_tab_waits_for_its_own_first_page_not_the_rest_of_the_library() {
        let (mut library, _) = LibraryState::new();
        library.finish(library_payload(vec![track("1")], 1));
        // The library's own two lists are done, but the root folder has not answered yet.
        assert_eq!(library.next_page(LibraryTab::Playlists), None);
    }

    #[test]
    fn losing_the_connection_while_only_the_root_folder_is_still_loading_fails_just_that() {
        let (mut library, _) = LibraryState::new();
        library.finish(library_payload(vec![track("1")], 1));
        let generation = library.generation;
        library.connection_lost();
        assert_eq!(library.phase, Phase::Done, "already finished, untouched");
        assert!(matches!(library.playlists_phase, Phase::Failed(_)));
        assert_ne!(
            library.generation, generation,
            "the late answer will not match"
        );
    }
}
