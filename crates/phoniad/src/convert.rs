//! Mapping the daemon's internal types onto the wire types. The wire format is its own thing (see
//! `phonia-ipc`): nothing internal is serialized directly, so an internal refactor can't change
//! what clients see by accident.

use phonia_core::decode::SourceSpec;
use phonia_core::engine::{self, EndReason, OutputState, ReleaseReason, SeekTarget};
use phonia_core::output::alsa::{ProcReading, SinkReport};
use phonia_core::queue::{self, ItemId, QueueSnapshot};
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
    ipc::Spec { sample_rate: spec.sample_rate, channels: spec.channels, bits_per_sample: spec.bits_per_sample }
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
            })
            .collect(),
        order: queue.order.iter().map(|id| ipc::ItemId(id.0)).collect(),
        current: queue.current.map(|id| ipc::ItemId(id.0)),
        shuffle: queue.shuffle,
        repeat: repeat(queue.repeat),
    }
}

/// The queue entry an engine track reference stands for, and its source.
fn entry_of(track: &engine::TrackRef, queue: &QueueSnapshot) -> (Option<ipc::ItemId>, Option<String>) {
    let Some(id) = ItemId::from_ref(track) else { return (None, None) };
    let source = queue.items.iter().find(|item| item.id == id).map(|item| item.track.source.0.clone());
    (Some(ipc::ItemId(id.0)), source)
}

pub fn status_dto(status: &engine::Status, queue: &QueueSnapshot) -> ipc::Status {
    ipc::Status {
        state: state(status.state),
        track: status.track.as_ref().map(|meta| {
            let (item_id, source) = entry_of(&meta.track, queue);
            ipc::Track { item_id, source, title: meta.title.clone(), duration_ms: meta.duration.map(ms) }
        }),
        spec: status.spec.map(spec),
        position_ms: ms(status.position),
        duration_ms: status.duration.map(ms),
        output: match &status.output {
            OutputState::Closed => ipc::Output::Closed,
            OutputState::Open => ipc::Output::Open,
            OutputState::Released { by } => ipc::Output::Released { by: by.clone() },
        },
    }
}

fn release_reason(reason: ReleaseReason) -> ipc::ReleaseReason {
    match reason {
        ReleaseReason::Idle => ipc::ReleaseReason::Idle,
        ReleaseReason::Command => ipc::ReleaseReason::Command,
        ReleaseReason::Requested => ipc::ReleaseReason::Requested,
    }
}

pub fn sink_report(report: &SinkReport) -> ipc::SinkReport {
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
    }
}

