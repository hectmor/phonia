//! The catalog as TIDAL serves it: plain HTTP calls to its v1 API, with the access token of the
//! shared login.
//!
//! It does not go through `tidlers`' search and listing calls. Those have types with required
//! fields TIDAL sometimes leaves out (one missing field fails the whole answer), page the
//! favorites with a misspelled offset, and hide the request builder, so nothing can be repaired
//! from outside. Here every field is optional except the id, and a single item that cannot be
//! read is left out of its page rather than failing the page.

use super::{
    Album, AlbumRef, Artist, ArtistRef, Catalog, CatalogError, Kind, MAX_ITEMS_LIMIT,
    MAX_SEARCH_LIMIT, Page, Playlist, SearchResults, Track,
};
use crate::config::Quality;
use crate::session::TidalSession;
use crate::tidal;
use futures_util::future::BoxFuture;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// TIDAL's catalog, through the login the [`TidalOpener`](crate::openers::TidalOpener) shares.
#[derive(Clone)]
pub struct TidalCatalog {
    http: reqwest::Client,
    session: Arc<TidalSession>,
    base: String,
}

impl TidalCatalog {
    pub(crate) fn new(http: reqwest::Client, session: Arc<TidalSession>) -> Self {
        Self {
            http,
            session,
            base: tidlers::urls::API_V1_LOCATION.to_string(),
        }
    }

    /// One GET, with the login's token and country.
    async fn get(
        &self,
        path: &str,
        query: Vec<(&'static str, String)>,
    ) -> Result<String, CatalogError> {
        let (token, country) = {
            let client = self.session.fresh().await.map_err(session_error)?;
            tidal::credentials(&client).map_err(session_error)?
        };
        let account = Account {
            base: &self.base,
            token: &token,
            country: &country,
        };
        get(&self.http, &account, path, &query).await
    }
}

/// Who is asking, and where the API is.
struct Account<'a> {
    base: &'a str,
    token: &'a str,
    country: &'a str,
}

/// A failure to have a usable login: not being logged in, unless it was the refreshing of the
/// token that could not reach TIDAL.
fn session_error(error: anyhow::Error) -> CatalogError {
    let text = format!("{error:#}");
    if text.starts_with("refreshing the TIDAL access token") {
        CatalogError::Unavailable(text)
    } else {
        CatalogError::NotLoggedIn(text)
    }
}

/// What an HTTP error status means for the catalog.
fn classify_status(status: StatusCode, body: &str) -> CatalogError {
    let snippet: String = body.chars().take(200).collect();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            CatalogError::NotLoggedIn(format!("TIDAL answered {status}"))
        }
        StatusCode::NOT_FOUND => CatalogError::NotFound,
        StatusCode::TOO_MANY_REQUESTS => CatalogError::RateLimited,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            CatalogError::Invalid(format!("TIDAL rejected the request ({status}): {snippet}"))
        }
        _ => CatalogError::Unavailable(format!("TIDAL answered {status}: {snippet}")),
    }
}

