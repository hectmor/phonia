//! #34 parts 3-4: the MPRIS zbus adapter against a private, non-session `dbus-daemon` -- never
//! the user's real bus -- mirroring `phonia-core`'s own `output/dbus.rs` tests. Ignored by
//! default because they need that binary: `cargo test -p phoniad --test mpris -- --ignored`.

use futures_util::StreamExt;
use phonia_core::config::OutputSpec;
use phonia_core::openers::DispatchOpener;
use phonia_core::output::fake::FakeSinkFactory;
use phonia_core::testutil::Bus;
use phonia_ipc::{AddAt, Event, NewTrack, Request, State};
use phoniad::daemon::{Daemon, DaemonParts};
use phoniad::mpris::service;
use phoniad::outputs::{Build, Outputs};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use zbus::fdo::DBusProxy;
use zbus::zvariant::OwnedValue;
use zbus::{Connection, Proxy, connection};

const TIMEOUT: Duration = Duration::from_secs(5);
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const NAME: &str = "org.mpris.MediaPlayer2.phonia";

struct Fixture {
    daemon: Arc<Daemon>,
    sinks: Arc<FakeSinkFactory>,
    dir: PathBuf,
}

/// A daemon with a fake, blocking audio device: once its queue fills (past a handful of
/// periods), a write blocks until the test calls `f.sinks.handles()[0].advance(...)`, which is
/// also the only moment the audio thread notices a command like `Pause` sent meanwhile.
async fn fixture(name: &str) -> Fixture {
    fixture_with(
        name,
        OutputSpec::Exclusive {
            device: "hw:fake,0".into(),
        },
        FakeSinkFactory::blocking(),
    )
    .await
}

/// Like [`fixture`], on an output of the test's choosing -- a `Shared` one with
/// `FakeSinkFactory::blocking().with_volume()` is what gives `Volume` something to write to (an
/// exclusive card has no mixer, per #31).
async fn fixture_with(name: &str, output: OutputSpec, sinks: Arc<FakeSinkFactory>) -> Fixture {
    let (_reports, report_rx) = mpsc::unbounded_channel();
    let build: Build = {
        let sinks = sinks.clone();
        Arc::new(move |_| sinks.clone())
    };
    let outputs = Outputs::new(output, build);
    let daemon = Daemon::start(DaemonParts {
        sinks: sinks.clone(),
        outputs,
        opener: Arc::new(DispatchOpener::new(None)),
        quality: None,
        catalog: None,
        play_log: None,
        reports: report_rx,
        engine: phonia_core::engine::Options::default(),
        replaygain: phonia_core::replaygain::Mode::default(),
        autoplay: false,
        recent_path: None,
    })
    .unwrap();
    let dir =
        std::env::temp_dir().join(format!("phoniad-mpris-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Fixture { daemon, sinks, dir }
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

    /// Queues one track and starts it, returning once a `TrackStarted` event confirms playback
    /// has actually begun (never a sleep-and-hope).
    async fn playing_track(&self, events: &mut broadcast::Receiver<(u64, Event)>) {
        let source = self.wav("song.wav", 400_000);
        self.daemon
            .handle(Request::QueueAdd {
                tracks: vec![NewTrack { source }],
                at: AddAt::End,
            })
            .await;
        self.daemon.handle(Request::Play { item: None }).await;
        wait_for(events, |event| matches!(event, Event::TrackStarted { .. })).await;
    }

    async fn finish(self) {
        self.daemon.request_shutdown();
        self.daemon.stop_engine().await;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn wait_for(events: &mut broadcast::Receiver<(u64, Event)>, pred: impl Fn(&Event) -> bool) {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let (_, event) = events.recv().await.unwrap();
            if pred(&event) {
                return;
            }
        }
    })
    .await
    .expect("timed out waiting for the expected event")
}

/// Waits for the next `TrackStarted` and returns its `source`.
async fn wait_for_track_started(events: &mut broadcast::Receiver<(u64, Event)>) -> Option<String> {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let (_, event) = events.recv().await.unwrap();
            if let Event::TrackStarted { source, .. } = event {
                return source;
            }
        }
    })
    .await
    .expect("timed out waiting for TrackStarted")
}

async fn connect(bus: &Bus) -> Connection {
    connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn player(conn: &Connection, name: &str) -> Proxy<'static> {
    Proxy::new(conn, name.to_string(), PATH, PLAYER_IFACE)
        .await
        .unwrap()
}

