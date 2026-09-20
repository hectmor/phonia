use super::audio_thread::Msg;
use super::*;
use crate::decode::{SourceSpec, duration_to_frames, frames_to_duration};
use crate::output::fake::{FakeSinkFactory, FakeSinkHandle};
use crate::testutil::{NonSeekable, RATE, expected, wav, wav_slice};
use futures_util::future::BoxFuture;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::TryRecvError;

const SPEC_48K: SourceSpec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };
/// A tiny rate, so "three seconds in" is only a few thousand frames.
const SPEC_1K: SourceSpec = SourceSpec { sample_rate: 1_000, channels: 2, bits_per_sample: 24 };
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
    /// A WAV that can only be opened in whole segments of `segment_frames`, like a DASH stream:
    /// opening it at a time gives the segment containing it, as a stream that can't rewind.
    Segmented { frames: usize, segment_frames: usize },
    Broken,
}

#[derive(Clone)]
struct TestTrack {
    id: &'static str,
    media: Media,
    open_delay: Duration,
    seek: SeekMode,
    /// Opening this track anywhere but the start fails, to exercise a failed seek.
    fail_reopen: bool,
}

impl TestTrack {
    fn new(id: &'static str, media: Media) -> Self {
        Self { id, media, open_delay: Duration::ZERO, seek: SeekMode::None, fail_reopen: false }
    }

    fn pcm(id: &'static str, frames: usize) -> Self {
        Self::pcm_with(id, frames, SPEC_48K)
    }

    fn pcm_with(id: &'static str, frames: usize, spec: SourceSpec) -> Self {
        Self::new(id, Media::Pcm { samples: ramp(frames), spec })
    }

    /// A WAV of `frames` frames at 44.1 kHz.
    fn wav(id: &'static str, frames: usize) -> Self {
        Self::new(id, Media::Wav(wav(frames)))
    }

    fn segmented(id: &'static str, frames: usize, segment_frames: usize) -> Self {
        Self { seek: SeekMode::Reopen, ..Self::new(id, Media::Segmented { frames, segment_frames }) }
    }

    fn slow(mut self, delay: Duration) -> Self {
        self.open_delay = delay;
        self
    }

    fn seekable(mut self, mode: SeekMode) -> Self {
        self.seek = mode;
        self
    }

    fn failing_reopen(mut self) -> Self {
        self.fail_reopen = true;
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
    fn advance(&self, how: Advance) -> Option<TrackRef> {
        let cursor = *self.cursor.lock().unwrap();
        let target = match how {
            Advance::Auto | Advance::Next => Some(cursor.map_or(0, |index| index + 1)),
            Advance::Previous => cursor.and_then(|index| index.checked_sub(1)),
            Advance::Restart => cursor,
        };
        target.and_then(|index| self.tracks.get(index)).map(|track| TrackRef(track.id.to_string()))
    }

    fn open(&self, track: TrackRef, at: Duration) -> BoxFuture<'static, anyhow::Result<LoadedTrack>> {
        let Some(index) = self.tracks.iter().position(|t| t.id == track.0) else {
            return Box::pin(async move { Err(anyhow!("no such track: {}", track.0)) });
        };
        *self.cursor.lock().unwrap() = Some(index);

        let test_track = self.tracks[index].clone();
        Box::pin(async move {
            tokio::time::sleep(test_track.open_delay).await;
            if test_track.fail_reopen && at > Duration::ZERO {
                return Err(anyhow!("the connection dropped while reopening the track"));
            }
            let meta = TrackMeta { track, title: None, duration: None };
            let loaded = match test_track.media {
                Media::Pcm { samples, spec } => LoadedTrack::new(meta, TrackMedia::RawPcm { samples, spec }),
                Media::Wav(bytes) => LoadedTrack::new(
                    meta,
                    TrackMedia::Encoded {
                        source: match test_track.seek {
                            // A network stream can't rewind, so neither can this one.
                            SeekMode::ForwardOnly | SeekMode::Reopen => Box::new(NonSeekable(std::io::Cursor::new(bytes))),
                            _ => Box::new(std::io::Cursor::new(bytes)),
                        },
                        extension: Some("wav".into()),
                    },
                ),
                Media::Segmented { frames, segment_frames } => {
                    let wanted = duration_to_frames(at, RATE) as usize;
                    let boundary = wanted / segment_frames * segment_frames;
                    LoadedTrack::new(
                        meta,
                        TrackMedia::Encoded {
                            source: Box::new(NonSeekable(std::io::Cursor::new(wav_slice(boundary, frames - boundary)))),
                            extension: Some("wav".into()),
                        },
                    )
                    .starting_at(frames_to_duration(boundary as u64, RATE))
                }
                Media::Broken => return Err(anyhow!("the track's media is broken")),
            };
            Ok(LoadedTrack { seek: test_track.seek, ..loaded })
        })
    }
}

