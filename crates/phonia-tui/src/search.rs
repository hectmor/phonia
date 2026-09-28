//! What the search knows: the query, where it stands, and what it found.
//!
//! It is plain data with the few rules that keep it honest. Each search gets a new *generation*,
//! and an answer only counts if it is for the current one, so a slow answer to an old query can
//! never overwrite the results of a newer one, or appear after the connection was lost.

use crate::cursor::Cursor;
use crate::input::TextInput;
use phonia_ipc::{
    AlbumSummary, ArtistSummary, Page, Payload, PlaylistSummary, Request, TrackSummary,
};

/// How many results of each kind one search asks for.
pub const PAGE_SIZE: u32 = 50;

/// When the cursor is this close to the end of what is loaded, the next page is asked for.
pub const PREFETCH: usize = 10;

/// The four lists of results, one shown at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Tracks,
    Albums,
    Artists,
    Playlists,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Tracks, Tab::Albums, Tab::Artists, Tab::Playlists];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Tracks => "Tracks",
            Tab::Albums => "Albums",
            Tab::Artists => "Artists",
            Tab::Playlists => "Playlists",
        }
    }

    /// The next tab to the right, stopping at the last.
    pub fn next(self) -> Tab {
        let index = Tab::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        Tab::ALL[(index + 1).min(Tab::ALL.len() - 1)]
    }

    /// The next tab to the left, stopping at the first.
    pub fn previous(self) -> Tab {
        let index = Tab::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        Tab::ALL[index.saturating_sub(1)]
    }
}

impl Tab {
    /// The kind of result this tab lists, as the protocol names it.
    pub fn kind(self) -> phonia_ipc::CatalogKind {
        match self {
            Tab::Tracks => phonia_ipc::CatalogKind::Tracks,
            Tab::Albums => phonia_ipc::CatalogKind::Albums,
            Tab::Artists => phonia_ipc::CatalogKind::Artists,
            Tab::Playlists => phonia_ipc::CatalogKind::Playlists,
        }
    }
}

/// The result under the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selected<'a> {
    Track(&'a TrackSummary),
    Album(&'a AlbumSummary),
    Artist(&'a ArtistSummary),
    Playlist(&'a PlaylistSummary),
}

/// Where the search stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    /// Nothing has been searched yet.
    #[default]
    Idle,
    /// Waiting for the daemon's answer.
    Searching,
    /// The last search did not work, and why.
    Failed(String),
    /// The results are in.
    Done,
}

/// What was found of one kind: the rows loaded so far, how many there are in all, and the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub cursor: Cursor,
    /// Whether the next page has been asked for and not come yet.
    pub loading: bool,
}

impl<T> Default for Found<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            total: 0,
            cursor: Cursor::default(),
            loading: false,
        }
    }
}

impl<T> Found<T> {
    fn from_page(page: Page<T>) -> Self {
        Self {
            items: page.items,
            total: page.total,
            cursor: Cursor::default(),
            loading: false,
        }
    }

    /// If the cursor is near the end of what is loaded and there is more, marks the next page as
    /// asked for and says where it starts. Asking again while it is on its way does nothing.
    fn next_offset(&mut self) -> Option<u32> {
        let loaded = self.items.len();
        let near_the_end = self.cursor.selected() + PREFETCH >= loaded;
        if self.loading || !near_the_end || loaded as u64 >= self.total {
            return None;
        }
        self.loading = true;
        Some(loaded as u32)
    }

