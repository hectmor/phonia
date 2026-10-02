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
use crate::config::Quality;
use crate::dash::{DashSegments, SegmentTiming};
use crate::decode::Decoder;
use crate::engine::{
    Delivered, LoadedTrack, SeekMode, TrackMedia, TrackMeta, TrackOpener, TrackRef,
};
use crate::session::TidalSession;
use crate::stream;
use crate::tidal::{self, ManifestKind};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::future::BoxFuture;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tidlers::TidalClient;
use tokio::sync::RwLock;

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
    /// The track's album's cover id (a UUID); `None` for a local file, or a TIDAL track with no
    /// album.
    pub cover: Option<String>,
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
            DescribeError::Invalid(reason) | DescribeError::Unavailable(reason) => {
                f.write_str(reason)
            }
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
            let decoder = Decoder::open(file, extension).map_err(|error| {
                DescribeError::Invalid(format!("{path:?} can't be played: {error:#}"))
            })?;
            Ok(SourceInfo {
                title: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
                duration: decoder.duration(),
                cover: None,
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
                title: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
                duration: None,
                quality: None,
                cover: None,
            };
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_string);
            Ok(LoadedTrack::new(
                meta,
                TrackMedia::Encoded {
                    source: Box::new(file),
                    extension,
                },
            )
            .seekable_in_place())
        })
    }
}

/// The tiers TIDAL is asked for: the best, which can be changed while the daemon runs, and the
/// worst that will be played.
#[derive(Debug)]
pub struct QualityLimits {
    max: Mutex<Quality>,
    min: Quality,
}

impl QualityLimits {
    pub fn new(max: Quality, min: Quality) -> Self {
        Self {
            max: Mutex::new(max),
            min,
        }
    }

    /// The best tier to ask for.
    pub fn max(&self) -> Quality {
        *self.max.lock().unwrap()
    }

    /// The worst tier that will be played.
    pub fn min(&self) -> Quality {
        self.min
    }

    /// Changes the best tier, from the next track opened on. Refused below the worst tier
    /// accepted: nothing could play.
    pub fn set_max(&self, max: Quality) -> Result<()> {
        if max < self.min {
            bail!(
                "{max} is below the minimum quality ({}): lower tidal.min_quality to allow it",
                self.min
            );
        }
        *self.max.lock().unwrap() = max;
        Ok(())
    }
}

/// Opens TIDAL tracks, which a track's reference names by TIDAL track id, and streams them.
pub struct TidalOpener {
    http: reqwest::Client,
    session: Arc<TidalSession>,
    /// The best tier to ask for, and the worst that will be played.
    limits: Arc<QualityLimits>,
    /// The tier TIDAL delivered for each track opened from its start, so that reopening it for a
    /// seek asks for that tier again: the format must not change in the middle of a track.
    delivered: Arc<Mutex<HashMap<String, Quality>>>,
    print_info: bool,
    /// Where to copy the audio of the next opening, if anywhere.
    save_to: Mutex<Option<std::fs::File>>,
}

impl TidalOpener {
    /// With a client that is already logged in.
    pub fn new(http: reqwest::Client, client: TidalClient, quality: Quality) -> Self {
        let session = TidalSession {
            client: RwLock::new(Some(client)),
            store: None,
        };
        Self {
            http,
            session: Arc::new(session),
            limits: Arc::new(QualityLimits::new(quality, Quality::Lossless)),
            delivered: Arc::default(),
            print_info: false,
            save_to: Mutex::new(None),
        }
    }

    /// Logging in from `store` the first time TIDAL is used, not now.
    pub fn from_store(
        http: reqwest::Client,
        store: Arc<dyn auth::SessionStore>,
        quality: Quality,
    ) -> Self {
        let session = TidalSession {
            client: RwLock::new(None),
            store: Some(store),
        };
        Self {
            http,
            session: Arc::new(session),
            limits: Arc::new(QualityLimits::new(quality, Quality::Lossless)),
            delivered: Arc::default(),
            print_info: false,
            save_to: Mutex::new(None),
        }
    }

    /// TIDAL's catalog (search, the tracks of an album or a playlist), through the same login.
    pub fn catalog(&self) -> crate::catalog::TidalCatalog {
        crate::catalog::TidalCatalog::new(self.http.clone(), self.session.clone())
    }

