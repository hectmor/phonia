//! Browsing TIDAL's catalog: searching it, and listing the tracks of an album or a playlist.
//!
//! What a client sees of this goes through the daemon (which owns the login), and the daemon
//! talks to a [`Catalog`]: the real one is [`TidalCatalog`], tests use [`fake::FakeCatalog`]. The
//! types here are phonia's own, small and complete for what the interface shows; nothing of the
//! library that speaks to TIDAL leaks through them, so it can be replaced without touching the
//! protocol.

use crate::config::Quality;
use futures_util::future::BoxFuture;
use std::fmt;
use std::time::Duration;

pub mod fake;
mod remote;

pub use remote::TidalCatalog;

/// The most a search may return in one page.
pub const MAX_SEARCH_LIMIT: u32 = 300;
/// The most a listing of an album's or a playlist's tracks may return in one page.
pub const MAX_ITEMS_LIMIT: u32 = 100;

/// The kinds of thing the catalog has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Tracks,
    Albums,
    Artists,
    Playlists,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Tracks, Kind::Albums, Kind::Artists, Kind::Playlists];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistRef {
    /// TIDAL's id, as text.
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumRef {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub title: String,
    /// "Remastered", "Live"... when TIDAL says so.
    pub version: Option<String>,
    pub artists: Vec<ArtistRef>,
    pub album: Option<AlbumRef>,
    pub duration: Option<Duration>,
    pub explicit: bool,
    pub track_number: Option<u32>,
    /// Which disc of the album the track is on, when TIDAL says (an album of several discs).
    pub volume_number: Option<u32>,
    /// The best tier TIDAL has the track in.
    pub quality: Option<Quality>,
    /// Whether it can be played at all (TIDAL lists tracks that are not available where you are).
    pub streamable: bool,
}

/// What kind of release an album is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumKind {
    Album,
    Ep,
    Single,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    pub id: String,
    pub title: String,
    pub version: Option<String>,
    pub artists: Vec<ArtistRef>,
    /// `2011-05-31`, as TIDAL gives it.
    pub release_date: Option<String>,
    pub track_count: Option<u32>,
    pub duration: Option<Duration>,
    pub explicit: bool,
    pub quality: Option<Quality>,
    /// An album, an EP or a single, when TIDAL says.
    pub kind: Option<AlbumKind>,
    pub copyright: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artist {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    /// A UUID.
    pub id: String,
    pub title: String,
    pub creator: Option<String>,
    pub description: Option<String>,
    pub track_count: Option<u32>,
    pub duration: Option<Duration>,
}

/// One page of a list that may be longer: `total` is how many there are in all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub offset: u64,
}

impl<T> Page<T> {
    pub fn empty() -> Self {
        Self {
            items: Vec::new(),
            total: 0,
            offset: 0,
        }
    }
}

/// What a search found; a kind that was not asked for is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchResults {
    pub tracks: Option<Page<Track>>,
    pub albums: Option<Page<Album>>,
    pub artists: Option<Page<Artist>>,
    pub playlists: Option<Page<Playlist>>,
}

/// Which of an artist's releases to list: the albums, or the EPs and the singles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumFilter {
    Albums,
    EpsAndSingles,
}

/// Why the catalog could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// There is no TIDAL login, or TIDAL no longer accepts it.
    NotLoggedIn(String),
    /// TIDAL, or the network to it, is not answering.
    Unavailable(String),
    /// TIDAL is being asked too often.
    RateLimited,
    /// There is no such album or playlist.
    NotFound,
    /// The request itself makes no sense.
    Invalid(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::NotLoggedIn(why) => {
                write!(f, "not logged in to TIDAL ({why}): run `phonia login`")
            }
            CatalogError::Unavailable(why) => write!(f, "TIDAL could not be reached: {why}"),
            CatalogError::RateLimited => {
                write!(
                    f,
                    "TIDAL says there were too many requests: wait a moment and try again"
                )
            }
            CatalogError::NotFound => write!(f, "TIDAL has no such item"),
            CatalogError::Invalid(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for CatalogError {}

/// The catalog. Every call is one request to TIDAL, and paging is the caller's: ask for the next
/// `offset` until `total` is reached.
pub trait Catalog: Send + Sync {
    /// Searches for `query` among `kinds` (all of them when empty). At most
    /// [`MAX_SEARCH_LIMIT`] results of each kind per page.
    fn search(
        &self,
        query: String,
        kinds: Vec<Kind>,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<SearchResults, CatalogError>>;

    /// The tracks of an album, in album order. At most [`MAX_ITEMS_LIMIT`] per page.
    fn album_tracks(
        &self,
        id: String,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<Page<Track>, CatalogError>>;

    /// The tracks of a playlist, in playlist order. At most [`MAX_ITEMS_LIMIT`] per page.
    fn playlist_tracks(
        &self,
        id: String,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<Page<Track>, CatalogError>>;

    /// An album itself: its title, artists, release date, and so on, not its tracks.
    fn album(&self, id: String) -> BoxFuture<'static, Result<Album, CatalogError>>;

    /// An artist itself.
    fn artist(&self, id: String) -> BoxFuture<'static, Result<Artist, CatalogError>>;

    /// What TIDAL says about an artist, as plain text; `None` when it has nothing to say.
    fn artist_bio(&self, id: String) -> BoxFuture<'static, Result<Option<String>, CatalogError>>;

    /// An artist's most listened to tracks. At most [`MAX_ITEMS_LIMIT`] per page.
    fn artist_top_tracks(
        &self,
        id: String,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<Page<Track>, CatalogError>>;

    /// An artist's albums, or its EPs and singles. At most [`MAX_ITEMS_LIMIT`] per page.
    fn artist_albums(
        &self,
        id: String,
        filter: AlbumFilter,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<Page<Album>, CatalogError>>;
}