async fn get(
    http: &reqwest::Client,
    account: &Account<'_>,
    path: &str,
    query: &[(&'static str, String)],
) -> Result<String, CatalogError> {
    let response = http
        .get(format!("{}{path}", account.base))
        .bearer_auth(account.token)
        .query(&[("countryCode", account.country)])
        .query(query)
        .send()
        .await
        .map_err(|error| CatalogError::Unavailable(format!("{error}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| CatalogError::Unavailable(format!("reading the answer: {error}")))?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(classify_status(status, &body))
    }
}

/// Ids go into paths: only what an id is made of is let through.
fn check_id(id: &str) -> Result<(), CatalogError> {
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        Ok(())
    } else {
        Err(CatalogError::Invalid(format!("{id:?} is not an id")))
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Tracks => "TRACKS",
        Kind::Albums => "ALBUMS",
        Kind::Artists => "ARTISTS",
        Kind::Playlists => "PLAYLISTS",
    }
}

/// The query of a search.
fn search_query(
    query: &str,
    kinds: &[Kind],
    offset: u32,
    limit: u32,
) -> Vec<(&'static str, String)> {
    let kinds = if kinds.is_empty() {
        &Kind::ALL[..]
    } else {
        kinds
    };
    let types: Vec<&str> = kinds.iter().map(|kind| kind_name(*kind)).collect();
    vec![
        ("query", query.to_string()),
        ("types", types.join(",")),
        ("limit", limit.clamp(1, MAX_SEARCH_LIMIT).to_string()),
        ("offset", offset.to_string()),
    ]
}

fn items_query(offset: u32, limit: u32) -> Vec<(&'static str, String)> {
    vec![
        ("limit", limit.clamp(1, MAX_ITEMS_LIMIT).to_string()),
        ("offset", offset.to_string()),
    ]
}

impl Catalog for TidalCatalog {
    fn search(
        &self,
        query: String,
        kinds: Vec<Kind>,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<SearchResults, CatalogError>> {
        let catalog = self.clone();
        Box::pin(async move {
            let query = query.trim().to_string();
            if query.is_empty() {
                return Err(CatalogError::Invalid(
                    "there is nothing to search for".into(),
                ));
            }
            let body = catalog
                .get("/search", search_query(&query, &kinds, offset, limit))
                .await?;
            Ok(only_kinds(parse_search(&body)?, &kinds))
        })
    }

    fn album_tracks(
        &self,
        id: String,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<Page<Track>, CatalogError>> {
        let catalog = self.clone();
        Box::pin(async move {
            check_id(&id)?;
            let body = catalog
                .get(&format!("/albums/{id}/items"), items_query(offset, limit))
                .await?;
            parse_track_items(&body)
        })
    }

    fn playlist_tracks(
        &self,
        id: String,
        offset: u32,
        limit: u32,
    ) -> BoxFuture<'static, Result<Page<Track>, CatalogError>> {
        let catalog = self.clone();
        Box::pin(async move {
            check_id(&id)?;
            let body = catalog
                .get(
                    &format!("/playlists/{id}/items"),
                    items_query(offset, limit),
                )
                .await?;
            parse_track_items(&body)
        })
    }
}

// --- Reading TIDAL's answers -------------------------------------------------------------------

/// An id that TIDAL writes as a number in some places and as text in others.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawId {
    Number(u64),
    Text(String),
}

impl RawId {
    fn text(self) -> String {
        match self {
            RawId::Number(n) => n.to_string(),
            RawId::Text(text) => text,
        }
    }
}

#[derive(Deserialize)]
struct RawArtist {
    id: RawId,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawAlbumRef {
    id: RawId,
    #[serde(default)]
    title: Option<String>,
}

/// TIDAL's `mediaMetadata`: `tags` lists what the item is available in, and `HIRES_LOSSLESS` is
/// there for a hi-res one, whose `audioQuality` still says only `LOSSLESS`.
#[derive(Deserialize, Default)]
struct RawMediaMetadata {
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct RawTrack {
    id: RawId,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    explicit: Option<bool>,
    #[serde(default, rename = "trackNumber")]
    track_number: Option<u32>,
    #[serde(default, rename = "audioQuality")]
    audio_quality: Option<String>,
    #[serde(default, rename = "mediaMetadata")]
    media_metadata: Option<RawMediaMetadata>,
    #[serde(default, rename = "streamReady")]
    stream_ready: Option<bool>,
    #[serde(default, rename = "allowStreaming")]
    allow_streaming: Option<bool>,
    #[serde(default)]
    artists: Vec<RawArtist>,
    #[serde(default)]
    artist: Option<RawArtist>,
    #[serde(default)]
    album: Option<RawAlbumRef>,
}

#[derive(Deserialize)]
struct RawAlbum {
    id: RawId,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default, rename = "numberOfTracks")]
    number_of_tracks: Option<u32>,
    #[serde(default, rename = "releaseDate")]
    release_date: Option<String>,
    #[serde(default)]
    explicit: Option<bool>,
    #[serde(default, rename = "audioQuality")]
    audio_quality: Option<String>,
    #[serde(default, rename = "mediaMetadata")]
    media_metadata: Option<RawMediaMetadata>,
    #[serde(default)]
    artists: Vec<RawArtist>,
    #[serde(default)]
    artist: Option<RawArtist>,
}

#[derive(Deserialize)]
struct RawCreator {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawPlaylist {
    uuid: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    creator: Option<RawCreator>,
    #[serde(default, rename = "numberOfTracks")]
    number_of_tracks: Option<u32>,
    #[serde(default)]
    duration: Option<u64>,
}

/// A page as TIDAL writes it, with its items still unread.
#[derive(Deserialize)]
struct RawPage {
    #[serde(default)]
    items: Vec<Value>,
    #[serde(default, rename = "totalNumberOfItems")]
    total: Option<u64>,
    #[serde(default)]
    offset: Option<u64>,
}

#[derive(Deserialize)]
struct RawSearch {
    #[serde(default)]
    tracks: Option<RawPage>,
    #[serde(default)]
    albums: Option<RawPage>,
    #[serde(default)]
    artists: Option<RawPage>,
    #[serde(default)]
    playlists: Option<RawPage>,
}

fn secs(seconds: Option<u64>) -> Option<Duration> {
    seconds.map(Duration::from_secs)
}

/// The best tier an item is available in: hi-res when its metadata says so, else what its
/// `audioQuality` says.
fn tier(audio_quality: Option<&str>, metadata: Option<&RawMediaMetadata>) -> Option<Quality> {
    let hires = metadata.is_some_and(|m| m.tags.iter().any(|tag| tag == "HIRES_LOSSLESS"));
    if hires {
        Some(Quality::Hires)
    } else {
        audio_quality.and_then(tidal::quality_from_api)
    }
}

fn artists_of(artists: Vec<RawArtist>, single: Option<RawArtist>) -> Vec<ArtistRef> {
    let artists = if artists.is_empty() {
        single.into_iter().collect()
    } else {
        artists
    };
    artists
        .into_iter()
        .map(|artist| ArtistRef {
            id: artist.id.text(),
            name: artist.name.unwrap_or_default(),
        })
        .collect()
}

impl From<RawTrack> for Track {
    fn from(raw: RawTrack) -> Self {
        Track {
            id: raw.id.text(),
            title: raw.title.unwrap_or_default(),
            version: raw.version.filter(|version| !version.is_empty()),
            artists: artists_of(raw.artists, raw.artist),
            album: raw.album.map(|album| AlbumRef {
                id: album.id.text(),
                title: album.title.unwrap_or_default(),
            }),
            duration: secs(raw.duration),
            explicit: raw.explicit.unwrap_or(false),
            track_number: raw.track_number,
            quality: tier(raw.audio_quality.as_deref(), raw.media_metadata.as_ref()),
            streamable: raw.allow_streaming.unwrap_or(true) && raw.stream_ready.unwrap_or(true),
        }
    }
}

impl From<RawAlbum> for Album {
    fn from(raw: RawAlbum) -> Self {
        Album {
            id: raw.id.text(),
            title: raw.title.unwrap_or_default(),
            version: raw.version.filter(|version| !version.is_empty()),
            artists: artists_of(raw.artists, raw.artist),
            release_date: raw.release_date,
            track_count: raw.number_of_tracks,
            duration: secs(raw.duration),
            explicit: raw.explicit.unwrap_or(false),
            quality: tier(raw.audio_quality.as_deref(), raw.media_metadata.as_ref()),
        }
    }
}

impl From<RawArtist> for Artist {
    fn from(raw: RawArtist) -> Self {
        Artist {
            id: raw.id.text(),
            name: raw.name.unwrap_or_default(),
        }
    }
}

impl From<RawPlaylist> for Playlist {
    fn from(raw: RawPlaylist) -> Self {
        Playlist {
            id: raw.uuid,
            title: raw.title.unwrap_or_default(),
            creator: raw.creator.and_then(|creator| creator.name),
            description: raw.description.filter(|text| !text.is_empty()),
            track_count: raw.number_of_tracks,
            duration: secs(raw.duration),
        }
    }
}

/// Reads each item of a page as `Raw`, leaving out (and saying so) any that cannot be read.
fn read_items<Raw: DeserializeOwned, T: From<Raw>>(page: RawPage) -> Page<T> {
    let mut skipped = 0;
    let items: Vec<T> = page
        .items
        .into_iter()
        .filter_map(|value| match serde_json::from_value::<Raw>(value) {
            Ok(raw) => Some(T::from(raw)),
            Err(_) => {
                skipped += 1;
                None
            }
        })
        .collect();
    if skipped > 0 {
        crate::warn!(
            "Warning: {skipped} item(s) of a TIDAL answer could not be read and were left out."
        );
    }
    Page {
        total: page.total.unwrap_or(items.len() as u64),
        offset: page.offset.unwrap_or(0),
        items,
    }
}

fn unreadable(error: serde_json::Error) -> CatalogError {
    CatalogError::Unavailable(format!("TIDAL's answer could not be read: {error}"))
}

fn parse_search(body: &str) -> Result<SearchResults, CatalogError> {
    let raw: RawSearch = serde_json::from_str(body).map_err(unreadable)?;
    Ok(SearchResults {
        tracks: raw.tracks.map(read_items::<RawTrack, Track>),
        albums: raw.albums.map(read_items::<RawAlbum, Album>),
        artists: raw.artists.map(read_items::<RawArtist, Artist>),
        playlists: raw.playlists.map(read_items::<RawPlaylist, Playlist>),
    })
}

/// Keeps only the kinds that were asked for (all of them when `kinds` is empty). TIDAL answers
/// with a page for every kind, empty for those it was not asked about, and a client cannot tell
/// that from "nothing found" unless the ones not asked for are left out.
fn only_kinds(mut results: SearchResults, kinds: &[Kind]) -> SearchResults {
    if kinds.is_empty() {
        return results;
    }
    if !kinds.contains(&Kind::Tracks) {
        results.tracks = None;
    }
    if !kinds.contains(&Kind::Albums) {
        results.albums = None;
    }
    if !kinds.contains(&Kind::Artists) {
        results.artists = None;
    }
    if !kinds.contains(&Kind::Playlists) {
        results.playlists = None;
    }
    results
}

/// The listing of an album's or a playlist's items: each is `{"item": {track}, "type": "track"}`,
/// and videos (`"type": "video"`) are not tracks, so they are left out.
fn parse_track_items(body: &str) -> Result<Page<Track>, CatalogError> {
    let mut page: RawPage = serde_json::from_str(body).map_err(unreadable)?;
    let total = page.total;
    page.items = page
        .items
        .into_iter()
        .filter(|entry| {
            entry
                .get("type")
                .and_then(Value::as_str)
                .is_none_or(|t| t == "track")
        })
        .map(|entry| match entry {
            Value::Object(mut fields) if fields.contains_key("item") => {
                fields.remove("item").unwrap_or(Value::Null)
            }
            other => other,
        })
        .collect();
    // Leaving videos out makes a page shorter than the total says: that is TIDAL's count, kept.
    let mut out = read_items::<RawTrack, Track>(page);
    if let Some(total) = total {
        out.total = total;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const SEARCH: &str = r#"{
        "artists": {"limit":50,"offset":0,"totalNumberOfItems":2,"items":[
            {"id":3606115,"name":"Korn","picture":"abc"},
            {"id":"77","name":"Korn Tribute"}
        ]},
        "albums": {"limit":50,"offset":0,"totalNumberOfItems":1,"items":[
            {"id":33723912,"title":"Untouchables","version":null,"duration":3103,
             "numberOfTracks":14,"releaseDate":"2002-06-11","explicit":true,
             "audioQuality":"HI_RES_LOSSLESS",
             "artists":[{"id":3606115,"name":"Korn","type":"MAIN"}]}
        ]},
        "tracks": {"limit":50,"offset":0,"totalNumberOfItems":123,"items":[
            {"id":33723914,"title":"Here to Stay","version":"Remastered","duration":271,
             "explicit":false,"trackNumber":2,"audioQuality":"HI_RES_LOSSLESS",
             "streamReady":true,"allowStreaming":true,
             "artists":[{"id":3606115,"name":"Korn"}],
             "album":{"id":33723912,"title":"Untouchables","cover":"x"}}
        ]},
        "playlists": {"limit":50,"offset":0,"totalNumberOfItems":1,"items":[
            {"uuid":"a1b2c3d4-0000-1111-2222-333344445555","title":"Nu metal",
             "description":"","creator":{"id":0,"name":"TIDAL"},
             "numberOfTracks":40,"duration":9000}
        ]},
        "topHit": {"type":"ARTISTS","value":{"id":1}}
    }"#;

    #[test]
    fn a_search_answer_is_read_into_the_four_kinds() {
        let results = parse_search(SEARCH).unwrap();

        let artists = results.artists.unwrap();
        assert_eq!(artists.total, 2);
        assert_eq!(
            artists.items[0],
            Artist {
                id: "3606115".into(),
                name: "Korn".into()
            }
        );
        assert_eq!(
            artists.items[1].id, "77",
            "an id written as text is fine too"
        );

        let album = &results.albums.unwrap().items[0];
        assert_eq!(album.id, "33723912");
        assert_eq!(album.title, "Untouchables");
        assert_eq!(album.version, None, "a null version is none");
        assert_eq!(album.track_count, Some(14));
        assert_eq!(album.release_date.as_deref(), Some("2002-06-11"));
        assert_eq!(album.duration, Some(Duration::from_secs(3103)));
        assert!(album.explicit);
        assert_eq!(album.quality, Some(Quality::Hires));
        assert_eq!(album.artists[0].name, "Korn");

        let tracks = results.tracks.unwrap();
        assert_eq!(
            tracks.total, 123,
            "the total is TIDAL's, not this page's length"
        );
        let track = &tracks.items[0];
        assert_eq!(track.id, "33723914");
        assert_eq!(track.version.as_deref(), Some("Remastered"));
        assert_eq!(track.track_number, Some(2));
        assert_eq!(track.duration, Some(Duration::from_secs(271)));
        assert_eq!(
            track.album,
            Some(AlbumRef {
                id: "33723912".into(),
                title: "Untouchables".into()
            })
        );
        assert_eq!(track.quality, Some(Quality::Hires));
        assert!(track.streamable);

        let playlist = &results.playlists.unwrap().items[0];
        assert_eq!(playlist.id, "a1b2c3d4-0000-1111-2222-333344445555");
        assert_eq!(playlist.creator.as_deref(), Some("TIDAL"));
        assert_eq!(playlist.description, None, "an empty description is none");
        assert_eq!(playlist.track_count, Some(40));
    }

    #[test]
    fn the_kinds_that_were_not_asked_for_are_left_out_even_when_tidal_sends_them_empty() {
        let answer = parse_search(SEARCH).unwrap();
        let only_albums = only_kinds(answer.clone(), &[Kind::Albums]);
        assert!(only_albums.albums.is_some());
        assert!(only_albums.tracks.is_none() && only_albums.artists.is_none());
        assert!(only_albums.playlists.is_none());

        let two = only_kinds(answer.clone(), &[Kind::Tracks, Kind::Playlists]);
        assert!(two.tracks.is_some() && two.playlists.is_some());
        assert!(two.albums.is_none() && two.artists.is_none());

        assert_eq!(
            only_kinds(answer.clone(), &[]),
            answer,
            "no kinds means all of them"
        );
    }

    #[test]
    fn only_the_id_is_required_of_an_item() {
        let results = parse_search(
            r#"{"tracks":{"items":[{"id":1}]},"albums":{"items":[{"id":2}]},
                "playlists":{"items":[{"uuid":"u"}]}}"#,
        )
        .unwrap();
        let track = &results.tracks.unwrap().items[0];
        assert_eq!((track.id.as_str(), track.title.as_str()), ("1", ""));
        assert!(track.artists.is_empty() && track.album.is_none() && track.streamable);
        assert_eq!(results.albums.unwrap().items[0].id, "2");
        assert_eq!(results.playlists.unwrap().items[0].id, "u");
    }

    #[test]
    fn a_kind_that_was_not_asked_for_is_none_and_an_empty_one_is_an_empty_page() {
        let results = parse_search(r#"{"albums":{"items":[],"totalNumberOfItems":0}}"#).unwrap();
        assert!(results.tracks.is_none() && results.artists.is_none());
        assert_eq!(results.albums.unwrap(), Page::empty());
    }

    #[test]
    fn a_single_unreadable_item_is_left_out_not_fatal() {
        let results = parse_search(
            r#"{"tracks":{"totalNumberOfItems":3,"items":[
                {"id":1,"title":"a"}, {"title":"no id"}, {"id":3,"title":"c"}]}}"#,
        )
        .unwrap();
        let tracks = results.tracks.unwrap();
        assert_eq!(
            tracks
                .items
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            ["1", "3"]
        );
        assert_eq!(tracks.total, 3);
    }

    #[test]
    fn an_answer_that_is_not_json_is_unavailable() {
        assert!(matches!(
            parse_search("<html>"),
            Err(CatalogError::Unavailable(_))
        ));
    }

    #[test]
    fn a_track_with_a_single_artist_field_and_no_artists_list_still_has_its_artist() {
        let results =
            parse_search(r#"{"tracks":{"items":[{"id":1,"artist":{"id":5,"name":"Solo"}}]}}"#)
                .unwrap();
        assert_eq!(results.tracks.unwrap().items[0].artists[0].name, "Solo");
    }

    #[test]
    fn a_hires_track_is_told_by_its_metadata_because_its_audio_quality_says_only_lossless() {
        let results = parse_search(
            r#"{"tracks":{"items":[
                {"id":1,"audioQuality":"LOSSLESS","mediaMetadata":{"tags":["LOSSLESS","HIRES_LOSSLESS"]}},
                {"id":2,"audioQuality":"LOSSLESS","mediaMetadata":{"tags":["LOSSLESS"]}},
                {"id":3,"audioQuality":"HIGH"},
                {"id":4,"mediaMetadata":{"tags":["HIRES_LOSSLESS"]}},
                {"id":5}
            ]},
            "albums":{"items":[
                {"id":6,"audioQuality":"LOSSLESS","mediaMetadata":{"tags":["HIRES_LOSSLESS"]}}
            ]}}"#,
        )
        .unwrap();
        let qualities: Vec<_> = results
            .tracks
            .unwrap()
            .items
            .iter()
            .map(|t| t.quality)
            .collect();
        assert_eq!(
            qualities,
            [
                Some(Quality::Hires),
                Some(Quality::Lossless),
                Some(Quality::High),
                Some(Quality::Hires),
                None
            ]
        );
        assert_eq!(
            results.albums.unwrap().items[0].quality,
            Some(Quality::Hires)
        );
    }

    #[test]
    fn a_track_that_cannot_be_streamed_says_so() {
        let results = parse_search(
            r#"{"tracks":{"items":[{"id":1,"allowStreaming":false},{"id":2,"streamReady":false}]}}"#,
        )
        .unwrap();
        assert!(results.tracks.unwrap().items.iter().all(|t| !t.streamable));
    }

    #[test]
    fn the_items_of_an_album_are_unwrapped_and_videos_are_left_out() {
        let page = parse_track_items(
            r#"{"limit":100,"offset":0,"totalNumberOfItems":3,"items":[
                {"type":"track","item":{"id":10,"title":"One","trackNumber":1}},
                {"type":"video","item":{"id":99,"title":"A clip"}},
                {"type":"track","item":{"id":11,"title":"Two","trackNumber":2}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|t| t.title.as_str())
                .collect::<Vec<_>>(),
            ["One", "Two"]
        );
        assert_eq!(page.total, 3, "TIDAL's count stays");
    }

    #[test]
    fn a_listing_of_bare_tracks_is_read_too() {
        let page = parse_track_items(r#"{"items":[{"id":1,"title":"x"}],"totalNumberOfItems":1}"#)
            .unwrap();
        assert_eq!(page.items[0].id, "1");
    }

    #[test]
    fn http_statuses_mean_what_the_interface_needs_to_know() {
        let is = |status: u16| classify_status(StatusCode::from_u16(status).unwrap(), "body");
        assert!(matches!(is(401), CatalogError::NotLoggedIn(_)));
        assert!(matches!(is(403), CatalogError::NotLoggedIn(_)));
        assert_eq!(is(404), CatalogError::NotFound);
        assert_eq!(is(429), CatalogError::RateLimited);
        assert!(matches!(is(400), CatalogError::Invalid(_)));
        assert!(matches!(is(500), CatalogError::Unavailable(_)));
        assert!(matches!(is(503), CatalogError::Unavailable(_)));
    }

    #[test]
    fn a_refresh_that_could_not_reach_tidal_is_unavailable_and_any_other_login_problem_is_not() {
        assert!(matches!(
            session_error(anyhow::anyhow!(
                "refreshing the TIDAL access token: timed out"
            )),
            CatalogError::Unavailable(_)
        ));
        assert!(matches!(
            session_error(anyhow::anyhow!("there is no TIDAL session")),
            CatalogError::NotLoggedIn(_)
        ));
    }

    #[test]
    fn ids_that_could_change_a_path_are_refused() {
        for good in ["33723912", "a1b2c3d4-0000-1111-2222-333344445555"] {
            assert!(check_id(good).is_ok(), "{good}");
        }
        for bad in ["", "../x", "1/2", "1?x=y", "a b", "1#"] {
            assert!(check_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_search_asks_for_all_kinds_by_default_and_never_more_than_the_limit() {
        let query = search_query("korn", &[], 0, 5000);
        assert_eq!(query[0], ("query", "korn".to_string()));
        assert_eq!(
            query[1],
            ("types", "TRACKS,ALBUMS,ARTISTS,PLAYLISTS".to_string())
        );
        assert_eq!(query[2], ("limit", "300".to_string()));
        assert_eq!(query[3], ("offset", "0".to_string()));
        let only = search_query("x", &[Kind::Albums, Kind::Tracks], 100, 0);
        assert_eq!(only[1].1, "ALBUMS,TRACKS");
        assert_eq!(only[2].1, "1", "at least one");
        assert_eq!(only[3].1, "100");
        assert_eq!(items_query(0, 1000)[0].1, "100");
    }

    /// A one-request server on a local port: answers with `status` and `body`, and hands back the
    /// request it received.
    async fn serve_once(
        status: u16,
        body: &str,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (sent, received) = tokio::sync::oneshot::channel();
        let body = body.to_string();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 8192];
            let n = socket.read(&mut request).await.unwrap_or(0);
            let _ = sent.send(String::from_utf8_lossy(&request[..n]).into_owned());
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
        (base, received)
    }

    #[tokio::test]
    async fn a_request_carries_the_token_the_country_and_the_query() {
        let (base, received) = serve_once(200, r#"{"tracks":{"items":[]}}"#).await;
        let account = Account {
            base: &base,
            token: "secret-token",
            country: "MX",
        };
        let http = reqwest::Client::new();
        let body = get(
            &http,
            &account,
            "/search",
            &search_query("nu metal", &[Kind::Tracks], 50, 25),
        )
        .await
        .unwrap();
        assert!(parse_search(&body).unwrap().tracks.is_some());

        let request = received.await.unwrap();
        let first = request.lines().next().unwrap();
        assert!(first.starts_with("GET /search?"), "{first}");
        for wanted in [
            "countryCode=MX",
            "query=nu+metal",
            "types=TRACKS",
            "limit=25",
            "offset=50",
        ] {
            assert!(
                first.contains(wanted) || first.replace("%20", "+").contains(wanted),
                "{wanted} not in {first}"
            );
        }
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer secret-token"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn an_error_status_becomes_the_matching_catalog_error() {
        for (status, matches) in [
            (404, CatalogError::NotFound),
            (429, CatalogError::RateLimited),
        ] {
            let (base, _) = serve_once(status, "").await;
            let account = Account {
                base: &base,
                token: "t",
                country: "US",
            };
            let error = get(&reqwest::Client::new(), &account, "/albums/1/items", &[])
                .await
                .unwrap_err();
            assert_eq!(error, matches, "status {status}");
        }
        let account = Account {
            base: "http://127.0.0.1:1",
            token: "t",
            country: "US",
        };
        let error = get(&reqwest::Client::new(), &account, "/search", &[])
            .await
            .unwrap_err();
        assert!(
            matches!(error, CatalogError::Unavailable(_)),
            "nobody listens: {error:?}"
        );
    }

    /// Against the real TIDAL, with the login this machine has: what the fixtures above are
    /// modelled on. Run with `--ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs a TIDAL login and network"]
    async fn a_real_search_and_album_listing() {
        use crate::auth::{Interaction, open_store};
        use crate::config::SessionStoreKind;
        use crate::openers::TidalOpener;

        let store = open_store(SessionStoreKind::default(), Interaction::Allow).unwrap();
        let opener =
            TidalOpener::from_store(tidal::build_http_client().unwrap(), store, Quality::Hires);
        let catalog = opener.catalog();

        let results = catalog
            .search("korn untouchables".into(), vec![], 0, 5)
            .await
            .unwrap();
        for (kind, count) in [
            ("tracks", results.tracks.as_ref().map(|p| p.items.len())),
            ("albums", results.albums.as_ref().map(|p| p.items.len())),
            ("artists", results.artists.as_ref().map(|p| p.items.len())),
            (
                "playlists",
                results.playlists.as_ref().map(|p| p.items.len()),
            ),
        ] {
            println!("{kind}: {count:?}");
        }
        let tracks = results.tracks.expect("tracks were asked for");
        assert!(!tracks.items.is_empty(), "a search for korn finds tracks");
        println!("first track: {:?}", tracks.items[0]);

        // A track known to be hi-res, whose `audioQuality` says only LOSSLESS.
        let found = catalog
            .search(
                "korn falling away from me".into(),
                vec![Kind::Tracks],
                0,
                10,
            )
            .await
            .unwrap()
            .tracks
            .unwrap();
        let hires = found
            .items
            .iter()
            .find(|track| track.id == "33723914")
            .expect("the track is found");
        assert_eq!(hires.quality, Some(Quality::Hires));

        let album = "33723912";
        let listing = catalog.album_tracks(album.into(), 0, 100).await.unwrap();
        println!(
            "album {album}: {} of {} tracks, first {:?}",
            listing.items.len(),
            listing.total,
            listing.items.first().map(|t| &t.title)
        );
        assert!(!listing.items.is_empty());
        assert_eq!(
            catalog.album_tracks("1".into(), 0, 10).await.unwrap_err(),
            CatalogError::NotFound
        );
    }
}
