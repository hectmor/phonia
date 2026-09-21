//! How to get the audio of one track: from a local file or from TIDAL.
//!
//! Each opener decides how its tracks can be sought (see [`SeekMode`]), because that depends on
//! where the audio comes from and the engine can't know it. Which track plays next is not their
//! business: that is the queue's.
//!
//! Tracks are named by a [`Source`], which has a text form (`file:/abs/path.flac`,
//! `tidal:12345678`) used in the queue and over the wire; the [`DispatchOpener`] reads it and
//! hands the track to the right opener.

use crate::auth;
use crate::dash::{DashSegments, SegmentTiming};
use crate::decode::Decoder;
use crate::engine::{LoadedTrack, SeekMode, TrackMedia, TrackMeta, TrackOpener, TrackRef};
use crate::stream;
use crate::tidal::{self, ManifestKind};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::future::BoxFuture;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tidlers::TidalClient;
use tidlers::client::models::playback::AudioQuality;
use tokio::sync::{RwLock, RwLockReadGuard};

/// Where a track's audio comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A local file, by absolute path.
    File(PathBuf),
    /// A TIDAL track, by its numeric id.
    Tidal(String),
}

impl Source {
    /// A local file. The path must be absolute (a relative one means nothing to whoever ends up
    /// opening it, such as a daemon with another working directory) and valid UTF-8 (it travels
    /// as text).
    pub fn file(path: &Path) -> Result<Source> {
        if !path.is_absolute() {
            bail!("a file source needs an absolute path, got {path:?}");
        }
        if path.to_str().is_none_or(|text| text.contains('\0')) {
            bail!("{path:?} can't be used as a source: it is not valid text");
        }
        Ok(Source::File(path.to_path_buf()))
    }

    /// Reads the text form: `file:/abs/path` or `tidal:<track id>`.
    pub fn parse(text: &str) -> Result<Source> {
        if let Some(path) = text.strip_prefix("file:") {
            return Source::file(Path::new(path));
        }
        if let Some(id) = text.strip_prefix("tidal:") {
            if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
                bail!("a TIDAL track id is a number, got {id:?}");
            }
            return Ok(Source::Tidal(id.to_string()));
        }
        bail!("unknown source {text:?}: expected file:/absolute/path or tidal:<track id>")
    }

    /// The text form, which [`Source::parse`] reads back.
    pub fn to_wire(&self) -> String {
        match self {
            Source::File(path) => format!("file:{}", path.display()),
            Source::Tidal(id) => format!("tidal:{id}"),
        }
    }
}

/// What is known about a track without playing it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceInfo {
    pub title: Option<String>,
    pub duration: Option<Duration>,
}

/// Why a source could not be described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescribeError {
    /// The source is wrong and will stay wrong: a missing or unplayable file, a track TIDAL does
    /// not have. Adding it to a queue is pointless.
    Invalid(String),
    /// Nothing is known about the source right now (no network, TIDAL unreachable), but that says
    /// nothing against it.
    Unavailable(String),
}

impl fmt::Display for DescribeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DescribeError::Invalid(reason) | DescribeError::Unavailable(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for DescribeError {}

/// Opens local audio files, which a track's reference names by path. A file can be repositioned
/// freely, so seeking is done in place.
pub struct FileOpener;

impl FileOpener {
    /// Probes the file: its name, and its length if the container says. A file that can't be
    /// opened or isn't audio the player understands is [`DescribeError::Invalid`].
    pub async fn describe(&self, path: &Path) -> Result<SourceInfo, DescribeError> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&path)
                .map_err(|error| DescribeError::Invalid(format!("opening {path:?}: {error}")))?;
            let extension = path.extension().and_then(|ext| ext.to_str());
            let decoder = Decoder::open(file, extension)
                .map_err(|error| DescribeError::Invalid(format!("{path:?} can't be played: {error:#}")))?;
            Ok(SourceInfo {
                title: path.file_name().map(|name| name.to_string_lossy().into_owned()),
                duration: decoder.duration(),
            })
        })
        .await
        .map_err(|error| DescribeError::Unavailable(format!("probing the file failed: {error}")))?
    }
}

