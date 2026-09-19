use super::audio_thread::Msg;
use super::*;
use crate::decode::SourceSpec;
use crate::output::fake::{FakeSinkFactory, FakeSinkHandle};
use crate::testutil::{expected, wav};
use futures_util::future::BoxFuture;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::TryRecvError;

const SPEC_48K: SourceSpec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };
const SPEC_96K: SourceSpec = SourceSpec { sample_rate: 96_000, channels: 2, bits_per_sample: 24 };
/// Matches the WAVs from `testutil::wav`.
const SPEC_WAV: SourceSpec = SourceSpec { sample_rate: 44_100, channels: 2, bits_per_sample: 16 };

const TIMEOUT: Duration = Duration::from_secs(5);
/// Frames per FakeSink period and queue capacity (its defaults).
const PERIOD: usize = 1024;
const CAPACITY: usize = 4096;

/// `frames` stereo frames of unique, increasing samples, so any loss, repeat or reordering shows.
fn ramp(frames: usize) -> Vec<i32> {
    (0..frames * 2).map(|i| i as i32).collect()
}

#[derive(Clone)]
enum Media {
    Pcm { samples: Vec<i32>, spec: SourceSpec },
    Wav(Vec<u8>),
    Broken,
}

#[derive(Clone)]
struct TestTrack {
    id: &'static str,
    media: Media,
    open_delay: Duration,
}

impl TestTrack {
    fn pcm(id: &'static str, frames: usize) -> Self {
        Self::pcm_with(id, frames, SPEC_48K)
    }

    fn pcm_with(id: &'static str, frames: usize, spec: SourceSpec) -> Self {
        Self { id, media: Media::Pcm { samples: ramp(frames), spec }, open_delay: Duration::ZERO }
    }

    fn slow(mut self, delay: Duration) -> Self {
        self.open_delay = delay;
        self
    }
}

/// A fixed list of tracks played in order; `open` moves the cursor to the track it opens.
struct TestSupplier {
    tracks: Vec<TestTrack>,
    cursor: Mutex<Option<usize>>,
}

impl TestSupplier {
    fn new(tracks: Vec<TestTrack>) -> Arc<Self> {
        Arc::new(Self { tracks, cursor: Mutex::new(None) })
    }
}

impl TrackSupplier for TestSupplier {
    fn advance(&self, _how: Advance) -> Option<TrackRef> {
        let next = self.cursor.lock().unwrap().map_or(0, |index| index + 1);
        self.tracks.get(next).map(|track| TrackRef(track.id.to_string()))
    }

    fn open(&self, track: TrackRef) -> BoxFuture<'static, anyhow::Result<LoadedTrack>> {
        let Some(index) = self.tracks.iter().position(|t| t.id == track.0) else {
            return Box::pin(async move { Err(anyhow!("no such track: {}", track.0)) });
        };
        *self.cursor.lock().unwrap() = Some(index);

        let test_track = self.tracks[index].clone();
        Box::pin(async move {
            tokio::time::sleep(test_track.open_delay).await;
            let meta = TrackMeta { track, title: None, duration: None };
            match test_track.media {
                Media::Pcm { samples, spec } => Ok(LoadedTrack { meta, media: TrackMedia::RawPcm { samples, spec } }),
                Media::Wav(bytes) => Ok(LoadedTrack {
                    meta,
                    media: TrackMedia::Encoded {
                        source: Box::new(std::io::Cursor::new(bytes)),
                        extension: Some("wav".into()),
                    },
                }),
                Media::Broken => Err(anyhow!("the track's media is broken")),
            }
        })
    }
}

/// An engine wired to fakes, plus a subscription created before anything happens.
struct Harness {
    // Field order matters: the engine (and its audio thread) must go before the runtime.
    engine: Engine,
    sinks: Arc<FakeSinkFactory>,
    events: broadcast::Receiver<Event>,
    _rt: tokio::runtime::Runtime,
}