    /// Name and length of a TIDAL track, without streaming it.
    pub async fn describe(&self, id: &str) -> Result<SourceInfo, DescribeError> {
        let client = self
            .session
            .fresh()
            .await
            .map_err(|error| DescribeError::Unavailable(format!("{error:#}")))?;
        match client.get_track(id).await {
            Ok(track) => Ok(SourceInfo {
                title: Some(format!("{} - {}", track.artist.name, track.title)),
                duration: Some(Duration::from_secs(track.duration)),
                cover: track.album.and_then(|album| album.cover),
            }),
            Err(error) => Err(classify_track_error(id, &error)),
        }
    }

    /// The worst tier to play (default `lossless`): a track TIDAL only has below it fails to
    /// open, rather than playing at a quality nobody asked to accept.
    pub fn min_quality(mut self, min_quality: Quality) -> Self {
        self.limits = Arc::new(QualityLimits::new(self.limits.max(), min_quality));
        self
    }

    /// The tiers this opener asks for, which the best can be changed on while it runs. Take it
    /// after [`TidalOpener::min_quality`]: that makes a new one.
    pub fn limits(&self) -> Arc<QualityLimits> {
        self.limits.clone()
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

impl TrackOpener for TidalOpener {
    fn open(&self, track: TrackRef, at: Duration) -> BoxFuture<'static, Result<LoadedTrack>> {
        let http = self.http.clone();
        let session = self.session.clone();
        let quality = self.limits.max();
        let min_quality = self.limits.min();
        let pinned = self.delivered.lock().unwrap().get(&track.0).copied();
        let ask_for = tier_to_ask(quality, at, pinned);
        let delivered_tiers = self.delivered.clone();
        let print_info = self.print_info;
        // Only the opening from the start is copied: reopening for a seek would append a piece.
        let tee = if at.is_zero() {
            self.save_to.lock().unwrap().take()
        } else {
            None
        };

        Box::pin(async move {
            let info = {
                let client = session.fresh().await?;
                tidal::fetch_playback_info(&http, &client, &track.0, ask_for, min_quality).await?
            };
            let quality = check_delivered(&track, quality, min_quality, &info.audio_quality)?;
            if at.is_zero() {
                let mut tiers = delivered_tiers.lock().unwrap();
                if tiers.len() >= MAX_REMEMBERED_TIERS {
                    tiers.clear();
                }
                tiers.insert(track.0.clone(), quality.delivered);
            }
            if print_info && at.is_zero() {
                tidal::print_playback_info(&info);
            }

            let mut meta = TrackMeta {
                track,
                title: None,
                duration: None,
                quality: Some(quality),
                cover: None,
            };
            Ok(match info.manifest {
                ManifestKind::Dash(dash) => {
                    // Mp4 fragments declare no length of their own; the manifest has it.
                    meta.duration = dash.total_duration();
                    let opening = dash_opening(&dash, at);
                    let source = stream::open_dash_from(&http, &dash, opening.first_segment, tee);
                    let track = LoadedTrack::new(
                        meta,
                        TrackMedia::Encoded {
                            source: Box::new(source),
                            extension: Some("mp4".into()),
                        },
                    );
                    LoadedTrack {
                        seek: opening.seek,
                        ..track
                    }
                    .starting_at(opening.start)
                }
                ManifestKind::Json { url, .. } => {
                    let source = stream::open_url(&http, &url, tee);
                    LoadedTrack::new(
                        meta,
                        TrackMedia::Encoded {
                            source: Box::new(source),
                            extension: None,
                        },
                    )
                    .forward_only()
                }
            })
        })
    }
}

/// How many tracks' delivered tiers an opener keeps: a queue's worth is plenty, and the entry
/// only matters while its track is playing.
const MAX_REMEMBERED_TIERS: usize = 64;

/// The tier to ask TIDAL for when opening a track at `at`.
///
/// Opening from the start asks for the best tier, whatever came before. Reopening in the middle
/// (a seek) asks for the tier the track was delivered in, if it is known, so the format doesn't
/// change under the listener partway through a track.
fn tier_to_ask(best: Quality, at: Duration, delivered: Option<Quality>) -> Quality {
    if at.is_zero() {
        best
    } else {
        delivered.unwrap_or(best)
    }
}

/// Judges the tier TIDAL answered with against what was asked for and the lowest one accepted.
///
/// TIDAL gives a lower tier by itself when a track doesn't exist at the one asked for, and says
/// so only in the response: this is where that is caught, so that playback never falls below the
/// floor without the listener having agreed to it.
fn check_delivered(
    track: &TrackRef,
    requested: Quality,
    min_quality: Quality,
    audio_quality: &str,
) -> Result<Delivered> {
    let Some(delivered) = tidal::quality_from_api(audio_quality) else {
        bail!(
            "TIDAL answered track {} with a quality phonia doesn't know: {audio_quality}",
            track.0
        );
    };
    if delivered < min_quality {
        bail!(
            "track {} is only available in {delivered} quality, below the {min_quality} that \
             tidal.min_quality allows: lower it to play the track at that quality",
            track.0
        );
    }
    Ok(Delivered {
        requested,
        delivered,
    })
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
            Ok(Source::File(path)) => {
                FileOpener.open(TrackRef(path.to_string_lossy().into_owned()), at)
            }
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
        return DashOpening {
            first_segment: dash.start_number,
            start: Duration::ZERO,
            seek: SeekMode::ForwardOnly,
        };
    }
    match dash.segment_for_time(at).filter(|_| !at.is_zero()) {
        Some(segment) => DashOpening {
            first_segment: segment.number,
            start: segment.start,
            seek: SeekMode::Reopen,
        },
        None => DashOpening {
            first_segment: dash.start_number,
            start: Duration::ZERO,
            seek: SeekMode::Reopen,
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::auth::{MemoryStore, StoredSession};

    fn tidal_track() -> TrackRef {
        TrackRef("123".into())
    }

    #[test]
    fn the_best_tier_can_change_but_not_below_the_floor() {
        let limits = QualityLimits::new(Quality::Hires, Quality::Lossless);
        limits.set_max(Quality::Lossless).unwrap();
        assert_eq!(limits.max(), Quality::Lossless);
        assert_eq!(limits.min(), Quality::Lossless);

        let error = limits.set_max(Quality::High).unwrap_err().to_string();
        assert!(error.contains("min_quality"), "{error}");
        assert_eq!(
            limits.max(),
            Quality::Lossless,
            "a refused change changes nothing"
        );
    }

    #[test]
    fn a_seek_reopens_a_track_at_the_tier_it_was_delivered_in() {
        let start = Duration::ZERO;
        let later = Duration::from_secs(90);
        // From the start, always the best: a track that fell back once may not the next time.
        assert_eq!(
            tier_to_ask(Quality::Hires, start, Some(Quality::Lossless)),
            Quality::Hires
        );
        assert_eq!(
            tier_to_ask(Quality::Hires, later, Some(Quality::Lossless)),
            Quality::Lossless
        );
        // Nothing remembered (a track opened by another process, or forgotten): the best.
        assert_eq!(tier_to_ask(Quality::Hires, later, None), Quality::Hires);
    }

    #[test]
    fn a_track_at_the_tier_asked_for_did_not_fall_back() {
        let delivered = check_delivered(
            &tidal_track(),
            Quality::Hires,
            Quality::Lossless,
            "HI_RES_LOSSLESS",
        )
        .unwrap();
        assert_eq!(delivered.delivered, Quality::Hires);
        assert!(!delivered.fell_back());
    }

    #[test]
    fn a_lower_tier_that_is_still_accepted_plays_and_is_reported_as_a_fallback() {
        let delivered = check_delivered(
            &tidal_track(),
            Quality::Hires,
            Quality::Lossless,
            "LOSSLESS",
        )
        .unwrap();
        assert_eq!(delivered.requested, Quality::Hires);
        assert_eq!(delivered.delivered, Quality::Lossless);
        assert!(delivered.fell_back());
    }

    #[test]
    fn a_tier_below_the_floor_is_an_error_naming_the_setting() {
        let error = check_delivered(&tidal_track(), Quality::Hires, Quality::Lossless, "HIGH")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("123") && error.contains("high") && error.contains("min_quality"),
            "{error}"
        );
        // Lowering the floor lets the same answer through.
        assert!(check_delivered(&tidal_track(), Quality::Hires, Quality::High, "HIGH").is_ok());
    }

    #[test]
    fn a_quality_phonia_does_not_know_is_an_error() {
        let error = check_delivered(&tidal_track(), Quality::Hires, Quality::Low, "SURROUND")
            .unwrap_err()
            .to_string();
        assert!(error.contains("SURROUND"), "{error}");
    }

    /// Gapless playback joins tracks by writing the next one's first sample right after the last
    /// of the one before, so a stream must decode to exactly the length it declares: any extra or
    /// missing frame at the end of the last DASH segment would be an audible click or a drift.
    #[tokio::test]
    #[ignore = "needs a TIDAL login and network"]
    async fn a_tidal_dash_track_decodes_to_exactly_the_length_its_manifest_declares() {
        use crate::auth::{Interaction, open_store};
        use crate::config::{Quality, SessionStoreKind};
        use crate::decode::{Decoder, duration_to_frames};

        let store = open_store(SessionStoreKind::default(), Interaction::Allow).unwrap();
        let opener = TidalOpener::from_store(
            crate::tidal::build_http_client().unwrap(),
            store,
            Quality::Hires,
        );
        // A HiRes track (24-bit/192 kHz), which TIDAL serves as DASH; TIDAL_TRACK picks another.
        let id = std::env::var("TIDAL_TRACK").unwrap_or_else(|_| "233059491".to_string());
        let loaded = opener
            .open(TrackRef(id.clone()), Duration::ZERO)
            .await
            .unwrap();
        let declared = loaded
            .meta
            .duration
            .expect("the manifest declares the length");
        let TrackMedia::Encoded { source, extension } = loaded.media else {
            panic!("not an encoded stream")
        };

        let (frames, spec) = tokio::task::spawn_blocking(move || {
            let mut decoder = Decoder::open_boxed(source, extension.as_deref()).unwrap();
            let spec = decoder.spec();
            let mut chunk = Vec::new();
            let mut frames = 0u64;
            while decoder.next_chunk_into(&mut chunk).unwrap() {
                frames += (chunk.len() / spec.channels as usize) as u64;
            }
            (frames, spec)
        })
        .await
        .unwrap();

        let declared_frames = duration_to_frames(declared, spec.sample_rate);
        eprintln!("track {id}: {frames} frames decoded, {declared_frames} declared ({spec:?})");
        assert_eq!(
            frames, declared_frames,
            "the decoded length differs from the manifest's"
        );
    }

    fn session_with(store: Option<Arc<MemoryStore>>) -> TidalSession {
        TidalSession {
            client: RwLock::new(None),
            store: store.map(|store| store as Arc<dyn auth::SessionStore>),
        }
    }

    #[tokio::test]
    async fn without_a_login_the_first_use_says_to_log_in() {
        let store = Arc::new(MemoryStore::new());
        let error = session_with(Some(store.clone()))
            .fresh()
            .await
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("phonia login"), "{error:#}");
        assert_eq!(store.loads(), 1, "the store is asked when TIDAL is used");
    }

