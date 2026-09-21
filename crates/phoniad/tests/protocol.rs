//! The daemon, its server and the client, talking through in-memory pipes with a fake audio
//! device: everything except real hardware and real sockets.

use phonia_core::openers::DispatchOpener;
use phonia_core::output::alsa::{ProcReading, SinkReport};
use phonia_core::output::fake::FakeSinkFactory;
use phoniad::daemon::{Daemon, DaemonParts};
use phoniad::server::serve_connection;
use phonia_ipc::framing::{read_frame, write_frame};
use phonia_ipc::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc;

const TIMEOUT: Duration = Duration::from_secs(10);

struct Fixture {
    daemon: Arc<Daemon>,
    sinks: Arc<FakeSinkFactory>,
    reports: mpsc::UnboundedSender<SinkReport>,
    dir: PathBuf,
}

/// A daemon with a fake device. A blocking device holds the engine mid-track, which is what lets
/// a test act on a track that is "playing".
async fn fixture(name: &str, blocking: bool) -> Fixture {
    let sinks = if blocking { FakeSinkFactory::blocking() } else { FakeSinkFactory::autoplay() };
    let (reports, report_rx) = mpsc::unbounded_channel();
    let daemon = Daemon::start(DaemonParts {
        sinks: sinks.clone(),
        opener: Arc::new(DispatchOpener::new(None)),
        reports: report_rx,
    })
    .unwrap();
    let dir = std::env::temp_dir().join(format!("phoniad-protocol-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Fixture { daemon, sinks, reports, dir }
}

impl Fixture {
    /// A stereo 16-bit 44.1 kHz WAV of silence, as a `file:` source.
    fn wav(&self, name: &str, frames: usize) -> String {
        let data = (frames * 4) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&44_100u32.to_le_bytes());
        bytes.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data.to_le_bytes());
        bytes.resize(bytes.len() + frames * 4, 0);
        let path = self.dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        phonia_ipc::source::file(&path).unwrap()
    }

    fn serve(&self) -> DuplexStream {
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        tokio::spawn(serve_connection(server_side, self.daemon.clone()));
        client_side
    }

    async fn client(&self) -> Client {
        Client::from_stream(self.serve(), ClientInfo { name: "test".into(), version: "0".into() }).await.unwrap()
    }

    async fn raw(&self) -> Raw {
        let (read_half, write_half) = tokio::io::split(self.serve());
        Raw { reader: BufReader::new(read_half), writer: write_half, buffer: Vec::new() }
    }

    async fn finish(self) {
        self.daemon.request_shutdown();
        self.daemon.stop_engine().await;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A client that speaks the protocol by hand, to test what the real client would never send.
struct Raw {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
    buffer: Vec<u8>,
}

impl Raw {
    async fn send(&mut self, line: &str) {
        write_frame(&mut self.writer, line.as_bytes()).await.unwrap();
    }

    async fn recv(&mut self) -> Option<ServerMessage> {
        let frame = tokio::time::timeout(TIMEOUT, read_frame(&mut self.reader, &mut self.buffer))
            .await
            .expect("timed out waiting for the daemon")
            .unwrap()?;
        Some(serde_json::from_slice(frame).unwrap_or_else(|error| panic!("bad message {}: {error}", String::from_utf8_lossy(frame))))
    }

    async fn hello(&mut self) {
        assert!(matches!(self.recv().await, Some(ServerMessage::Hello(_))), "the server speaks first");
        self.send(r#"{"id":1,"request":{"type":"hello","protocol":{"major":1,"minor":0},"client":{"name":"raw","version":"0"}}}"#).await;
        assert_eq!(self.recv().await, Some(ServerMessage::Response { id: RequestId(1), reply: Reply::Ok(Payload::Ack) }));
    }
}

fn code_of(reply: Option<ServerMessage>) -> (RequestId, ErrorCode) {
    match reply {
        Some(ServerMessage::Response { id, reply: Reply::Err(error) }) => (id, error.code),
        other => panic!("expected an error response, got {other:?}"),
    }
}

async fn next_event(events: &mut EventStream) -> (u64, Event) {
    tokio::time::timeout(TIMEOUT, events.next_seq()).await.expect("timed out waiting for an event").expect("the connection closed")
}

/// Events until one satisfies `stop`, which is included.
async fn events_until(events: &mut EventStream, stop: impl Fn(&Event) -> bool) -> Vec<(u64, Event)> {
    let mut collected = Vec::new();
    loop {
        let event = next_event(events).await;
        let done = stop(&event.1);
        collected.push(event);
        if done {
            return collected;
        }
    }
}

fn added(payload: Payload) -> (Vec<ItemId>, Vec<Rejected>, Vec<Unresolved>) {
    match payload {
        Payload::Added { ids, rejected, unresolved } => (ids, rejected, unresolved),
        other => panic!("expected added, got {other:?}"),
    }
}

fn add(tracks: &[&str], at: AddAt) -> Request {
    Request::QueueAdd { tracks: tracks.iter().map(|source| NewTrack { source: source.to_string() }).collect(), at }
}

fn protocol_code(error: ClientError) -> ErrorCode {
    match error {
        ClientError::Protocol(error) => error.code,
        other => panic!("expected a protocol error, got {other}"),
    }
}

// ---- connecting ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_connects_and_reads_the_state() {
    let f = fixture("connect", false).await;
    let client = f.client().await;
    assert_eq!(client.server().server.name, "phoniad");
    assert_eq!(client.server().protocol, PROTOCOL);

    let status = client.status().await.unwrap();
    assert_eq!(status.state, State::Stopped);
    assert_eq!((status.track, status.position_ms), (None, 0));
    assert!(client.queue().await.unwrap().items.is_empty());
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_before_the_handshake_are_refused() {
    let f = fixture("handshake", false).await;
    let mut raw = f.raw().await;
    assert!(matches!(raw.recv().await, Some(ServerMessage::Hello(_))));

    raw.send(r#"{"id":4,"request":{"type":"status"}}"#).await;
    assert_eq!(code_of(raw.recv().await), (RequestId(4), ErrorCode::HandshakeRequired));

    // After the handshake the same request works.
    raw.send(r#"{"id":5,"request":{"type":"hello","protocol":{"major":1,"minor":3},"client":{"name":"raw","version":"0"}}}"#).await;
    assert!(matches!(raw.recv().await, Some(ServerMessage::Response { reply: Reply::Ok(Payload::Ack), .. })), "a newer minor version is fine");
    raw.send(r#"{"id":6,"request":{"type":"status"}}"#).await;
    assert!(matches!(raw.recv().await, Some(ServerMessage::Response { reply: Reply::Ok(Payload::Status(_)), .. })));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_of_another_major_version_is_refused_and_disconnected() {
    let f = fixture("version", false).await;
    let mut raw = f.raw().await;
    assert!(matches!(raw.recv().await, Some(ServerMessage::Hello(_))));

    raw.send(r#"{"id":1,"request":{"type":"hello","protocol":{"major":2,"minor":0},"client":{"name":"raw","version":"0"}}}"#).await;
    let (id, code) = code_of(raw.recv().await);
    assert_eq!((id, code), (RequestId(1), ErrorCode::UnsupportedVersion));
    assert!(raw.recv().await.is_none(), "the connection is closed after refusing the version");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_request_is_answered_and_the_connection_survives() {
    let f = fixture("malformed", false).await;
    let mut raw = f.raw().await;
    raw.hello().await;

    // Not JSON at all: nothing to match the answer to.
    raw.send("this is not json").await;
    assert_eq!(code_of(raw.recv().await), (RequestId(0), ErrorCode::BadRequest));

    // Valid JSON of the wrong shape, with an id: the answer names it.
    raw.send(r#"{"id":7,"request":{"type":"seek"}}"#).await;
    assert_eq!(code_of(raw.recv().await), (RequestId(7), ErrorCode::BadRequest));

    // A request this version has never heard of.
    raw.send(r#"{"id":8,"request":{"type":"teleport"}}"#).await;
    assert_eq!(code_of(raw.recv().await), (RequestId(8), ErrorCode::UnknownRequest));

    // And the connection still works.
    raw.send(r#"{"id":9,"request":{"type":"status"}}"#).await;
    assert!(matches!(raw.recv().await, Some(ServerMessage::Response { id: RequestId(9), reply: Reply::Ok(Payload::Status(_)) })));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_message_is_refused_and_the_connection_closed() {
    let f = fixture("oversized", false).await;
    let mut raw = f.raw().await;
    raw.hello().await;

    let Raw { reader, mut writer, .. } = raw;
    let sender = tokio::spawn(async move {
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..10 {
            if writer.write_all(&chunk).await.is_err() {
                break;
            }
        }
    });
    let mut receiver = Raw { reader, writer: tokio::io::split(tokio::io::duplex(1).0).1, buffer: Vec::new() };
    assert_eq!(code_of(receiver.recv().await).1, ErrorCode::BadRequest);
    assert!(receiver.recv().await.is_none());
    sender.abort();
    f.finish().await;
}

// ---- the queue -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adding_tracks_resolves_their_details_and_refuses_the_wrong_ones() {
    let f = fixture("add", false).await;
    let client = f.client().await;
    let (one, two) = (f.wav("one.wav", 44_100), f.wav("two.wav", 88_200));
    let missing = format!("file:{}", f.dir.join("missing.flac").display());

    let payload = client.request(add(&[&one, "not a source", &missing, &two], AddAt::End)).await.unwrap();
    let (ids, rejected, unresolved) = added(payload);
    assert_eq!(ids.len(), 2, "the two real files");
    assert!(unresolved.is_empty());
    let reasons: Vec<(&str, bool)> = rejected.iter().map(|r| (r.source.as_str(), r.reason.contains("unknown source") || r.reason.contains("opening"))).collect();
    assert_eq!(reasons, [("not a source", true), (missing.as_str(), true)], "each refusal says why");

    let queue = client.queue().await.unwrap();
    assert_eq!(queue.items.len(), 2);
    assert_eq!((queue.items[0].title.as_deref(), queue.items[0].duration_ms), (Some("one.wav"), Some(1_000)));
    assert_eq!((queue.items[1].title.as_deref(), queue.items[1].duration_ms), (Some("two.wav"), Some(2_000)));
    assert_eq!(queue.items[0].source, one);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn where_tracks_are_added_is_honoured() {
    let f = fixture("at", false).await;
    let client = f.client().await;
    let sources: Vec<String> = ["a", "b", "c", "d"].iter().map(|name| f.wav(&format!("{name}.wav"), 100)).collect();
    client.request(add(&[&sources[0], &sources[1]], AddAt::End)).await.unwrap();
    client.request(add(&[&sources[2]], AddAt::Index { index: 0 })).await.unwrap();
    client.request(add(&[&sources[3]], AddAt::Index { index: 99 })).await.unwrap();

    let titles = |queue: &Queue| queue.items.iter().map(|item| item.title.clone().unwrap()).collect::<Vec<_>>();
    assert_eq!(titles(&client.queue().await.unwrap()), ["c.wav", "a.wav", "b.wav", "d.wav"], "an index is clamped to the end");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_track_whose_details_cannot_be_fetched_is_added_without_them() {
    let f = fixture("unresolved", false).await;
    let client = f.client().await;

    // There is no TIDAL in this daemon, so nothing can be learned about the track: that is not a
    // reason to refuse it.
    let (ids, rejected, unresolved) = added(client.request(add(&["tidal:233059491"], AddAt::End)).await.unwrap());
    assert_eq!((ids.len(), rejected.len()), (1, 0));
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].id, ids[0]);
    assert!(unresolved[0].reason.contains("phonia login"), "{:?}", unresolved[0].reason);

    let item = &client.queue().await.unwrap().items[0];
    assert_eq!((item.source.as_str(), item.title.as_deref(), item.duration_ms), ("tidal:233059491", None, None));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn too_many_tracks_at_once_is_a_bad_request() {
    let f = fixture("many", false).await;
    let client = f.client().await;
    let many: Vec<String> = (0..1001).map(|i| format!("tidal:{i}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(protocol_code(client.request(add(&refs, AddAt::End)).await.unwrap_err()), ErrorCode::BadRequest);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editing_the_queue_and_asking_for_what_does_not_exist() {
    let f = fixture("edit", false).await;
    let client = f.client().await;
    let sources: Vec<String> = ["a", "b", "c"].iter().map(|name| f.wav(&format!("{name}.wav"), 100)).collect();
    let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
    let (ids, _, _) = added(client.request(add(&refs, AddAt::End)).await.unwrap());

    assert_eq!(client.request(Request::QueueMove { id: ids[2], to: 0 }).await.unwrap(), Payload::Ack);
    assert_eq!(client.request(Request::SetShuffle { shuffle: true }).await.unwrap(), Payload::Ack);
    assert_eq!(client.request(Request::SetRepeat { repeat: Repeat::All }).await.unwrap(), Payload::Ack);
    let queue = client.queue().await.unwrap();
    assert_eq!(queue.items[0].id, ids[2]);
    assert_eq!((queue.shuffle, queue.repeat), (true, Repeat::All));

    assert_eq!(client.request(Request::QueueRemove { ids: vec![ids[0], ItemId(9999)] }).await.unwrap(), Payload::Removed { count: 1 });
    assert_eq!(protocol_code(client.request(Request::QueueMove { id: ItemId(9999), to: 0 }).await.unwrap_err()), ErrorCode::NotFound);
    assert_eq!(protocol_code(client.request(Request::Play { item: Some(ItemId(9999)) }).await.unwrap_err()), ErrorCode::NotFound);

    assert_eq!(client.request(Request::QueueClear).await.unwrap(), Payload::Ack);
    assert!(client.queue().await.unwrap().items.is_empty());
    f.finish().await;
}

// ---- events --------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribing_gives_a_snapshot_and_then_every_event_in_order() {
    let f = fixture("events", false).await;
    let client = f.client().await;
    let (snapshot, mut events) = client.subscribe().await.unwrap();
    assert_eq!(snapshot.status.state, State::Stopped);

    let source = f.wav("song.wav", 20_000);
    let (ids, _, _) = added(client.request(add(&[&source], AddAt::End)).await.unwrap());
    client.request(Request::Play { item: None }).await.unwrap();

    let seen = events_until(&mut events, |event| matches!(event, Event::QueueExhausted)).await;
    let seqs: Vec<u64> = seen.iter().map(|(seq, _)| *seq).collect();
    let expected: Vec<u64> = (snapshot.seq + 1..=snapshot.seq + seqs.len() as u64).collect();
    assert_eq!(seqs, expected, "consecutive numbers: no event was missed or repeated");

    let kinds: Vec<&Event> = seen.iter().map(|(_, event)| event).collect();
    assert!(kinds.iter().any(|e| matches!(e, Event::QueueChanged { queue } if queue.items.len() == 1)));
    let started = kinds.iter().find_map(|e| match e {
        Event::TrackStarted { item_id, source, title, spec, .. } => Some((*item_id, source.clone(), title.clone(), *spec)),
        _ => None,
    });
    let (item_id, started_source, title, spec) = started.expect("a track started");
    assert_eq!((item_id, started_source.as_deref(), title.as_deref()), (Some(ids[0]), Some(source.as_str()), Some("song.wav")));
    assert_eq!((spec.sample_rate, spec.bits_per_sample), (44_100, 16));
    assert!(kinds.iter().any(|e| matches!(e, Event::TrackEnded { reason: EndReason::Completed, .. })));
    assert!(kinds.iter().any(|e| matches!(e, Event::Position { .. })));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_client_sees_the_same_events_in_the_same_order() {
    let f = fixture("fanout", false).await;
    let (a, b) = (f.client().await, f.client().await);
    let (snapshot_a, mut events_a) = a.subscribe().await.unwrap();
    let (snapshot_b, mut events_b) = b.subscribe().await.unwrap();
    assert_eq!(snapshot_a.seq, snapshot_b.seq);

    let (one, two) = (f.wav("one.wav", 10_000), f.wav("two.wav", 10_000));
    a.request(add(&[&one], AddAt::End)).await.unwrap();
    b.request(add(&[&two], AddAt::End)).await.unwrap();
    a.request(Request::Play { item: None }).await.unwrap();

    let stop = |event: &Event| matches!(event, Event::QueueExhausted);
    let (seen_a, seen_b) = (events_until(&mut events_a, stop).await, events_until(&mut events_b, stop).await);
    assert_eq!(seen_a, seen_b);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribing_stops_the_events() {
    let f = fixture("unsubscribe", false).await;
    let client = f.client().await;
    let (_, mut events) = client.subscribe().await.unwrap();
    client.request(Request::SetShuffle { shuffle: true }).await.unwrap();
    assert!(matches!(next_event(&mut events).await.1, Event::QueueChanged { .. }));

    assert_eq!(client.request(Request::Unsubscribe).await.unwrap(), Payload::Ack);
    client.request(Request::SetShuffle { shuffle: false }).await.unwrap();
    let silence = tokio::time::timeout(Duration::from_millis(300), events.next_seq()).await;
    assert!(silence.is_err(), "nothing may arrive after unsubscribing: {silence:?}");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_falls_behind_is_resynced_not_dropped() {
    let f = fixture("lag", false).await;
    let mut slow = f.raw().await;
    slow.hello().await;
    slow.send(r#"{"id":2,"request":{"type":"subscribe"}}"#).await;
    let Some(ServerMessage::Response { reply: Reply::Ok(Payload::Snapshot { .. }), .. }) = slow.recv().await else { panic!("no snapshot") };

    // The slow client stops reading while thousands of events go by: far more than the daemon
    // keeps for it.
    let busy = f.client().await;
    for i in 0..2_500 {
        busy.request(Request::SetShuffle { shuffle: i % 2 == 0 }).await.unwrap();
    }

    // When it reads again it gets a resync with the current state (after whatever the daemon had
    // already queued for it).
    let mut resync = None;
    for _ in 0..3_000 {
        if let Some(ServerMessage::Event { seq, event: Event::Resync { skipped, seq: at, queue, .. } }) = slow.recv().await {
            assert!(skipped > 0);
            assert_eq!(seq, at);
            assert!(queue.version >= 1_000, "the resync carries the current state");
            resync = Some(at);
            break;
        }
    }
    let resync_seq = resync.expect("a lagging client must be told to resync");

    // From then on it is in step again: a new event arrives, numbered after the resync.
    busy.request(Request::SetShuffle { shuffle: true }).await.unwrap();
    let Some(ServerMessage::Event { seq, event }) = slow.recv().await else { panic!("no event after the resync") };
    assert!(seq > resync_seq, "event {seq} came at or before the resync {resync_seq}");
    assert!(matches!(event, Event::QueueChanged { .. }), "{event:?}");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bit_perfect_report_reaches_subscribers() {
    let f = fixture("report", false).await;
    let client = f.client().await;
    let (_, mut events) = client.subscribe().await.unwrap();

    let contents = "format: S24_3LE\nrate: 96000 (96000/1)\n";
    f.reports
        .send(SinkReport::new(
            "hw:1,0".into(),
            phonia_core::decode::SourceSpec { sample_rate: 96_000, channels: 2, bits_per_sample: 24 },
            "S24_3LE".into(),
            ProcReading::Read { path: "/p".into(), contents: contents.into() },
        ))
        .unwrap();

    let (_, event) = events_until(&mut events, |event| matches!(event, Event::SinkReport(_))).await.pop().unwrap();
    let Event::SinkReport(report) = event else { unreachable!() };
    assert!(report.bit_perfect);
    assert_eq!((report.device.as_str(), report.negotiated_format.as_str()), ("hw:1,0", "S24_3LE"));
    assert_eq!(report.hw_params.as_deref(), Some(contents));
    f.finish().await;
}

// ---- playback control ----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seek_with_nothing_playing_is_answered_and_then_reported_as_rejected() {
    let f = fixture("seek", false).await;
    let client = f.client().await;
    let (_, mut events) = client.subscribe().await.unwrap();

    let ack = client.request(Request::Seek { target: SeekTarget::Absolute { ms: 5_000 } }).await.unwrap();
    assert_eq!(ack, Payload::Ack, "the request was accepted; what the engine makes of it comes as an event");
    let (_, event) = next_event(&mut events).await;
    assert!(matches!(event, Event::SeekRejected { .. }), "{event:?}");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_the_playing_entry_skips_to_the_next_one() {
    let f = fixture("remove", true).await;
    let client = f.client().await;
    let (a, b) = (f.wav("a.wav", 200_000), f.wav("b.wav", 100));
    let (ids, _, _) = added(client.request(add(&[&a, &b], AddAt::End)).await.unwrap());
    let (_, mut events) = client.subscribe().await.unwrap();

    client.request(Request::Play { item: Some(ids[0]) }).await.unwrap();
    events_until(&mut events, |event| matches!(event, Event::TrackStarted { .. })).await;
    assert_eq!(client.status().await.unwrap().track.unwrap().item_id, Some(ids[0]));

    assert_eq!(client.request(Request::QueueRemove { ids: vec![ids[0]] }).await.unwrap(), Payload::Removed { count: 1 });
    f.sinks.handles()[0].advance(1024); // lets the blocked write return so the engine sees the skip
    let seen = events_until(&mut events, |event| matches!(event, Event::TrackStarted { .. })).await;
    let events: Vec<&Event> = seen.iter().map(|(_, event)| event).collect();
    assert!(events.iter().any(|e| matches!(e, Event::TrackEnded { item_id, reason: EndReason::Interrupted } if *item_id == Some(ids[0]))));
    assert!(matches!(events.last(), Some(Event::TrackStarted { item_id, .. }) if *item_id == Some(ids[1])));
    f.sinks.handles()[0].set_blocking(false);
    f.finish().await;
}

// ---- shutting down -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shutdown_request_stops_the_daemon_and_tells_subscribers() {
    let f = fixture("shutdown", false).await;
    let (asker, watcher) = (f.client().await, f.client().await);
    let (_, mut events) = watcher.subscribe().await.unwrap();
    let mut stopping = f.daemon.shutdown_signal();

    assert_eq!(asker.request(Request::Shutdown).await.unwrap(), Payload::Ack);
    let seen = events_until(&mut events, |event| matches!(event, Event::ShuttingDown)).await;
    assert!(matches!(seen.last(), Some((_, Event::ShuttingDown))));
    tokio::time::timeout(TIMEOUT, phoniad::daemon::wait_for_shutdown(&mut stopping)).await.expect("the daemon must signal that it is stopping");

    tokio::time::timeout(TIMEOUT, asker.closed()).await.expect("connections are closed on shutdown");
    tokio::time::timeout(TIMEOUT, watcher.closed()).await.unwrap();
    assert!(matches!(asker.request(Request::Status).await, Err(ClientError::Closed | ClientError::Io(_))));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_after_the_connection_drops_fails_instead_of_hanging() {
    let f = fixture("drop", false).await;
    let client = f.client().await;
    f.daemon.request_shutdown();
    tokio::time::timeout(TIMEOUT, client.closed()).await.unwrap();
    let error = tokio::time::timeout(TIMEOUT, client.request(Request::Status)).await.expect("must not hang").unwrap_err();
    assert!(matches!(error, ClientError::Closed | ClientError::Io(_)), "{error}");
    f.finish().await;
}