/// An engine event as clients see it. `queue` is the queue as it is now, to name entries.
pub fn event(event: &engine::Event, queue: &QueueSnapshot) -> ipc::Event {
    match event {
        engine::Event::StateChanged(new) => ipc::Event::StateChanged { state: state(*new) },
        engine::Event::TrackStarted { meta, spec: format } => {
            let (item_id, source) = entry_of(&meta.track, queue);
            ipc::Event::TrackStarted {
                item_id,
                source,
                title: meta.title.clone(),
                duration_ms: meta.duration.map(ms),
                spec: spec(*format),
            }
        }
        engine::Event::TrackEnded { meta, reason } => {
            ipc::Event::TrackEnded { item_id: entry_of(&meta.track, queue).0, reason: end_reason(*reason) }
        }
        engine::Event::Position { position, duration } => {
            ipc::Event::Position { position_ms: ms(*position), duration_ms: duration.map(ms) }
        }
        engine::Event::Seeked { position } => ipc::Event::Seeked { position_ms: ms(*position) },
        engine::Event::SeekRejected { reason } => ipc::Event::SeekRejected { reason: reason.clone() },
        engine::Event::QueueExhausted => ipc::Event::QueueExhausted,
        engine::Event::OutputReleased { by, reason } => {
            ipc::Event::OutputReleased { by: by.clone(), reason: release_reason(*reason) }
        }
        engine::Event::OutputAcquired => ipc::Event::OutputAcquired,
        engine::Event::Error { message } => ipc::Event::Error { message: message.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phonia_core::engine::{Status, TrackMeta, TrackRef};
    use phonia_core::queue::{QueueItem, QueueTrack, Repeat};

    fn snapshot() -> QueueSnapshot {
        let item = |id, source: &str, title: Option<&str>, secs: Option<u64>| QueueItem {
            id: ItemId(id),
            track: QueueTrack {
                source: TrackRef(source.to_string()),
                title: title.map(str::to_string),
                duration: secs.map(Duration::from_secs),
            },
        };
        QueueSnapshot {
            version: 3,
            items: vec![item(7, "file:/m/a.flac", Some("a.flac"), Some(215)), item(8, "tidal:1", None, None)],
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
        assert_eq!(dto.items[1].title, None);
        assert_eq!(dto.order, [ipc::ItemId(8), ipc::ItemId(7)]);
        assert_eq!((dto.current, dto.shuffle, dto.repeat), (Some(ipc::ItemId(7)), true, ipc::Repeat::All));
    }

    #[test]
    fn the_engines_track_reference_is_the_item_id_and_names_its_source() {
        let status = Status {
            state: engine::State::Playing,
            track: Some(TrackMeta { track: ItemId(7).track_ref(), title: Some("a.flac".into()), duration: Some(Duration::from_secs(215)) }),
            spec: Some(SourceSpec { sample_rate: 96_000, channels: 2, bits_per_sample: 24 }),
            position: Duration::from_millis(1_500),
            duration: Some(Duration::from_secs(215)),
            output: OutputState::Released { by: Some("jackd".into()) },
        };
        let dto = status_dto(&status, &snapshot());
        let track = dto.track.unwrap();
        assert_eq!(track.item_id, Some(ipc::ItemId(7)));
        assert_eq!(track.source.as_deref(), Some("file:/m/a.flac"), "the wire names the source, never the engine's reference");
        assert_eq!((dto.position_ms, dto.state), (1_500, ipc::State::Playing));
        assert_eq!(dto.spec.unwrap().sample_rate, 96_000);
        assert_eq!(dto.output, ipc::Output::Released { by: Some("jackd".into()) });
    }

    #[test]
    fn output_events_map_to_wire_events() {
        let q = snapshot();
        let released = engine::Event::OutputReleased { by: Some("jackd".into()), reason: ReleaseReason::Requested };
        assert_eq!(
            event(&released, &q),
            ipc::Event::OutputReleased { by: Some("jackd".into()), reason: ipc::ReleaseReason::Requested }
        );
        assert_eq!(event(&engine::Event::OutputAcquired, &q), ipc::Event::OutputAcquired);
    }

    #[test]
    fn a_track_that_is_not_in_the_queue_is_reported_without_a_source() {
        let meta = TrackMeta { track: TrackRef("999".into()), title: None, duration: None };
        let event = event(&engine::Event::TrackEnded { meta, reason: EndReason::Failed }, &snapshot());
        assert_eq!(event, ipc::Event::TrackEnded { item_id: Some(ipc::ItemId(999)), reason: ipc::EndReason::Failed });
    }

    #[test]
    fn engine_events_map_to_wire_events() {
        let q = snapshot();
        assert_eq!(event(&engine::Event::StateChanged(engine::State::Seeking), &q), ipc::Event::StateChanged { state: ipc::State::Seeking });
        assert_eq!(
            event(&engine::Event::Position { position: Duration::from_millis(2_500), duration: None }, &q),
            ipc::Event::Position { position_ms: 2_500, duration_ms: None }
        );
        assert_eq!(event(&engine::Event::QueueExhausted, &q), ipc::Event::QueueExhausted);
        assert_eq!(event(&engine::Event::Seeked { position: Duration::from_secs(3) }, &q), ipc::Event::Seeked { position_ms: 3_000 });
    }

    #[test]
    fn seek_targets_and_repeat_modes_round_trip() {
        assert_eq!(seek_target(ipc::SeekTarget::Forward { ms: 10_000 }), SeekTarget::Forward(Duration::from_secs(10)));
        for wire in [ipc::Repeat::Off, ipc::Repeat::One, ipc::Repeat::All] {
            assert_eq!(repeat(repeat_from_wire(wire)), wire);
        }
    }

    #[test]
    fn a_sink_report_carries_the_verdict_and_the_evidence() {
        let report = SinkReport::new(
            "hw:1,0".into(),
            SourceSpec { sample_rate: 96_000, channels: 2, bits_per_sample: 24 },
            "S24_3LE".into(),
            ProcReading::Read { path: "/p".into(), contents: "format: S24_3LE\nrate: 96000 (96000/1)\n".into() },
        );
        let dto = sink_report(&report);
        assert!(dto.bit_perfect);
        assert_eq!(dto.problem, None);
        assert!(dto.hw_params.unwrap().contains("rate: 96000"));

        let converted = SinkReport::new(
            "default".into(),
            SourceSpec { sample_rate: 96_000, channels: 2, bits_per_sample: 24 },
            "S24_3LE".into(),
            ProcReading::NotHw,
        );
        let dto = sink_report(&converted);
        assert!(!dto.bit_perfect);
        assert!(dto.problem.unwrap().contains("not hw:N,D"));
        assert_eq!(dto.hw_params, None);
    }

    #[test]
    fn milliseconds_saturate_instead_of_overflowing() {
        assert_eq!(ms(Duration::MAX), u64::MAX);
        assert_eq!(ms(Duration::from_micros(1_999)), 1);
    }
}
