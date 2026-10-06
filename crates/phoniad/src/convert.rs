//! Mapping the daemon's internal types onto the wire types. The wire format is its own thing (see
//! `phonia-ipc`): nothing internal is serialized directly, so an internal refactor can't change
//! what clients see by accident.

use phonia_core::catalog::{self as tidal_catalog, CatalogError};
use phonia_core::config::Quality;
use phonia_core::decode::SourceSpec;
use phonia_core::engine::{self, Delivered, EndReason, OutputState, ReleaseReason, SeekTarget};
use phonia_core::output::alsa::{ProcReading, SinkReport};
use phonia_core::output::catalog::{self, Entry};
use phonia_core::queue::{self, ItemId, QueueSnapshot};
use phonia_core::replaygain;
use phonia_ipc as ipc;
use std::time::Duration;

pub fn ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn state(state: engine::State) -> ipc::State {
    match state {
        engine::State::Stopped => ipc::State::Stopped,
        engine::State::Loading => ipc::State::Loading,
        engine::State::Playing => ipc::State::Playing,
        engine::State::Paused => ipc::State::Paused,
        engine::State::Seeking => ipc::State::Seeking,
    }
}

pub fn spec(spec: SourceSpec) -> ipc::Spec {
    ipc::Spec {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        bits_per_sample: spec.bits_per_sample,
    }
}

pub fn repeat(repeat: queue::Repeat) -> ipc::Repeat {
    match repeat {
        queue::Repeat::Off => ipc::Repeat::Off,
        queue::Repeat::One => ipc::Repeat::One,
        queue::Repeat::All => ipc::Repeat::All,
    }
}

pub fn repeat_from_wire(repeat: ipc::Repeat) -> queue::Repeat {
    match repeat {
        ipc::Repeat::Off => queue::Repeat::Off,
        ipc::Repeat::One => queue::Repeat::One,
        ipc::Repeat::All => queue::Repeat::All,
    }
}

pub fn seek_target(target: ipc::SeekTarget) -> SeekTarget {
    match target {
        ipc::SeekTarget::Absolute { ms } => SeekTarget::Absolute(Duration::from_millis(ms)),
        ipc::SeekTarget::Forward { ms } => SeekTarget::Forward(Duration::from_millis(ms)),
        ipc::SeekTarget::Backward { ms } => SeekTarget::Backward(Duration::from_millis(ms)),
    }
}

fn end_reason(reason: EndReason) -> ipc::EndReason {
    match reason {
        EndReason::Completed => ipc::EndReason::Completed,
        EndReason::Interrupted => ipc::EndReason::Interrupted,
        EndReason::Failed => ipc::EndReason::Failed,
    }
}

pub fn queue_dto(queue: &QueueSnapshot) -> ipc::Queue {
    ipc::Queue {
        version: queue.version,
        items: queue
            .items
            .iter()
            .map(|item| ipc::QueueItem {
                id: ipc::ItemId(item.id.0),
                source: item.track.source.0.clone(),
                title: item.track.title.clone(),
                duration_ms: item.track.duration.map(ms),
                cover: item.track.cover.clone(),
            })
            .collect(),
        order: queue.order.iter().map(|id| ipc::ItemId(id.0)).collect(),
        current: queue.current.map(|id| ipc::ItemId(id.0)),
        shuffle: queue.shuffle,
        repeat: repeat(queue.repeat),
    }
}

/// The queue entry an engine track reference stands for, and its source.
pub(crate) fn entry_of(
    track: &engine::TrackRef,
    queue: &QueueSnapshot,
) -> (Option<ipc::ItemId>, Option<String>) {
    let Some(id) = ItemId::from_ref(track) else {
        return (None, None);
    };
    let source = queue
        .items
        .iter()
        .find(|item| item.id == id)
        .map(|item| item.track.source.0.clone());
    (Some(ipc::ItemId(id.0)), source)
}

pub fn quality(quality: Quality) -> ipc::Quality {
    match quality {
        Quality::Hires => ipc::Quality::Hires,
        Quality::Lossless => ipc::Quality::Lossless,
        Quality::High => ipc::Quality::High,
        Quality::Low => ipc::Quality::Low,
    }
}

/// The other way round; `None` for a tier this version doesn't know.
pub fn core_quality(quality: ipc::Quality) -> Option<Quality> {
    match quality {
        ipc::Quality::Hires => Some(Quality::Hires),
        ipc::Quality::Lossless => Some(Quality::Lossless),
        ipc::Quality::High => Some(Quality::High),
        ipc::Quality::Low => Some(Quality::Low),
        ipc::Quality::Unknown => None,
    }
}