/// An engine wired to fakes, plus a subscription created before anything happens.
struct Harness {
    // Field order matters: the engine (and its audio thread) must go before the runtime.
    engine: Engine,
    sinks: Arc<FakeSinkFactory>,
    events: broadcast::Receiver<Event>,
    /// A second subscription that, unlike `events`, keeps the `Position` events.
    raw_events: broadcast::Receiver<Event>,
    _rt: tokio::runtime::Runtime,
}

impl Harness {
    fn new(tracks: Vec<TestTrack>, sinks: Arc<FakeSinkFactory>) -> Self {
        Self::with_position_interval(tracks, sinks, Duration::from_millis(250))
    }

    fn with_position_interval(tracks: Vec<TestTrack>, sinks: Arc<FakeSinkFactory>, interval: Duration) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let engine =
            Engine::spawn_with_position_interval(rt.handle().clone(), sinks.clone(), TestSupplier::new(tracks), interval)
                .unwrap();
        let (events, raw_events) = (engine.subscribe(), engine.subscribe());
        Self { engine, sinks, events, raw_events, _rt: rt }
    }

    fn send(&self, command: Command) {
        self.engine.send(command).unwrap();
    }

    fn play(&self, id: &str) {
        self.send(Command::Play(Some(TrackRef(id.to_string()))));
    }

    /// The next event other than `Position`, which has its own helpers so it doesn't clutter the
    /// lifecycle sequences the other tests assert on.
    fn next_event(&mut self) -> Event {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(Event::Position { .. }) => continue,
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

    /// Asserts nothing else is reported for `duration` (position reports aside).
    fn assert_quiet_for(&mut self, duration: Duration) {
        std::thread::sleep(duration);
        loop {
            match self.events.try_recv() {
                Ok(Event::Position { .. }) => continue,
                Err(TryRecvError::Empty) => return,
                other => panic!("expected no more events, got {other:?}"),
            }
        }
    }

    /// The next reported position (and the track length), skipping every other event.
    fn next_position(&mut self) -> (Duration, Option<Duration>) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.raw_events.try_recv() {
                Ok(Event::Position { position, duration }) => return (position, duration),
                Ok(_) | Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Closed) => panic!("the event channel closed"),
                Err(TryRecvError::Empty) if Instant::now() > deadline => panic!("timed out waiting for a position"),
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }

    /// Positions until one satisfies `stop`, which is included.
    fn positions_until(&mut self, stop: impl Fn(Duration) -> bool) -> Vec<Duration> {
        let mut positions = Vec::new();
        loop {
            let (position, _) = self.next_position();
            positions.push(position);
            if stop(position) {
                return positions;
            }
        }
    }

    fn sink(&self, index: usize) -> FakeSinkHandle {
        self.sinks.handles()[index].clone()
    }
}

fn stopped_status() -> Status {
    Status { state: State::Stopped, track: None, spec: None, position: Duration::ZERO, duration: None }
}

