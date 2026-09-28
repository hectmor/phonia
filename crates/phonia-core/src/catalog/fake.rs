//! A catalog that answers from what it was given, for tests of everything that uses one.

use super::{
    Album, AlbumFilter, Artist, Catalog, CatalogError, Kind, MAX_ITEMS_LIMIT, MAX_SEARCH_LIMIT,
    Page, SearchResults, Track,
};
use futures_util::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What a [`FakeCatalog`] was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Search {
        query: String,
        kinds: Vec<Kind>,
        offset: u32,
        limit: u32,
    },
    AlbumTracks {
        id: String,
        offset: u32,
        limit: u32,
    },
    PlaylistTracks {
        id: String,
        offset: u32,
        limit: u32,
    },
    Album {
        id: String,
    },
    Artist {
        id: String,
    },
    ArtistBio {
        id: String,
    },
    ArtistTopTracks {
        id: String,
        offset: u32,
        limit: u32,
    },
    ArtistAlbums {
        id: String,
        filter: AlbumFilter,
        offset: u32,
        limit: u32,
    },
}

/// What a [`FakeCatalog`] knows of one artist.
#[derive(Clone)]
struct ArtistData {
    artist: Artist,
    bio: Option<String>,
    top_tracks: Vec<Track>,
    albums: Vec<Album>,
    singles: Vec<Album>,
}

#[derive(Default)]
struct State {
    search: SearchResults,
    albums: HashMap<String, Vec<Track>>,
    playlists: HashMap<String, Vec<Track>>,
    album_details: HashMap<String, Album>,
    artists: HashMap<String, ArtistData>,
    error: Option<CatalogError>,
    delay: Duration,
    calls: Vec<Call>,
}

/// Answers searches with the results it was given (only the kinds asked for), and listings from
/// the albums and playlists it was given, paged like the real one. It can fail every call, or
/// take its time, and remembers what it was asked.
#[derive(Clone, Default)]
pub struct FakeCatalog {
    state: Arc<Mutex<State>>,
}

impl FakeCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_search(self, results: SearchResults) -> Self {
        self.state.lock().unwrap().search = results;
        self
    }

    pub fn with_album(self, id: &str, tracks: Vec<Track>) -> Self {
        self.state
            .lock()
            .unwrap()
            .albums
            .insert(id.to_string(), tracks);
        self
    }

    pub fn with_playlist(self, id: &str, tracks: Vec<Track>) -> Self {
        self.state
            .lock()
            .unwrap()
            .playlists
            .insert(id.to_string(), tracks);
        self
    }

    /// An album's own details, for `album`.
    pub fn with_album_details(self, album: Album) -> Self {
        self.state
            .lock()
            .unwrap()
            .album_details
            .insert(album.id.clone(), album);
        self
    }

    /// An artist, its bio (if it has one), its top tracks, its albums, and its EPs and singles.
    pub fn with_artist(
        self,
        artist: Artist,
        bio: Option<&str>,
        top_tracks: Vec<Track>,
        albums: Vec<Album>,
        singles: Vec<Album>,
    ) -> Self {
        let data = ArtistData {
            bio: bio.map(str::to_string),
            top_tracks,
            albums,
            singles,
            artist: artist.clone(),
        };
        self.state.lock().unwrap().artists.insert(artist.id, data);
        self
    }

    /// Every call fails with `error`.
    pub fn failing(self, error: CatalogError) -> Self {
        self.state.lock().unwrap().error = Some(error);
        self
    }

    /// Every call takes `delay` to answer.
    pub fn delayed(self, delay: Duration) -> Self {
        self.state.lock().unwrap().delay = delay;
        self
    }

    /// What it was asked, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.state.lock().unwrap().calls.clone()
    }

    fn answer<T: Send + 'static>(
        &self,
        call: Call,
        make: impl FnOnce(&State) -> Result<T, CatalogError> + Send + 'static,
    ) -> BoxFuture<'static, Result<T, CatalogError>> {
        let state = self.state.clone();
        Box::pin(async move {
            let delay = {
                let mut state = state.lock().unwrap();
                state.calls.push(call);
                state.delay
            };
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let state = state.lock().unwrap();
            match &state.error {
                Some(error) => Err(error.clone()),
                None => make(&state),
            }
        })
    }
}