impl TrackOpener for FileOpener {
    fn open(&self, track: TrackRef, _at: Duration) -> BoxFuture<'static, Result<LoadedTrack>> {
        Box::pin(async move {
            let path = PathBuf::from(&track.0);
            let file = std::fs::File::open(&path).with_context(|| format!("opening {path:?}"))?;
            let meta = TrackMeta {
                track,
                title: path.file_name().map(|name| name.to_string_lossy().into_owned()),
                duration: None,
            };
            let extension = path.extension().and_then(|ext| ext.to_str()).map(str::to_string);
            Ok(LoadedTrack::new(meta, TrackMedia::Encoded { source: Box::new(file), extension })
                .seekable_in_place())
        })
    }
}

/// Opens TIDAL tracks, which a track's reference names by TIDAL track id, and streams them.
pub struct TidalOpener {
    http: reqwest::Client,
    /// Behind a lock because the access token expires after a while and has to be refreshed in
    /// place, which a long-running process (a daemon) will always eventually need.
    client: Arc<RwLock<TidalClient>>,
    quality: AudioQuality,
    print_info: bool,
    /// Where to copy the audio of the next opening, if anywhere.
    save_to: Mutex<Option<std::fs::File>>,
}

impl TidalOpener {
    pub fn new(http: reqwest::Client, client: TidalClient, quality: AudioQuality) -> Self {
        Self { http, client: Arc::new(RwLock::new(client)), quality, print_info: false, save_to: Mutex::new(None) }
    }

    /// Name and length of a TIDAL track, without streaming it.
    pub async fn describe(&self, id: &str) -> Result<SourceInfo, DescribeError> {
        let client = fresh_client(&self.client)
            .await
            .map_err(|error| DescribeError::Unavailable(format!("{error:#}")))?;
        match client.get_track(id).await {
            Ok(track) => Ok(SourceInfo {
                title: Some(format!("{} - {}", track.artist.name, track.title)),
                duration: Some(Duration::from_secs(track.duration)),
            }),
            Err(error) => Err(classify_track_error(id, &error)),
        }
    }

    /// Print what TIDAL says about a track (quality, bit depth, rate) whenever one is opened
    /// from its start.
    pub fn print_info(mut self) -> Self {
        self.print_info = true;
        self
    }

    /// Copy the audio of the first opening to `file` as it streams.
    pub fn saving_to(self, file: std::fs::File) -> Self {
        *self.save_to.lock().unwrap() = Some(file);
        self
    }
}

/// Whether an error from asking TIDAL about a track means the track does not exist (which will
/// not change) or merely that TIDAL could not be asked.
fn classify_track_error(id: &str, error: &tidlers::TidalError) -> DescribeError {
    use tidlers::TidalError;
    use tidlers::requests::RequestClientError;
    let missing = match error {
        TidalError::NotFound => true,
        // TIDAL answers 404 for an id it doesn't have, which tidlers reports as a status error.
        TidalError::RequestClient(RequestClientError::StatusCode { status, .. }) => {
            *status == reqwest::StatusCode::NOT_FOUND
        }
        _ => false,
    };
    if missing {
        DescribeError::Invalid(format!("TIDAL has no track {id}"))
    } else {
        DescribeError::Unavailable(format!("asking TIDAL about track {id}: {error}"))
    }
}

/// The TIDAL client, with a valid access token: refreshed (and saved, so the next run starts from
/// the new one) if the old one has expired.
async fn fresh_client(lock: &RwLock<TidalClient>) -> Result<RwLockReadGuard<'_, TidalClient>> {
    {
        let mut client = lock.write().await;
        let refreshed = client
            .refresh_access_token(false)
            .await
            .map_err(|error| anyhow!("refreshing the TIDAL access token: {error}"))?;
        if refreshed {
            auth::save_session(&client)?;
        }
    }
    Ok(lock.read().await)
}

impl TrackOpener for TidalOpener {
    fn open(&self, track: TrackRef, at: Duration) -> BoxFuture<'static, Result<LoadedTrack>> {
        let http = self.http.clone();
        let client = self.client.clone();
        let quality = self.quality.clone();
        let print_info = self.print_info;
        // Only the opening from the start is copied: reopening for a seek would append a piece.
        let tee = if at.is_zero() { self.save_to.lock().unwrap().take() } else { None };

