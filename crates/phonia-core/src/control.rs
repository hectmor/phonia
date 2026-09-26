//! Driving an engine from a queue: the rules that sit between "the user asked for X" and the
//! engine and queue calls that do it.
//!
//! The engine only plays and the queue only answers "what is next"; neither knows what should
//! happen when, say, the entry that is playing is removed. Those rules live here so that every
//! front end (the terminal player, the daemon) behaves the same.

use crate::engine::{Command, Engine, Event, State, Status};
use crate::queue::{ItemId, Queue, QueueSnapshot};
use anyhow::{Result, anyhow};
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct Controller {
    engine: Engine,
    queue: Arc<Queue>,
}

impl Controller {
    pub fn new(engine: Engine, queue: Arc<Queue>) -> Self {
        Self { engine, queue }
    }

    pub fn queue(&self) -> &Arc<Queue> {
        &self.queue
    }

    /// Plays through another output from now on, keeping the track and the position. Blocks until
    /// the switch is done (see [`Engine::set_output`]).
    pub fn set_output(&self, sinks: Arc<dyn crate::output::SinkFactory>) -> Result<()> {
        self.engine.set_output(sinks)
    }

    /// Sends a command to the engine. Fails only once the engine has shut down.
    pub fn send(&self, command: Command) -> Result<()> {
        self.engine.send(command)
    }

    /// Plays the entry `id` now, wherever it is in the queue.
    pub fn play_item(&self, id: ItemId) -> Result<()> {
        if !self.queue.snapshot().items.iter().any(|item| item.id == id) {
            return Err(anyhow!("there is no queue entry {}", id.0));
        }
        self.engine.send(Command::Play(Some(id.track_ref())))
    }

    /// Removes entries from the queue. The queue leaves a track that is playing alone (it only
    /// answers what comes next), so if the playing entry was one of them, this skips to the next.
    /// Returns how many entries existed.
    pub fn remove(&self, ids: &[ItemId]) -> usize {
        let playing = self.playing_entry();
        let removed = self.queue.remove(ids);
        if removed > 0 && playing.is_some_and(|current| ids.contains(&current)) {
            let _ = self.engine.send(Command::Next);
        }
        removed
    }

    /// Empties the queue and stops playback, since what was playing is no longer in it.
    pub fn clear(&self) {
        self.queue.clear();
        let _ = self.engine.send(Command::Stop);
    }

    /// The queue entry the engine is playing (or has loaded), if any. A stopped engine is not
    /// playing anything, however recently it was: removing "the last entry that played" must not
    /// start playback.
    fn playing_entry(&self) -> Option<ItemId> {
        (self.engine.status().state != State::Stopped)
            .then(|| self.queue.snapshot().current)
            .flatten()
    }

    pub fn status(&self) -> Status {
        self.engine.status()
    }

    pub fn snapshot(&self) -> Arc<QueueSnapshot> {
        self.queue.snapshot()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.engine.subscribe()
    }

    /// Stops playback, releases the audio device and waits for the audio thread to exit.
    pub fn shutdown(&self) {
        self.engine.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::SourceSpec;
    use crate::engine::{LoadedTrack, TrackMedia, TrackMeta, TrackOpener, TrackRef};
    use crate::output::fake::FakeSinkFactory;
    use crate::queue::QueueTrack;
    use futures_util::future::BoxFuture;
    use std::time::{Duration, Instant};
    use tokio::sync::broadcast::error::TryRecvError;

    const SPEC: SourceSpec = SourceSpec {
        sample_rate: 48_000,
        channels: 2,
        bits_per_sample: 24,
    };
    const PERIOD: usize = 1024;
    const CAPACITY: usize = 4096;
    const TIMEOUT: Duration = Duration::from_secs(5);

    /// Serves `RawPcm` of the length named by the reference: `"200000"` is a long track.
    struct LengthOpener;

    impl TrackOpener for LengthOpener {
        fn open(&self, track: TrackRef, _at: Duration) -> BoxFuture<'static, Result<LoadedTrack>> {
            Box::pin(async move {
                let frames: usize = track.0.parse()?;
                let meta = TrackMeta {
                    track,
                    title: None,
                    duration: None,
                };
                Ok(LoadedTrack::new(
                    meta,
                    TrackMedia::RawPcm {
                        samples: vec![0; frames * 2],
                        spec: SPEC,
                    },
                ))
            })
        }
    }

    fn entry(frames: usize, title: &str) -> QueueTrack {
        QueueTrack {
            source: TrackRef(frames.to_string()),
            title: Some(title.to_string()),
            duration: None,
        }
    }

    struct Harness {
        // The controller (and its engine's audio thread) must go before the runtime.
        controller: Controller,
        sinks: Arc<FakeSinkFactory>,
        events: broadcast::Receiver<Event>,
        _rt: tokio::runtime::Runtime,
    }

