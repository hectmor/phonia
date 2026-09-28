//! A catalog that answers from what it was given, for tests of everything that uses one.

use super::{
    Catalog, CatalogError, Kind, MAX_ITEMS_LIMIT, MAX_SEARCH_LIMIT, Page, SearchResults, Track,
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
}

#[derive(Default)]
struct State {
    search: SearchResults,
    albums: HashMap<String, Vec<Track>>,
    playlists: HashMap<String, Vec<Track>>,
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
fn page_of(items: &[Track], offset: u32, limit: u32) -> Page<Track> {
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
}
