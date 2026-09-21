//! How to get the audio of one track: from a local file or from TIDAL.
//!
//! Each opener decides how its tracks can be sought (see [`SeekMode`]), because that depends on
//! where the audio comes from and the engine can't know it. Which track plays next is not their
//! business: that is the queue's.

use crate::dash::{DashSegments, SegmentTiming};
use crate::engine::{LoadedTrack, SeekMode, TrackMedia, TrackMeta, TrackOpener, TrackRef};
use crate::tidal::{self, ManifestKind};
use crate::stream;
use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tidlers::TidalClient;
use tidlers::client::models::playback::AudioQuality;

/// Opens local audio files, which a track's reference names by path. A file can be repositioned
/// freely, so seeking is done in place.
pub struct FileOpener;

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
    client: Arc<TidalClient>,
    quality: AudioQuality,
    print_info: bool,
    /// Where to copy the audio of the next opening, if anywhere.
    save_to: Mutex<Option<std::fs::File>>,
}

impl TidalOpener {
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

impl TrackOpener for TidalOpener {
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
}