    #[tokio::test]
    async fn a_login_made_after_the_daemon_started_is_found_without_a_restart() {
        let store = Arc::new(MemoryStore::new());
        let session = session_with(Some(store.clone()));
        assert!(session.fresh().await.is_err());

        // `phonia login` runs in another process, writing to the same store.
        let stored = StoredSession {
            v: 1,
            refresh_token: "r".into(),
            client_id: "c".into(),
            client_secret: "s".into(),
        };
        crate::auth::SessionStore::save(&*store, &stored)
            .await
            .unwrap();
        // It loads now; refreshing then needs TIDAL, which is not asked in a test.
        let mut guard = session.client.write().await;
        assert!(guard.is_none());
        *guard = Some(auth::load_client(&*store).await.unwrap());
        assert!(
            guard
                .as_ref()
                .unwrap()
                .session
                .auth
                .refresh_token
                .as_deref()
                == Some("r")
        );
    }

    #[tokio::test]
    async fn a_store_that_cannot_be_reached_is_named_in_the_error() {
        let store = Arc::new(MemoryStore::new());
        store.fail_with("no bus");
        let error = session_with(Some(store)).fresh().await.err().unwrap();
        let text = format!("{error:#}");
        assert!(text.contains("memory") && text.contains("no bus"), "{text}");
    }

