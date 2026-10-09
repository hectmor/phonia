//! The daemon, its server and the client, talking through in-memory pipes with a fake audio
//! device: everything except real hardware and real sockets.

use phonia_core::catalog::fake::FakeCatalog;
use phonia_core::catalog::{self, Catalog, CatalogError};
use phonia_core::config::OutputSpec;
use phonia_core::openers::{DispatchOpener, QualityLimits};
use phonia_core::output::VolumeControl as _;
use phonia_core::output::alsa::{ProcReading, SinkReport};
use phonia_core::output::catalog::{Entry, Mode};
use phonia_core::output::fake::FakeSinkFactory;
use phonia_ipc::framing::{read_frame, write_frame};
use phonia_ipc::*;
use phoniad::daemon::{Daemon, DaemonParts, OutputReport};
use phoniad::outputs::{Build, Outputs};
use phoniad::server::serve_connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Every sink factory built for an output the daemon was switched to, in order, by output id.
type Switched = Arc<Mutex<Vec<(String, Arc<FakeSinkFactory>)>>>;

struct Fixture {
    daemon: Arc<Daemon>,
    sinks: Arc<FakeSinkFactory>,
    switched: Switched,
    reports: mpsc::UnboundedSender<OutputReport>,
    dir: PathBuf,
}

/// A daemon with a fake device. A blocking device holds the engine mid-track, which is what lets
/// a test act on a track that is "playing".
async fn fixture(name: &str, blocking: bool) -> Fixture {
    fixture_with(name, blocking, Default::default()).await
}

/// Like [`fixture`], with engine options of the test's choosing.
async fn fixture_with(
    name: &str,
    blocking: bool,
    options: phonia_core::engine::Options,
) -> Fixture {
    fixture_full(name, blocking, options, None).await
}

/// A fixture with a catalog of the test's choosing (none, like a daemon with no TIDAL login).
async fn fixture_with_catalog(name: &str, catalog: Option<Arc<dyn Catalog>>) -> Fixture {
    fixture_full(name, false, Default::default(), catalog).await
}