    fn harness(entries: &[(usize, &str)]) -> (Harness, Vec<ItemId>) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let queue = Queue::with_seed(Arc::new(LengthOpener), 1);
        let ids = queue.add(entries.iter().map(|(frames, title)| entry(*frames, title)));
        let sinks = FakeSinkFactory::blocking();
        let engine = Engine::spawn(rt.handle().clone(), sinks.clone(), queue.clone()).unwrap();
        let events = engine.subscribe();
        (
            Harness {
                controller: Controller::new(engine, queue),
                sinks,
                events,
                _rt: rt,
            },
            ids,
        )
    }

    impl Harness {
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

        /// Events until one matches; the matching one is returned.
        fn wait_for(&mut self, matches: impl Fn(&Event) -> bool) -> Event {
            loop {
                let event = self.next_event();
                if matches(&event) {
                    return event;
                }
            }
        }

        fn started(&mut self) -> Option<String> {
            match self.wait_for(|e| matches!(e, Event::TrackStarted { .. } | Event::QueueExhausted))
            {
                Event::TrackStarted { meta, .. } => meta.title,
                _ => None,
            }
        }

        /// Blocks until the engine is stuck writing mid-track, then lets it go on when asked.
        fn wait_until_blocked(&self) {
            let sink = self.sinks.handles()[0].clone();
            let (mut last, deadline) = (None, Instant::now() + TIMEOUT);
            loop {
                let queued = sink.queued_frames();
                if last == Some(queued) && queued > CAPACITY - PERIOD {
                    return;
                }
                assert!(Instant::now() < deadline, "the engine never blocked");
                last = Some(queued);
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn release(&self) {
            let sink = self.sinks.handles()[0].clone();
            sink.set_blocking(false);
        }
    }

    #[test]
    fn removing_the_playing_entry_skips_to_the_next_one() {
        let (mut h, ids) = harness(&[(200_000, "a"), (300, "b"), (300, "c")]);
        h.controller.send(Command::Play(None)).unwrap();
        assert_eq!(h.started().as_deref(), Some("a"));
        h.wait_until_blocked();

        assert_eq!(h.controller.remove(&[ids[0]]), 1);
        h.sinks.handles()[0].advance(PERIOD); // let the blocked write return
        assert_eq!(h.started().as_deref(), Some("b"), "b took a's place");
        h.release();
    }

    #[test]
    fn removing_an_entry_that_is_not_playing_leaves_playback_alone() {
        let (mut h, ids) = harness(&[(200_000, "a"), (300, "b"), (300, "c")]);
        h.controller.send(Command::Play(None)).unwrap();
        assert_eq!(h.started().as_deref(), Some("a"));
        h.wait_until_blocked();

        assert_eq!(h.controller.remove(&[ids[2]]), 1);
        assert_eq!(h.controller.snapshot().items.len(), 2);
        assert_eq!(h.controller.snapshot().current, Some(ids[0]), "still on a");
        assert_eq!(h.controller.status().state, State::Playing);
        h.release();
    }

    #[test]
    fn removing_the_playing_last_entry_ends_the_queue() {
        let (mut h, ids) = harness(&[(200_000, "a")]);
        h.controller.send(Command::Play(None)).unwrap();
        assert_eq!(h.started().as_deref(), Some("a"));
        h.wait_until_blocked();

        h.controller.remove(&[ids[0]]);
        h.sinks.handles()[0].advance(PERIOD);
        assert_eq!(
            h.started(),
            None,
            "nothing is left to play: the queue is exhausted"
        );
    }

    #[test]
    fn removing_the_last_played_entry_after_the_engine_stopped_does_not_restart_playback() {
        let (mut h, ids) = harness(&[(300, "a"), (300, "b")]);
        h.controller.send(Command::Play(None)).unwrap();
        h.sinks.handles(); // (the sink appears once the first track starts)
        h.wait_for(|e| matches!(e, Event::QueueExhausted));
        h.wait_for(|e| matches!(e, Event::StateChanged(State::Stopped)));
        assert_eq!(
            h.controller.snapshot().current,
            Some(ids[1]),
            "the queue remembers the last entry"
        );

        h.controller.remove(&[ids[1]]);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            h.controller.status().state,
            State::Stopped,
            "removing it must not start anything"
        );
    }

    #[test]
    fn clearing_empties_the_queue_and_stops_playback() {
        let (mut h, _) = harness(&[(200_000, "a"), (300, "b")]);
        h.controller.send(Command::Play(None)).unwrap();
        assert_eq!(h.started().as_deref(), Some("a"));
        h.wait_until_blocked();

        h.controller.clear();
        h.sinks.handles()[0].advance(PERIOD);
        h.wait_for(|e| matches!(e, Event::StateChanged(State::Stopped)));
        assert!(h.controller.snapshot().items.is_empty());
    }

    #[test]
    fn playing_an_entry_jumps_to_it_and_an_unknown_one_is_an_error() {
        let (mut h, ids) = harness(&[(300, "a"), (300, "b"), (300, "c")]);
        h.controller.play_item(ids[2]).unwrap();
        assert_eq!(h.started().as_deref(), Some("c"));

        let error = h.controller.play_item(ItemId(9999)).unwrap_err();
        assert!(error.to_string().contains("no queue entry 9999"), "{error}");
    }
}