    #[tokio::test]
    async fn a_session_with_no_store_and_no_client_is_an_error() {
        assert!(session_with(None).fresh().await.is_err());
    }
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
            DashOpening {
                first_segment: 1,
                start: Duration::ZERO,
                seek: SeekMode::Reopen
            }
        );
    }

    #[test]
    fn a_dash_seek_reopens_at_the_segment_containing_the_target() {
        let dash = parse_mpd(TIDAL_LIKE).unwrap();
        let opening = dash_opening(&dash, Duration::from_secs(30));
        // 30 s falls in the eighth 3.989 s segment.
        assert_eq!((opening.first_segment, opening.seek), (8, SeekMode::Reopen));
        assert!(opening.start <= Duration::from_secs(30));
        assert!(
            Duration::from_secs(30) - opening.start < Duration::from_secs(4),
            "the rest is skipped by the engine"
        );
    }

    #[test]
    fn a_dash_track_without_timing_can_only_be_read_forward() {
        let xml = r#"<MPD><Period><AdaptationSet><Representation>
              <SegmentTemplate initialization="i" media="m$Number$" startNumber="1"/>
            </Representation></AdaptationSet></Period></MPD>"#;
        let dash = parse_mpd(xml).unwrap();
        assert_eq!(
            dash_opening(&dash, Duration::from_secs(30)),
            DashOpening {
                first_segment: 1,
                start: Duration::ZERO,
                seek: SeekMode::ForwardOnly
            }
        );
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phonia-openers-test-{}-{name}", std::process::id()));
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
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn file_opener_reports_a_missing_file_clearly() {
        let error =
            block_on(FileOpener.open(TrackRef("/no/such/file.flac".into()), Duration::ZERO))
                .err()
                .unwrap();
        assert!(error.to_string().contains("opening"), "{error}");
    }

    #[test]
    fn file_opener_opens_a_file_that_can_be_repositioned_and_names_it() {
        let dir = temp_dir("open");
        let path = write_wav(&dir, "one.wav", 100);
        let loaded = block_on(FileOpener.open(
            TrackRef(path.to_string_lossy().into_owned()),
            Duration::ZERO,
        ))
        .unwrap();
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
            (
                "file:/música/a b (1).flac",
                Source::File("/música/a b (1).flac".into()),
            ),
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
        assert!(
            matches!(&missing, DescribeError::Invalid(reason) if reason.contains("opening")),
            "{missing:?}"
        );

        let text = dir.join("notes.flac");
        std::fs::write(&text, "this is not audio at all, just some words").unwrap();
        let garbage = block_on(FileOpener.describe(&text)).unwrap_err();
        assert!(
            matches!(&garbage, DescribeError::Invalid(reason) if reason.contains("can't be played")),
            "{garbage:?}"
        );
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
        let error = block_on(opener.open(TrackRef("tidal:123".into()), Duration::ZERO))
            .err()
            .unwrap();
        assert!(error.to_string().contains("phonia login"), "{error}");

        let info = block_on(opener.describe(&Source::Tidal("123".into()))).unwrap_err();
        assert!(
            matches!(info, DescribeError::Unavailable(ref reason) if reason.contains("phonia login")),
            "{info:?}"
        );
    }

    #[test]
    fn the_dispatcher_refuses_a_source_it_cannot_read() {
        let opener = DispatchOpener::new(None);
        for bad in ["not a source", "file:relative.flac", "tidal:x"] {
            assert!(
                block_on(opener.open(TrackRef(bad.into()), Duration::ZERO)).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_dispatcher_describes_files() {
        let dir = temp_dir("dispatch-describe");
        let path = write_wav(&dir, "two.wav", crate::testutil::RATE as usize);
        let info =
            block_on(DispatchOpener::new(None).describe(&Source::file(&path).unwrap())).unwrap();
        assert_eq!(info.duration, Some(Duration::from_secs(1)));
    }

    #[test]
    fn describe_errors_read_as_their_reason() {
        assert_eq!(DescribeError::Invalid("bad".into()).to_string(), "bad");
        assert_eq!(
            DescribeError::Unavailable("later".into()).to_string(),
            "later"
        );
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
        assert_eq!(
            error,
            DescribeError::Invalid("TIDAL has no track 12".into())
        );
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
            assert!(
                matches!(error, DescribeError::Unavailable(_)),
                "{status}: {error:?}"
            );
        }
        let unauthorized = classify_track_error("12", &tidlers::TidalError::NotAuthenticated);
        assert!(
            matches!(unauthorized, DescribeError::Unavailable(_)),
            "{unauthorized:?}"
        );
    }
}