        Box::pin(async move {
            let info = {
                let client = fresh_client(&client).await?;
                tidal::fetch_playback_info(&http, &client, &track.0, quality).await?
            };
            if print_info && at.is_zero() {
                tidal::print_playback_info(&info);
            }

            let mut meta = TrackMeta { track, title: None, duration: None };
            Ok(match info.manifest {
                ManifestKind::Dash(dash) => {
                    // Mp4 fragments declare no length of their own; the manifest has it.
                    meta.duration = dash.total_duration();
                    let opening = dash_opening(&dash, at);
                    let source = stream::open_dash_from(&http, &dash, opening.first_segment, tee);
                    let track = LoadedTrack::new(
                        meta,
                        TrackMedia::Encoded { source: Box::new(source), extension: Some("mp4".into()) },
                    );
                    LoadedTrack { seek: opening.seek, ..track }.starting_at(opening.start)
                }
                ManifestKind::Json { url, .. } => {
                    let source = stream::open_url(&http, &url, tee);
                    LoadedTrack::new(meta, TrackMedia::Encoded { source: Box::new(source), extension: None })
                        .forward_only()
                }
            })
        })
    }
}

/// Opens tracks named by a [`Source`] text: `file:` ones itself, `tidal:` ones through a TIDAL
/// opener, if there is one (there is none when nobody is logged in).
pub struct DispatchOpener {
    tidal: Option<TidalOpener>,
}

impl DispatchOpener {
    pub fn new(tidal: Option<TidalOpener>) -> Self {
        Self { tidal }
    }

    /// Name and length of a source without playing it.
    pub async fn describe(&self, source: &Source) -> Result<SourceInfo, DescribeError> {
        match source {
            Source::File(path) => FileOpener.describe(path).await,
            Source::Tidal(id) => match &self.tidal {
                Some(tidal) => tidal.describe(id).await,
                None => Err(DescribeError::Unavailable(TIDAL_UNAVAILABLE.to_string())),
            },
        }
    }
}

const TIDAL_UNAVAILABLE: &str = "TIDAL is not available: run `phonia login` first";

impl TrackOpener for DispatchOpener {
    fn open(&self, track: TrackRef, at: Duration) -> BoxFuture<'static, Result<LoadedTrack>> {
        match Source::parse(&track.0) {
            Err(error) => Box::pin(async move { Err(error) }),
            Ok(Source::File(path)) => FileOpener.open(TrackRef(path.to_string_lossy().into_owned()), at),
            Ok(Source::Tidal(id)) => match &self.tidal {
                Some(tidal) => tidal.open(TrackRef(id), at),
                None => Box::pin(async { Err(anyhow!(TIDAL_UNAVAILABLE)) }),
            },
        }
    }
}

/// Where to start streaming a DASH track so that playback can begin at `at`, and how the result
/// can be sought afterwards.
#[derive(Debug, PartialEq, Eq)]
struct DashOpening {
    first_segment: u32,
    /// Where that segment starts in the track.
    start: Duration,
    seek: SeekMode,
}