/// The page of `items` that `offset` and `limit` ask for.
fn page_of<T: Clone>(items: &[T], offset: u32, limit: u32) -> Page<T> {
    let limit = limit.clamp(1, MAX_ITEMS_LIMIT) as usize;
    Page {
        items: items
            .iter()
            .skip(offset as usize)
            .take(limit)
            .cloned()
            .collect(),
        total: items.len() as u64,
        offset: u64::from(offset),
    }
}

impl Catalog for FakeCatalog {
    fn search(
        &self,
        query: String,
        kinds: Vec<Kind>,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<SearchResults, CatalogError>> {
        let call = Call::Search {
            query,
            kinds: kinds.clone(),
            offset,
            limit: limit.clamp(1, MAX_SEARCH_LIMIT),
        };
        self.answer(call, move |state| {
            let wanted = |kind| kinds.is_empty() || kinds.contains(&kind);
            Ok(SearchResults {
                tracks: state.search.tracks.clone().filter(|_| wanted(Kind::Tracks)),
                albums: state.search.albums.clone().filter(|_| wanted(Kind::Albums)),
                artists: state
                    .search
                    .artists
                    .clone()
                    .filter(|_| wanted(Kind::Artists)),
                playlists: state
                    .search
                    .playlists
                    .clone()
                    .filter(|_| wanted(Kind::Playlists)),
            })
        })
    }

    fn album_tracks(
        &self,
        id: String,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<Page<Track>, CatalogError>> {
        let call = Call::AlbumTracks {
            id: id.clone(),
            offset,
            limit,
        };
        self.answer(call, move |state| match state.albums.get(&id) {
            Some(tracks) => Ok(page_of(tracks, offset, limit)),
            None => Err(CatalogError::NotFound),
        })
    }

    fn playlist_tracks(
        &self,
        id: String,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<Page<Track>, CatalogError>> {
        let call = Call::PlaylistTracks {
            id: id.clone(),
            offset,
            limit,
        };
        self.answer(call, move |state| match state.playlists.get(&id) {
            Some(tracks) => Ok(page_of(tracks, offset, limit)),
            None => Err(CatalogError::NotFound),
        })
    }

    fn album(&self, id: String) -> BoxFuture<'static, Result<Album, CatalogError>> {
        let call = Call::Album { id: id.clone() };
        self.answer(call, move |state| {
            state
                .album_details
                .get(&id)
                .cloned()
                .ok_or(CatalogError::NotFound)
        })
    }

    fn artist(&self, id: String) -> BoxFuture<'static, Result<Artist, CatalogError>> {
        let call = Call::Artist { id: id.clone() };
        self.answer(call, move |state| {
            state
                .artists
                .get(&id)
                .map(|data| data.artist.clone())
                .ok_or(CatalogError::NotFound)
        })
    }

    fn artist_bio(&self, id: String) -> BoxFuture<'static, Result<Option<String>, CatalogError>> {
        let call = Call::ArtistBio { id: id.clone() };
        self.answer(call, move |state| match state.artists.get(&id) {
            Some(data) => Ok(data.bio.clone()),
            None => Err(CatalogError::NotFound),
        })
    }