impl Harness {
    fn new(tracks: Vec<TestTrack>, sinks: Arc<FakeSinkFactory>) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let engine = Engine::spawn(rt.handle().clone(), sinks.clone(), TestSupplier::new(tracks)).unwrap();
        let events = engine.subscribe();
        Self { engine, sinks, events, _rt: rt }
    }

    fn send(&self, command: Command) {
        self.engine.send(command).unwrap();
    }

    fn play(&self, id: &str) {
        self.send(Command::Play(Some(TrackRef(id.to_string()))));
    }

    fn next_event(&mut self) -> Event {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(event) => return event,
                Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Closed) => panic!("the event channel closed"),
                Err(TryRecvError::Empty) if Instant::now() > deadline => panic!("timed out waiting for an event"),
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }

    /// Every event up to and including the first one `stop` accepts.
    fn events_until(&mut self, stop: impl Fn(&Event) -> bool) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            let event = self.next_event();
            let done = stop(&event);
            events.push(event);
            if done {
                return events;
            }
        }
    }

    fn labels_until(&mut self, stop: impl Fn(&Event) -> bool) -> Vec<String> {
        self.events_until(stop).iter().map(label).collect()
    }

    /// Asserts nothing else is reported for `duration`.
    fn assert_quiet_for(&mut self, duration: Duration) {
        std::thread::sleep(duration);
        match self.events.try_recv() {
            Err(TryRecvError::Empty) => {}
            other => panic!("expected no more events, got {other:?}"),
        }
    }

    fn sink(&self, index: usize) -> FakeSinkHandle {
        self.sinks.handles()[index].clone()
    }
}

fn label(event: &Event) -> String {
    match event {
        Event::StateChanged(state) => format!("state:{state:?}"),
        Event::TrackStarted { meta, .. } => format!("started:{}", meta.track.0),
        Event::TrackEnded { meta, reason } => format!("ended:{}:{reason:?}", meta.track.0),
        Event::QueueExhausted => "exhausted".to_string(),
        Event::Error { message } => format!("error:{message}"),
    }
}

fn is_stopped(event: &Event) -> bool {
    matches!(event, Event::StateChanged(State::Stopped))
}