pub fn stream_quality(delivered: &Delivered) -> ipc::StreamQuality {
    ipc::StreamQuality {
        requested: quality(delivered.requested),
        delivered: quality(delivered.delivered),
    }
}

pub fn catalog_kind(kind: ipc::CatalogKind) -> Option<tidal_catalog::Kind> {
    match kind {
        ipc::CatalogKind::Tracks => Some(tidal_catalog::Kind::Tracks),
        ipc::CatalogKind::Albums => Some(tidal_catalog::Kind::Albums),
        ipc::CatalogKind::Artists => Some(tidal_catalog::Kind::Artists),
        ipc::CatalogKind::Playlists => Some(tidal_catalog::Kind::Playlists),
        ipc::CatalogKind::Unknown => None,
    }
}

fn artist_ref(artist: &tidal_catalog::ArtistRef) -> ipc::ArtistRef {
    ipc::ArtistRef {
        id: artist.id.clone(),
        name: artist.name.clone(),
    }
}

pub fn track_summary(track: &tidal_catalog::Track) -> ipc::TrackSummary {
    ipc::TrackSummary {
        id: track.id.clone(),
        title: track.title.clone(),
        version: track.version.clone(),
        artists: track.artists.iter().map(artist_ref).collect(),
        album: track.album.as_ref().map(|album| ipc::AlbumRef {
            id: album.id.clone(),
            title: album.title.clone(),
            cover: album.cover.clone(),
        }),
        duration_ms: track.duration.map(ms),
        explicit: track.explicit,
        track_number: track.track_number,
        volume_number: track.volume_number,
        quality: track.quality.map(quality),
        streamable: track.streamable,
    }
}

pub fn album_summary(album: &tidal_catalog::Album) -> ipc::AlbumSummary {
    ipc::AlbumSummary {
        id: album.id.clone(),
        title: album.title.clone(),
        version: album.version.clone(),
        artists: album.artists.iter().map(artist_ref).collect(),
        release_date: album.release_date.clone(),
        track_count: album.track_count,
        duration_ms: album.duration.map(ms),
        explicit: album.explicit,
        quality: album.quality.map(quality),
        kind: album.kind.map(|kind| match kind {
            tidal_catalog::AlbumKind::Album => ipc::AlbumKind::Album,
            tidal_catalog::AlbumKind::Ep => ipc::AlbumKind::Ep,
            tidal_catalog::AlbumKind::Single => ipc::AlbumKind::Single,
        }),
        copyright: album.copyright.clone(),
        cover: album.cover.clone(),
    }
}

pub fn artist_summary(artist: &tidal_catalog::Artist) -> ipc::ArtistSummary {
    ipc::ArtistSummary {
        id: artist.id.clone(),
        name: artist.name.clone(),
        picture: artist.picture.clone(),
    }
}

pub fn playlist_summary(playlist: &tidal_catalog::Playlist) -> ipc::PlaylistSummary {
    ipc::PlaylistSummary {
        id: playlist.id.clone(),
        title: playlist.title.clone(),
        creator: playlist.creator.clone(),
        description: playlist.description.clone(),
        track_count: playlist.track_count,
        duration_ms: playlist.duration.map(ms),
        cover: playlist.cover.clone(),
    }
}

/// A page of the catalog as clients see it, each item mapped by `each`.
pub fn page<T, U>(page: &tidal_catalog::Page<T>, each: impl Fn(&T) -> U) -> ipc::Page<U> {
    ipc::Page {
        items: page.items.iter().map(each).collect(),
        total: page.total,
        offset: page.offset,
    }
}

/// How the catalog's failures are told to clients.
pub fn catalog_error(error: &CatalogError) -> (ipc::ErrorCode, String) {
    let code = match error {
        CatalogError::NotLoggedIn(_) => ipc::ErrorCode::NotLoggedIn,
        CatalogError::Unavailable(_) => ipc::ErrorCode::Unavailable,
        CatalogError::RateLimited => ipc::ErrorCode::RateLimited,
        CatalogError::NotFound => ipc::ErrorCode::NotFound,
        CatalogError::Invalid(_) => ipc::ErrorCode::BadRequest,
    };
    (code, error.to_string())
}