    /// Adds a page that came. One with nothing in it ends the list, even if the total said more,
    /// so a listing that is short of its total cannot be asked for over and over.
    fn append(&mut self, page: Option<Page<T>>) {
        self.loading = false;
        match page {
            Some(page) if !page.items.is_empty() => {
                self.items.extend(page.items);
                self.total = self.total.max(page.total);
            }
            _ => self.total = self.items.len() as u64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchState {
    /// What was last searched for.
    pub query: String,
    /// Counts the searches; only the answer to the latest one is taken.
    pub generation: u64,
    pub phase: Phase,
    pub tab: Tab,
    pub tracks: Found<TrackSummary>,
    pub albums: Found<AlbumSummary>,
    pub artists: Found<ArtistSummary>,
    pub playlists: Found<PlaylistSummary>,
    /// The line being typed (kept when typing stops, so it can be edited again).
    pub input: TextInput,
    /// Whether keys go to the line above instead of being commands.
    pub editing: bool,
}

impl SearchState {
    /// Where the cursor of each list is, to tell whether a key moved one.
    pub fn list_cursors(&self) -> [Cursor; 4] {
        [
            self.tracks.cursor,
            self.albums.cursor,
            self.artists.cursor,
            self.playlists.cursor,
        ]
    }

    /// The cursor of a tab's list and how many rows the list has.
    pub fn list_of(&mut self, tab: Tab) -> (&mut Cursor, usize) {
        match tab {
            Tab::Tracks => (&mut self.tracks.cursor, self.tracks.items.len()),
            Tab::Albums => (&mut self.albums.cursor, self.albums.items.len()),
            Tab::Artists => (&mut self.artists.cursor, self.artists.items.len()),
            Tab::Playlists => (&mut self.playlists.cursor, self.playlists.items.len()),
        }
    }

    /// Starts a search for `query`: the old results go, and the request to send comes back.
    pub fn begin(&mut self, query: String) -> Request {
        self.generation += 1;
        self.query = query.clone();
        self.phase = Phase::Searching;
        self.tracks = Found::default();
        self.albums = Found::default();
        self.artists = Found::default();
        self.playlists = Found::default();
        Request::Search {
            query,
            kinds: Vec::new(),
            offset: 0,
            limit: Some(PAGE_SIZE),
        }
    }

    /// Takes the daemon's answer to the current search. `false` if it was not the answer to a
    /// search (which is then a failure).
    pub fn finish(&mut self, payload: Payload) -> bool {
        let Payload::SearchResults {
            tracks,
            albums,
            artists,
            playlists,
            ..
        } = payload
        else {
            self.fail("the daemon answered something other than search results".to_string());
            return false;
        };
        self.tracks = tracks.map(Found::from_page).unwrap_or_default();
        self.albums = albums.map(Found::from_page).unwrap_or_default();
        self.artists = artists.map(Found::from_page).unwrap_or_default();
        self.playlists = playlists.map(Found::from_page).unwrap_or_default();
        self.phase = Phase::Done;
        true
    }

    pub fn fail(&mut self, reason: String) {
        self.phase = Phase::Failed(reason);
    }

    /// The next page of the list shown, if the cursor is close to its end and there is more: the
    /// request for it, with the list marked as waiting for it.
    pub fn next_page(&mut self, tab: Tab) -> Option<Request> {
        if self.phase != Phase::Done {
            return None;
        }
        let offset = match tab {
            Tab::Tracks => self.tracks.next_offset(),
            Tab::Albums => self.albums.next_offset(),
            Tab::Artists => self.artists.next_offset(),
            Tab::Playlists => self.playlists.next_offset(),
        }?;
        Some(Request::Search {
            query: self.query.clone(),
            kinds: vec![tab.kind()],
            offset,
            limit: Some(PAGE_SIZE),
        })
    }

    /// Adds the page that came for `tab`.
    pub fn add_page(&mut self, tab: Tab, payload: Payload) {
        let Payload::SearchResults {
            tracks,
            albums,
            artists,
            playlists,
            ..
        } = payload
        else {
            self.page_failed(tab);
            return;
        };
        match tab {
            Tab::Tracks => self.tracks.append(tracks),
            Tab::Albums => self.albums.append(albums),
            Tab::Artists => self.artists.append(artists),
            Tab::Playlists => self.playlists.append(playlists),
        }
    }

    /// The page asked for did not come: it can be asked for again.
    pub fn page_failed(&mut self, tab: Tab) {
        match tab {
            Tab::Tracks => self.tracks.loading = false,
            Tab::Albums => self.albums.loading = false,
            Tab::Artists => self.artists.loading = false,
            Tab::Playlists => self.playlists.loading = false,
        }
    }

    /// The row under the cursor in the list shown, if there is one.
    pub fn selected(&self) -> Option<Selected<'_>> {
        match self.tab {
            Tab::Tracks => self
                .tracks
                .items
                .get(self.tracks.cursor.selected())
                .map(Selected::Track),
            Tab::Albums => self
                .albums
                .items
                .get(self.albums.cursor.selected())
                .map(Selected::Album),
            Tab::Artists => self
                .artists
                .items
                .get(self.artists.cursor.selected())
                .map(Selected::Artist),
            Tab::Playlists => self
                .playlists
                .items
                .get(self.playlists.cursor.selected())
                .map(Selected::Playlist),
        }
    }

    /// The connection went away: a search still waiting will not be answered, and if it were, the
    /// answer must not count.
    pub fn connection_lost(&mut self) {
        if self.phase == Phase::Searching {
            self.generation += 1;
            self.phase = Phase::Failed("the connection to the daemon was lost".to_string());
        }
    }

    /// How many results of a kind the search found in all.
    pub fn total(&self, tab: Tab) -> u64 {
        match tab {
            Tab::Tracks => self.tracks.total,
            Tab::Albums => self.albums.total,
            Tab::Artists => self.artists.total,
            Tab::Playlists => self.playlists.total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page<T>(items: Vec<T>, total: u64) -> Option<Page<T>> {
        Some(Page {
            items,
            total,
            offset: 0,
        })
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
            quality: None,
            streamable: true,
        }
    }

    fn results(tracks: Vec<TrackSummary>, total: u64) -> Payload {
        Payload::SearchResults {
            query: "q".into(),
            tracks: page(tracks, total),
            albums: page(vec![], 0),
            artists: None,
            playlists: None,
        }
    }

    #[test]
    fn a_new_search_clears_the_old_results_and_asks_for_every_kind() {
        let mut search = SearchState::default();
        search.begin("korn".into());
        search.finish(results(vec![track("1")], 1));
        assert_eq!(search.tracks.items.len(), 1);

        let request = search.begin("nu metal".into());
        assert_eq!(search.phase, Phase::Searching);
        assert_eq!(search.query, "nu metal");
        assert!(search.tracks.items.is_empty(), "the old results are gone");
        assert_eq!(
            request,
            Request::Search {
                query: "nu metal".into(),
                kinds: vec![],
                offset: 0,
                limit: Some(50)
            }
        );
    }

    #[test]
    fn each_search_has_its_own_generation() {
        let mut search = SearchState::default();
        assert_eq!(search.generation, 0);
        search.begin("a".into());
        search.begin("b".into());
        assert_eq!(search.generation, 2);
    }

    #[test]
    fn an_answer_fills_the_lists_with_their_totals_and_leaves_absent_kinds_empty() {
        let mut search = SearchState::default();
        search.begin("q".into());
        assert!(search.finish(results(vec![track("1"), track("2")], 123)));
        assert_eq!(search.phase, Phase::Done);
        assert_eq!(search.tracks.items.len(), 2);
        assert_eq!(search.total(Tab::Tracks), 123, "the total is the daemon's");
        assert_eq!(search.total(Tab::Albums), 0);
        assert_eq!(
            search.total(Tab::Artists),
            0,
            "a kind not asked for is empty"
        );
    }

    #[test]
    fn an_answer_that_is_not_search_results_is_a_failure() {
        let mut search = SearchState::default();
        search.begin("q".into());
        assert!(!search.finish(Payload::Ack));
        assert!(matches!(search.phase, Phase::Failed(_)));
    }

    #[test]
    fn losing_the_connection_ends_a_pending_search_and_drops_its_answer() {
        let mut search = SearchState::default();
        search.begin("q".into());
        let waiting_for = search.generation;
        search.connection_lost();
        assert!(matches!(search.phase, Phase::Failed(_)));
        assert_ne!(
            search.generation, waiting_for,
            "the late answer will not match"
        );

        // A search that was already done is not touched by a later disconnect.
        let mut done = SearchState::default();
        done.begin("q".into());
        done.finish(results(vec![track("1")], 1));
        let generation = done.generation;
        done.connection_lost();
        assert_eq!(done.phase, Phase::Done);
        assert_eq!(done.generation, generation);
    }

    #[test]
    fn the_tabs_step_left_and_right_and_stop_at_the_ends() {
        assert_eq!(Tab::default(), Tab::Tracks);
        assert_eq!(Tab::Tracks.next(), Tab::Albums);
        assert_eq!(Tab::Artists.next(), Tab::Playlists);
        assert_eq!(Tab::Playlists.next(), Tab::Playlists);
        assert_eq!(Tab::Albums.previous(), Tab::Tracks);
        assert_eq!(Tab::Tracks.previous(), Tab::Tracks);
        assert_eq!(Tab::Playlists.title(), "Playlists");
    }
}