fn is_started(event: &Event) -> bool {
    matches!(event, Event::TrackStarted { .. })
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_fresh_engine_is_stopped() {
    let h = Harness::new(vec![], FakeSinkFactory::autoplay());
    assert_eq!(h.engine.status(), Status { state: State::Stopped, track: None, spec: None });
}

#[test]
fn shutdown_is_idempotent_and_later_commands_fail() {
    let h = Harness::new(vec![], FakeSinkFactory::autoplay());
    h.engine.shutdown();
    h.engine.shutdown();
    assert!(h.engine.send(Command::Stop).is_err());
}

#[test]
fn dropping_the_engine_stops_the_audio_thread() {
    let h = Harness::new(vec![], FakeSinkFactory::autoplay());
    let Harness { engine, _rt, .. } = h;
    drop(engine); // would hang forever if the thread didn't exit
}

#[test]
fn plays_a_track_to_the_end_and_reports_the_whole_lifecycle() {
    let mut h = Harness::new(vec![TestTrack::pcm("a", 10_000)], FakeSinkFactory::autoplay());
    h.play("a");

    let labels = h.labels_until(is_stopped);
    assert_eq!(
        labels,
        [
            "state:Loading",
            "started:a",
            "state:Playing",
            "ended:a:Completed",
            "state:Loading",
            "exhausted",
            "state:Stopped"
        ]
    );
    assert_eq!(h.sink(0).played(), ramp(10_000), "every sample, in order, exactly once");
}

#[test]
fn started_event_and_status_carry_the_track_and_its_format() {
    let mut h = Harness::new(vec![TestTrack::pcm_with("a", 10_000, SPEC_96K)], FakeSinkFactory::blocking());
    h.play("a");

    let events = h.events_until(is_started);
    let Some(Event::TrackStarted { meta, spec }) = events.last() else { unreachable!() };
    assert_eq!(meta.track, TrackRef("a".into()));
    assert_eq!(*spec, SPEC_96K);

    assert_eq!(h.next_event(), Event::StateChanged(State::Playing));
    let status = h.engine.status();
    assert_eq!(status.state, State::Playing);
    assert_eq!(status.track.map(|t| t.track), Some(TrackRef("a".into())));
    assert_eq!(status.spec, Some(SPEC_96K));

    // The engine is blocked writing to the full queue; release it so shutting down is instant.
    h.sink(0).set_blocking(false);
}

#[test]
fn auto_advances_and_keeps_the_sink_open_while_the_format_stays_the_same() {
    let mut h = Harness::new(
        vec![TestTrack::pcm("a", 5_000), TestTrack::pcm("b", 3_000)],
        FakeSinkFactory::autoplay(),
    );
    h.play("a");

    let labels = h.labels_until(is_stopped);
    assert_eq!(
        labels,
        [
            "state:Loading",
            "started:a",
            "state:Playing",
            "ended:a:Completed",
            "state:Loading",
            "started:b",
            "state:Playing",
            "ended:b:Completed",
            "state:Loading",
            "exhausted",
            "state:Stopped"
        ]
    );
    assert_eq!(h.sinks.handles().len(), 1, "one sink for both tracks");
    let mut both = ramp(5_000);
    both.extend(ramp(3_000));
    assert_eq!(h.sink(0).played(), both);
}

#[test]
fn a_different_format_reopens_the_sink() {
    let mut h = Harness::new(
        vec![TestTrack::pcm_with("a", 2_000, SPEC_48K), TestTrack::pcm_with("b", 2_000, SPEC_96K)],
        FakeSinkFactory::autoplay(),
    );
    h.play("a");
    h.labels_until(is_stopped);

    assert_eq!(h.sinks.handles().len(), 2);
    assert_eq!(h.sink(0).played(), ramp(2_000));
    assert_eq!(h.sink(1).played(), ramp(2_000));
}

#[test]
fn play_with_no_track_asks_the_supplier_and_reports_an_empty_queue() {
    let mut h = Harness::new(vec![], FakeSinkFactory::autoplay());
    h.send(Command::Play(None));
    assert_eq!(h.labels_until(is_stopped), ["state:Loading", "exhausted", "state:Stopped"]);
}

#[test]
fn play_with_no_track_starts_the_suppliers_first_track() {
    let mut h = Harness::new(vec![TestTrack::pcm("a", 100)], FakeSinkFactory::autoplay());
    h.send(Command::Play(None));
    let labels = h.labels_until(is_stopped);
    assert_eq!(labels[..3], ["state:Loading", "started:a", "state:Playing"]);
}

#[test]
fn stop_mid_track_discards_queued_audio_and_releases_the_sink() {
    let mut h = Harness::new(vec![TestTrack::pcm("long", 200_000)], FakeSinkFactory::blocking());
    h.play("long");
    h.events_until(is_started);
    let sink = h.sink(0);

    // The engine fills the device queue and then blocks in `write`, mid-track.
    wait_until("the device queue is full", || sink.queued_frames() == CAPACITY);
    h.send(Command::Stop);
    sink.advance(PERIOD); // lets the blocked write return, so the engine can see the Stop

    let labels = h.labels_until(is_stopped);
    assert_eq!(labels, ["state:Playing", "ended:long:Interrupted", "state:Stopped"]);
    assert_eq!(sink.flush_count(), 1);

    let played = sink.played();
    assert!(played.len() <= PERIOD * 2, "audio queued before the Stop must not be heard: {} samples", played.len());
    assert_eq!(played, ramp(200_000)[..played.len()], "what was heard is still an exact prefix");
    assert_eq!(h.engine.status().state, State::Stopped);
    assert_eq!(h.engine.status().track, None);
}

#[test]
fn next_mid_track_skips_ahead_and_never_plays_the_flushed_audio() {
    let b_frames = 3_000;
    let mut h = Harness::new(
        vec![TestTrack::pcm("a", 200_000), TestTrack::pcm("b", b_frames)],
        FakeSinkFactory::blocking(),
    );
    h.play("a");
    h.events_until(is_started);
    let sink = h.sink(0);

    wait_until("the device queue is full", || sink.queued_frames() == CAPACITY);
    h.send(Command::Next);
    sink.advance(PERIOD);

    let labels = h.labels_until(is_stopped);
    assert_eq!(
        labels,
        [
            "state:Playing",
            "ended:a:Interrupted",
            "state:Loading",
            "started:b",
            "state:Playing",
            "ended:b:Completed",
            "state:Loading",
            "exhausted",
            "state:Stopped"
        ]
    );

    let played = sink.played();
    let a_part = played.len() - b_frames * 2;
    assert!(a_part <= PERIOD * 2, "only audio that reached the DAC before Next may be heard");
    assert_eq!(played[..a_part], ramp(200_000)[..a_part]);
    assert_eq!(played[a_part..], ramp(b_frames), "then all of b, in order");
    assert_eq!(h.sinks.handles().len(), 1, "the sink is reused across the skip");
}

#[test]
fn a_newer_request_supersedes_a_slow_load() {
    let mut h = Harness::new(
        vec![TestTrack::pcm("slow", 1_000).slow(Duration::from_millis(400)), TestTrack::pcm("fast", 1_000)],
        FakeSinkFactory::autoplay(),
    );
    h.play("slow");
    h.play("fast");

    let labels = h.labels_until(is_stopped);
    assert_eq!(labels[..3], ["state:Loading", "started:fast", "state:Playing"]);
    assert!(!labels.iter().any(|l| l == "started:slow"));

    // Long enough for the slow load to have finished, had it not been superseded.
    h.assert_quiet_for(Duration::from_millis(700));
    assert_eq!(h.sink(0).played(), ramp(1_000));
}

#[test]
fn load_results_nobody_asked_for_are_ignored() {
    let mut h = Harness::new(vec![], FakeSinkFactory::autoplay());
    let unsolicited = LoadedTrack {
        meta: TrackMeta { track: TrackRef("ghost".into()), title: None, duration: None },
        media: TrackMedia::RawPcm { samples: ramp(100), spec: SPEC_48K },
    };
    h.engine.tx.send(Msg::Loaded { generation: 0, track: Box::new(unsolicited) }).unwrap();
    h.engine.tx.send(Msg::LoadFailed { generation: 7, error: "stale".into() }).unwrap();
    h.engine.tx.send(Msg::QueueExhausted { generation: 7 }).unwrap();

    h.assert_quiet_for(Duration::from_millis(150));
    assert_eq!(h.engine.status().state, State::Stopped);
    assert!(h.sinks.handles().is_empty());
}

#[test]
fn a_track_that_fails_to_load_reports_the_error_and_the_engine_stays_usable() {
    let mut h = Harness::new(vec![TestTrack::pcm("a", 500)], FakeSinkFactory::autoplay());
    h.play("nope");
    assert_eq!(
        h.labels_until(is_stopped),
        ["state:Loading", "error:no such track: nope", "state:Stopped"]
    );

    h.play("a");
    let labels = h.labels_until(is_stopped);
    assert_eq!(labels[..3], ["state:Loading", "started:a", "state:Playing"]);
    assert_eq!(h.sink(0).played(), ramp(500));
}

#[test]
fn media_that_cannot_be_opened_is_an_error_not_a_crash() {
    let broken = TestTrack { id: "bad", media: Media::Broken, open_delay: Duration::ZERO };
    let mut h = Harness::new(vec![broken], FakeSinkFactory::autoplay());
    h.play("bad");
    let labels = h.labels_until(is_stopped);
    assert_eq!(labels, ["state:Loading", "error:the track's media is broken", "state:Stopped"]);
}

#[test]
fn encoded_media_goes_through_the_decoder_and_plays_bit_for_bit() {
    let frames = 30_000;
    let track = TestTrack { id: "wav", media: Media::Wav(wav(frames)), open_delay: Duration::ZERO };
    let mut h = Harness::new(vec![track], FakeSinkFactory::autoplay());
    h.play("wav");
    h.labels_until(is_stopped);

    assert_eq!(h.sink(0).spec(), SPEC_WAV, "the sink is opened with the decoder's format");
    assert_eq!(h.sink(0).played(), expected(0, frames * 2));
}

#[test]
fn undecodable_encoded_media_is_reported_as_an_error() {
    let garbage = TestTrack { id: "junk", media: Media::Wav(vec![0u8; 64]), open_delay: Duration::ZERO };
    let mut h = Harness::new(vec![garbage], FakeSinkFactory::autoplay());
    h.play("junk");
    let labels = h.labels_until(is_stopped);
    assert_eq!(labels[0], "state:Loading");
    assert!(labels[1].starts_with("error:opening the decoder"), "{labels:?}");
    assert_eq!(labels[2], "state:Stopped");
}

/// Starts `id` on a blocking sink and waits until the engine is stuck writing to a full queue,
/// mid-track.
fn play_until_queue_is_full(h: &mut Harness, id: &str) -> FakeSinkHandle {
    h.play(id);
    h.events_until(is_started);
    let sink = h.sink(0);
    wait_until("the device queue is full", || sink.queued_frames() == CAPACITY);
    sink
}

#[test]
fn pause_silences_the_device_and_resume_continues_bit_for_bit() {
    let frames = 30_000;
    let mut h = Harness::new(vec![TestTrack::pcm("a", frames)], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");

    h.send(Command::Pause);
    sink.advance(PERIOD); // lets the blocked write return, so the engine can see the Pause
    assert_eq!(h.labels_until(|e| matches!(e, Event::StateChanged(State::Paused))), ["state:Playing", "state:Paused"]);
    assert!(sink.is_paused());
    assert_eq!(h.engine.status().state, State::Paused);
    assert!(h.engine.status().track.is_some(), "the track is kept while paused");

    // Time passes while paused: nothing may move, and the engine must not keep writing (the
    // fake rejects writes on a paused sink, which would surface as an error).
    let (played, queued) = (sink.played(), sink.queued_frames());
    sink.advance(10_000);
    h.assert_quiet_for(Duration::from_millis(150));
    assert_eq!((sink.played(), sink.queued_frames()), (played, queued));

    h.send(Command::Resume);
    assert_eq!(h.next_event(), Event::StateChanged(State::Playing));
    assert!(!sink.is_paused());

    sink.set_blocking(false);
    h.labels_until(is_stopped);
    assert_eq!(sink.played(), ramp(frames), "no sample lost or repeated across the pause");
}

#[test]
fn pausing_twice_or_resuming_while_playing_changes_nothing() {
    let mut h = Harness::new(vec![TestTrack::pcm("a", 30_000)], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");

    h.send(Command::Resume); // not paused: ignored
    h.send(Command::Pause);
    h.send(Command::Pause); // already paused: ignored
    sink.advance(PERIOD);
    assert_eq!(h.labels_until(|e| matches!(e, Event::StateChanged(State::Paused))), ["state:Playing", "state:Paused"]);
    h.assert_quiet_for(Duration::from_millis(100));

    sink.set_blocking(false);
    h.send(Command::Resume);
    h.send(Command::Resume);
    assert_eq!(h.next_event(), Event::StateChanged(State::Playing));
}

#[test]
fn toggle_pause_alternates_between_playing_and_paused() {
    let mut h = Harness::new(vec![TestTrack::pcm("a", 30_000)], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");

    h.send(Command::TogglePause);
    sink.advance(PERIOD);
    h.events_until(|e| matches!(e, Event::StateChanged(State::Paused)));

    sink.set_blocking(false);
    h.send(Command::TogglePause);
    assert_eq!(h.next_event(), Event::StateChanged(State::Playing));
}

#[test]
fn pausing_while_a_track_loads_starts_it_paused_without_touching_the_device() {
    let frames = 20_000;
    let track = TestTrack::pcm("a", frames).slow(Duration::from_millis(150));
    let mut h = Harness::new(vec![track], FakeSinkFactory::autoplay());
    h.play("a");
    h.send(Command::Pause);

    assert_eq!(h.labels_until(|e| matches!(e, Event::StateChanged(State::Paused))), ["state:Loading", "started:a", "state:Paused"]);
    let sink = h.sink(0);
    h.assert_quiet_for(Duration::from_millis(100));
    assert!(sink.played().is_empty(), "nothing is written while paused");
    assert!(!sink.is_paused(), "the sink was never asked to pause");

    h.send(Command::Resume);
    assert_eq!(h.next_event(), Event::StateChanged(State::Playing));
    h.labels_until(is_stopped);
    assert_eq!(sink.played(), ramp(frames));
}

#[test]
fn resuming_while_a_track_loads_cancels_the_pending_pause() {
    let track = TestTrack::pcm("a", 1_000).slow(Duration::from_millis(150));
    let mut h = Harness::new(vec![track], FakeSinkFactory::autoplay());
    h.play("a");
    h.send(Command::Pause);
    h.send(Command::Resume);

    let labels = h.labels_until(is_stopped);
    assert!(!labels.iter().any(|l| l == "state:Paused"), "{labels:?}");
    assert_eq!(labels[..3], ["state:Loading", "started:a", "state:Playing"]);
}

#[test]
fn stop_while_paused_discards_the_queue_and_releases_the_device() {
    let mut h = Harness::new(vec![TestTrack::pcm("a", 30_000)], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");
    h.send(Command::Pause);
    sink.advance(PERIOD);
    h.events_until(|e| matches!(e, Event::StateChanged(State::Paused)));

    h.send(Command::Stop);
    assert_eq!(h.labels_until(is_stopped), ["ended:a:Interrupted", "state:Stopped"]);
    assert_eq!(sink.flush_count(), 1);
    assert!(!sink.is_paused());
    assert_eq!(h.engine.status(), Status { state: State::Stopped, track: None, spec: None });
}

#[test]
fn next_while_paused_starts_the_next_track_playing() {
    let b_frames = 2_000;
    let mut h = Harness::new(
        vec![TestTrack::pcm("a", 30_000), TestTrack::pcm("b", b_frames)],
        FakeSinkFactory::blocking(),
    );
    let sink = play_until_queue_is_full(&mut h, "a");
    h.send(Command::Pause);
    sink.advance(PERIOD);
    h.events_until(|e| matches!(e, Event::StateChanged(State::Paused)));

    h.send(Command::Next);
    let labels = h.labels_until(is_stopped);
    assert_eq!(labels[..5], ["ended:a:Interrupted", "state:Loading", "started:b", "state:Playing", "ended:b:Completed"]);
    let played = sink.played();
    assert_eq!(played[played.len() - b_frames * 2..], ramp(b_frames), "b plays in full");
}

#[test]
fn shutdown_while_paused_does_not_hang() {
    let mut h = Harness::new(vec![TestTrack::pcm("a", 30_000)], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");
    h.send(Command::Pause);
    sink.advance(PERIOD);
    h.events_until(|e| matches!(e, Event::StateChanged(State::Paused)));
    h.engine.shutdown();
    assert_eq!(sink.flush_count(), 1);
}
