//! The queue driving a real engine (with fake sinks), and the queue's own `TrackSupplier` side.

use super::*;
use crate::decode::{SourceSpec, duration_to_frames};
use crate::engine::{Command, EndReason, Engine, Event, SeekTarget, State, TrackMedia};
use crate::output::fake::{FakeSinkFactory, FakeSinkHandle};
use crate::testutil::{NonSeekable, RATE, expected, wav_slice};
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::broadcast::{self, error::TryRecvError};

const SPEC: SourceSpec = SourceSpec {
    sample_rate: 48_000,
    channels: 2,
    bits_per_sample: 24,
};
const TIMEOUT: Duration = Duration::from_secs(5);
const PERIOD: usize = 1024;
const CAPACITY: usize = 4096;

fn ramp(frames: usize) -> Vec<i32> {
    (0..frames * 2).map(|i| i as i32).collect()
}

#[derive(Clone)]
enum Media {
    Pcm(usize),
    /// A 44.1 kHz WAV that can only be opened in whole segments, like a DASH stream.
    Segmented {
        frames: usize,
        segment_frames: usize,
    },
}

/// Opens the tracks named in `media` and nothing else. A track whose name starts with `x` comes
/// with a title of its own, so the queue's handling of what opening reveals can be checked;
/// the others have none, and show the title the queue holds.
struct TestOpener {
    media: HashMap<String, Media>,
}

impl TestOpener {
    fn new(media: &[(&str, Media)]) -> Arc<Self> {
        Arc::new(Self {
            media: media
                .iter()
                .map(|(name, media)| (name.to_string(), media.clone()))
                .collect(),
        })
    }
}

impl TrackOpener for TestOpener {
    fn open(&self, track: TrackRef, at: Duration) -> BoxFuture<'static, Result<LoadedTrack>> {
        let media = self.media.get(&track.0).cloned();
        Box::pin(async move {
            let media = media.ok_or_else(|| anyhow!("the opener has no track {:?}", track.0))?;
            let title = track
                .0
                .starts_with('x')
                .then(|| format!("opened {}", track.0));
            let meta = TrackMeta {
                track: track.clone(),
                title,
                duration: None,
            };
            Ok(match media {
                Media::Pcm(frames) => LoadedTrack::new(
                    meta,
                    TrackMedia::RawPcm {
                        samples: ramp(frames),
                        spec: SPEC,
                    },
                ),
                Media::Segmented {
                    frames,
                    segment_frames,
                } => {
                    let boundary =
                        duration_to_frames(at, RATE) as usize / segment_frames * segment_frames;
                    LoadedTrack::new(
                        meta,
                        TrackMedia::Encoded {
                            source: Box::new(NonSeekable(std::io::Cursor::new(wav_slice(
                                boundary,
                                frames - boundary,
                            )))),
                            extension: Some("wav".into()),
                        },
                    )
                    .starting_at(crate::decode::frames_to_duration(boundary as u64, RATE))
                    .reopenable()
                }
            })
        })
    }
}

fn entry(name: &str) -> QueueTrack {
    QueueTrack {
        source: TrackRef(name.to_string()),
        title: Some(name.to_string()),
        duration: None,
    }
}

struct Harness {
    // Field order matters: the engine (and its audio thread) must go before the runtime.
    engine: Engine,
    queue: Arc<Queue>,
    sinks: Arc<FakeSinkFactory>,
    events: broadcast::Receiver<Event>,
    _rt: tokio::runtime::Runtime,
}

impl Harness {
    fn new(media: &[(&str, Media)], sinks: Arc<FakeSinkFactory>) -> Self {
        Self::with_seed(media, sinks, 1)
    }