fn label(event: &Event) -> String {
    match event {
        Event::StateChanged(state) => format!("state:{state:?}"),
        Event::TrackStarted { meta, .. } => format!("started:{}", meta.track.0),
        Event::TrackEnded { meta, reason } => format!("ended:{}:{reason:?}", meta.track.0),
        Event::Seeked { position } => format!("seeked:{position:?}"),
        Event::SeekRejected { reason } => format!("seek-rejected:{reason}"),
        Event::QueueExhausted => "exhausted".to_string(),
        Event::Error { message } => format!("error:{message}"),
        Event::Position { .. } => unreachable!("positions are filtered out before labelling"),
    }
}

fn is_stopped(event: &Event) -> bool {
    matches!(event, Event::StateChanged(State::Stopped))
}

fn is_started(event: &Event) -> bool {
    matches!(event, Event::TrackStarted { .. })
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_fresh_engine_is_stopped() {
    let h = Harness::new(vec![], FakeSinkFactory::autoplay());
    assert_eq!(h.engine.status(), stopped_status());
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
    let unsolicited = LoadedTrack::new(
        TrackMeta { track: TrackRef("ghost".into()), title: None, duration: None },
        TrackMedia::RawPcm { samples: ramp(100), spec: SPEC_48K },
    );
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
    let broken = TestTrack::new("bad", Media::Broken);
    let mut h = Harness::new(vec![broken], FakeSinkFactory::autoplay());
    h.play("bad");
    let labels = h.labels_until(is_stopped);
    assert_eq!(labels, ["state:Loading", "error:the track's media is broken", "state:Stopped"]);
}

#[test]
fn encoded_media_goes_through_the_decoder_and_plays_bit_for_bit() {
    let frames = 30_000;
    let track = TestTrack::wav("wav", frames);
    let mut h = Harness::new(vec![track], FakeSinkFactory::autoplay());
    h.play("wav");
    h.labels_until(is_stopped);

    assert_eq!(h.sink(0).spec(), SPEC_WAV, "the sink is opened with the decoder's format");
    assert_eq!(h.sink(0).played(), expected(0, frames * 2));
}

#[test]
fn undecodable_encoded_media_is_reported_as_an_error() {
    let garbage = TestTrack::new("junk", Media::Wav(vec![0u8; 64]));
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
    wait_until_the_writer_is_blocked(&sink);
    sink
}

/// Waits until the queue is (nearly) full and has stopped growing: the engine is stuck in
/// `write`. Decoded chunks aren't a whole number of periods, so the queue can stall a little
/// short of its capacity.
fn wait_until_the_writer_is_blocked(sink: &FakeSinkHandle) {
    let mut last = None;
    wait_until("the writer is blocked on a full queue", || {
        let queued = sink.queued_frames();
        let stable = last == Some(queued);
        last = Some(queued);
        std::thread::sleep(Duration::from_millis(20));
        stable && queued > CAPACITY - PERIOD
    });
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
    assert_eq!(h.engine.status(), stopped_status());
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

#[test]
fn position_is_what_was_heard_not_what_was_written() {
    let frames = 20_000;
    let mut h = Harness::with_position_interval(
        vec![TestTrack::pcm("a", frames)],
        FakeSinkFactory::blocking(),
        Duration::ZERO,
    );
    let sink = play_until_queue_is_full(&mut h, "a");

    // Four periods are written but none played yet: nothing has been heard.
    assert_eq!(h.engine.status().position, Duration::ZERO);

    // The DAC plays 1500 frames, freeing room for the engine's blocked write to complete.
    sink.advance(1500);
    let heard = frames_to_duration(1500, 48_000);
    let positions = h.positions_until(|p| p > Duration::ZERO);
    assert_eq!(positions.last(), Some(&heard), "written minus still queued");

    let status = h.engine.status();
    assert_eq!(status.position, heard);
    assert_eq!(status.duration, Some(frames_to_duration(frames as u64, 48_000)));
    sink.set_blocking(false);
}

#[test]
fn a_completed_track_ends_with_its_full_length_as_the_last_position() {
    let frames = 10_000;
    let mut h = Harness::with_position_interval(
        vec![TestTrack::pcm("a", frames)],
        FakeSinkFactory::autoplay(),
        Duration::ZERO,
    );
    h.play("a");
    let length = frames_to_duration(frames as u64, 48_000);

    let positions = h.positions_until(|p| p == length);
    assert_eq!(positions[0], Duration::ZERO, "a track starts at zero");
    assert!(positions.windows(2).all(|w| w[0] <= w[1]), "position never goes backwards: {positions:?}");

    // Completion reports the position once more, after the device has played everything.
    assert_eq!(h.next_position(), (length, Some(length)));
}

#[test]
fn position_holds_still_while_paused() {
    let mut h = Harness::with_position_interval(
        vec![TestTrack::pcm("a", 30_000)],
        FakeSinkFactory::blocking(),
        Duration::ZERO,
    );
    let sink = play_until_queue_is_full(&mut h, "a");
    sink.advance(2000);
    wait_until("the writer is blocked again", || sink.queued_frames() > CAPACITY - PERIOD);

    h.send(Command::Pause);
    sink.advance(PERIOD);
    h.events_until(|e| matches!(e, Event::StateChanged(State::Paused)));

    let heard_at_pause = frames_to_duration((sink.played().len() / 2) as u64, 48_000);
    let paused = h.engine.status();
    assert_eq!(paused.position, heard_at_pause);

    sink.advance(10_000); // ignored: the device is paused
    h.assert_quiet_for(Duration::from_millis(100));
    assert_eq!(h.engine.status().position, heard_at_pause);
}

#[test]
fn previous_early_in_a_track_goes_back_one_track() {
    let mut h = Harness::new(
        vec![TestTrack::pcm("a", 30_000), TestTrack::pcm("b", 30_000)],
        FakeSinkFactory::blocking(),
    );
    let sink = play_until_queue_is_full(&mut h, "b");

    h.send(Command::Previous);
    sink.advance(PERIOD);
    let labels = h.labels_until(is_started_label("started:a"));
    assert_eq!(labels, ["state:Playing", "ended:b:Interrupted", "state:Loading", "started:a"]);
    sink.set_blocking(false);
}

fn is_started_label(wanted: &'static str) -> impl Fn(&Event) -> bool {
    move |event| label(event) == wanted
}

#[test]
fn previous_after_three_seconds_restarts_the_current_track() {
    let frames = 10_000;
    let mut h = Harness::new(
        vec![TestTrack::pcm("prev", 100), TestTrack::pcm_with("long", frames, SPEC_1K)],
        FakeSinkFactory::blocking(),
    );
    let sink = play_until_queue_is_full(&mut h, "long");

    // Let the device play 3.5 s (at 1 kHz) and wait for the engine to be blocked again.
    sink.advance(3_500);
    wait_until("the writer is blocked again", || sink.queued_frames() > CAPACITY - PERIOD);

    h.send(Command::Previous);
    sink.advance(PERIOD);
    let labels = h.labels_until(is_started_label("started:long"));
    assert_eq!(labels, ["state:Playing", "ended:long:Interrupted", "state:Loading", "started:long"]);

    sink.set_blocking(false);
    h.labels_until(is_stopped);
    let played = sink.played();
    assert_eq!(played[played.len() - frames * 2..], ramp(frames), "the restarted track plays from its first frame");
}

#[test]
fn previous_on_the_first_track_reports_an_empty_queue() {
    let mut h = Harness::new(vec![TestTrack::pcm("a", 30_000)], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");

    h.send(Command::Previous);
    sink.advance(PERIOD);
    assert_eq!(
        h.labels_until(is_stopped),
        ["state:Playing", "ended:a:Interrupted", "state:Loading", "exhausted", "state:Stopped"]
    );
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

/// Whether the audio the DAC played ends with exactly `expected`.
fn ends_with(played: &[i32], expected: &[i32]) -> bool {
    played.len() >= expected.len() && played[played.len() - expected.len()..] == *expected
}

/// What a 44.1 kHz WAV of `frames` frames sounds like from frame `from` on.
fn wav_from(from: usize, frames: usize) -> Vec<i32> {
    expected(from * 2, frames * 2)
}

/// Sends `command`, then lets the DAC play one period so an engine blocked writing to the full
/// queue can return and see it.
fn send_and_unblock(h: &Harness, sink: &FakeSinkHandle, command: Command) {
    h.send(command);
    sink.advance(PERIOD);
}

fn seek_to(h: &Harness, sink: &FakeSinkHandle, at: Duration) {
    send_and_unblock(h, sink, Command::Seek(SeekTarget::Absolute(at)));
}

#[test]
fn seeking_in_place_jumps_and_never_plays_the_flushed_audio() {
    let frames = 100_000;
    let mut h = Harness::new(
        vec![TestTrack::wav("a", frames).seekable(SeekMode::InPlace)],
        FakeSinkFactory::blocking(),
    );
    let sink = play_until_queue_is_full(&mut h, "a");

    seek_to(&h, &sink, secs(1));
    assert_eq!(h.labels_until(|e| matches!(e, Event::Seeked { .. })), ["state:Playing", "seeked:1s"]);
    assert_eq!(h.positions_until(|p| p == secs(1)).last(), Some(&secs(1)), "the position is the target at once");
    assert_eq!(sink.flush_count(), 1);

    sink.set_blocking(false);
    h.labels_until(is_stopped);
    let played = sink.played();
    let after_seek = wav_from(44_100, frames);
    assert!(ends_with(&played, &after_seek), "after the seek, exactly the audio from 1 s on");
    let before_seek = played.len() - after_seek.len();
    assert!(before_seek <= PERIOD * 2, "only audio the DAC had already taken before the seek may be heard");
    assert_eq!(played[..before_seek], wav_from(0, before_seek / 2)[..]);
}

#[test]
fn seeking_raw_pcm_in_place_also_lands_on_the_exact_frame() {
    let frames = 100_000;
    let mut h = Harness::new(
        vec![TestTrack::pcm("a", frames).seekable(SeekMode::InPlace)],
        FakeSinkFactory::blocking(),
    );
    let sink = play_until_queue_is_full(&mut h, "a");

    seek_to(&h, &sink, secs(1));
    h.labels_until(|e| matches!(e, Event::Seeked { .. }));
    sink.set_blocking(false);
    h.labels_until(is_stopped);
    assert!(ends_with(&sink.played(), &ramp(frames)[48_000 * 2..]));
}

#[test]
fn relative_seeks_are_measured_from_what_has_been_heard() {
    let mut h = Harness::new(
        vec![TestTrack::wav("a", 500_000).seekable(SeekMode::InPlace)],
        FakeSinkFactory::blocking(),
    );
    let sink = play_until_queue_is_full(&mut h, "a");

    send_and_unblock(&h, &sink, Command::Seek(SeekTarget::Forward(secs(2))));
    let labels = h.events_until(|e| matches!(e, Event::Seeked { .. }));
    let Some(Event::Seeked { position }) = labels.last() else { unreachable!() };
    // The DAC had played one period when the seek was handled; +2 s from there (give or take the
    // frame the container rounds to).
    let expected_position = frames_to_duration(PERIOD as u64, RATE) + secs(2);
    let one_frame = frames_to_duration(1, RATE) + Duration::from_nanos(10);
    assert!(position.abs_diff(expected_position) <= one_frame, "{position:?} vs {expected_position:?}");

    // Going back further than the start stops at the start.
    send_and_unblock(&h, &sink, Command::Seek(SeekTarget::Backward(secs(60))));
    assert_eq!(h.next_event(), Event::Seeked { position: Duration::ZERO });
    sink.set_blocking(false);
}

#[test]
fn seeking_past_the_end_ends_the_track_and_moves_on() {
    let mut h = Harness::new(
        vec![TestTrack::wav("a", 30_000).seekable(SeekMode::InPlace), TestTrack::pcm("b", 3_000)],
        FakeSinkFactory::blocking(),
    );
    let sink = play_until_queue_is_full(&mut h, "a");

    seek_to(&h, &sink, secs(60));
    assert_eq!(
        h.labels_until(is_stopped),
        [
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
}

#[test]
fn a_track_that_cannot_seek_rejects_the_request_and_keeps_playing() {
    let frames = 30_000;
    let mut h = Harness::new(vec![TestTrack::pcm("a", frames)], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");

    seek_to(&h, &sink, secs(1));
    assert_eq!(
        h.labels_until(|e| matches!(e, Event::SeekRejected { .. })),
        ["state:Playing", "seek-rejected:this track can't be seeked"]
    );
    assert_eq!(sink.flush_count(), 0, "a rejected seek touches nothing");

    sink.set_blocking(false);
    h.labels_until(is_stopped);
    assert_eq!(sink.played(), ramp(frames), "the track played on undisturbed");
}

#[test]
fn seeking_with_nothing_to_seek_in_is_rejected() {
    let mut h = Harness::new(
        vec![TestTrack::pcm("slow", 1_000).slow(Duration::from_millis(300))],
        FakeSinkFactory::autoplay(),
    );
    h.send(Command::Seek(SeekTarget::Absolute(secs(1))));
    assert_eq!(h.next_event(), Event::SeekRejected { reason: "nothing is playing".into() });

    h.play("slow"); // still loading: no position to seek from either
    assert_eq!(h.next_event(), Event::StateChanged(State::Loading));
    h.send(Command::Seek(SeekTarget::Absolute(secs(1))));
    assert_eq!(h.next_event(), Event::SeekRejected { reason: "nothing is playing".into() });
}

#[test]
fn a_forward_only_stream_skips_ahead_but_refuses_to_go_back() {
    let frames = 200_000;
    let mut h = Harness::new(
        vec![TestTrack::wav("a", frames).seekable(SeekMode::ForwardOnly)],
        FakeSinkFactory::blocking(),
    );
    let sink = play_until_queue_is_full(&mut h, "a");

    seek_to(&h, &sink, secs(1));
    assert_eq!(h.labels_until(|e| matches!(e, Event::Seeked { .. })), ["state:Playing", "seeked:1s"]);

    // Back to before the point it has already decoded up to: impossible on this stream.
    seek_to(&h, &sink, Duration::from_millis(500));
    let rejected = h.next_event();
    assert!(matches!(&rejected, Event::SeekRejected { reason } if reason.contains("only seek forward")), "{rejected:?}");

    seek_to(&h, &sink, secs(2));
    assert_eq!(h.next_event(), Event::Seeked { position: secs(2) });

    sink.set_blocking(false);
    h.labels_until(is_stopped);
    assert!(ends_with(&sink.played(), &wav_from(88_200, frames)), "lands on exactly 2 s");
}

#[test]
fn forward_seeks_in_a_row_compose() {
    let frames = 200_000;
    let mut h = Harness::new(
        vec![TestTrack::wav("a", frames).seekable(SeekMode::ForwardOnly)],
        FakeSinkFactory::blocking(),
    );
    let sink = play_until_queue_is_full(&mut h, "a");

    // Sent together, so the second arrives before the decoder has dropped what the first asked.
    h.send(Command::Seek(SeekTarget::Absolute(secs(1))));
    h.send(Command::Seek(SeekTarget::Absolute(secs(2))));
    sink.advance(PERIOD);
    let seeks: Vec<Event> = h.events_until(|e| matches!(e, Event::Seeked { position } if *position == secs(2)));
    assert!(seeks.iter().any(|e| *e == Event::Seeked { position: secs(1) }));

    sink.set_blocking(false);
    h.labels_until(is_stopped);
    assert!(ends_with(&sink.played(), &wav_from(88_200, frames)));
}

#[test]
fn a_stream_that_cannot_rewind_is_reopened_at_the_target_and_lands_on_the_exact_frame() {
    let frames = 100_000;
    // 0.5 s segments, and a target of 1.2 s that falls inside the third one.
    let mut h = Harness::new(vec![TestTrack::segmented("a", frames, 22_050)], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");

    seek_to(&h, &sink, Duration::from_millis(1_200));
    assert_eq!(
        h.labels_until(|e| matches!(e, Event::Seeked { .. })),
        ["state:Playing", "state:Seeking", "seeked:1.2s"]
    );
    assert_eq!(h.next_event(), Event::StateChanged(State::Playing), "audio resumes without a new track starting");

    sink.set_blocking(false);
    let labels = h.labels_until(is_stopped);
    assert_eq!(labels, ["ended:a:Completed", "state:Loading", "exhausted", "state:Stopped"]);
    assert!(ends_with(&sink.played(), &wav_from(52_920, frames)), "starts exactly at 1.2 s, not at the segment boundary");
}

#[test]
fn while_a_seek_reopens_the_track_the_position_is_already_the_target() {
    let track = TestTrack::segmented("a", 100_000, 22_050).slow(Duration::from_millis(300));
    let mut h = Harness::new(vec![track], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");

    seek_to(&h, &sink, Duration::from_millis(1_200));
    wait_until("the engine is reopening the track", || h.engine.status().state == State::Seeking);
    let status = h.engine.status();
    assert_eq!(status.position, Duration::from_millis(1_200));
    assert!(status.track.is_some(), "the track is known while it is reopened");

    wait_until("playing again", || h.engine.status().state == State::Playing);
    sink.set_blocking(false);
}

#[test]
fn a_seek_while_paused_stays_paused_and_resumes_at_the_target() {
    let frames = 100_000;
    let mut h = Harness::new(vec![TestTrack::segmented("a", frames, 22_050)], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");
    send_and_unblock(&h, &sink, Command::Pause);
    h.events_until(|e| matches!(e, Event::StateChanged(State::Paused)));

    h.send(Command::Seek(SeekTarget::Absolute(Duration::from_millis(1_200))));
    assert_eq!(
        h.labels_until(|e| matches!(e, Event::StateChanged(State::Paused))),
        ["state:Seeking", "seeked:1.2s", "state:Paused"]
    );
    h.assert_quiet_for(Duration::from_millis(100));
    assert_eq!(sink.queued_frames(), 0, "nothing is written while paused");

    h.send(Command::Resume);
    assert_eq!(h.next_event(), Event::StateChanged(State::Playing));
    sink.set_blocking(false);
    h.labels_until(is_stopped);
    assert!(ends_with(&sink.played(), &wav_from(52_920, frames)));
}

#[test]
fn a_new_track_request_during_a_reopening_seek_wins() {
    let slow = TestTrack::segmented("a", 100_000, 22_050).slow(Duration::from_millis(300));
    let mut h = Harness::new(vec![slow, TestTrack::pcm("b", 2_000)], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");

    seek_to(&h, &sink, Duration::from_millis(1_200));
    wait_until("the engine is reopening the track", || h.engine.status().state == State::Seeking);
    h.play("b");

    let labels = h.labels_until(is_stopped);
    let tail = &labels[labels.iter().position(|l| l == "ended:a:Interrupted").unwrap()..];
    assert_eq!(
        tail,
        ["ended:a:Interrupted", "state:Loading", "started:b", "state:Playing", "ended:b:Completed", "state:Loading", "exhausted", "state:Stopped"]
    );
    // Long enough for the abandoned reopening to have arrived, had it not been superseded.
    h.assert_quiet_for(Duration::from_millis(400));
}

#[test]
fn a_seek_whose_reopening_fails_stops_the_engine_with_an_error() {
    let track = TestTrack::segmented("a", 100_000, 22_050).failing_reopen();
    let mut h = Harness::new(vec![track], FakeSinkFactory::blocking());
    let sink = play_until_queue_is_full(&mut h, "a");

    seek_to(&h, &sink, Duration::from_millis(1_200));
    assert_eq!(
        h.labels_until(is_stopped),
        [
            "state:Playing",
            "state:Seeking",
            "seeked:1.2s",
            "error:the connection dropped while reopening the track",
            "ended:a:Failed",
            "state:Stopped"
        ]
    );
    assert_eq!(h.engine.status().state, State::Stopped);
}