    fn artist_top_tracks(
        &self,
        id: String,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<Page<Track>, CatalogError>> {
        let call = Call::ArtistTopTracks {
            id: id.clone(),
            offset,
            limit,
        };
        self.answer(call, move |state| match state.artists.get(&id) {
            Some(data) => Ok(page_of(&data.top_tracks, offset, limit)),
            None => Err(CatalogError::NotFound),
        })
    }

    fn artist_albums(
        &self,
        id: String,
        filter: AlbumFilter,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<Page<Album>, CatalogError>> {
        let call = Call::ArtistAlbums {
            id: id.clone(),
            filter,
            offset,
            limit,
        };
        self.answer(call, move |state| match state.artists.get(&id) {
            Some(data) => {
                let list = match filter {
                    AlbumFilter::Albums => &data.albums,
                    AlbumFilter::EpsAndSingles => &data.singles,
                };
                Ok(page_of(list, offset, limit))
            }
            None => Err(CatalogError::NotFound),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(n: u32) -> Track {
        Track {
            id: n.to_string(),
            title: format!("Track {n}"),
            version: None,
            artists: Vec::new(),
            album: None,
            duration: None,
            explicit: false,
            track_number: Some(n),
            volume_number: None,
            quality: None,
            streamable: true,
        }
    }

    #[tokio::test]
    async fn it_answers_only_the_kinds_asked_for_and_remembers_the_call() {
        let all = SearchResults {
            tracks: Some(Page {
                items: vec![track(1)],
                total: 1,
                offset: 0,
            }),
            albums: Some(Page::empty()),
            ..SearchResults::default()
        };
        let catalog = FakeCatalog::new().with_search(all);
        let only_albums = catalog
            .search("x".into(), vec![Kind::Albums], 0, 50)
            .await
            .unwrap();
        assert!(only_albums.tracks.is_none() && only_albums.albums.is_some());
        let everything = catalog.search("x".into(), vec![], 0, 50).await.unwrap();
        assert!(everything.tracks.is_some());
        assert_eq!(
            catalog.calls()[0],
            Call::Search {
                query: "x".into(),
                kinds: vec![Kind::Albums],
                offset: 0,
                limit: 50
            }
        );
    }

    #[tokio::test]
    async fn listings_are_paged_and_an_unknown_id_is_not_found() {
        let catalog = FakeCatalog::new().with_album("9", (1..=5).map(track).collect());
        let page = catalog.album_tracks("9".into(), 2, 2).await.unwrap();
        assert_eq!(page.total, 5);
        assert_eq!(page.offset, 2);
        assert_eq!(
            page.items.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["3", "4"]
        );
        assert_eq!(
            catalog.playlist_tracks("nope".into(), 0, 10).await,
            Err(CatalogError::NotFound)
        );
    }

    #[tokio::test]
    async fn it_can_fail_every_call() {
        let catalog = FakeCatalog::new().failing(CatalogError::RateLimited);
        assert_eq!(
            catalog.search("x".into(), vec![], 0, 10).await,
            Err(CatalogError::RateLimited)
        );
        assert_eq!(catalog.calls().len(), 1, "a failing call is still recorded");
    }

    fn album_of(id: &str) -> Album {
        Album {
            id: id.into(),
            title: format!("Album {id}"),
            version: None,
            artists: vec![],
            release_date: None,
            track_count: None,
            duration: None,
            explicit: false,
            quality: None,
            kind: None,
            copyright: None,
        }
    }

    #[tokio::test]
    async fn an_artist_and_its_lists_come_back_paged_and_split_by_kind() {
        let korn = Artist {
            id: "780".into(),
            name: "Korn".into(),
        };
        let catalog = FakeCatalog::new()
            .with_album_details(album_of("9"))
            .with_artist(
                korn.clone(),
                Some("A band."),
                (1..=5).map(track).collect(),
                (1..=3).map(|n| album_of(&format!("a{n}"))).collect(),
                vec![album_of("s1")],
            );
        assert_eq!(catalog.album("9".into()).await.unwrap().title, "Album 9");
        assert_eq!(catalog.artist("780".into()).await.unwrap(), korn);
        assert_eq!(
            catalog.artist_bio("780".into()).await.unwrap().as_deref(),
            Some("A band.")
        );
        let top = catalog.artist_top_tracks("780".into(), 2, 2).await.unwrap();
        assert_eq!((top.total, top.items.len()), (5, 2));
        let albums = catalog
            .artist_albums("780".into(), AlbumFilter::Albums, 0, 10)
            .await
            .unwrap();
        assert_eq!(albums.total, 3);
        let singles = catalog
            .artist_albums("780".into(), AlbumFilter::EpsAndSingles, 0, 10)
            .await
            .unwrap();
        assert_eq!(singles.items[0].id, "s1");
        assert_eq!(catalog.calls()[0], Call::Album { id: "9".into() });
    }

    #[tokio::test]
    async fn an_unknown_album_or_artist_is_not_found() {
        let catalog = FakeCatalog::new();
        assert_eq!(catalog.album("x".into()).await, Err(CatalogError::NotFound));
        assert_eq!(
            catalog.artist("x".into()).await,
            Err(CatalogError::NotFound)
        );
        assert_eq!(
            catalog.artist_bio("x".into()).await,
            Err(CatalogError::NotFound)
        );
    }
}