    fn with_seed(media: &[(&str, Media)], sinks: Arc<FakeSinkFactory>, seed: u64) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let queue = Queue::with_seed(TestOpener::new(media), seed);
        let engine = Engine::spawn(rt.handle().clone(), sinks.clone(), queue.clone()).unwrap();
        let events = engine.subscribe();
        Self {
            engine,
            queue,
            sinks,
            events,
            _rt: rt,
        }
    }

    fn send(&self, command: Command) {
        self.engine.send(command).unwrap();
    }

    /// The next event other than `Position`.
    fn next_event(&mut self) -> Event {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(Event::Position { .. }) => continue,
                Ok(event) => return event,
                Err(TryRecvError::Lagged(_)) => continue,
                Err(TryRecvError::Closed) => panic!("the event channel closed"),
                Err(TryRecvError::Empty) if Instant::now() > deadline => {
                    panic!("timed out waiting for an event")
                }
                Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }

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

    /// The titles of the tracks that start, until the engine stops.
    fn titles_until_stopped(&mut self) -> Vec<String> {
        self.events_until(is_stopped)
            .into_iter()
            .filter_map(started_title)
            .collect()
    }

    /// The first `count` titles that start.
    fn first_titles(&mut self, count: usize) -> Vec<String> {
        let mut titles = Vec::new();
        while titles.len() < count {
            if let Some(title) = started_title(self.next_event()) {
                titles.push(title);
            }
        }
        titles
    }

    fn sink(&self, index: usize) -> FakeSinkHandle {
        self.sinks.handles()[index].clone()
    }
}

fn started_title(event: Event) -> Option<String> {
    match event {
        Event::TrackStarted { meta, .. } => meta.title,
        _ => None,
    }
}