async fn root(conn: &Connection, name: &str) -> Proxy<'static> {
    Proxy::new(conn, name.to_string(), PATH, ROOT_IFACE)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs dbus-daemon"]
async fn metadata_and_playback_status_are_served_once_a_track_is_playing() {
    let f = fixture("status").await;
    let mut events = f.daemon.subscribe();
    f.playing_track(&mut events).await;

    let bus = Bus::start();
    let _mpris = service::start_on_bus(f.daemon.clone(), &bus.address)
        .await
        .unwrap();
    let client = connect(&bus).await;
    let player = player(&client, NAME).await;

    assert_eq!(
        player
            .get_property::<String>("PlaybackStatus")
            .await
            .unwrap(),
        "Playing"
    );
    let mut metadata = player
        .get_property::<HashMap<String, OwnedValue>>("Metadata")
        .await
        .unwrap();
    assert_eq!(
        String::try_from(metadata.remove("xesam:title").unwrap()).unwrap(),
        "song.wav"
    );
    assert!(metadata.contains_key("mpris:trackid"));

    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs dbus-daemon"]
async fn seek_moves_real_playback_and_fires_the_seeked_signal() {
    let f = fixture("seek").await;
    let mut events = f.daemon.subscribe();
    f.playing_track(&mut events).await;

    let bus = Bus::start();
    let _mpris = service::start_on_bus(f.daemon.clone(), &bus.address)
        .await
        .unwrap();
    let client = connect(&bus).await;
    let player = player(&client, NAME).await;

    assert!(
        player.get_property::<bool>("CanSeek").await.unwrap(),
        "a track with a known duration can be sought"
    );

    let mut seeked = player.receive_signal("Seeked").await.unwrap();
    player
        .call::<_, _, ()>("Seek", &(2_000_000i64,)) // +2s
        .await
        .unwrap();
    // The audio thread only notices a command between writes; once its queue is full it is
    // blocked inside one until this lets it return.
    f.sinks.handles()[0].advance(4096);

    let message = tokio::time::timeout(TIMEOUT, seeked.next())
        .await
        .expect("timed out waiting for the Seeked signal")
        .expect("the stream ended");
    let (position,): (i64,) = message.body().deserialize().unwrap();
    assert!(
        (1_900_000..=2_100_000).contains(&position),
        "expected roughly 2s (in microseconds) in, got {position}"
    );

    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs dbus-daemon"]
async fn writing_volume_loop_status_and_shuffle_reaches_the_daemon() {
    let f = fixture_with(
        "writable",
        OutputSpec::Shared { sink: None },
        FakeSinkFactory::blocking().with_volume(),
    )
    .await;
    let mut events = f.daemon.subscribe();
    f.playing_track(&mut events).await;

    let bus = Bus::start();
    let _mpris = service::start_on_bus(f.daemon.clone(), &bus.address)
        .await
        .unwrap();
    let client = connect(&bus).await;
    let player = player(&client, NAME).await;

    player.set_property("Volume", 0.5f64).await.unwrap();
    let volume = f.daemon.snapshot().1.volume.unwrap();
    assert_eq!((volume.percent, volume.muted), (50, false));

    player.set_property("LoopStatus", "Track").await.unwrap();
    assert_eq!(f.daemon.snapshot().2.repeat, phonia_ipc::Repeat::One);

    player.set_property("Shuffle", true).await.unwrap();
    assert!(f.daemon.snapshot().2.shuffle);

    assert!(
        player
            .set_property("LoopStatus", "not-a-real-status")
            .await
            .is_err(),
        "an invalid LoopStatus is refused, not silently ignored"
    );

    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs dbus-daemon"]
async fn transport_methods_route_through_daemon_handle_and_can_flags_follow_the_queue() {
    let f = fixture("transport").await;
    let mut events = f.daemon.subscribe();
    let (one, two) = (f.wav("one.wav", 400_000), f.wav("two.wav", 400_000));
    f.daemon
        .handle(Request::QueueAdd {
            tracks: vec![
                NewTrack { source: one },
                NewTrack {
                    source: two.clone(),
                },
            ],
            at: AddAt::End,
        })
        .await;
    f.daemon.handle(Request::Play { item: None }).await;
    wait_for(&mut events, |event| {
        matches!(event, Event::TrackStarted { .. })
    })
    .await;

    let bus = Bus::start();
    let _mpris = service::start_on_bus(f.daemon.clone(), &bus.address)
        .await
        .unwrap();
    let client = connect(&bus).await;
    let player = player(&client, NAME).await;

    assert!(
        player.get_property::<bool>("CanGoNext").await.unwrap(),
        "a second track is queued"
    );
    assert!(
        player.get_property::<bool>("CanGoPrevious").await.unwrap(),
        "something is already current, so Previous always does something"
    );
    assert!(player.get_property::<bool>("CanPlay").await.unwrap());
    assert!(player.get_property::<bool>("CanPause").await.unwrap());

    // Pause really pauses the engine, not just the model's own idea of it.
    player.call::<_, _, ()>("Pause", &()).await.unwrap();
    f.sinks.handles()[0].advance(4096);
    wait_for(&mut events, |event| {
        matches!(
            event,
            Event::StateChanged {
                state: State::Paused
            }
        )
    })
    .await;
    assert_eq!(f.daemon.snapshot().1.state, State::Paused);

    // Next really advances the queue to the second track.
    player.call::<_, _, ()>("Next", &()).await.unwrap();
    f.sinks.handles()[0].advance(4096);
    let started = wait_for_track_started(&mut events).await;
    assert_eq!(started.as_deref(), Some(two.as_str()));

    // Stop really stops playback.
    player.call::<_, _, ()>("Stop", &()).await.unwrap();
    f.sinks.handles()[0].advance(4096);
    wait_for(&mut events, |event| {
        matches!(
            event,
            Event::StateChanged {
                state: State::Stopped
            }
        )
    })
    .await;

    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs dbus-daemon"]
async fn a_state_change_is_announced_as_one_batched_properties_changed_signal() {
    use zbus::fdo::PropertiesProxy;

    let f = fixture("changed").await;
    let mut events = f.daemon.subscribe();
    f.playing_track(&mut events).await;

    let bus = Bus::start();
    let _mpris = service::start_on_bus(f.daemon.clone(), &bus.address)
        .await
        .unwrap();
    let client = connect(&bus).await;
    let props = PropertiesProxy::builder(&client)
        .destination(NAME)
        .unwrap()
        .path(PATH)
        .unwrap()
        .build()
        .await
        .unwrap();
    let mut changes = props.receive_properties_changed().await.unwrap();

    f.daemon.handle(Request::Pause).await;
    // The audio thread only notices a command between writes; once its queue is full it is
    // blocked inside one until this lets it return.
    f.sinks.handles()[0].advance(4096);

    let signal = tokio::time::timeout(TIMEOUT, changes.next())
        .await
        .expect("timed out waiting for PropertiesChanged")
        .expect("the stream ended");
    let args = signal.args().unwrap();
    let changed = args.changed_properties();
    assert_eq!(
        String::try_from(&changed["PlaybackStatus"]).unwrap(),
        "Paused"
    );

    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs dbus-daemon"]
async fn the_root_object_never_quits_or_raises_even_if_asked() {
    let f = fixture("root").await;

    let bus = Bus::start();
    let _mpris = service::start_on_bus(f.daemon.clone(), &bus.address)
        .await
        .unwrap();
    let client = connect(&bus).await;
    let root = root(&client, NAME).await;

    assert_eq!(
        root.get_property::<String>("Identity").await.unwrap(),
        "phonia"
    );
    assert!(!root.get_property::<bool>("CanQuit").await.unwrap());
    assert!(!root.get_property::<bool>("CanRaise").await.unwrap());
    // Calling them anyway must not do anything the daemon would notice (there is nothing to
    // assert on besides "the call succeeds and the daemon keeps running").
    root.call::<_, _, ()>("Quit", &()).await.unwrap();
    root.call::<_, _, ()>("Raise", &()).await.unwrap();
    assert!(
        !*f.daemon.shutdown_signal().borrow(),
        "Quit must not shut the daemon down"
    );

    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs dbus-daemon"]
async fn the_bus_name_falls_back_to_an_instance_name_when_the_plain_one_is_taken() {
    let f = fixture("fallback").await;
    let bus = Bus::start();
    let holder = connect(&bus).await;
    let reply = holder
        .request_name_with_flags(NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await
        .unwrap();
    assert!(matches!(reply, zbus::fdo::RequestNameReply::PrimaryOwner));

    let _mpris = service::start_on_bus(f.daemon.clone(), &bus.address)
        .await
        .unwrap();

    let names = DBusProxy::new(&holder)
        .await
        .unwrap()
        .list_names()
        .await
        .unwrap();
    let fallback = format!("{NAME}.instance{}", std::process::id());
    assert!(
        names.iter().any(|n| n.as_str() == fallback),
        "expected {fallback} among {names:?}"
    );

    f.finish().await;
}
