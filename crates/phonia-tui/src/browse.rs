//! Looking at an album or a playlist opened from a result: what is shown, and the stack of them
//! (opening one from within another is not possible yet, so the stack is at most one deep, but it
//! is written to hold more).

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

/// A view that can be pushed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    TrackList(TrackListView),
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
            let View::TrackList(view) = view;
            if view.phase == Phase::Loading {
                view.phase = Phase::Failed("the connection to the daemon was lost".to_string());
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
        })
    }

    fn view(id: &str) -> View {
        View::TrackList(TrackListView::new(
            CatalogRef::Album { id: id.into() },
            album(id),
        ))
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
        let View::TrackList(loaded) = stack.find_mut(1).unwrap();
        loaded.phase = Phase::Done;
        stack.push(2, view("10"));

        stack.connection_lost();
        let View::TrackList(still_done) = stack.find_mut(1).unwrap();
        assert_eq!(still_done.phase, Phase::Done);
        let View::TrackList(now_failed) = stack.find_mut(2).unwrap();
        assert!(matches!(now_failed.phase, Phase::Failed(_)));
    }

    #[test]
    fn the_title_of_an_album_or_a_playlist_is_its_own() {
        assert_eq!(
            Header::Playlist(PlaylistSummary {
                id: "u".into(),
                title: "Nu metal".into(),
                creator: None,
                description: None,
                track_count: None,
                duration_ms: None,
            })
            .title(),
            "Nu metal"
        );
        assert_eq!(album("9").title(), "Album 9");
    }
}