fn replay_gain(gain: replaygain::AppliedGain) -> ipc::ReplayGain {
    ipc::ReplayGain {
        kind: match gain.kind {
            replaygain::Kind::Track => ipc::GainKind::Track,
            replaygain::Kind::Album => ipc::GainKind::Album,
        },
        millibels: (gain.db * 100.0).round() as i32,
    }
}

pub fn status_dto(
    status: &engine::Status,
    queue: &QueueSnapshot,
    route: Option<ipc::Route>,
    volume: Option<phonia_core::output::Volume>,
    quality_range: Option<ipc::QualityRange>,
) -> ipc::Status {
    ipc::Status {
        state: state(status.state),
        track: status.track.as_ref().map(|meta| {
            let (item_id, source) = entry_of(&meta.track, queue);
            ipc::Track {
                item_id,
                source,
                title: meta.title.clone(),
                duration_ms: meta.duration.map(ms),
                quality: meta.quality.as_ref().map(stream_quality),
                cover: meta.cover.clone(),
                replay_gain: meta.gain.map(replay_gain),
            }
        }),
        spec: status.spec.map(spec),
        position_ms: ms(status.position),
        duration_ms: status.duration.map(ms),
        output: match &status.output {
            OutputState::Closed => ipc::Output::Closed,
            OutputState::Open => ipc::Output::Open,
            OutputState::Released { by } => ipc::Output::Released { by: by.clone() },
        },
        route,
        volume: volume.map(|volume| ipc::Volume {
            percent: volume.percent,
            muted: volume.muted,
        }),
        quality_range,
        // Filled in by `Daemon::state`, which alone knows the last report and can gate it against
        // the rest of this very status with `SinkReport::applies_to`.
        sink_report: None,
    }
}

pub fn output_info(entry: &Entry) -> ipc::OutputInfo {
    ipc::OutputInfo {
        id: entry.id.clone(),
        mode: match entry.mode {
            catalog::Mode::Exclusive => ipc::OutputMode::Exclusive,
            catalog::Mode::Shared => ipc::OutputMode::Shared,
        },
        name: entry.name.clone(),
        detail: entry.detail.clone(),
        bit_perfect: entry.bit_perfect,
        lossy: entry.lossy,
        codec: entry.codec.clone(),
        is_default: entry.is_default,
    }
}

fn release_reason(reason: ReleaseReason) -> ipc::ReleaseReason {
    match reason {
        ReleaseReason::Idle => ipc::ReleaseReason::Idle,
        ReleaseReason::Command => ipc::ReleaseReason::Command,
        ReleaseReason::Requested => ipc::ReleaseReason::Requested,
        ReleaseReason::Lost => ipc::ReleaseReason::Lost,
    }
}

/// `output` is the route id of whatever opened this sink (`exclusive:hw:DS2,0`,
/// `shared:default`), stamped by the one place that knows it for certain: the per-output factory
/// closure in `main.rs`, not guessed back from the daemon's current route, which can change before
/// this report is even converted.
pub fn sink_report(report: &SinkReport, output: &str) -> ipc::SinkReport {
    ipc::SinkReport {
        device: report.device.clone(),
        source: spec(report.source),
        negotiated_format: report.negotiated_format.clone(),
        bit_perfect: report.bit_perfect(),
        problem: report.problem(),
        hw_params: match &report.proc {
            ProcReading::Read { contents, .. } => Some(contents.clone()),
            _ => None,
        },
        mode: Some(if report.shared.is_some() {
            ipc::OutputMode::Shared
        } else {
            ipc::OutputMode::Exclusive
        }),
        resampled_to: report
            .shared
            .as_ref()
            .map(|route| route.sink_rate)
            .filter(|rate| *rate != report.source.sample_rate),
        codec: report.shared.as_ref().and_then(|route| route.codec.clone()),
        lossy: report.shared.as_ref().is_some_and(|route| route.lossy),
        output: Some(output.to_string()),
    }
}