fn dash_opening(dash: &DashSegments, at: Duration) -> DashOpening {
    // Without segment timing there's no way to find a position, so the stream is only good for
    // reading forward.
    if dash.timing == SegmentTiming::Unknown {
        return DashOpening { first_segment: dash.start_number, start: Duration::ZERO, seek: SeekMode::ForwardOnly };
    }
    match dash.segment_for_time(at).filter(|_| !at.is_zero()) {
        Some(segment) => DashOpening { first_segment: segment.number, start: segment.start, seek: SeekMode::Reopen },
        None => DashOpening { first_segment: dash.start_number, start: Duration::ZERO, seek: SeekMode::Reopen },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dash::parse_mpd;
    use crate::testutil::wav;

    const TIDAL_LIKE: &str = r#"
        <MPD mediaPresentationDuration="PT5M48.68S">
          <Period><AdaptationSet><Representation codecs="flac">
            <SegmentTemplate timescale="192000" initialization="https://cdn/0.mp4" media="https://cdn/$Number$.mp4" startNumber="1">
              <SegmentTimeline><S d="765952" r="86"/><S d="308736"/></SegmentTimeline>
            </SegmentTemplate>
          </Representation></AdaptationSet></Period>
        </MPD>
    "#;

    #[test]
    fn a_dash_track_starts_from_its_first_segment_and_can_be_reopened() {
        let dash = parse_mpd(TIDAL_LIKE).unwrap();
        assert_eq!(
            dash_opening(&dash, Duration::ZERO),
            DashOpening { first_segment: 1, start: Duration::ZERO, seek: SeekMode::Reopen }
        );
    }

    #[test]
    fn a_dash_seek_reopens_at_the_segment_containing_the_target() {
        let dash = parse_mpd(TIDAL_LIKE).unwrap();
        let opening = dash_opening(&dash, Duration::from_secs(30));
        // 30 s falls in the eighth 3.989 s segment.
        assert_eq!((opening.first_segment, opening.seek), (8, SeekMode::Reopen));
        assert!(opening.start <= Duration::from_secs(30));
        assert!(Duration::from_secs(30) - opening.start < Duration::from_secs(4), "the rest is skipped by the engine");
    }

    #[test]
    fn a_dash_track_without_timing_can_only_be_read_forward() {
        let xml = r#"<MPD><Period><AdaptationSet><Representation>
              <SegmentTemplate initialization="i" media="m$Number$" startNumber="1"/>
            </Representation></AdaptationSet></Period></MPD>"#;
        let dash = parse_mpd(xml).unwrap();
        assert_eq!(
            dash_opening(&dash, Duration::from_secs(30)),
            DashOpening { first_segment: 1, start: Duration::ZERO, seek: SeekMode::ForwardOnly }
        );
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("phonia-openers-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_wav(dir: &std::path::Path, name: &str, frames: usize) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, wav(frames)).unwrap();
        path
    }

    /// Runs a future to completion on a throwaway runtime.
    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(future)
    }

    #[test]
    fn file_opener_reports_a_missing_file_clearly() {
        let error = block_on(FileOpener.open(TrackRef("/no/such/file.flac".into()), Duration::ZERO)).err().unwrap();
        assert!(error.to_string().contains("opening"), "{error}");
    }

    #[test]
    fn file_opener_opens_a_file_that_can_be_repositioned_and_names_it() {
        let dir = temp_dir("open");
        let path = write_wav(&dir, "one.wav", 100);
        let loaded = block_on(FileOpener.open(TrackRef(path.to_string_lossy().into_owned()), Duration::ZERO)).unwrap();
        assert_eq!(loaded.seek, SeekMode::InPlace);
        assert_eq!(loaded.meta.title.as_deref(), Some("one.wav"));
        assert_eq!(loaded.start, Duration::ZERO);
    }

    // ---- Source ----------------------------------------------------------------------------

    /// The text forms that clients and the queue rely on: changing one is a protocol change.
    #[test]
    fn source_text_forms_are_pinned() {
        let cases = [
            ("file:/music/a.flac", Source::File("/music/a.flac".into())),
            ("file:/música/a b (1).flac", Source::File("/música/a b (1).flac".into())),
            ("tidal:12345678", Source::Tidal("12345678".into())),
        ];
        for (text, source) in cases {
            assert_eq!(Source::parse(text).unwrap(), source, "{text}");
            assert_eq!(source.to_wire(), text);
        }
    }

    #[test]
    fn a_source_that_is_not_well_formed_is_refused() {
        let bad = [
            "",
            "a.flac",
            "/music/a.flac",
            "file:",
            "file:relative/a.flac",
            "file:./a.flac",
            "file:/music/a\0b.flac",
            "tidal:",
            "tidal:abc",
            "tidal:12 34",
            "tidal:-5",
            "spotify:123",
            "TIDAL:123",
        ];
        for text in bad {
            assert!(Source::parse(text).is_err(), "{text:?} should be refused");
        }
    }

    #[test]
    fn a_file_source_must_be_absolute_and_says_so() {
        let error = Source::file(Path::new("a.flac")).unwrap_err();
        assert!(error.to_string().contains("absolute"), "{error}");
        assert!(Source::file(Path::new("/tmp/a.flac")).is_ok());
    }

    // ---- describe --------------------------------------------------------------------------

    #[test]
    fn describing_a_file_gives_its_name_and_length() {
        let dir = temp_dir("describe");
        let path = write_wav(&dir, "song.wav", crate::testutil::RATE as usize * 2);
        let info = block_on(FileOpener.describe(&path)).unwrap();
        assert_eq!(info.title.as_deref(), Some("song.wav"));
        assert_eq!(info.duration, Some(Duration::from_secs(2)));
    }

    #[test]
    fn a_missing_or_unplayable_file_is_invalid() {
        let dir = temp_dir("invalid");
        let missing = block_on(FileOpener.describe(&dir.join("nope.flac"))).unwrap_err();
        assert!(matches!(&missing, DescribeError::Invalid(reason) if reason.contains("opening")), "{missing:?}");

        let text = dir.join("notes.flac");
        std::fs::write(&text, "this is not audio at all, just some words").unwrap();
        let garbage = block_on(FileOpener.describe(&text)).unwrap_err();
        assert!(matches!(&garbage, DescribeError::Invalid(reason) if reason.contains("can't be played")), "{garbage:?}");
    }

    // ---- DispatchOpener --------------------------------------------------------------------

    #[test]
    fn the_dispatcher_opens_file_sources_and_keeps_them_seekable() {
        let dir = temp_dir("dispatch");
        let path = write_wav(&dir, "one.wav", 100);
        let opener = DispatchOpener::new(None);
        let source = Source::file(&path).unwrap().to_wire();
        let loaded = block_on(opener.open(TrackRef(source), Duration::ZERO)).unwrap();
        assert_eq!(loaded.seek, SeekMode::InPlace);
        assert_eq!(loaded.meta.title.as_deref(), Some("one.wav"));
    }

    #[test]
    fn the_dispatcher_says_when_tidal_is_not_available() {
        let opener = DispatchOpener::new(None);
        let error = block_on(opener.open(TrackRef("tidal:123".into()), Duration::ZERO)).err().unwrap();
        assert!(error.to_string().contains("phonia login"), "{error}");

        let info = block_on(opener.describe(&Source::Tidal("123".into()))).unwrap_err();
        assert!(matches!(info, DescribeError::Unavailable(ref reason) if reason.contains("phonia login")), "{info:?}");
    }

    #[test]
    fn the_dispatcher_refuses_a_source_it_cannot_read() {
        let opener = DispatchOpener::new(None);
        for bad in ["not a source", "file:relative.flac", "tidal:x"] {
            assert!(block_on(opener.open(TrackRef(bad.into()), Duration::ZERO)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_dispatcher_describes_files() {
        let dir = temp_dir("dispatch-describe");
        let path = write_wav(&dir, "two.wav", crate::testutil::RATE as usize);
        let info = block_on(DispatchOpener::new(None).describe(&Source::file(&path).unwrap())).unwrap();
        assert_eq!(info.duration, Some(Duration::from_secs(1)));
    }

    #[test]
    fn describe_errors_read_as_their_reason() {
        assert_eq!(DescribeError::Invalid("bad".into()).to_string(), "bad");
        assert_eq!(DescribeError::Unavailable("later".into()).to_string(), "later");
    }

    // ---- classifying TIDAL's answers -------------------------------------------------------

    fn status_error(status: reqwest::StatusCode) -> tidlers::TidalError {
        tidlers::TidalError::RequestClient(tidlers::requests::RequestClientError::StatusCode {
            status,
            url: "https://api.tidal.com/v1/tracks/1/".to_string(),
            body_snippet: "{}".to_string(),
        })
    }

    #[test]
    fn a_404_from_tidal_means_the_track_does_not_exist() {
        let error = classify_track_error("12", &status_error(reqwest::StatusCode::NOT_FOUND));
        assert_eq!(error, DescribeError::Invalid("TIDAL has no track 12".into()));
        assert_eq!(
            classify_track_error("12", &tidlers::TidalError::NotFound),
            DescribeError::Invalid("TIDAL has no track 12".into())
        );
    }

    #[test]
    fn other_failures_say_nothing_against_the_track() {
        for status in [
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            reqwest::StatusCode::BAD_GATEWAY,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::FORBIDDEN,
        ] {
            let error = classify_track_error("12", &status_error(status));
            assert!(matches!(error, DescribeError::Unavailable(_)), "{status}: {error:?}");
        }
        let unauthorized = classify_track_error("12", &tidlers::TidalError::NotAuthenticated);
        assert!(matches!(unauthorized, DescribeError::Unavailable(_)), "{unauthorized:?}");
    }
}