fn is_stopped(event: &Event) -> bool {
    matches!(event, Event::StateChanged(State::Stopped))
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Waits until the engine is stuck writing to a full queue, mid-track.
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

fn three_tracks() -> [(&'static str, Media); 3] {
    [
        ("a", Media::Pcm(1_000)),
        ("b", Media::Pcm(1_000)),
        ("c", Media::Pcm(1_000)),
    ]
}

// ---- the queue as a supplier ---------------------------------------------------------------

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn opening_an_entry_rewrites_the_reference_and_fills_in_what_the_queue_did_not_know() {
    let queue = Queue::with_seed(TestOpener::new(&[("x", Media::Pcm(10))]), 1);
    let ids = queue.add([QueueTrack {
        source: TrackRef("x".into()),
        title: None,
        duration: None,
    }]);

    let loaded = block_on(queue.open(ids[0].track_ref(), Duration::ZERO)).unwrap();
    assert_eq!(
        loaded.meta.track,
        ids[0].track_ref(),
        "the engine must see the queue's reference, not the opener's"
    );
    assert_eq!(loaded.meta.title.as_deref(), Some("opened x"));

    let snapshot = queue.snapshot();
    assert_eq!(
        snapshot.current,
        Some(ids[0]),
        "opening it made it the current entry"
    );
    assert_eq!(
        snapshot.items[0].track.title.as_deref(),
        Some("opened x"),
        "and the queue learned its title"
    );
}

#[test]
fn the_queues_own_title_wins_over_the_openers() {
    let queue = Queue::with_seed(TestOpener::new(&[("x", Media::Pcm(10))]), 1);
    let ids = queue.add([entry("x")]);
    let loaded = block_on(queue.open(ids[0].track_ref(), Duration::ZERO)).unwrap();
    assert_eq!(
        loaded.meta.title.as_deref(),
        Some("opened x"),
        "the opener's own title is kept when it has one"
    );
    assert_eq!(
        queue.snapshot().items[0].track.title.as_deref(),
        Some("x"),
        "and the queue's is not overwritten"
    );
}

#[test]
fn opening_something_that_is_not_an_entry_or_no_longer_is_an_error() {
    let queue = Queue::with_seed(TestOpener::new(&[("x", Media::Pcm(10))]), 1);
    let error = block_on(queue.open(TrackRef("x".into()), Duration::ZERO))
        .err()
        .unwrap();
    assert!(error.to_string().contains("not a queue entry"), "{error}");

    let ids = queue.add([entry("x")]);
    queue.remove(&ids);
    let error = block_on(queue.open(ids[0].track_ref(), Duration::ZERO))
        .err()
        .unwrap();
    assert!(error.to_string().contains("no longer exists"), "{error}");
}

#[test]
fn an_opener_failure_reaches_the_engine_as_an_error() {
    let queue = Queue::with_seed(TestOpener::new(&[]), 1);
    let ids = queue.add([entry("missing")]);
    let error = block_on(queue.open(ids[0].track_ref(), Duration::ZERO))
        .err()
        .unwrap();
    assert!(error.to_string().contains("has no track"), "{error}");
}

#[test]
fn subscribers_see_every_change() {
    let queue = Queue::with_seed(TestOpener::new(&[]), 1);
    let mut changes = queue.subscribe();
    assert!(!changes.has_changed().unwrap());

    let ids = queue.add([entry("a"), entry("b")]);
    assert!(changes.has_changed().unwrap());
    assert_eq!(changes.borrow_and_update().items.len(), 2);

    queue.move_to(ids[0], 1);
    queue.set_repeat(Repeat::All);
    queue.set_shuffle(true);
    let latest = changes.borrow_and_update().clone();
    assert_eq!((latest.repeat, latest.shuffle), (Repeat::All, true));
    assert_eq!(latest.version, queue.snapshot().version);

    // Doing nothing publishes nothing.
    queue.set_repeat(Repeat::All);
    assert!(!changes.has_changed().unwrap());
}

// ---- the queue driving the engine ----------------------------------------------------------

#[test]
fn entries_play_in_order_and_then_the_queue_is_exhausted() {
    let mut h = Harness::new(&three_tracks(), FakeSinkFactory::autoplay());
    h.queue.add([entry("a"), entry("b"), entry("c")]);
    h.send(Command::Play(None));

    let events = h.events_until(is_stopped);
    let titles: Vec<String> = events.iter().cloned().filter_map(started_title).collect();
    assert_eq!(titles, ["a", "b", "c"]);
    assert!(events.contains(&Event::QueueExhausted));

    let mut all = ramp(1_000);
    all.extend(ramp(1_000));
    all.extend(ramp(1_000));
    assert_eq!(h.sink(0).played(), all);
    assert_eq!(
        h.queue.snapshot().current.map(|id| id.0),
        Some(3),
        "the cursor ended on the last entry"
    );
}

#[test]
fn repeat_all_goes_round_until_stopped() {
    let mut h = Harness::new(&three_tracks(), FakeSinkFactory::autoplay());
    h.queue.add([entry("a"), entry("b"), entry("c")]);
    h.queue.set_repeat(Repeat::All);
    h.send(Command::Play(None));

    assert_eq!(h.first_titles(7), ["a", "b", "c", "a", "b", "c", "a"]);
    h.send(Command::Stop);
    h.events_until(is_stopped);
}

#[test]
fn repeat_one_repeats_the_track_until_the_user_skips() {
    let mut h = Harness::new(&three_tracks(), FakeSinkFactory::autoplay());
    h.queue.add([entry("a"), entry("b")]);
    h.queue.set_repeat(Repeat::One);
    h.send(Command::Play(None));

    assert_eq!(h.first_titles(3), ["a", "a", "a"]);
    h.send(Command::Next);
    // Some more repeats may have started before the skip was seen; then b takes over.
    let next_different = loop {
        let title = h.first_titles(1).remove(0);
        if title != "a" {
            break title;
        }
    };
    assert_eq!(next_different, "b");
    h.send(Command::Stop);
    h.events_until(is_stopped);
}

#[test]
fn a_seeded_shuffle_plays_the_planned_order_and_every_entry_once() {
    let media: Vec<(String, Media)> = (0..6).map(|i| (format!("t{i}"), Media::Pcm(200))).collect();
    let media_refs: Vec<(&str, Media)> = media
        .iter()
        .map(|(name, m)| (name.as_str(), m.clone()))
        .collect();

    let order_for = |seed| {
        let mut h = Harness::with_seed(&media_refs, FakeSinkFactory::autoplay(), seed);
        h.queue.add(media.iter().map(|(name, _)| entry(name)));
        h.queue.set_shuffle(true);
        let snapshot = h.queue.snapshot();
        let planned: Vec<String> = snapshot
            .order
            .iter()
            .map(|id| {
                snapshot
                    .items
                    .iter()
                    .find(|item| item.id == *id)
                    .unwrap()
                    .track
                    .title
                    .clone()
                    .unwrap()
            })
            .collect();
        h.send(Command::Play(None));
        (planned, h.titles_until_stopped())
    };

    let (planned, played) = order_for(11);
    assert_eq!(played, planned, "the queue plays the order it announced");
    let mut sorted = played.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        ["t0", "t1", "t2", "t3", "t4", "t5"],
        "each entry exactly once"
    );

    assert_eq!(
        order_for(11).1,
        played,
        "the same seed shuffles the same way"
    );
    assert_ne!(order_for(12).1, played);
}

#[test]
fn the_same_track_queued_twice_plays_twice() {
    let mut h = Harness::new(&three_tracks(), FakeSinkFactory::autoplay());
    let ids = h.queue.add([entry("a"), entry("a")]);
    assert_ne!(ids[0], ids[1]);
    h.send(Command::Play(None));
    assert_eq!(h.titles_until_stopped(), ["a", "a"]);
}

#[test]
fn playing_a_specific_entry_jumps_to_it_and_previous_goes_back_by_order() {
    // Long enough that the engine ends up blocked on the device mid-track.
    let long = [
        ("a", Media::Pcm(200_000)),
        ("b", Media::Pcm(200_000)),
        ("c", Media::Pcm(200_000)),
    ];
    let mut h = Harness::new(&long, FakeSinkFactory::blocking());
    let ids = h.queue.add([entry("a"), entry("b"), entry("c")]);

    h.send(Command::Play(Some(ids[2].track_ref())));
    assert_eq!(h.first_titles(1), ["c"]);
    wait_until_the_writer_is_blocked(&h.sink(0));

    h.send(Command::Previous);
    h.sink(0).advance(PERIOD);
    assert_eq!(
        h.first_titles(1),
        ["b"],
        "nothing was played before c, so back means the entry above it"
    );
    h.sink(0).set_blocking(false);
}

#[test]
fn removing_the_playing_entry_then_skipping_lands_on_the_one_that_took_its_place() {
    let mut h = Harness::new(
        &[
            ("a", Media::Pcm(200_000)),
            ("b", Media::Pcm(300)),
            ("c", Media::Pcm(300)),
        ],
        FakeSinkFactory::blocking(),
    );
    let ids = h.queue.add([entry("a"), entry("b"), entry("c")]);
    h.send(Command::Play(None));
    assert_eq!(h.first_titles(1), ["a"]);
    let sink = h.sink(0);
    wait_until_the_writer_is_blocked(&sink);

    // What a controller does: take the entry out of the queue, then skip.
    h.queue.remove(&[ids[0]]);
    h.send(Command::Next);
    sink.advance(PERIOD);

    let events = h.events_until(is_stopped);
    let ended = events.iter().find_map(|e| match e {
        Event::TrackEnded { meta, reason } => Some((meta.title.clone(), *reason)),
        _ => None,
    });
    assert_eq!(ended, Some((Some("a".into()), EndReason::Interrupted)));
    let titles: Vec<String> = events.into_iter().filter_map(started_title).collect();
    assert_eq!(titles, ["b", "c"]);
}

#[test]
fn a_seek_that_reopens_the_stream_still_finds_its_entry() {
    // If the queue's reference were not what the engine holds, reopening for the seek would ask
    // the queue for a track it has never heard of and the engine would stop with an error.
    let frames = 100_000;
    let mut h = Harness::new(
        &[(
            "s",
            Media::Segmented {
                frames,
                segment_frames: 22_050,
            },
        )],
        FakeSinkFactory::blocking(),
    );
    h.queue.add([entry("s")]);
    h.send(Command::Play(None));
    assert_eq!(h.first_titles(1), ["s"]);
    let sink = h.sink(0);
    wait_until_the_writer_is_blocked(&sink);

    h.send(Command::Seek(SeekTarget::Absolute(Duration::from_millis(
        1_200,
    ))));
    sink.advance(PERIOD);
    let events = h.events_until(|e| matches!(e, Event::Seeked { .. }));
    assert!(
        events.iter().all(|e| !matches!(e, Event::Error { .. })),
        "{events:?}"
    );
    assert_eq!(
        h.next_event(),
        Event::StateChanged(State::Playing),
        "audio resumed"
    );

    sink.set_blocking(false);
    let rest = h.events_until(is_stopped);
    assert!(
        rest.iter().all(|e| !matches!(e, Event::Error { .. })),
        "{rest:?}"
    );
    let played = sink.played();
    let tail = expected(52_920 * 2, frames * 2);
    assert!(
        played.len() >= tail.len() && played[played.len() - tail.len()..] == tail[..],
        "landed exactly at 1.2 s"
    );
}

// ---- opening ahead -------------------------------------------------------------------------

#[test]
fn opening_ahead_does_not_make_the_entry_current_and_starting_it_does() {
    let queue = Queue::with_seed(
        TestOpener::new(&[("a", Media::Pcm(10)), ("b", Media::Pcm(10))]),
        1,
    );
    let ids = queue.add([entry("a"), entry("b")]);
    block_on(queue.open(ids[0].track_ref(), Duration::ZERO)).unwrap();
    let version = queue.snapshot().version;

    assert_eq!(queue.peek(Advance::Auto), Peek::Next(ids[1].track_ref()));
    let ahead = block_on(queue.open_ahead(ids[1].track_ref())).unwrap();
    assert_eq!(
        ahead.meta.track,
        ids[1].track_ref(),
        "with the queue's reference"
    );
    let snapshot = queue.snapshot();
    assert_eq!(
        (snapshot.current, snapshot.version),
        (Some(ids[0]), version),
        "the queue did not notice: b is not playing yet"
    );

    assert!(queue.started(&ids[1].track_ref()));
    let snapshot = queue.snapshot();
    assert_eq!(snapshot.current, Some(ids[1]), "now b is the current entry");
    assert_eq!(
        queue.advance(Advance::Previous),
        Some(ids[0].track_ref()),
        "and the history is as if it had been opened the ordinary way"
    );
}

#[test]
fn an_entry_removed_before_it_starts_says_so() {
    let queue = Queue::with_seed(
        TestOpener::new(&[("a", Media::Pcm(10)), ("b", Media::Pcm(10))]),
        1,
    );
    let ids = queue.add([entry("a"), entry("b")]);
    block_on(queue.open(ids[0].track_ref(), Duration::ZERO)).unwrap();
    block_on(queue.open_ahead(ids[1].track_ref())).unwrap();

    queue.remove(&[ids[1]]);
    assert!(
        !queue.started(&ids[1].track_ref()),
        "the caller has to move on"
    );
    assert_eq!(queue.snapshot().current, Some(ids[0]));
    assert!(!queue.started(&TrackRef("not an entry".into())));
}

#[test]
fn opening_ahead_something_that_is_gone_is_an_error() {
    let queue = Queue::with_seed(TestOpener::new(&[("a", Media::Pcm(10))]), 1);
    let ids = queue.add([entry("a")]);
    queue.remove(&ids);
    assert!(block_on(queue.open_ahead(ids[0].track_ref())).is_err());
}

#[test]
fn a_supplier_that_does_not_say_what_is_next_says_unknown() {
    struct Plain;
    impl TrackSupplier for Plain {
        fn advance(&self, _: Advance) -> Option<TrackRef> {
            None
        }
        fn open(&self, _: TrackRef, _: Duration) -> BoxFuture<'static, Result<LoadedTrack>> {
            Box::pin(async { Err(anyhow!("nothing")) })
        }
    }
    assert_eq!(Plain.peek(Advance::Auto), Peek::Unknown);
    assert!(
        Plain.started(&TrackRef("x".into())),
        "and starting is a no-op that succeeds"
    );
}