async fn fixture_full(
    name: &str,
    blocking: bool,
    options: phonia_core::engine::Options,
    catalog: Option<Arc<dyn Catalog>>,
) -> Fixture {
    let sinks = if blocking {
        FakeSinkFactory::blocking()
    } else {
        FakeSinkFactory::autoplay()
    };
    let (reports, report_rx) = mpsc::unbounded_channel();
    let switched: Switched = Arc::default();
    let built = switched.clone();
    let build: Build = Arc::new(move |spec| {
        // A shared output has a volume of its own to set; an exclusive card has none.
        let factory = match spec {
            OutputSpec::Shared { .. } => FakeSinkFactory::blocking().with_volume(),
            OutputSpec::Exclusive { .. } => FakeSinkFactory::blocking(),
        };
        built.lock().unwrap().push((spec.id(), factory.clone()));
        factory
    });
    let outputs = Outputs::new(
        OutputSpec::Exclusive {
            device: "hw:fake,0".into(),
        },
        build,
    )
    .with_lister(Arc::new(|| {
        Box::pin(async {
            let entry = |id: &str, mode, name: &str, bit_perfect| Entry {
                id: id.into(),
                mode,
                name: name.into(),
                detail: None,
                bit_perfect,
                lossy: false,
                codec: None,
                is_default: false,
            };
            vec![
                entry("exclusive:hw:fake,0", Mode::Exclusive, "Fake DAC", true),
                entry("shared:speaker", Mode::Shared, "Fake speaker", false),
            ]
        })
    }));
    let daemon = Daemon::start(DaemonParts {
        sinks: sinks.clone(),
        outputs,
        opener: Arc::new(DispatchOpener::new(None)),
        catalog,
        quality: Some(Arc::new(QualityLimits::new(
            phonia_core::config::Quality::Hires,
            phonia_core::config::Quality::Lossless,
        ))),
        play_log: None,
        reports: report_rx,
        engine: options,
        replaygain: phonia_core::replaygain::Mode::default(),
        autoplay: false,
    })
    .unwrap();
    let dir = std::env::temp_dir().join(format!(
        "phoniad-protocol-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Fixture {
        daemon,
        sinks,
        switched,
        reports,
        dir,
    }
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
        Client::from_stream(
            self.serve(),
            ClientInfo {
                name: "test".into(),
                version: "0".into(),
            },
        )
        .await
        .unwrap()
    }

    async fn raw(&self) -> Raw {
        let (read_half, write_half) = tokio::io::split(self.serve());
        Raw {
            reader: BufReader::new(read_half),
            writer: write_half,
            buffer: Vec::new(),
        }
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
        write_frame(&mut self.writer, line.as_bytes())
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> Option<ServerMessage> {
        let frame = tokio::time::timeout(TIMEOUT, read_frame(&mut self.reader, &mut self.buffer))
            .await
            .expect("timed out waiting for the daemon")
            .unwrap()?;
        Some(serde_json::from_slice(frame).unwrap_or_else(|error| {
            panic!("bad message {}: {error}", String::from_utf8_lossy(frame))
        }))
    }

    async fn hello(&mut self) {
        assert!(
            matches!(self.recv().await, Some(ServerMessage::Hello(_))),
            "the server speaks first"
        );
        self.send(r#"{"id":1,"request":{"type":"hello","protocol":{"major":1,"minor":0},"client":{"name":"raw","version":"0"}}}"#).await;
        assert_eq!(
            self.recv().await,
            Some(ServerMessage::Response {
                id: RequestId(1),
                reply: Reply::Ok(Payload::Ack)
            })
        );
    }
}

fn code_of(reply: Option<ServerMessage>) -> (RequestId, ErrorCode) {
    match reply {
        Some(ServerMessage::Response {
            id,
            reply: Reply::Err(error),
        }) => (id, error.code),
        other => panic!("expected an error response, got {other:?}"),
    }
}

async fn next_event(events: &mut EventStream) -> (u64, Event) {
    tokio::time::timeout(TIMEOUT, events.next_seq())
        .await
        .expect("timed out waiting for an event")
        .expect("the connection closed")
}

/// Events until one satisfies `stop`, which is included.
async fn events_until(
    events: &mut EventStream,
    stop: impl Fn(&Event) -> bool,
) -> Vec<(u64, Event)> {
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
        Payload::Added {
            ids,
            rejected,
            unresolved,
        } => (ids, rejected, unresolved),
        other => panic!("expected added, got {other:?}"),
    }
}

fn add(tracks: &[&str], at: AddAt) -> Request {
    Request::QueueAdd {
        tracks: tracks
            .iter()
            .map(|source| NewTrack {
                source: source.to_string(),
            })
            .collect(),
        at,
    }
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
    assert!(
        client
            .server()
            .capabilities
            .iter()
            .any(|capability| capability == CAP_OUTPUT_RELEASE)
    );

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
    assert_eq!(
        code_of(raw.recv().await),
        (RequestId(4), ErrorCode::HandshakeRequired)
    );

    // After the handshake the same request works.
    raw.send(r#"{"id":5,"request":{"type":"hello","protocol":{"major":1,"minor":3},"client":{"name":"raw","version":"0"}}}"#).await;
    assert!(
        matches!(
            raw.recv().await,
            Some(ServerMessage::Response {
                reply: Reply::Ok(Payload::Ack),
                ..
            })
        ),
        "a newer minor version is fine"
    );
    raw.send(r#"{"id":6,"request":{"type":"status"}}"#).await;
    assert!(matches!(
        raw.recv().await,
        Some(ServerMessage::Response {
            reply: Reply::Ok(Payload::Status(_)),
            ..
        })
    ));
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
    assert!(
        raw.recv().await.is_none(),
        "the connection is closed after refusing the version"
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_request_is_answered_and_the_connection_survives() {
    let f = fixture("malformed", false).await;
    let mut raw = f.raw().await;
    raw.hello().await;

    // Not JSON at all: nothing to match the answer to.
    raw.send("this is not json").await;
    assert_eq!(
        code_of(raw.recv().await),
        (RequestId(0), ErrorCode::BadRequest)
    );

    // Valid JSON of the wrong shape, with an id: the answer names it.
    raw.send(r#"{"id":7,"request":{"type":"seek"}}"#).await;
    assert_eq!(
        code_of(raw.recv().await),
        (RequestId(7), ErrorCode::BadRequest)
    );

    // A request this version has never heard of.
    raw.send(r#"{"id":8,"request":{"type":"teleport"}}"#).await;
    assert_eq!(
        code_of(raw.recv().await),
        (RequestId(8), ErrorCode::UnknownRequest)
    );

    // And the connection still works.
    raw.send(r#"{"id":9,"request":{"type":"status"}}"#).await;
    assert!(matches!(
        raw.recv().await,
        Some(ServerMessage::Response {
            id: RequestId(9),
            reply: Reply::Ok(Payload::Status(_))
        })
    ));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_message_is_refused_and_the_connection_closed() {
    let f = fixture("oversized", false).await;
    let mut raw = f.raw().await;
    raw.hello().await;

    let Raw {
        reader, mut writer, ..
    } = raw;
    let sender = tokio::spawn(async move {
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..10 {
            if writer.write_all(&chunk).await.is_err() {
                break;
            }
        }
    });
    let mut receiver = Raw {
        reader,
        writer: tokio::io::split(tokio::io::duplex(1).0).1,
        buffer: Vec::new(),
    };
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

    let payload = client
        .request(add(&[&one, "not a source", &missing, &two], AddAt::End))
        .await
        .unwrap();
    let (ids, rejected, unresolved) = added(payload);
    assert_eq!(ids.len(), 2, "the two real files");
    assert!(unresolved.is_empty());
    let reasons: Vec<(&str, bool)> = rejected
        .iter()
        .map(|r| {
            (
                r.source.as_str(),
                r.reason.contains("unknown source") || r.reason.contains("opening"),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        [("not a source", true), (missing.as_str(), true)],
        "each refusal says why"
    );

    let queue = client.queue().await.unwrap();
    assert_eq!(queue.items.len(), 2);
    assert_eq!(
        (queue.items[0].title.as_deref(), queue.items[0].duration_ms),
        (Some("one.wav"), Some(1_000))
    );
    assert_eq!(
        (queue.items[1].title.as_deref(), queue.items[1].duration_ms),
        (Some("two.wav"), Some(2_000))
    );
    assert_eq!(queue.items[0].source, one);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn where_tracks_are_added_is_honoured() {
    let f = fixture("at", false).await;
    let client = f.client().await;
    let sources: Vec<String> = ["a", "b", "c", "d"]
        .iter()
        .map(|name| f.wav(&format!("{name}.wav"), 100))
        .collect();
    client
        .request(add(&[&sources[0], &sources[1]], AddAt::End))
        .await
        .unwrap();
    client
        .request(add(&[&sources[2]], AddAt::Index { index: 0 }))
        .await
        .unwrap();
    client
        .request(add(&[&sources[3]], AddAt::Index { index: 99 }))
        .await
        .unwrap();

    let titles = |queue: &Queue| {
        queue
            .items
            .iter()
            .map(|item| item.title.clone().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles(&client.queue().await.unwrap()),
        ["c.wav", "a.wav", "b.wav", "d.wav"],
        "an index is clamped to the end"
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_track_whose_details_cannot_be_fetched_is_added_without_them() {
    let f = fixture("unresolved", false).await;
    let client = f.client().await;

    // There is no TIDAL in this daemon, so nothing can be learned about the track: that is not a
    // reason to refuse it.
    let (ids, rejected, unresolved) = added(
        client
            .request(add(&["tidal:233059491"], AddAt::End))
            .await
            .unwrap(),
    );
    assert_eq!((ids.len(), rejected.len()), (1, 0));
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].id, ids[0]);
    assert!(
        unresolved[0].reason.contains("phonia login"),
        "{:?}",
        unresolved[0].reason
    );

    let item = &client.queue().await.unwrap().items[0];
    assert_eq!(
        (
            item.source.as_str(),
            item.title.as_deref(),
            item.duration_ms
        ),
        ("tidal:233059491", None, None)
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn too_many_tracks_at_once_is_a_bad_request() {
    let f = fixture("many", false).await;
    let client = f.client().await;
    let many: Vec<String> = (0..1001).map(|i| format!("tidal:{i}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(
        protocol_code(client.request(add(&refs, AddAt::End)).await.unwrap_err()),
        ErrorCode::BadRequest
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editing_the_queue_and_asking_for_what_does_not_exist() {
    let f = fixture("edit", false).await;
    let client = f.client().await;
    let sources: Vec<String> = ["a", "b", "c"]
        .iter()
        .map(|name| f.wav(&format!("{name}.wav"), 100))
        .collect();
    let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
    let (ids, _, _) = added(client.request(add(&refs, AddAt::End)).await.unwrap());

    assert_eq!(
        client
            .request(Request::QueueMove { id: ids[2], to: 0 })
            .await
            .unwrap(),
        Payload::Ack
    );
    assert_eq!(
        client
            .request(Request::SetShuffle { shuffle: true })
            .await
            .unwrap(),
        Payload::Ack
    );
    assert_eq!(
        client
            .request(Request::SetRepeat {
                repeat: Repeat::All
            })
            .await
            .unwrap(),
        Payload::Ack
    );
    assert_eq!(
        client
            .request(Request::SetAutoplay { autoplay: true })
            .await
            .unwrap(),
        Payload::Ack
    );
    let queue = client.queue().await.unwrap();
    assert_eq!(queue.items[0].id, ids[2]);
    assert_eq!(
        (queue.shuffle, queue.repeat, queue.autoplay),
        (true, Repeat::All, true)
    );

    assert_eq!(
        client
            .request(Request::QueueRemove {
                ids: vec![ids[0], ItemId(9999)]
            })
            .await
            .unwrap(),
        Payload::Removed { count: 1 }
    );
    assert_eq!(
        protocol_code(
            client
                .request(Request::QueueMove {
                    id: ItemId(9999),
                    to: 0
                })
                .await
                .unwrap_err()
        ),
        ErrorCode::NotFound
    );
    assert_eq!(
        protocol_code(
            client
                .request(Request::Play {
                    item: Some(ItemId(9999))
                })
                .await
                .unwrap_err()
        ),
        ErrorCode::NotFound
    );

    assert_eq!(
        client.request(Request::QueueClear).await.unwrap(),
        Payload::Ack
    );
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
    assert_eq!(
        seqs, expected,
        "consecutive numbers: no event was missed or repeated"
    );

    let kinds: Vec<&Event> = seen.iter().map(|(_, event)| event).collect();
    assert!(
        kinds
            .iter()
            .any(|e| matches!(e, Event::QueueChanged { queue } if queue.items.len() == 1))
    );
    let started = kinds.iter().find_map(|e| match e {
        Event::TrackStarted {
            item_id,
            source,
            title,
            spec,
            ..
        } => Some((*item_id, source.clone(), title.clone(), *spec)),
        _ => None,
    });
    let (item_id, started_source, title, spec) = started.expect("a track started");
    assert_eq!(
        (item_id, started_source.as_deref(), title.as_deref()),
        (Some(ids[0]), Some(source.as_str()), Some("song.wav"))
    );
    assert_eq!((spec.sample_rate, spec.bits_per_sample), (44_100, 16));
    assert!(kinds.iter().any(|e| matches!(
        e,
        Event::TrackEnded {
            reason: EndReason::Completed,
            ..
        }
    )));
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
    let (seen_a, seen_b) = (
        events_until(&mut events_a, stop).await,
        events_until(&mut events_b, stop).await,
    );
    assert_eq!(seen_a, seen_b);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribing_stops_the_events() {
    let f = fixture("unsubscribe", false).await;
    let client = f.client().await;
    let (_, mut events) = client.subscribe().await.unwrap();
    client
        .request(Request::SetShuffle { shuffle: true })
        .await
        .unwrap();
    assert!(matches!(
        next_event(&mut events).await.1,
        Event::QueueChanged { .. }
    ));

    assert_eq!(
        client.request(Request::Unsubscribe).await.unwrap(),
        Payload::Ack
    );
    client
        .request(Request::SetShuffle { shuffle: false })
        .await
        .unwrap();
    let silence = tokio::time::timeout(Duration::from_millis(300), events.next_seq()).await;
    assert!(
        silence.is_err(),
        "nothing may arrive after unsubscribing: {silence:?}"
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_falls_behind_is_resynced_not_dropped() {
    let f = fixture("lag", false).await;
    let mut slow = f.raw().await;
    slow.hello().await;
    slow.send(r#"{"id":2,"request":{"type":"subscribe"}}"#)
        .await;
    let Some(ServerMessage::Response {
        reply: Reply::Ok(Payload::Snapshot { .. }),
        ..
    }) = slow.recv().await
    else {
        panic!("no snapshot")
    };

    // The slow client stops reading while thousands of events go by: far more than the daemon
    // keeps for it.
    let busy = f.client().await;
    for i in 0..2_500 {
        busy.request(Request::SetShuffle {
            shuffle: i % 2 == 0,
        })
        .await
        .unwrap();
    }

    // When it reads again it gets a resync with the current state (after whatever the daemon had
    // already queued for it).
    let mut resync = None;
    for _ in 0..3_000 {
        if let Some(ServerMessage::Event {
            seq,
            event:
                Event::Resync {
                    skipped,
                    seq: at,
                    queue,
                    ..
                },
        }) = slow.recv().await
        {
            assert!(skipped > 0);
            assert_eq!(seq, at);
            assert!(
                queue.version >= 1_000,
                "the resync carries the current state"
            );
            resync = Some(at);
            break;
        }
    }
    let resync_seq = resync.expect("a lagging client must be told to resync");

    // From then on it is in step again: a new event arrives, numbered after the resync.
    busy.request(Request::SetShuffle { shuffle: true })
        .await
        .unwrap();
    let Some(ServerMessage::Event { seq, event }) = slow.recv().await else {
        panic!("no event after the resync")
    };
    assert!(
        seq > resync_seq,
        "event {seq} came at or before the resync {resync_seq}"
    );
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
        .send(OutputReport {
            output: "exclusive:hw:fake,0".into(),
            report: SinkReport::new(
                "hw:1,0".into(),
                phonia_core::decode::SourceSpec {
                    sample_rate: 96_000,
                    channels: 2,
                    bits_per_sample: 24,
                },
                "S24_3LE".into(),
                ProcReading::Read {
                    path: "/p".into(),
                    contents: contents.into(),
                },
            ),
        })
        .unwrap();

    let (_, event) = events_until(&mut events, |event| matches!(event, Event::SinkReport(_)))
        .await
        .pop()
        .unwrap();
    let Event::SinkReport(report) = event else {
        unreachable!()
    };
    assert!(report.bit_perfect);
    assert_eq!(
        (report.device.as_str(), report.negotiated_format.as_str()),
        ("hw:1,0", "S24_3LE")
    );
    assert_eq!(report.hw_params.as_deref(), Some(contents));
    assert_eq!(report.output.as_deref(), Some("exclusive:hw:fake,0"));
    f.finish().await;
}

/// A helper the new `sink_report`-in-`Status` tests share: plays a short track and waits for it to
/// start, so `controller.status().spec` is known and a report can be made to match (or not match)
/// it on purpose.
async fn playing_a_track(f: &Fixture, client: &Client, events: &mut EventStream) {
    // Long enough, and the fixture's own blocking sink, that the track is still "playing" (not
    // finished and stopped, which would itself clear the verdict) for as long as the test needs.
    let a = f.wav("a.wav", 200_000);
    let (ids, _, _) = added(client.request(add(&[&a], AddAt::End)).await.unwrap());
    client
        .request(Request::Play { item: Some(ids[0]) })
        .await
        .unwrap();
    events_until(events, |event| matches!(event, Event::TrackStarted { .. })).await;
}

/// The `SourceSpec`/`Spec` of the WAV [`Fixture::wav`] writes: 44.1 kHz, 16-bit, stereo.
const WAV_SPEC: phonia_core::decode::SourceSpec = phonia_core::decode::SourceSpec {
    sample_rate: 44_100,
    channels: 2,
    bits_per_sample: 16,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_subscribes_mid_track_sees_the_verdict_in_its_snapshot() {
    let f = fixture("report-snapshot", true).await;
    let client = f.client().await;
    let (_, mut events) = client.subscribe().await.unwrap();
    playing_a_track(&f, &client, &mut events).await;

    f.reports
        .send(OutputReport {
            output: "exclusive:hw:fake,0".into(),
            report: SinkReport::new(
                "hw:fake,0".into(),
                WAV_SPEC,
                "S16_LE".into(),
                ProcReading::NotHw,
            ),
        })
        .unwrap();
    events_until(&mut events, |event| matches!(event, Event::SinkReport(_))).await;

    // A second, freshly-subscribing client sees the verdict right away, with no need to wait for
    // the next sink to open (there may not be one for a long time, on a gapless album).
    let second = f.client().await;
    let (snapshot, _events) = second.subscribe().await.unwrap();
    let report = snapshot
        .status
        .sink_report
        .expect("the verdict is in the snapshot");
    assert_eq!(report.device, "hw:fake,0");
    f.sinks.handles()[0].set_blocking(false);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn releasing_the_output_clears_the_verdict() {
    let f = fixture("report-release", true).await;
    let client = f.client().await;
    let (_, mut events) = client.subscribe().await.unwrap();
    playing_a_track(&f, &client, &mut events).await;

    f.reports
        .send(OutputReport {
            output: "exclusive:hw:fake,0".into(),
            report: SinkReport::new(
                "hw:fake,0".into(),
                WAV_SPEC,
                "S16_LE".into(),
                ProcReading::NotHw,
            ),
        })
        .unwrap();
    events_until(&mut events, |event| matches!(event, Event::SinkReport(_))).await;
    assert!(client.status().await.unwrap().sink_report.is_some());

    client.request(Request::Release).await.unwrap();
    f.sinks.handles()[0].advance(1024); // lets the blocked write return so the engine sees it
    events_until(&mut events, |event| {
        matches!(event, Event::OutputReleased { .. })
    })
    .await;
    assert_eq!(client.status().await.unwrap().sink_report, None);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_for_a_different_format_is_not_shown_as_the_current_verdict() {
    let f = fixture("report-mismatch", true).await;
    let client = f.client().await;
    let (_, mut events) = client.subscribe().await.unwrap();
    playing_a_track(&f, &client, &mut events).await;

    f.reports
        .send(OutputReport {
            output: "exclusive:hw:fake,0".into(),
            report: SinkReport::new(
                "hw:fake,0".into(),
                phonia_core::decode::SourceSpec {
                    sample_rate: 96_000,
                    channels: 2,
                    bits_per_sample: 24,
                },
                "S24_3LE".into(),
                ProcReading::NotHw,
            ),
        })
        .unwrap();
    events_until(&mut events, |event| matches!(event, Event::SinkReport(_))).await;

    assert_eq!(client.status().await.unwrap().sink_report, None);
    f.sinks.handles()[0].set_blocking(false);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_for_another_output_is_not_shown_as_the_current_verdict() {
    let f = fixture("report-other-output", true).await;
    let client = f.client().await;
    let (_, mut events) = client.subscribe().await.unwrap();
    playing_a_track(&f, &client, &mut events).await;

    f.reports
        .send(OutputReport {
            output: "exclusive:hw:other,0".into(),
            report: SinkReport::new(
                "hw:other,0".into(),
                WAV_SPEC,
                "S16_LE".into(),
                ProcReading::NotHw,
            ),
        })
        .unwrap();
    events_until(&mut events, |event| matches!(event, Event::SinkReport(_))).await;

    assert_eq!(client.status().await.unwrap().sink_report, None);
    f.sinks.handles()[0].set_blocking(false);
    f.finish().await;
}

// ---- outputs -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_outputs_are_listed_and_the_status_says_which_one_is_playing() {
    let f = fixture("outputs", false).await;
    let client = f.client().await;
    assert!(
        client
            .server()
            .capabilities
            .iter()
            .any(|capability| capability == CAP_OUTPUT_SELECT)
    );

    let Payload::Outputs { outputs, current } = client.request(Request::Outputs).await.unwrap()
    else {
        panic!("not a list")
    };
    assert_eq!(
        outputs.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["exclusive:hw:fake,0", "shared:speaker"]
    );
    assert_eq!(current.as_deref(), Some("exclusive:hw:fake,0"));
    assert!(outputs[0].bit_perfect && !outputs[1].bit_perfect);

    let route = client
        .status()
        .await
        .unwrap()
        .route
        .expect("the daemon says where the sound goes");
    assert_eq!(
        (route.id.as_str(), route.mode),
        ("exclusive:hw:fake,0", OutputMode::Exclusive)
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_the_output_moves_playback_keeps_the_position_and_tells_everyone() {
    let f = fixture("switch", true).await;
    let client = f.client().await;
    let a = f.wav("a.wav", 300_000);
    let (ids, _, _) = added(client.request(add(&[&a], AddAt::End)).await.unwrap());
    let (_, mut events) = client.subscribe().await.unwrap();
    client
        .request(Request::Play { item: Some(ids[0]) })
        .await
        .unwrap();
    events_until(&mut events, |event| {
        matches!(event, Event::TrackStarted { .. })
    })
    .await;

    // The engine is writing to a full queue: let the DAC play a period while the switch waits.
    let sink = f.sinks.handles()[0].clone();
    let switching = {
        let client = f.client().await;
        tokio::spawn(async move {
            client
                .request(Request::SetOutput {
                    output: "shared:speaker".into(),
                })
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    sink.advance(1024);
    assert_eq!(switching.await.unwrap().unwrap(), Payload::Ack);

    let seen = events_until(&mut events, |event| {
        matches!(event, Event::OutputChanged { .. })
    })
    .await;
    let Some((_, Event::OutputChanged { route })) = seen.last() else {
        unreachable!()
    };
    assert_eq!(
        (route.id.as_str(), route.mode),
        ("shared:speaker", OutputMode::Shared)
    );
    assert_eq!(
        route.description, "Fake speaker",
        "named from the list of outputs"
    );

    let status = client.status().await.unwrap();
    assert_eq!(status.route.unwrap().id, "shared:speaker");
    assert_eq!(status.state, State::Playing, "playback carried on");
    assert_eq!(f.sinks.release_count(), 1, "the old output was given back");
    let (id, second) = {
        let switched = f.switched.lock().unwrap();
        assert_eq!(switched.len(), 1);
        switched[0].clone()
    };
    assert_eq!(id, "shared:speaker");
    wait_until_open(&second).await;
    second.handles()[0].set_blocking(false);
    f.finish().await;
}

async fn wait_until_open(factory: &FakeSinkFactory) {
    for _ in 0..200 {
        if !factory.handles().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the new output was never opened");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_output_id_is_a_bad_request_and_changes_nothing() {
    let f = fixture("badoutput", false).await;
    let client = f.client().await;
    for bad in ["", "hw:1,0", "cloud:x"] {
        let error = client
            .request(Request::SetOutput { output: bad.into() })
            .await
            .unwrap_err();
        assert_eq!(protocol_code(error), ErrorCode::BadRequest, "{bad:?}");
    }
    assert_eq!(
        client.status().await.unwrap().route.unwrap().id,
        "exclusive:hw:fake,0"
    );
    assert!(f.switched.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribers_hear_when_the_outputs_change() {
    let f = fixture("outputs-changed", false).await;
    let client = f.client().await;
    let (_, mut events) = client.subscribe().await.unwrap();
    f.daemon.outputs_changed();
    let (_, event) = next_event(&mut events).await;
    assert_eq!(event, Event::OutputsChanged);
    f.finish().await;
}

// ---- gapless -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracks_joined_with_no_gap_say_so_and_change_no_state() {
    let options = phonia_core::engine::Options {
        position_interval: Duration::ZERO,
        ..Default::default()
    };
    let f = fixture_with("gapless", true, options).await;
    let client = f.client().await;
    assert!(
        client
            .server()
            .capabilities
            .iter()
            .any(|capability| capability == CAP_GAPLESS)
    );
    let (a, b) = (f.wav("a.wav", 200_000), f.wav("b.wav", 20_000));
    let (ids, _, _) = added(client.request(add(&[&a, &b], AddAt::End)).await.unwrap());
    let (_, mut events) = client.subscribe().await.unwrap();

    client
        .request(Request::Play { item: Some(ids[0]) })
        .await
        .unwrap();
    let first = events_until(&mut events, |e| matches!(e, Event::TrackStarted { .. })).await;
    assert!(matches!(
        first.last(),
        Some((_, Event::TrackStarted { gapless: false, .. }))
    ));

    // b is opened ahead while a plays; then the DAC runs freely.
    tokio::time::sleep(Duration::from_millis(300)).await;
    f.sinks.handles()[0].set_blocking(false);
    let joined = events_until(&mut events, |e| matches!(e, Event::TrackStarted { .. })).await;
    assert!(
        matches!(joined.last(), Some((_, Event::TrackStarted { item_id, gapless: true, .. })) if *item_id == Some(ids[1])),
        "{joined:?}"
    );
    // From the end of a to the start of b (the state event before it is a's own start).
    let boundary: Vec<&Event> = joined
        .iter()
        .map(|(_, event)| event)
        .skip_while(|event| !matches!(event, Event::TrackEnded { .. }))
        .filter(|event| !matches!(event, Event::Position { .. } | Event::QueueChanged { .. }))
        .collect();
    assert!(
        matches!(
            boundary.as_slice(),
            [Event::TrackEnded { .. }, Event::TrackStarted { .. }]
        ),
        "playback never stopped between the tracks: {boundary:?}"
    );
    f.finish().await;
}

// ---- volume --------------------------------------------------------------------------------

/// Switches the daemon to `id` (idle: nothing is playing) and returns the factory built for it.
async fn switch_to(f: &Fixture, client: &Client, id: &str) -> Arc<FakeSinkFactory> {
    assert_eq!(
        client
            .request(Request::SetOutput { output: id.into() })
            .await
            .unwrap(),
        Payload::Ack
    );
    f.switched
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(built, _)| built == id)
        .unwrap()
        .1
        .clone()
}

async fn volume_of(client: &Client) -> Option<Volume> {
    client.status().await.unwrap().volume
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exclusive_output_has_no_volume_and_says_why() {
    let f = fixture("volume-exclusive", false).await;
    let client = f.client().await;
    assert!(
        client
            .server()
            .capabilities
            .iter()
            .any(|capability| capability == CAP_VOLUME)
    );
    assert_eq!(volume_of(&client).await, None);
    for request in [
        Request::SetVolume { percent: 50 },
        Request::SetMute { mute: true },
    ] {
        let error = client.request(request).await.unwrap_err();
        assert_eq!(protocol_code(error), ErrorCode::Unsupported);
    }
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_volume_of_a_shared_output_is_set_reported_and_announced() {
    let f = fixture("volume-shared", false).await;
    let client = f.client().await;
    let shared = switch_to(&f, &client, "shared:speaker").await;
    let control = shared.fake_volume().unwrap();
    assert_eq!(
        volume_of(&client).await,
        Some(Volume {
            percent: 100,
            muted: false
        })
    );

    let (_, mut events) = client.subscribe().await.unwrap();
    assert_eq!(
        client
            .request(Request::SetVolume { percent: 40 })
            .await
            .unwrap(),
        Payload::Ack
    );
    let (_, event) = next_event(&mut events).await;
    assert_eq!(
        event,
        Event::VolumeChanged {
            percent: 40,
            muted: false
        }
    );
    assert_eq!(
        control.get(),
        phonia_core::output::Volume {
            percent: 40,
            muted: false
        }
    );
    assert_eq!(
        volume_of(&client).await,
        Some(Volume {
            percent: 40,
            muted: false
        })
    );

    client
        .request(Request::SetMute { mute: true })
        .await
        .unwrap();
    let (_, event) = next_event(&mut events).await;
    assert_eq!(
        event,
        Event::VolumeChanged {
            percent: 40,
            muted: true
        },
        "muting keeps the level"
    );

    client
        .request(Request::SetVolume { percent: 250 })
        .await
        .unwrap();
    assert_eq!(
        volume_of(&client).await.unwrap().percent,
        100,
        "never above unity gain"
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_volume_carries_over_to_the_next_output_and_is_told_to_clients() {
    let f = fixture("volume-carry", false).await;
    let client = f.client().await;
    switch_to(&f, &client, "shared:speaker").await;
    client
        .request(Request::SetVolume { percent: 35 })
        .await
        .unwrap();
    client
        .request(Request::SetMute { mute: true })
        .await
        .unwrap();

    let (_, mut events) = client.subscribe().await.unwrap();
    let other = switch_to(&f, &client, "shared:headphones").await;
    assert_eq!(
        other.fake_volume().unwrap().get(),
        phonia_core::output::Volume {
            percent: 35,
            muted: true
        }
    );
    let seen = events_until(&mut events, |event| {
        matches!(event, Event::VolumeChanged { .. })
    })
    .await;
    assert!(
        matches!(
            seen.last(),
            Some((
                _,
                Event::VolumeChanged {
                    percent: 35,
                    muted: true
                }
            ))
        ),
        "{seen:?}"
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_change_from_the_desktops_mixer_is_announced_and_remembered() {
    let f = fixture("volume-mixer", false).await;
    let client = f.client().await;
    let shared = switch_to(&f, &client, "shared:speaker").await;
    let (_, mut events) = client.subscribe().await.unwrap();

    shared
        .fake_volume()
        .unwrap()
        .change_from_outside(phonia_core::output::Volume {
            percent: 60,
            muted: false,
        });
    let (_, event) = next_event(&mut events).await;
    assert_eq!(
        event,
        Event::VolumeChanged {
            percent: 60,
            muted: false
        }
    );
    assert_eq!(
        volume_of(&client).await,
        Some(Volume {
            percent: 60,
            muted: false
        })
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sound_server_that_does_not_answer_is_an_internal_error_and_nothing_changes() {
    let f = fixture("volume-fails", false).await;
    let client = f.client().await;
    let shared = switch_to(&f, &client, "shared:speaker").await;
    shared.fake_volume().unwrap().fail_sets();
    let error = client
        .request(Request::SetVolume { percent: 10 })
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::Internal);
    assert_eq!(
        volume_of(&client).await,
        Some(Volume {
            percent: 100,
            muted: false
        })
    );
    f.finish().await;
}

// ---- playback control ----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn releasing_hands_the_device_back_and_resuming_takes_it_again() {
    let f = fixture("release", true).await;
    let client = f.client().await;
    let a = f.wav("a.wav", 200_000);
    let (ids, _, _) = added(client.request(add(&[&a], AddAt::End)).await.unwrap());
    let (_, mut events) = client.subscribe().await.unwrap();

    client
        .request(Request::Play { item: Some(ids[0]) })
        .await
        .unwrap();
    events_until(&mut events, |event| {
        matches!(event, Event::TrackStarted { .. })
    })
    .await;
    assert_eq!(client.status().await.unwrap().output, Output::Open);

    assert_eq!(
        client.request(Request::Release).await.unwrap(),
        Payload::Ack
    );
    f.sinks.handles()[0].advance(1024); // lets the blocked write return so the engine sees it
    let seen = events_until(&mut events, |event| {
        matches!(event, Event::OutputReleased { .. })
    })
    .await;
    assert!(matches!(
        seen.last(),
        Some((
            _,
            Event::OutputReleased {
                by: None,
                reason: ReleaseReason::Command
            }
        ))
    ));
    let status = client.status().await.unwrap();
    assert_eq!(
        (status.state, status.output),
        (State::Paused, Output::Released { by: None })
    );
    assert_eq!(f.sinks.release_count(), 1);

    client.request(Request::Resume).await.unwrap();
    events_until(&mut events, |event| matches!(event, Event::OutputAcquired)).await;
    assert_eq!(client.status().await.unwrap().output, Output::Open);
    f.sinks.handles()[1].set_blocking(false);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seek_with_nothing_playing_is_answered_and_then_reported_as_rejected() {
    let f = fixture("seek", false).await;
    let client = f.client().await;
    let (_, mut events) = client.subscribe().await.unwrap();

    let ack = client
        .request(Request::Seek {
            target: SeekTarget::Absolute { ms: 5_000 },
        })
        .await
        .unwrap();
    assert_eq!(
        ack,
        Payload::Ack,
        "the request was accepted; what the engine makes of it comes as an event"
    );
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

    client
        .request(Request::Play { item: Some(ids[0]) })
        .await
        .unwrap();
    events_until(&mut events, |event| {
        matches!(event, Event::TrackStarted { .. })
    })
    .await;
    assert_eq!(
        client.status().await.unwrap().track.unwrap().item_id,
        Some(ids[0])
    );

    assert_eq!(
        client
            .request(Request::QueueRemove { ids: vec![ids[0]] })
            .await
            .unwrap(),
        Payload::Removed { count: 1 }
    );
    f.sinks.handles()[0].advance(1024); // lets the blocked write return so the engine sees the skip
    let seen = events_until(&mut events, |event| {
        matches!(event, Event::TrackStarted { .. })
    })
    .await;
    let events: Vec<&Event> = seen.iter().map(|(_, event)| event).collect();
    assert!(events.iter().any(|e| matches!(e, Event::TrackEnded { item_id, reason: EndReason::Interrupted } if *item_id == Some(ids[0]))));
    assert!(
        matches!(events.last(), Some(Event::TrackStarted { item_id, .. }) if *item_id == Some(ids[1]))
    );
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

    assert_eq!(
        asker.request(Request::Shutdown).await.unwrap(),
        Payload::Ack
    );
    let seen = events_until(&mut events, |event| matches!(event, Event::ShuttingDown)).await;
    assert!(matches!(seen.last(), Some((_, Event::ShuttingDown))));
    tokio::time::timeout(TIMEOUT, phoniad::daemon::wait_for_shutdown(&mut stopping))
        .await
        .expect("the daemon must signal that it is stopping");

    tokio::time::timeout(TIMEOUT, asker.closed())
        .await
        .expect("connections are closed on shutdown");
    tokio::time::timeout(TIMEOUT, watcher.closed())
        .await
        .unwrap();
    assert!(matches!(
        asker.request(Request::Status).await,
        Err(ClientError::Closed | ClientError::Io(_))
    ));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_after_the_connection_drops_fails_instead_of_hanging() {
    let f = fixture("drop", false).await;
    let client = f.client().await;
    f.daemon.request_shutdown();
    tokio::time::timeout(TIMEOUT, client.closed())
        .await
        .unwrap();
    let error = tokio::time::timeout(TIMEOUT, client.request(Request::Status))
        .await
        .expect("must not hang")
        .unwrap_err();
    assert!(
        matches!(error, ClientError::Closed | ClientError::Io(_)),
        "{error}"
    );
    f.finish().await;
}

async fn range_of(client: &Client) -> Option<QualityRange> {
    client.status().await.unwrap().quality_range
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_best_quality_is_changed_announced_and_never_set_below_the_floor() {
    let f = fixture("max-quality", false).await;
    let client = f.client().await;
    assert!(
        client
            .server()
            .capabilities
            .iter()
            .any(|capability| capability == CAP_QUALITY)
    );
    assert_eq!(
        range_of(&client).await,
        Some(QualityRange {
            max: Quality::Hires,
            min: Quality::Lossless
        })
    );

    let (_, mut events) = client.subscribe().await.unwrap();
    assert_eq!(
        client
            .request(Request::SetMaxQuality {
                quality: Quality::Lossless
            })
            .await
            .unwrap(),
        Payload::Ack
    );
    let (_, event) = next_event(&mut events).await;
    assert_eq!(
        event,
        Event::MaxQualityChanged {
            quality: Quality::Lossless
        }
    );
    assert_eq!(range_of(&client).await.unwrap().max, Quality::Lossless);

    let error = client
        .request(Request::SetMaxQuality {
            quality: Quality::High,
        })
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::BadRequest);
    assert_eq!(
        range_of(&client).await.unwrap().max,
        Quality::Lossless,
        "a refused change changes nothing"
    );
    f.finish().await;
}

// --- Searching the catalog (protocol 1.6) ---------------------------------------------------------

fn a_track(id: &str, title: &str) -> catalog::Track {
    catalog::Track {
        id: id.into(),
        title: title.into(),
        version: None,
        artists: vec![catalog::ArtistRef {
            id: "780".into(),
            name: "Korn".into(),
        }],
        album: Some(catalog::AlbumRef {
            id: "9".into(),
            title: "Untouchables".into(),
            cover: None,
        }),
        duration: Some(Duration::from_secs(271)),
        explicit: false,
        track_number: Some(2),
        volume_number: None,
        quality: Some(phonia_core::config::Quality::Hires),
        streamable: true,
    }
}

fn a_catalog() -> FakeCatalog {
    FakeCatalog::new().with_search(catalog::SearchResults {
        tracks: Some(catalog::Page {
            items: vec![a_track("33723914", "Here to Stay")],
            total: 123,
            offset: 0,
        }),
        albums: Some(catalog::Page {
            items: vec![catalog::Album {
                id: "9".into(),
                title: "Untouchables".into(),
                version: None,
                artists: vec![],
                release_date: Some("2002-06-11".into()),
                track_count: Some(14),
                duration: None,
                explicit: false,
                quality: None,
                kind: None,
                copyright: None,
                cover: None,
            }],
            total: 1,
            offset: 0,
        }),
        artists: Some(catalog::Page::empty()),
        playlists: Some(catalog::Page::empty()),
    })
}

fn search(query: &str, kinds: Vec<CatalogKind>, limit: Option<u32>) -> Request {
    Request::Search {
        query: query.into(),
        kinds,
        offset: 0,
        limit,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_daemon_with_a_catalog_says_so_and_searches() {
    let f = fixture_with_catalog("no-catalog", None).await;
    let client = f.client().await;
    assert!(
        !client
            .server()
            .capabilities
            .iter()
            .any(|c| c == CAP_CATALOG)
    );
    let error = client
        .request(search("korn", vec![], None))
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::Unsupported);
    f.finish().await;

    let f = fixture_with_catalog("with-catalog", Some(Arc::new(a_catalog()))).await;
    let client = f.client().await;
    assert!(
        client
            .server()
            .capabilities
            .iter()
            .any(|c| c == CAP_CATALOG)
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_search_answers_with_a_page_of_each_kind_asked_for() {
    let catalog = a_catalog();
    let f = fixture_with_catalog("search", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;

    let Payload::SearchResults {
        query,
        tracks,
        albums,
        artists,
        playlists,
    } = client
        .request(search("  korn  ", vec![], None))
        .await
        .unwrap()
    else {
        panic!("not search results");
    };
    assert_eq!(query, "korn", "the query is trimmed");
    let tracks = tracks.unwrap();
    assert_eq!((tracks.total, tracks.offset), (123, 0));
    let track = &tracks.items[0];
    assert_eq!(track.id, "33723914");
    assert_eq!(track.duration_ms, Some(271_000));
    assert_eq!(track.quality, Some(Quality::Hires));
    assert_eq!(track.album.as_ref().unwrap().title, "Untouchables");
    assert_eq!(
        albums.unwrap().items[0].release_date.as_deref(),
        Some("2002-06-11")
    );
    assert!(artists.is_some() && playlists.is_some());

    // What the catalog was asked: every kind, from the start, the daemon's page size.
    assert_eq!(
        catalog.calls(),
        [phonia_core::catalog::fake::Call::Search {
            query: "korn".into(),
            kinds: vec![],
            offset: 0,
            limit: 50
        }]
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_search_for_some_kinds_asks_for_only_those_and_passes_the_paging_on() {
    let catalog = a_catalog();
    let f = fixture_with_catalog("search-kinds", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let Payload::SearchResults { tracks, albums, .. } = client
        .request(Request::Search {
            query: "korn".into(),
            kinds: vec![
                CatalogKind::Tracks,
                CatalogKind::Unknown,
                CatalogKind::Tracks,
            ],
            offset: 100,
            limit: Some(25),
        })
        .await
        .unwrap()
    else {
        panic!("not search results");
    };
    assert!(
        tracks.is_some() && albums.is_none(),
        "only tracks were asked for"
    );
    assert_eq!(
        catalog.calls(),
        [phonia_core::catalog::fake::Call::Search {
            query: "korn".into(),
            kinds: vec![catalog::Kind::Tracks],
            offset: 100,
            limit: 25
        }],
        "unknown kinds are dropped and repeats collapse"
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_search_that_makes_no_sense_is_refused_before_it_reaches_tidal() {
    let catalog = a_catalog();
    let f = fixture_with_catalog("search-invalid", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    for request in [
        search("", vec![], None),
        search("   ", vec![], None),
        search("korn", vec![], Some(0)),
        search("korn", vec![], Some(301)),
        search("korn", vec![CatalogKind::Unknown], None),
    ] {
        let error = client.request(request.clone()).await.unwrap_err();
        assert_eq!(protocol_code(error), ErrorCode::BadRequest, "{request:?}");
    }
    assert!(catalog.calls().is_empty(), "none of them went to TIDAL");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalog_failures_become_their_own_error_codes() {
    for (failure, code) in [
        (
            CatalogError::NotLoggedIn("no session".into()),
            ErrorCode::NotLoggedIn,
        ),
        (
            CatalogError::Unavailable("timed out".into()),
            ErrorCode::Unavailable,
        ),
        (CatalogError::RateLimited, ErrorCode::RateLimited),
        (CatalogError::NotFound, ErrorCode::NotFound),
        (CatalogError::Invalid("bad".into()), ErrorCode::BadRequest),
    ] {
        let catalog = FakeCatalog::new().failing(failure.clone());
        let f = fixture_with_catalog("search-fails", Some(Arc::new(catalog))).await;
        let client = f.client().await;
        let error = client
            .request(search("korn", vec![], None))
            .await
            .unwrap_err();
        let ClientError::Protocol(error) = error else {
            panic!("not a protocol error");
        };
        assert_eq!(error.code, code, "{failure:?}");
        assert_eq!(error.message, failure.to_string());
        f.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_search_does_not_hold_up_the_requests_behind_it() {
    let catalog = a_catalog().delayed(Duration::from_millis(700));
    let f = fixture_with_catalog("search-slow", Some(Arc::new(catalog))).await;
    let mut raw = f.raw().await;
    raw.hello().await;

    raw.send(r#"{"id":2,"request":{"type":"search","query":"korn"}}"#)
        .await;
    raw.send(r#"{"id":3,"request":{"type":"status"}}"#).await;
    // The status was asked for second and comes first: the search is still under way.
    let started = std::time::Instant::now();
    let Some(ServerMessage::Response { id, reply }) = raw.recv().await else {
        panic!("no response");
    };
    assert_eq!(id, RequestId(3));
    assert!(matches!(reply, Reply::Ok(Payload::Status(_))));
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "it did not wait for the search"
    );

    let Some(ServerMessage::Response { id, reply }) = raw.recv().await else {
        panic!("no response");
    };
    assert_eq!(id, RequestId(2));
    assert!(matches!(reply, Reply::Ok(Payload::SearchResults { .. })));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn too_many_searches_at_once_on_a_connection_are_refused_at_once() {
    let catalog = a_catalog().delayed(Duration::from_millis(600));
    let f = fixture_with_catalog("search-many", Some(Arc::new(catalog))).await;
    let mut raw = f.raw().await;
    raw.hello().await;
    for id in 2..=6 {
        raw.send(&format!(
            r#"{{"id":{id},"request":{{"type":"search","query":"korn"}}}}"#
        ))
        .await;
    }
    // Four are under way; the fifth is turned away without waiting for them.
    let (id, code) = code_of(raw.recv().await);
    assert_eq!((id, code), (RequestId(6), ErrorCode::RateLimited));
    let mut answered = 0;
    for _ in 0..4 {
        if let Some(ServerMessage::Response {
            reply: Reply::Ok(Payload::SearchResults { .. }),
            ..
        }) = raw.recv().await
        {
            answered += 1;
        }
    }
    assert_eq!(answered, 4);
    f.finish().await;
}

// --- Adding an album or a playlist (protocol 1.6) --------------------------------------------------

fn tracks(count: u32) -> Vec<catalog::Track> {
    (1..=count)
        .map(|n| a_track(&(1000 + n).to_string(), &format!("Song {n}")))
        .collect()
}

fn add_from(from: CatalogRef, at: AddAt) -> Request {
    Request::QueueAddFrom { from, at }
}

fn album(id: &str) -> CatalogRef {
    CatalogRef::Album { id: id.into() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_album_is_added_with_the_titles_and_lengths_that_came_with_its_listing() {
    let catalog = FakeCatalog::new().with_album("9", tracks(3));
    let f = fixture_with_catalog("add-album", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;

    let Payload::Added {
        ids,
        rejected,
        unresolved,
    } = client
        .request(add_from(album("9"), AddAt::End))
        .await
        .unwrap()
    else {
        panic!("not an added answer");
    };
    assert_eq!(ids.len(), 3);
    assert!(rejected.is_empty() && unresolved.is_empty());

    let queue = client.queue().await.unwrap();
    assert_eq!(
        queue
            .items
            .iter()
            .map(|item| item.source.as_str())
            .collect::<Vec<_>>(),
        ["tidal:1001", "tidal:1002", "tidal:1003"]
    );
    assert_eq!(queue.items[0].title.as_deref(), Some("Song 1"));
    assert_eq!(queue.items[0].artist.as_deref(), Some("Korn"));
    assert_eq!(queue.items[0].duration_ms, Some(271_000));
    // One listing, and nothing asked about track by track (there is no TIDAL to ask here, which
    // would have left the titles out).
    assert_eq!(catalog.calls().len(), 1);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_playlist_goes_where_at_says() {
    let catalog = FakeCatalog::new().with_playlist("p-1", tracks(2));
    let f = fixture_with_catalog("add-playlist", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    client
        .request(Request::QueueAdd {
            tracks: vec![
                NewTrack {
                    source: f.wav("a.wav", 10),
                },
                NewTrack {
                    source: f.wav("b.wav", 10),
                },
            ],
            at: AddAt::End,
        })
        .await
        .unwrap();

    client
        .request(add_from(
            CatalogRef::Playlist { id: "p-1".into() },
            AddAt::Index { index: 1 },
        ))
        .await
        .unwrap();
    let sources: Vec<String> = client
        .queue()
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.source)
        .collect();
    assert_eq!(sources.len(), 4);
    assert!(sources[0].ends_with("a.wav") && sources[3].ends_with("b.wav"));
    assert_eq!(
        (sources[1].as_str(), sources[2].as_str()),
        ("tidal:1001", "tidal:1002")
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_album_is_listed_page_by_page() {
    let catalog = FakeCatalog::new().with_album("big", tracks(250));
    let f = fixture_with_catalog("add-long", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let Payload::Added { ids, .. } = client
        .request(add_from(album("big"), AddAt::End))
        .await
        .unwrap()
    else {
        panic!("not an added answer");
    };
    assert_eq!(ids.len(), 250);
    let calls = catalog.calls();
    let offsets: Vec<u32> = calls
        .iter()
        .map(|call| match call {
            phonia_core::catalog::fake::Call::AlbumTracks { offset, limit, .. } => {
                assert_eq!(*limit, 100);
                *offset
            }
            other => panic!("unexpected call {other:?}"),
        })
        .collect();
    assert_eq!(offsets, [0, 100, 200]);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_album_with_too_many_tracks_is_refused_after_one_listing() {
    let catalog = FakeCatalog::new().with_playlist("huge", tracks(1200));
    let f = fixture_with_catalog("add-huge", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let error = client
        .request(add_from(
            CatalogRef::Playlist { id: "huge".into() },
            AddAt::End,
        ))
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::BadRequest);
    assert_eq!(catalog.calls().len(), 1, "the rest was not even listed");
    assert!(client.queue().await.unwrap().items.is_empty());
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_track_tidal_lists_but_does_not_stream_is_refused_and_the_rest_is_added() {
    let mut all = tracks(3);
    all[1].streamable = false;
    let catalog = FakeCatalog::new().with_album("9", all);
    let f = fixture_with_catalog("add-unstreamable", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let Payload::Added { ids, rejected, .. } = client
        .request(add_from(album("9"), AddAt::End))
        .await
        .unwrap()
    else {
        panic!("not an added answer");
    };
    assert_eq!(ids.len(), 2);
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].source, "tidal:1002");
    assert!(
        rejected[0].reason.contains("Song 2"),
        "{}",
        rejected[0].reason
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_album_adds_nothing_and_says_so() {
    let catalog = FakeCatalog::new().with_album("empty", vec![]);
    let f = fixture_with_catalog("add-empty", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let Payload::Added { ids, .. } = client
        .request(add_from(album("empty"), AddAt::End))
        .await
        .unwrap()
    else {
        panic!("not an added answer");
    };
    assert!(ids.is_empty());
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adding_from_the_catalog_fails_with_the_catalogs_own_reasons() {
    // An id TIDAL does not have.
    let f = fixture_with_catalog("add-missing", Some(Arc::new(FakeCatalog::new()))).await;
    let client = f.client().await;
    let error = client
        .request(add_from(album("nope"), AddAt::End))
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::NotFound);
    // Something this version cannot add.
    let error = client
        .request(add_from(CatalogRef::Unknown, AddAt::End))
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::BadRequest);
    f.finish().await;

    // No login.
    let catalog = FakeCatalog::new().failing(CatalogError::NotLoggedIn("no session".into()));
    let f = fixture_with_catalog("add-nologin", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let error = client
        .request(add_from(album("9"), AddAt::End))
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::NotLoggedIn);
    f.finish().await;

    // No catalog at all.
    let f = fixture_with_catalog("add-nocatalog", None).await;
    let client = f.client().await;
    let error = client
        .request(add_from(album("9"), AddAt::End))
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::Unsupported);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listing_an_album_does_not_hold_up_the_requests_behind_it() {
    let catalog = FakeCatalog::new()
        .with_album("9", tracks(2))
        .delayed(Duration::from_millis(700));
    let f = fixture_with_catalog("add-slow", Some(Arc::new(catalog))).await;
    let mut raw = f.raw().await;
    raw.hello().await;
    raw.send(r#"{"id":2,"request":{"type":"queue_add_from","from":{"type":"album","id":"9"}}}"#)
        .await;
    raw.send(r#"{"id":3,"request":{"type":"status"}}"#).await;
    let Some(ServerMessage::Response { id, .. }) = raw.recv().await else {
        panic!("no response");
    };
    assert_eq!(id, RequestId(3), "the status comes first");
    let Some(ServerMessage::Response { id, reply }) = raw.recv().await else {
        panic!("no response");
    };
    assert_eq!(id, RequestId(2));
    assert!(matches!(reply, Reply::Ok(Payload::Added { .. })));
    f.finish().await;
}

// --- Albums and artists (protocol 1.6) -------------------------------------------------------------

fn korn() -> catalog::Artist {
    catalog::Artist {
        id: "780".into(),
        name: "Korn".into(),
        picture: None,
    }
}

fn album_named(id: &str, title: &str, kind: catalog::AlbumKind) -> catalog::Album {
    catalog::Album {
        id: id.into(),
        title: title.into(),
        version: None,
        artists: vec![catalog::ArtistRef {
            id: "780".into(),
            name: "Korn".into(),
        }],
        release_date: Some("1999-11-16".into()),
        track_count: Some(16),
        duration: Some(Duration::from_secs(3200)),
        explicit: true,
        quality: Some(phonia_core::config::Quality::Hires),
        kind: Some(kind),
        copyright: Some("(P) 1999".into()),
        cover: None,
    }
}

/// An album of three tracks and an artist with a bio, 120 top tracks, two albums and a single.
fn browsable() -> FakeCatalog {
    FakeCatalog::new()
        .with_album_details(album_named("9", "Issues", catalog::AlbumKind::Album))
        .with_album("9", tracks(3))
        .with_artist(
            korn(),
            Some("A nu metal band."),
            tracks(120),
            vec![
                album_named("9", "Issues", catalog::AlbumKind::Album),
                album_named("10", "Untouchables", catalog::AlbumKind::Album),
            ],
            vec![album_named("11", "Freak", catalog::AlbumKind::Single)],
        )
}

fn ask_album(id: &str, limit: Option<u32>) -> Request {
    Request::Album {
        id: id.into(),
        limit,
    }
}

fn ask_artist(id: &str, limit: Option<u32>) -> Request {
    Request::Artist {
        id: id.into(),
        limit,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_album_comes_with_its_details_and_the_first_page_of_its_tracks() {
    let catalog = browsable();
    let f = fixture_with_catalog("album", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let Payload::Album { album, tracks } = client.request(ask_album("9", None)).await.unwrap()
    else {
        panic!("not an album");
    };
    assert_eq!(album.title, "Issues");
    assert_eq!(album.kind, Some(AlbumKind::Album));
    assert_eq!(album.copyright.as_deref(), Some("(P) 1999"));
    assert_eq!((tracks.items.len(), tracks.total), (3, 3));
    assert_eq!(tracks.items[0].id, "1001");

    // Both were asked for, the tracks with the biggest page.
    let calls = catalog.calls();
    assert!(calls.contains(&phonia_core::catalog::fake::Call::Album { id: "9".into() }));
    assert!(
        calls.contains(&phonia_core::catalog::fake::Call::AlbumTracks {
            id: "9".into(),
            offset: 0,
            limit: 100
        })
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_artist_comes_whole_in_one_answer_with_a_page_of_each_list() {
    let catalog = browsable();
    let f = fixture_with_catalog("artist", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let Payload::Artist {
        artist,
        bio,
        top_tracks,
        albums,
        singles,
    } = client.request(ask_artist("780", None)).await.unwrap()
    else {
        panic!("not an artist");
    };
    assert_eq!(artist.name, "Korn");
    assert_eq!(bio.as_deref(), Some("A nu metal band."));
    assert_eq!(
        (top_tracks.items.len(), top_tracks.total),
        (50, 120),
        "the default page is 50"
    );
    assert_eq!(albums.items.len(), 2);
    assert_eq!(singles.items[0].kind, Some(AlbumKind::Single));
    assert_eq!(catalog.calls().len(), 5, "five asks, one answer");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_artist_with_no_bio_is_still_shown() {
    let catalog = FakeCatalog::new().with_artist(korn(), None, tracks(2), vec![], vec![]);
    let f = fixture_with_catalog("artist-nobio", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let Payload::Artist {
        bio, top_tracks, ..
    } = client.request(ask_artist("780", Some(10))).await.unwrap()
    else {
        panic!("not an artist");
    };
    assert_eq!(bio, None);
    assert_eq!(top_tracks.items.len(), 2);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_of_tracks_is_asked_for_by_the_list_it_is_of() {
    let catalog = browsable().with_playlist("p-1", tracks(4));
    let f = fixture_with_catalog("tracks", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let ask = |from: CatalogRef, offset: u32, limit: Option<u32>| Request::Tracks {
        from,
        offset,
        limit,
    };

    let Payload::Tracks { from, page } = client
        .request(ask(
            CatalogRef::ArtistTopTracks { id: "780".into() },
            100,
            None,
        ))
        .await
        .unwrap()
    else {
        panic!("not tracks");
    };
    assert_eq!(from, CatalogRef::ArtistTopTracks { id: "780".into() });
    assert_eq!((page.items.len(), page.total, page.offset), (20, 120, 100));

    let Payload::Tracks { page, .. } = client
        .request(ask(CatalogRef::Album { id: "9".into() }, 1, Some(1)))
        .await
        .unwrap()
    else {
        panic!("not tracks");
    };
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, "1002");

    let Payload::Tracks { page, .. } = client
        .request(ask(CatalogRef::Playlist { id: "p-1".into() }, 0, None))
        .await
        .unwrap()
    else {
        panic!("not tracks");
    };
    assert_eq!(page.total, 4);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_of_albums_is_the_albums_or_the_singles_of_an_artist() {
    let catalog = browsable();
    let f = fixture_with_catalog("albums", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let ask = |from: AlbumListRef| Request::Albums {
        from,
        offset: 0,
        limit: None,
    };
    let Payload::Albums { page, .. } = client
        .request(ask(AlbumListRef::ArtistAlbums { id: "780".into() }))
        .await
        .unwrap()
    else {
        panic!("not albums");
    };
    assert_eq!(page.total, 2);
    let Payload::Albums { from, page } = client
        .request(ask(AlbumListRef::ArtistSingles { id: "780".into() }))
        .await
        .unwrap()
    else {
        panic!("not albums");
    };
    assert_eq!(from, AlbumListRef::ArtistSingles { id: "780".into() });
    assert_eq!(page.items[0].title, "Freak");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_that_make_no_sense_are_refused_before_they_reach_tidal() {
    let catalog = browsable();
    let f = fixture_with_catalog("browse-invalid", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    for request in [
        ask_album("9", Some(0)),
        ask_album("9", Some(101)),
        ask_artist("780", Some(0)),
        ask_artist("780", Some(500)),
        Request::Tracks {
            from: CatalogRef::Unknown,
            offset: 0,
            limit: None,
        },
        Request::Tracks {
            from: CatalogRef::Album { id: "9".into() },
            offset: 0,
            limit: Some(101),
        },
        Request::Albums {
            from: AlbumListRef::Unknown,
            offset: 0,
            limit: None,
        },
        Request::Artists {
            from: ArtistListRef::Unknown,
            offset: 0,
            limit: None,
        },
        Request::Playlists {
            from: PlaylistListRef::Unknown,
            offset: 0,
            limit: None,
        },
        Request::Library { limit: Some(0) },
        Request::Library { limit: Some(500) },
    ] {
        let error = client.request(request.clone()).await.unwrap_err();
        assert_eq!(protocol_code(error), ErrorCode::BadRequest, "{request:?}");
    }
    assert!(catalog.calls().is_empty(), "none of them went to TIDAL");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_album_or_artist_is_not_found_and_other_failures_keep_their_codes() {
    let f = fixture_with_catalog("browse-missing", Some(Arc::new(FakeCatalog::new()))).await;
    let client = f.client().await;
    for request in [ask_album("nope", None), ask_artist("nope", None)] {
        let error = client.request(request).await.unwrap_err();
        assert_eq!(protocol_code(error), ErrorCode::NotFound);
    }
    f.finish().await;

    let catalog = FakeCatalog::new().failing(CatalogError::RateLimited);
    let f = fixture_with_catalog("browse-limited", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let error = client.request(ask_artist("780", None)).await.unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::RateLimited);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_catalog_none_of_them_can_be_answered() {
    let f = fixture_with_catalog("browse-nocatalog", None).await;
    let client = f.client().await;
    for request in [
        ask_album("9", None),
        ask_artist("780", None),
        Request::Tracks {
            from: CatalogRef::Album { id: "9".into() },
            offset: 0,
            limit: None,
        },
        Request::Albums {
            from: AlbumListRef::ArtistAlbums { id: "780".into() },
            offset: 0,
            limit: None,
        },
        Request::Artists {
            from: ArtistListRef::FavoriteArtists,
            offset: 0,
            limit: None,
        },
        Request::Playlists {
            from: PlaylistListRef::Mine,
            offset: 0,
            limit: None,
        },
        Request::Library { limit: None },
        Request::Lyrics { id: "9".into() },
        Request::PlaylistFolder {
            folder: None,
            offset: 0,
            limit: None,
        },
    ] {
        let error = client.request(request).await.unwrap_err();
        assert_eq!(protocol_code(error), ErrorCode::Unsupported);
    }
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_artists_top_tracks_can_be_added_to_the_queue_whole() {
    let catalog = browsable();
    let f = fixture_with_catalog("add-top", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let Payload::Added { ids, .. } = client
        .request(add_from(
            CatalogRef::ArtistTopTracks { id: "780".into() },
            AddAt::End,
        ))
        .await
        .unwrap()
    else {
        panic!("not an added answer");
    };
    assert_eq!(ids.len(), 120);
    let offsets: Vec<u32> = catalog
        .calls()
        .iter()
        .filter_map(|call| match call {
            phonia_core::catalog::fake::Call::ArtistTopTracks { offset, .. } => Some(*offset),
            _ => None,
        })
        .collect();
    assert_eq!(offsets, [0, 100], "listed a hundred at a time");
    f.finish().await;
}

// --- A track's radio (protocol 1.12, #33) ------------------------------------------------------

/// A seed track followed by 3 others TIDAL's radio picked, in the same shape the real
/// `/tracks/{id}/radio` answers with the seed itself as the first item.
fn radio_with_seed(seed: &str) -> Vec<catalog::Track> {
    let mut items = vec![a_track(seed, "The seed itself")];
    items.extend(tracks(3));
    items
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_of_a_tracks_radio_never_includes_the_seed() {
    let catalog = browsable().with_radio("1", radio_with_seed("1"));
    let f = fixture_with_catalog("radio", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let Payload::Tracks { from, page } = client
        .request(Request::Tracks {
            from: CatalogRef::TrackRadio { id: "1".into() },
            offset: 0,
            limit: None,
        })
        .await
        .unwrap()
    else {
        panic!("not tracks");
    };
    assert_eq!(from, CatalogRef::TrackRadio { id: "1".into() });
    // The fake filters the seed the same way the real catalog does: of the 4 given, "1" itself
    // (the seed, given first) is left out.
    assert_eq!(page.items.len(), 3);
    assert!(page.items.iter().all(|track| track.id != "1"));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tracks_radio_can_be_added_to_the_queue_whole() {
    let catalog = browsable().with_radio("1", radio_with_seed("1"));
    let f = fixture_with_catalog("add-radio", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let Payload::Added { ids, .. } = client
        .request(add_from(
            CatalogRef::TrackRadio { id: "1".into() },
            AddAt::End,
        ))
        .await
        .unwrap()
    else {
        panic!("not an added answer");
    };
    // 4 given, minus the seed ("1") itself.
    assert_eq!(ids.len(), 3);
    f.finish().await;
}

// --- The library (protocol 1.6, #21) ---------------------------------------------------------

fn playlist_named(id: &str, title: &str) -> catalog::Playlist {
    catalog::Playlist {
        id: id.into(),
        title: title.into(),
        creator: Some("hectmor".into()),
        description: None,
        track_count: Some(10),
        duration: Some(Duration::from_secs(2400)),
        cover: None,
    }
}

/// Two favorite tracks, one favorite album, one favorite artist and two of the user's own
/// playlists.
fn with_a_library() -> FakeCatalog {
    browsable()
        .with_favorite_tracks(tracks(2))
        .with_favorite_albums(vec![album_named(
            "20",
            "Untouchables",
            catalog::AlbumKind::Album,
        )])
        .with_favorite_artists(vec![catalog::Artist {
            id: "780".into(),
            name: "Korn".into(),
            picture: None,
        }])
        .with_my_playlists(vec![
            playlist_named("p-mine-1", "Road trip"),
            playlist_named("p-mine-2", "Focus"),
        ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_library_comes_whole_in_one_answer_with_a_page_of_each_list() {
    let catalog = with_a_library();
    let f = fixture_with_catalog("library", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let Payload::Library {
        favorite_tracks,
        favorite_albums,
        my_playlists,
    } = client
        .request(Request::Library { limit: None })
        .await
        .unwrap()
    else {
        panic!("not a library");
    };
    assert_eq!(favorite_tracks.items.len(), 2);
    assert_eq!(favorite_albums.items[0].title, "Untouchables");
    assert_eq!(
        my_playlists
            .items
            .iter()
            .map(|p| p.title.as_str())
            .collect::<Vec<_>>(),
        ["Road trip", "Focus"]
    );
    assert_eq!(catalog.calls().len(), 3, "three asks, one answer");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn favorite_tracks_and_albums_are_paged_by_their_own_catalog_ref() {
    let catalog = with_a_library();
    let f = fixture_with_catalog("favorites-paged", Some(Arc::new(catalog))).await;
    let client = f.client().await;

    let Payload::Tracks { from, page } = client
        .request(Request::Tracks {
            from: CatalogRef::FavoriteTracks,
            offset: 1,
            limit: None,
        })
        .await
        .unwrap()
    else {
        panic!("not tracks");
    };
    assert_eq!(from, CatalogRef::FavoriteTracks);
    assert_eq!(page.items.len(), 1, "the second of the two favorites");

    let Payload::Albums { from, page } = client
        .request(Request::Albums {
            from: AlbumListRef::FavoriteAlbums,
            offset: 0,
            limit: None,
        })
        .await
        .unwrap()
    else {
        panic!("not albums");
    };
    assert_eq!(from, AlbumListRef::FavoriteAlbums);
    assert_eq!(page.items[0].title, "Untouchables");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_of_artists_is_the_users_favorites() {
    let catalog = with_a_library();
    let f = fixture_with_catalog("favorite-artists", Some(Arc::new(catalog))).await;
    let client = f.client().await;

    let Payload::Artists { from, page } = client
        .request(Request::Artists {
            from: ArtistListRef::FavoriteArtists,
            offset: 0,
            limit: None,
        })
        .await
        .unwrap()
    else {
        panic!("not artists");
    };
    assert_eq!(from, ArtistListRef::FavoriteArtists);
    assert_eq!(page.items[0].name, "Korn");
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_of_playlists_is_the_users_own() {
    let catalog = with_a_library();
    let f = fixture_with_catalog("playlists", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let Payload::Playlists { from, page } = client
        .request(Request::Playlists {
            from: PlaylistListRef::Mine,
            offset: 1,
            limit: None,
        })
        .await
        .unwrap()
    else {
        panic!("not playlists");
    };
    assert_eq!(from, PlaylistListRef::Mine);
    assert_eq!(page.items[0].title, "Focus");
    assert_eq!((page.total, page.offset), (2, 1));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asking_for_an_artist_does_not_hold_up_the_requests_behind_it() {
    let catalog = browsable().delayed(Duration::from_millis(700));
    let f = fixture_with_catalog("artist-slow", Some(Arc::new(catalog))).await;
    let mut raw = f.raw().await;
    raw.hello().await;
    raw.send(r#"{"id":2,"request":{"type":"artist","id":"780"}}"#)
        .await;
    raw.send(r#"{"id":3,"request":{"type":"status"}}"#).await;
    let Some(ServerMessage::Response { id, .. }) = raw.recv().await else {
        panic!("no response");
    };
    assert_eq!(id, RequestId(3), "the status comes first");
    let Some(ServerMessage::Response { id, reply }) = raw.recv().await else {
        panic!("no response");
    };
    assert_eq!(id, RequestId(2));
    assert!(matches!(reply, Reply::Ok(Payload::Artist { .. })));
    f.finish().await;
}

// --- Lyrics (protocol 1.10) -------------------------------------------------------------------

fn synced_lyrics() -> catalog::Lyrics {
    catalog::Lyrics {
        lines: vec![catalog::LyricLine {
            at: Duration::from_millis(12_440),
            text: "You get a shiver in the dark".into(),
        }],
        plain: Some("You get a shiver in the dark".into()),
        right_to_left: false,
        provider: Some("MUSIXMATCH".into()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lyrics_answer_the_id_asked_for_and_are_cached_after_the_first_ask() {
    let catalog = FakeCatalog::new().with_lyrics("1", synced_lyrics());
    let f = fixture_with_catalog("lyrics", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let Payload::Lyrics { id, lyrics } = client
        .request(Request::Lyrics { id: "1".into() })
        .await
        .unwrap()
    else {
        panic!("not a lyrics answer");
    };
    assert_eq!(id, "1");
    let lyrics = lyrics.expect("this track has lyrics");
    assert_eq!(lyrics.lines[0].text, "You get a shiver in the dark");
    assert_eq!(lyrics.lines[0].at_ms, 12_440);
    assert_eq!(lyrics.provider.as_deref(), Some("MUSIXMATCH"));
    assert_eq!(catalog.calls().len(), 1);

    // Asked again: answered from the cache, not from the catalog a second time.
    let Payload::Lyrics { lyrics, .. } = client
        .request(Request::Lyrics { id: "1".into() })
        .await
        .unwrap()
    else {
        panic!("not a lyrics answer");
    };
    assert!(lyrics.is_some());
    assert_eq!(
        catalog.calls().len(),
        1,
        "the second ask was answered from the cache"
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_instrumental_track_has_no_lyrics_and_that_is_not_cached_as_a_failure() {
    let catalog = FakeCatalog::new();
    let f = fixture_with_catalog("lyrics-none", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let Payload::Lyrics { lyrics, .. } = client
        .request(Request::Lyrics {
            id: "518338".into(),
        })
        .await
        .unwrap()
    else {
        panic!("not a lyrics answer");
    };
    assert_eq!(lyrics, None);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lyrics_failure_is_not_cached_and_can_be_retried() {
    let catalog = FakeCatalog::new().failing(CatalogError::RateLimited);
    let f = fixture_with_catalog("lyrics-limited", Some(Arc::new(catalog.clone()))).await;
    let client = f.client().await;
    let error = client
        .request(Request::Lyrics { id: "1".into() })
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::RateLimited);
    assert_eq!(catalog.calls().len(), 1);
    // Asked again: hits the catalog again, not a cached failure.
    let error = client
        .request(Request::Lyrics { id: "1".into() })
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::RateLimited);
    assert_eq!(catalog.calls().len(), 2);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_catalog_lyrics_cannot_be_answered() {
    let f = fixture_with_catalog("lyrics-nocatalog", None).await;
    let client = f.client().await;
    let error = client
        .request(Request::Lyrics { id: "1".into() })
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::Unsupported);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_with_a_catalog_announces_the_lyrics_capability() {
    let f = fixture_with_catalog("lyrics-hello", Some(Arc::new(FakeCatalog::new()))).await;
    let client = f.client().await;
    assert!(client.server().capabilities.iter().any(|c| c == CAP_LYRICS));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_with_a_catalog_announces_the_autoplay_capability_but_not_without_one() {
    let f = fixture_with_catalog("autoplay-hello", Some(Arc::new(FakeCatalog::new()))).await;
    let client = f.client().await;
    assert!(
        client
            .server()
            .capabilities
            .iter()
            .any(|c| c == CAP_AUTOPLAY)
    );
    f.finish().await;

    let f = fixture_with_catalog("autoplay-hello-nocatalog", None).await;
    let client = f.client().await;
    assert!(
        !client
            .server()
            .capabilities
            .iter()
            .any(|c| c == CAP_AUTOPLAY)
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asking_for_lyrics_does_not_hold_up_the_requests_behind_it() {
    let catalog = FakeCatalog::new()
        .with_lyrics("1", synced_lyrics())
        .delayed(Duration::from_millis(700));
    let f = fixture_with_catalog("lyrics-slow", Some(Arc::new(catalog))).await;
    let mut raw = f.raw().await;
    raw.hello().await;
    raw.send(r#"{"id":2,"request":{"type":"lyrics","id":"1"}}"#)
        .await;
    raw.send(r#"{"id":3,"request":{"type":"status"}}"#).await;
    let Some(ServerMessage::Response { id, .. }) = raw.recv().await else {
        panic!("no response");
    };
    assert_eq!(id, RequestId(3), "the status comes first");
    let Some(ServerMessage::Response { id, reply }) = raw.recv().await else {
        panic!("no response");
    };
    assert_eq!(id, RequestId(2));
    assert!(matches!(reply, Reply::Ok(Payload::Lyrics { .. })));
    f.finish().await;
}

// --- Playlist folders (protocol 1.11, #39) ----------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folders_contents_are_told_apart_from_a_sub_folder_and_the_offset_is_kept() {
    let catalog = browsable().with_folder(
        None,
        vec![
            catalog::FolderEntry::Folder {
                id: "f1".into(),
                name: "Moods".into(),
                item_count: 1,
            },
            catalog::FolderEntry::Playlist(playlist_named("p-root", "Road trip")),
        ],
    );
    let f = fixture_with_catalog("folders", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let Payload::PlaylistFolder { folder, page } = client
        .request(Request::PlaylistFolder {
            folder: None,
            offset: 1,
            limit: None,
        })
        .await
        .unwrap()
    else {
        panic!("not a playlist folder answer");
    };
    assert_eq!(folder, None);
    assert_eq!(page.offset, 1, "the caller's own offset is kept");
    assert_eq!(
        page.items,
        vec![FolderEntry::Playlist(PlaylistSummary {
            id: "p-root".into(),
            title: "Road trip".into(),
            creator: Some("hectmor".into()),
            description: None,
            track_count: Some(10),
            duration_ms: Some(2_400_000),
            cover: None,
        })]
    );
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sub_folder_is_opened_by_the_id_an_earlier_page_handed_back() {
    let catalog = browsable()
        .with_folder(
            None,
            vec![catalog::FolderEntry::Folder {
                id: "f1".into(),
                name: "Moods".into(),
                item_count: 1,
            }],
        )
        .with_folder(
            Some("f1"),
            vec![catalog::FolderEntry::Playlist(playlist_named(
                "p-inside",
                "Dark Jazz",
            ))],
        );
    let f = fixture_with_catalog("sub-folder", Some(Arc::new(catalog))).await;
    let client = f.client().await;
    let Payload::PlaylistFolder { folder, page } = client
        .request(Request::PlaylistFolder {
            folder: Some("f1".into()),
            offset: 0,
            limit: None,
        })
        .await
        .unwrap()
    else {
        panic!("not a playlist folder answer");
    };
    assert_eq!(folder, Some("f1".into()));
    assert_eq!(page.items.len(), 1);
    assert!(matches!(&page.items[0], FolderEntry::Playlist(p) if p.title == "Dark Jazz"));
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_catalog_a_playlist_folder_cannot_be_answered() {
    let f = fixture_with_catalog("folders-nocatalog", None).await;
    let client = f.client().await;
    let error = client
        .request(Request::PlaylistFolder {
            folder: None,
            offset: 0,
            limit: None,
        })
        .await
        .unwrap_err();
    assert_eq!(protocol_code(error), ErrorCode::Unsupported);
    f.finish().await;
}
