//! Looking at an album, a playlist or an artist opened from a result: what is shown, and the
//! stack of them (opening one from within another, an album from an artist's page, is the only
//! way the stack grows past one).

use crate::list::Found;
use phonia_ipc::{AlbumSummary, CatalogRef, PlaylistSummary, TrackSummary};

/// Where a view stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Loading,
    Failed(String),
    Done,
}

/// The metadata shown above a track list: enough to tell an album from a playlist, and already
/// known from the result it was opened from, so it is there before the tracks have loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Header {
    Album(AlbumSummary),
    Playlist(PlaylistSummary),
}

impl Header {
    pub fn title(&self) -> &str {
        match self {
            Header::Album(album) => &album.title,
            Header::Playlist(playlist) => &playlist.title,
        }
    }
}

/// An album or a playlist, opened to see its tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackListView {
    /// What to keep asking for when more tracks are needed.
    pub of: CatalogRef,
    pub header: Header,
    pub phase: Phase,
    pub tracks: Found<TrackSummary>,
}

impl TrackListView {
    pub fn new(of: CatalogRef, header: Header) -> Self {
        Self {
            of,
            header,
            phase: Phase::Loading,
            tracks: Found::default(),
        }
    }

    pub fn title(&self) -> &str {
        self.header.title()
    }

    pub fn header(&self) -> &Header {
        &self.header
    }
}

/// The three things an artist's page lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArtistTab {
    #[default]
    TopTracks,
    Albums,
    Singles,
}

impl ArtistTab {
    pub const ALL: [ArtistTab; 3] = [ArtistTab::TopTracks, ArtistTab::Albums, ArtistTab::Singles];

    pub fn title(self) -> &'static str {
        match self {
            ArtistTab::TopTracks => "Top tracks",
            ArtistTab::Albums => "Albums",
            ArtistTab::Singles => "EPs & singles",
        }
    }

    /// The next tab to the right, stopping at the last.
    pub fn next(self) -> ArtistTab {
        let index = ArtistTab::ALL
            .iter()
            .position(|tab| *tab == self)
            .unwrap_or(0);
        ArtistTab::ALL[(index + 1).min(ArtistTab::ALL.len() - 1)]
    }

    /// The next tab to the left, stopping at the first.
    pub fn previous(self) -> ArtistTab {
        let index = ArtistTab::ALL
            .iter()
            .position(|tab| *tab == self)
            .unwrap_or(0);
        ArtistTab::ALL[index.saturating_sub(1)]
    }
}

/// An artist, opened to see its top tracks, its albums, and its EPs and singles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistView {
    pub id: String,
    pub name: String,
    /// Plain text; absent when TIDAL has nothing to say, or once the artist has loaded if
    /// fetching it failed (a nicety, not worth failing the rest of the page over).
    pub bio: Option<String>,
    pub phase: Phase,
    pub tab: ArtistTab,
    pub top_tracks: Found<TrackSummary>,
    pub albums: Found<AlbumSummary>,
    pub singles: Found<AlbumSummary>,
}

impl ArtistView {
    /// `name` is already known from the result the view was opened from, so it (and the title)
    /// show before anything has loaded.
    pub fn new(id: String, name: String) -> Self {
        Self {
            id,
            name,
            bio: None,
            phase: Phase::Loading,
            tab: ArtistTab::default(),
            top_tracks: Found::default(),
            albums: Found::default(),
            singles: Found::default(),
        }
    }

    pub fn title(&self) -> &str {
        &self.name
    }

    /// The list the current tab shows, and how long it is.
    pub fn tracks_or_albums(&mut self) -> ListRef<'_> {
        match self.tab {
            ArtistTab::TopTracks => ListRef::Tracks(&mut self.top_tracks),
            ArtistTab::Albums => ListRef::Albums(&mut self.albums),
            ArtistTab::Singles => ListRef::Albums(&mut self.singles),
        }
    }
}

/// Whichever of an artist's lists the current tab is showing.
pub enum ListRef<'a> {
    Tracks(&'a mut Found<TrackSummary>),
    Albums(&'a mut Found<AlbumSummary>),
}

/// A view that can be pushed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    TrackList(TrackListView),
    Artist(ArtistView),
}

impl View {
    pub fn title(&self) -> &str {
        match self {
            View::TrackList(view) => view.title(),
            View::Artist(view) => view.title(),
        }
    }

    pub fn phase(&self) -> &Phase {
        match self {
            View::TrackList(view) => &view.phase,
            View::Artist(view) => &view.phase,
        }
    }
}

/// The views opened one from another, each tagged with the number it was pushed with, so a late
/// answer to one that has since been closed is told apart from one to the view still open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stack {
    views: Vec<(u64, View)>,
}