/// An engine event as clients see it. `queue` is the queue as it is now, to name entries.
pub fn event(event: &engine::Event, queue: &QueueSnapshot) -> ipc::Event {
    match event {
        engine::Event::StateChanged(new) => ipc::Event::StateChanged { state: state(*new) },
        engine::Event::TrackStarted {
            meta,
            spec: format,
            gapless,
        } => {
            let (item_id, source) = entry_of(&meta.track, queue);
            ipc::Event::TrackStarted {
                item_id,
                source,
                title: meta.title.clone(),
                duration_ms: meta.duration.map(ms),
                spec: spec(*format),
                gapless: *gapless,
                quality: meta.quality.as_ref().map(stream_quality),
                cover: meta.cover.clone(),
                replay_gain: meta.gain.map(replay_gain),
            }
        }
        engine::Event::TrackEnded { meta, reason } => ipc::Event::TrackEnded {
            item_id: entry_of(&meta.track, queue).0,
            reason: end_reason(*reason),
        },
        engine::Event::Position { position, duration } => ipc::Event::Position {
            position_ms: ms(*position),
            duration_ms: duration.map(ms),
        },
        engine::Event::Seeked { position } => ipc::Event::Seeked {
            position_ms: ms(*position),
        },
        engine::Event::SeekRejected { reason } => ipc::Event::SeekRejected {
            reason: reason.clone(),
        },
        engine::Event::QueueExhausted => ipc::Event::QueueExhausted,
        engine::Event::OutputReleased { by, reason } => ipc::Event::OutputReleased {
            by: by.clone(),
            reason: release_reason(*reason),
        },
        engine::Event::OutputAcquired => ipc::Event::OutputAcquired,
        engine::Event::Error { message } => ipc::Event::Error {
            message: message.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phonia_core::engine::{Status, TrackMeta, TrackRef};
    use phonia_core::queue::{QueueItem, QueueTrack, Repeat};

    fn snapshot() -> QueueSnapshot {
        let item =
            |id, source: &str, title: Option<&str>, secs: Option<u64>, cover: Option<&str>| {
                QueueItem {
                    id: ItemId(id),
                    track: QueueTrack {
                        source: TrackRef(source.to_string()),
                        title: title.map(str::to_string),
                        duration: secs.map(Duration::from_secs),
                        cover: cover.map(str::to_string),
                        album_id: None,
                    },
                }
            };
        QueueSnapshot {
            version: 3,
            items: vec![
                item(7, "file:/m/a.flac", Some("a.flac"), Some(215), None),
                item(8, "tidal:1", None, None, Some("cover-uuid")),
            ],
            order: vec![ItemId(8), ItemId(7)],
            current: Some(ItemId(7)),
            shuffle: true,
            repeat: Repeat::All,
        }
    }

    #[test]
    fn the_queue_maps_field_by_field() {
        let dto = queue_dto(&snapshot());
        assert_eq!(dto.version, 3);
        assert_eq!(dto.items[0].id, ipc::ItemId(7));
        assert_eq!(dto.items[0].source, "file:/m/a.flac");
        assert_eq!(dto.items[0].duration_ms, Some(215_000));
        assert_eq!(dto.items[0].cover, None);
        assert_eq!(dto.items[1].title, None);
        assert_eq!(dto.items[1].cover.as_deref(), Some("cover-uuid"));
        assert_eq!(dto.order, [ipc::ItemId(8), ipc::ItemId(7)]);
        assert_eq!(
            (dto.current, dto.shuffle, dto.repeat),
            (Some(ipc::ItemId(7)), true, ipc::Repeat::All)
        );
    }

    #[test]
    fn the_engines_track_reference_is_the_item_id_and_names_its_source() {
        let status = Status {
            state: engine::State::Playing,
            track: Some(TrackMeta {
                track: ItemId(7).track_ref(),
                title: Some("a.flac".into()),
                duration: Some(Duration::from_secs(215)),
                quality: None,
                cover: Some("cover-uuid".into()),
                loudness: None,
                gain: None,
            }),
            spec: Some(SourceSpec {
                sample_rate: 96_000,
                channels: 2,
                bits_per_sample: 24,
            }),
            position: Duration::from_millis(1_500),
            duration: Some(Duration::from_secs(215)),
            output: OutputState::Released {
                by: Some("jackd".into()),
            },
        };
        let dto = status_dto(&status, &snapshot(), None, None, None);
        let track = dto.track.unwrap();
        assert_eq!(track.item_id, Some(ipc::ItemId(7)));
        assert_eq!(
            track.source.as_deref(),
            Some("file:/m/a.flac"),
            "the wire names the source, never the engine's reference"
        );
        assert_eq!(track.cover.as_deref(), Some("cover-uuid"));
        assert_eq!((dto.position_ms, dto.state), (1_500, ipc::State::Playing));
        assert_eq!(dto.spec.unwrap().sample_rate, 96_000);
        assert_eq!(
            dto.output,
            ipc::Output::Released {
                by: Some("jackd".into())
            }
        );
    }

    #[test]
    fn what_tidal_delivered_reaches_the_clients_in_events_and_status() {
        let delivered = Delivered {
            requested: Quality::Hires,
            delivered: Quality::Lossless,
        };
        let meta = TrackMeta {
            track: TrackRef("999".into()),
            title: None,
            duration: None,
            quality: Some(delivered),
            cover: None,
            loudness: None,
            gain: None,
        };
        let started = engine::Event::TrackStarted {
            meta: meta.clone(),
            spec: SourceSpec {
                sample_rate: 44_100,
                channels: 2,
                bits_per_sample: 16,
            },
            gapless: false,
        };
        let ipc::Event::TrackStarted { quality, .. } = event(&started, &snapshot()) else {
            panic!("not a track_started");
        };
        let quality = quality.expect("the quality is reported");
        assert_eq!(quality.requested, ipc::Quality::Hires);
        assert_eq!(quality.delivered, ipc::Quality::Lossless);
        assert!(quality.fell_back());

        let status = Status {
            state: engine::State::Playing,
            track: Some(meta),
            spec: None,
            position: Duration::ZERO,
            duration: None,
            output: OutputState::Open,
        };
        let dto = status_dto(&status, &snapshot(), None, None, None);
        assert_eq!(dto.track.unwrap().quality, Some(quality));
    }

    #[test]
    fn a_track_that_started_with_no_gap_says_so() {
        let meta = TrackMeta {
            track: ItemId(7).track_ref(),
            title: None,
            duration: None,
            quality: None,
            cover: None,
            loudness: None,
            gain: None,
        };
        let spec = SourceSpec {
            sample_rate: 96_000,
            channels: 2,
            bits_per_sample: 24,
        };
        for gapless in [false, true] {
            let started = engine::Event::TrackStarted {
                meta: meta.clone(),
                spec,
                gapless,
            };
            let ipc::Event::TrackStarted { gapless: told, .. } = event(&started, &snapshot())
            else {
                panic!("not a track_started")
            };
            assert_eq!(told, gapless);
        }
    }

    #[test]
    fn output_events_map_to_wire_events() {
        let q = snapshot();
        let released = engine::Event::OutputReleased {
            by: Some("jackd".into()),
            reason: ReleaseReason::Requested,
        };
        assert_eq!(
            event(&released, &q),
            ipc::Event::OutputReleased {
                by: Some("jackd".into()),
                reason: ipc::ReleaseReason::Requested
            }
        );
        assert_eq!(
            event(&engine::Event::OutputAcquired, &q),
            ipc::Event::OutputAcquired
        );
    }

    #[test]
    fn a_track_that_is_not_in_the_queue_is_reported_without_a_source() {
        let meta = TrackMeta {
            track: TrackRef("999".into()),
            title: None,
            duration: None,
            quality: None,
            cover: None,
            loudness: None,
            gain: None,
        };
        let event = event(
            &engine::Event::TrackEnded {
                meta,
                reason: EndReason::Failed,
            },
            &snapshot(),
        );
        assert_eq!(
            event,
            ipc::Event::TrackEnded {
                item_id: Some(ipc::ItemId(999)),
                reason: ipc::EndReason::Failed
            }
        );
    }

    #[test]
    fn engine_events_map_to_wire_events() {
        let q = snapshot();
        assert_eq!(
            event(&engine::Event::StateChanged(engine::State::Seeking), &q),
            ipc::Event::StateChanged {
                state: ipc::State::Seeking
            }
        );
        assert_eq!(
            event(
                &engine::Event::Position {
                    position: Duration::from_millis(2_500),
                    duration: None
                },
                &q
            ),
            ipc::Event::Position {
                position_ms: 2_500,
                duration_ms: None
            }
        );
        assert_eq!(
            event(&engine::Event::QueueExhausted, &q),
            ipc::Event::QueueExhausted
        );
        assert_eq!(
            event(
                &engine::Event::Seeked {
                    position: Duration::from_secs(3)
                },
                &q
            ),
            ipc::Event::Seeked { position_ms: 3_000 }
        );
    }

    #[test]
    fn seek_targets_and_repeat_modes_round_trip() {
        assert_eq!(
            seek_target(ipc::SeekTarget::Forward { ms: 10_000 }),
            SeekTarget::Forward(Duration::from_secs(10))
        );
        for wire in [ipc::Repeat::Off, ipc::Repeat::One, ipc::Repeat::All] {
            assert_eq!(repeat(repeat_from_wire(wire)), wire);
        }
    }

    #[test]
    fn a_sink_report_carries_the_verdict_and_the_evidence() {
        let report = SinkReport::new(
            "hw:1,0".into(),
            SourceSpec {
                sample_rate: 96_000,
                channels: 2,
                bits_per_sample: 24,
            },
            "S24_3LE".into(),
            ProcReading::Read {
                path: "/p".into(),
                contents: "format: S24_3LE\nrate: 96000 (96000/1)\n".into(),
            },
        );
        let dto = sink_report(&report, "exclusive:hw:1,0");
        assert!(dto.bit_perfect);
        assert_eq!(dto.problem, None);
        assert!(dto.hw_params.unwrap().contains("rate: 96000"));
        assert_eq!(dto.output.as_deref(), Some("exclusive:hw:1,0"));

        let converted = SinkReport::new(
            "default".into(),
            SourceSpec {
                sample_rate: 96_000,
                channels: 2,
                bits_per_sample: 24,
            },
            "S24_3LE".into(),
            ProcReading::NotHw,
        );
        let dto = sink_report(&converted, "shared:default");
        assert!(!dto.bit_perfect);
        assert!(dto.problem.unwrap().contains("not hw:N,D"));
        assert_eq!(dto.hw_params, None);
        assert_eq!(dto.output.as_deref(), Some("shared:default"));
    }

    #[test]
    fn a_shared_report_reaches_clients_as_not_bit_perfect_with_the_reason() {
        use phonia_core::output::alsa::SharedRoute;
        let report = SinkReport::shared(
            SourceSpec {
                sample_rate: 96_000,
                channels: 2,
                bits_per_sample: 24,
            },
            "S32LE".into(),
            SharedRoute {
                sink: "Soundcore Life P2".into(),
                sink_rate: 48_000,
                kind: "Bluetooth".into(),
                codec: Some("SBC".into()),
                lossy: true,
            },
        );
        let dto = sink_report(&report, "shared:default");
        assert!(!dto.bit_perfect);
        assert_eq!(dto.device, "Soundcore Life P2");
        assert!(dto.problem.unwrap().contains("SBC"));
        assert_eq!(
            dto.hw_params, None,
            "there is no /proc/asound for a stream through the sound server"
        );
    }

    #[test]
    fn milliseconds_saturate_instead_of_overflowing() {
        assert_eq!(ms(Duration::MAX), u64::MAX);
        assert_eq!(ms(Duration::from_micros(1_999)), 1);
    }

    #[test]
    fn cover_and_picture_ids_cross_the_wire_unchanged() {
        let mut album = tidal_catalog::Album {
            id: "9".into(),
            title: "Issues".into(),
            version: None,
            artists: vec![],
            release_date: None,
            track_count: None,
            duration: None,
            explicit: false,
            quality: None,
            kind: None,
            copyright: None,
            cover: Some("cover-id".into()),
        };
        assert_eq!(album_summary(&album).cover.as_deref(), Some("cover-id"));
        album.cover = None;
        assert_eq!(album_summary(&album).cover, None, "no cover is none");

        let artist = tidal_catalog::Artist {
            id: "780".into(),
            name: "Korn".into(),
            picture: Some("picture-id".into()),
        };
        assert_eq!(
            artist_summary(&artist).picture.as_deref(),
            Some("picture-id")
        );

        let playlist = tidal_catalog::Playlist {
            id: "p-1".into(),
            title: "Road trip".into(),
            creator: None,
            description: None,
            track_count: None,
            duration: None,
            cover: Some("square-id".into()),
        };
        assert_eq!(
            playlist_summary(&playlist).cover.as_deref(),
            Some("square-id")
        );

        let track = tidal_catalog::Track {
            id: "1".into(),
            title: "Blind".into(),
            version: None,
            artists: vec![],
            album: Some(tidal_catalog::AlbumRef {
                id: "9".into(),
                title: "Issues".into(),
                cover: Some("cover-id".into()),
            }),
            duration: None,
            explicit: false,
            track_number: None,
            volume_number: None,
            quality: None,
            streamable: true,
        };
        assert_eq!(
            track_summary(&track).album.unwrap().cover.as_deref(),
            Some("cover-id"),
            "a track's embedded album keeps its cover too"
        );
    }
}
