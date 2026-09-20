//! Where the playback engine's tracks come from: local files and TIDAL.
//!
//! Each supplier decides how its tracks can be sought (see [`SeekMode`]), because that depends
//! on where the audio comes from and the engine can't know it.

use crate::dash::{DashSegments, SegmentTiming};
use crate::engine::{Advance, LoadedTrack, SeekMode, TrackMedia, TrackMeta, TrackRef, TrackSupplier};
use crate::tidal::{self, ManifestKind};
use crate::stream;
use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tidlers::TidalClient;
use tidlers::client::models::playback::AudioQuality;

/// Local audio files, played in the order given. A file can be repositioned freely, so seeking
/// is done in place.
pub struct FileSupplier {
    paths: Vec<PathBuf>,
    cursor: Mutex<Option<usize>>,
}

impl FileSupplier {
    pub fn new(paths: Vec<PathBuf>) -> Arc<Self> {
        Arc::new(Self { paths, cursor: Mutex::new(None) })
    }

    fn track_ref(path: &std::path::Path) -> TrackRef {
        TrackRef(path.to_string_lossy().into_owned())
    }
}

impl TrackSupplier for FileSupplier {
    fn advance(&self, how: Advance) -> Option<TrackRef> {
        let cursor = *self.cursor.lock().unwrap();
        let target = match how {
            Advance::Auto | Advance::Next => Some(cursor.map_or(0, |index| index + 1)),
            Advance::Previous => cursor.and_then(|index| index.checked_sub(1)),
            Advance::Restart => cursor,
        };
        target.and_then(|index| self.paths.get(index)).map(|path| Self::track_ref(path))
    }

    fn open(&self, track: TrackRef, _at: Duration) -> BoxFuture<'static, Result<LoadedTrack>> {
        let path = PathBuf::from(&track.0);
        if let Some(index) = self.paths.iter().position(|candidate| *candidate == path) {
            *self.cursor.lock().unwrap() = Some(index);
        }
        Box::pin(async move {
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

/// A single TIDAL track, streamed. `advance` never offers another one: a queue supplies those.
pub struct TidalSupplier {
    http: reqwest::Client,
    client: Arc<TidalClient>,
    quality: AudioQuality,
    print_info: bool,
    /// Where to copy the audio of the next opening, if anywhere.
    save_to: Mutex<Option<std::fs::File>>,
}

impl TidalSupplier {
    pub fn new(http: reqwest::Client, client: Arc<TidalClient>, quality: AudioQuality) -> Self {
        Self { http, client, quality, print_info: false, save_to: Mutex::new(None) }
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

impl TrackSupplier for TidalSupplier {
    fn advance(&self, _how: Advance) -> Option<TrackRef> {
        None
    }

    fn open(&self, track: TrackRef, at: Duration) -> BoxFuture<'static, Result<LoadedTrack>> {
        let http = self.http.clone();
        let client = self.client.clone();
        let quality = self.quality.clone();
        let print_info = self.print_info;
        // Only the opening from the start is copied: reopening for a seek would append a piece.
        let tee = if at.is_zero() { self.save_to.lock().unwrap().take() } else { None };

        Box::pin(async move {
            let info = tidal::fetch_playback_info(&http, &client, &track.0, quality).await?;
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
    use crate::engine::{Command, Engine, Event, SeekTarget, State};
    use crate::output::fake::FakeSinkFactory;
    use crate::testutil::{expected, wav};
    use std::time::Instant;

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
        let dir = std::env::temp_dir().join(format!("phonia-suppliers-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_wav(dir: &std::path::Path, name: &str, frames: usize) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, wav(frames)).unwrap();
        path
    }

    #[test]
    fn file_supplier_walks_its_list_in_both_directions() {
        let supplier = FileSupplier::new(vec!["a.wav".into(), "b.wav".into(), "c.wav".into()]);
        let name = |track: Option<TrackRef>| track.map(|t| t.0);
        assert_eq!(name(supplier.advance(Advance::Auto)), Some("a.wav".into()));

        // The cursor moves when a track is opened, not when it is asked for. (These files don't
        // exist, so opening fails; the cursor has moved all the same.)
        let open = |name: &str| block_on(supplier.open(TrackRef(name.into()), Duration::ZERO)).err();
        assert!(open("b.wav").is_some());
        assert_eq!(name(supplier.advance(Advance::Next)), Some("c.wav".into()));
        assert_eq!(name(supplier.advance(Advance::Previous)), Some("a.wav".into()));
        assert_eq!(name(supplier.advance(Advance::Restart)), Some("b.wav".into()));

        assert!(open("c.wav").is_some());
        assert_eq!(supplier.advance(Advance::Auto), None, "nothing after the last file");
        assert!(open("a.wav").is_some());
        assert_eq!(supplier.advance(Advance::Previous), None, "nothing before the first");
    }

    #[test]
    fn file_supplier_reports_a_missing_file_clearly() {
        let supplier = FileSupplier::new(vec![]);
        let error = block_on(supplier.open(TrackRef("/no/such/file.flac".into()), Duration::ZERO))
            .err()
            .unwrap();
        assert!(error.to_string().contains("opening"), "{error}");
    }

    /// Runs a future to completion on a throwaway runtime.
    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(future)
    }

    /// Two real files on disk played through the whole engine: opened from disk, decoded,
    /// sought in place and handed on to the next one, all bit for bit.
    #[test]
    fn files_play_and_seek_through_the_engine() {
        let (first, second) = (40_000, 20_000);
        let dir = temp_dir("engine");
        let paths = vec![write_wav(&dir, "one.wav", first), write_wav(&dir, "two.wav", second)];

        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let sinks = FakeSinkFactory::blocking();
        let engine = Engine::spawn(rt.handle().clone(), sinks.clone(), FileSupplier::new(paths)).unwrap();
        let mut events = engine.subscribe();

        engine.send(Command::Play(None)).unwrap();
        let sink = loop {
            if let Some(handle) = sinks.handles().first() {
                break handle.clone();
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        // Seek 0.5 s into the first file (22_050 frames) while the engine is blocked on the DAC.
        engine.send(Command::Seek(SeekTarget::Absolute(Duration::from_millis(500)))).unwrap();
        sink.advance(1024);
        sink.set_blocking(false);

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut titles = Vec::new();
        loop {
            match events.try_recv() {
                Ok(Event::TrackStarted { meta, .. }) => titles.push(meta.title.unwrap()),
                Ok(Event::StateChanged(State::Stopped)) => break,
                Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(_) => {
                    assert!(Instant::now() < deadline, "timed out; titles so far: {titles:?}");
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
        assert_eq!(titles, ["one.wav", "two.wav"]);

        let played = sink.played();
        let mut tail = expected(22_050 * 2, first * 2);
        tail.extend(expected(0, second * 2));
        assert!(played.len() >= tail.len() && played[played.len() - tail.len()..] == tail[..],
            "after the seek: the rest of the first file from 0.5 s, then all of the second");
    }
}