impl Stack {
    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }

    pub fn push(&mut self, serial: u64, view: View) {
        self.views.push((serial, view));
    }

    /// Closes the view on top, if there is one.
    pub fn pop(&mut self) -> bool {
        self.views.pop().is_some()
    }

    pub fn top(&self) -> Option<&View> {
        self.views.last().map(|(_, view)| view)
    }

    pub fn top_mut(&mut self) -> Option<&mut View> {
        self.views.last_mut().map(|(_, view)| view)
    }

    /// The number the view on top was pushed with, if there is one.
    pub fn top_serial(&self) -> Option<u64> {
        self.views.last().map(|(serial, _)| *serial)
    }

    /// The breadcrumb of titles, from the bottom of the stack to the top.
    pub fn titles(&self) -> Vec<&str> {
        self.views.iter().map(|(_, view)| view.title()).collect()
    }

    /// The view pushed with `serial`, wherever it is (in practice always the one on top, since
    /// nothing can be opened from a view that has not finished loading).
    pub fn find_mut(&mut self, serial: u64) -> Option<&mut View> {
        self.views
            .iter_mut()
            .find(|(number, _)| *number == serial)
            .map(|(_, view)| view)
    }

    /// The connection is gone: a view still loading will not be answered.
    pub fn connection_lost(&mut self) {
        for (_, view) in &mut self.views {
            let phase = match view {
                View::TrackList(view) => &mut view.phase,
                View::Artist(view) => &mut view.phase,
            };
            if *phase == Phase::Loading {
                *phase = Phase::Failed("the connection to the daemon was lost".to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(id: &str) -> Header {
        Header::Album(AlbumSummary {
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
        })
    }

    fn view(id: &str) -> View {
        View::TrackList(TrackListView::new(
            CatalogRef::Album { id: id.into() },
            album(id),
        ))
    }

    fn artist_view(id: &str, name: &str) -> View {
        View::Artist(ArtistView::new(id.into(), name.into()))
    }

    #[test]
    fn pushing_and_popping_views() {
        let mut stack = Stack::default();
        assert!(stack.is_empty());
        assert!(!stack.pop());

        stack.push(1, view("9"));
        assert_eq!(stack.top_serial(), Some(1));
        assert!(!stack.is_empty());
        stack.push(2, view("10"));
        assert_eq!(stack.top_serial(), Some(2));

        assert!(stack.pop());
        assert_eq!(stack.top_serial(), Some(1));
        assert!(stack.pop());
        assert!(stack.is_empty());
    }

    #[test]
    fn a_view_is_found_by_the_number_it_was_pushed_with_wherever_it_is() {
        let mut stack = Stack::default();
        stack.push(1, view("9"));
        assert!(stack.find_mut(1).is_some());
        assert!(stack.find_mut(2).is_none(), "never pushed");
        stack.pop();
        assert!(stack.find_mut(1).is_none(), "closed");
    }

    #[test]
    fn losing_the_connection_fails_every_view_still_loading_and_leaves_the_rest_alone() {
        let mut stack = Stack::default();
        stack.push(1, view("9"));
        let Some(View::TrackList(loaded)) = stack.find_mut(1) else {
            panic!("not a track list")
        };
        loaded.phase = Phase::Done;
        stack.push(2, artist_view("780", "Korn"));

        stack.connection_lost();
        assert_eq!(*stack.find_mut(1).unwrap().phase(), Phase::Done);
        assert!(matches!(
            stack.find_mut(2).unwrap().phase(),
            Phase::Failed(_)
        ));
    }

    #[test]
    fn the_title_of_an_album_a_playlist_or_an_artist_is_its_own() {
        assert_eq!(
            Header::Playlist(PlaylistSummary {
                id: "u".into(),
                title: "Nu metal".into(),
                creator: None,
                description: None,
                track_count: None,
                duration_ms: None,
                cover: None,
            })
            .title(),
            "Nu metal"
        );
        assert_eq!(album("9").title(), "Album 9");
        assert_eq!(artist_view("780", "Korn").title(), "Korn");
    }

    #[test]
    fn the_breadcrumb_lists_every_title_from_the_bottom_up() {
        let mut stack = Stack::default();
        stack.push(1, artist_view("780", "Korn"));
        stack.push(2, view("9"));
        assert_eq!(stack.titles(), ["Korn", "Album 9"]);
    }

    #[test]
    fn the_artist_tabs_step_left_and_right_and_stop_at_the_ends() {
        assert_eq!(ArtistTab::default(), ArtistTab::TopTracks);
        assert_eq!(ArtistTab::TopTracks.next(), ArtistTab::Albums);
        assert_eq!(ArtistTab::Albums.next(), ArtistTab::Singles);
        assert_eq!(ArtistTab::Singles.next(), ArtistTab::Singles);
        assert_eq!(ArtistTab::Singles.previous(), ArtistTab::Albums);
        assert_eq!(ArtistTab::TopTracks.previous(), ArtistTab::TopTracks);
        assert_eq!(ArtistTab::Singles.title(), "EPs & singles");
    }

    #[test]
    fn an_artist_views_current_list_follows_its_tab() {
        let mut view = ArtistView::new("780".into(), "Korn".into());
        assert!(matches!(view.tracks_or_albums(), ListRef::Tracks(_)));
        view.tab = ArtistTab::Albums;
        assert!(matches!(view.tracks_or_albums(), ListRef::Albums(_)));
        view.tab = ArtistTab::Singles;
        assert!(matches!(view.tracks_or_albums(), ListRef::Albums(_)));
    }
}
