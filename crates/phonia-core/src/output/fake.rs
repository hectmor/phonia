//! An in-memory [`AudioSink`] for tests: no hardware, no timing.
//!
//! It models a device with a bounded queue. Written audio sits in the queue until it is "played",
//! either explicitly with [`FakeSinkHandle::advance`] or automatically when the queue overflows
//! (which is what a blocking `writei` does: it waits for the DAC to consume audio). Everything
//! that reaches the "DAC" is recorded, so tests can assert on the exact sample stream, including
//! across pauses, flushes and seeks.
//!
//! In blocking mode ([`FakeSinkHandle::set_blocking`]) a write to a full queue waits until the test
//! calls [`FakeSinkHandle::advance`], like a real `writei`. That holds the audio thread at a known
//! point, which is what makes "stop/next in the middle of a track" deterministic to test.

use super::reserve::{DeviceReserver, Reservation, ReserveError};
use super::{AudioSink, ReleaseHandler, ReleaseRequest, SinkFactory};
use crate::decode::SourceSpec;
use anyhow::{Result, bail};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// A blocked write gives up after this long, so a test bug fails instead of hanging.
const BLOCKED_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

const DEFAULT_PERIOD_FRAMES: usize = 1024;
const DEFAULT_CAPACITY_FRAMES: usize = 4096;

#[derive(Default)]
struct State {
    /// Audio accepted by the device but not yet played.
    queued: VecDeque<i32>,
    /// Everything that reached the DAC, in order.
    played: Vec<i32>,
    paused: bool,
    autoplay: bool,
    blocking: bool,
    flushes: u32,
    drains: u32,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled whenever room may have opened up in the queue.
    room: Condvar,
}

pub struct FakeSink {
    spec: SourceSpec,
    period_frames: usize,
    capacity_frames: usize,
    shared: Arc<Shared>,
}

/// A view onto a [`FakeSink`] that stays usable from the test thread while the sink itself has
/// been moved into the audio thread.
#[derive(Clone)]
pub struct FakeSinkHandle {
    spec: SourceSpec,
    shared: Arc<Shared>,
}

impl FakeSink {
    /// A sink with a 1024-frame period and a 4096-frame queue, in manual mode: nothing is played
    /// until [`FakeSinkHandle::advance`] is called (or the queue overflows).
    pub fn new(spec: SourceSpec) -> (Self, FakeSinkHandle) {
        Self::with_sizes(spec, DEFAULT_PERIOD_FRAMES, DEFAULT_CAPACITY_FRAMES)
    }

    pub fn with_sizes(spec: SourceSpec, period_frames: usize, capacity_frames: usize) -> (Self, FakeSinkHandle) {
        assert!(period_frames > 0 && capacity_frames >= period_frames);
        let shared = Arc::new(Shared { state: Mutex::new(State::default()), room: Condvar::new() });
        let handle = FakeSinkHandle { spec, shared: shared.clone() };
        (Self { spec, period_frames, capacity_frames, shared }, handle)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap()
    }
}

impl FakeSinkHandle {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap()
    }

    /// The format the sink was opened with.
    pub fn spec(&self) -> SourceSpec {
        self.spec
    }

    /// In blocking mode a write to a full queue waits for [`FakeSinkHandle::advance`] instead of
    /// playing the oldest audio to make room.
    pub fn set_blocking(&self, blocking: bool) {
        self.lock().blocking = blocking;
        self.shared.room.notify_all();
    }

    /// In autoplay mode every write is played immediately (the queue is always empty), which
    /// suits tests that only care about what the engine wrote, not about timing.
    pub fn set_autoplay(&self, autoplay: bool) {
        self.lock().autoplay = autoplay;
    }

    /// Plays up to `frames` queued frames, as the DAC would over time. Does nothing while paused.
    pub fn advance(&self, frames: usize) {
        let channels = self.spec.channels as usize;
        let mut state = self.lock();
        if state.paused {
            return;
        }
        let samples = (frames * channels).min(state.queued.len());
        let drained: Vec<i32> = state.queued.drain(..samples).collect();
        state.played.extend(drained);
        self.shared.room.notify_all();
    }

    /// Everything that has reached the DAC so far.
    pub fn played(&self) -> Vec<i32> {
        self.lock().played.clone()
    }

    pub fn queued_frames(&self) -> usize {
        self.lock().queued.len() / self.spec.channels as usize
    }

    pub fn is_paused(&self) -> bool {
        self.lock().paused
    }

    pub fn flush_count(&self) -> u32 {
        self.lock().flushes
    }

    pub fn drain_count(&self) -> u32 {
        self.lock().drains
    }
}

impl AudioSink for FakeSink {
    fn spec(&self) -> SourceSpec {
        self.spec
    }

    fn capacity_frames(&self) -> u64 {
        self.capacity_frames as u64
    }

    fn write(&mut self, samples: &[i32]) -> Result<usize> {
        let channels = self.spec.channels as usize;
        let frames = (samples.len() / channels).min(self.period_frames);
        let samples = &samples[..frames * channels];
        let capacity = self.capacity_frames * channels;

        let deadline = Instant::now() + BLOCKED_WRITE_TIMEOUT;
        let mut state = self.lock();
        loop {
            if state.paused {
                bail!("write() called on a paused sink");
            }
            if state.autoplay {
                state.played.extend_from_slice(samples);
                return Ok(frames);
            }
            if !state.blocking || state.queued.len() + samples.len() <= capacity {
                break;
            }
            // A real blocking write waits for the DAC to consume audio when the queue is full.
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                bail!("write blocked for more than {BLOCKED_WRITE_TIMEOUT:?}: the test never advanced the sink");
            };
            state = self.shared.room.wait_timeout(state, remaining).unwrap().0;
        }

        // Non-blocking: when the queue is full, play the oldest audio to make room.
        let overflow = (state.queued.len() + samples.len()).saturating_sub(capacity);
        if overflow > 0 {
            let drained: Vec<i32> = state.queued.drain(..overflow).collect();
            state.played.extend(drained);
        }
        state.queued.extend(samples.iter().copied());
        Ok(frames)
    }

    fn delay_frames(&mut self) -> Result<u64> {
        Ok((self.lock().queued.len() / self.spec.channels as usize) as u64)
    }

    fn pause(&mut self) -> Result<()> {
        self.lock().paused = true;
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        self.lock().paused = false;
        self.shared.room.notify_all();
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        let mut state = self.lock();
        state.queued.clear();
        state.paused = false;
        state.flushes += 1;
        self.shared.room.notify_all();
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        let mut state = self.lock();
        state.paused = false;
        let drained: Vec<i32> = state.queued.drain(..).collect();
        state.played.extend(drained);
        state.drains += 1;
        self.shared.room.notify_all();
        Ok(())
    }
}

/// How the sinks a [`FakeSinkFactory`] opens behave.
#[derive(Clone, Copy)]
enum Mode {
    Autoplay,
    Blocking,
}

/// A [`SinkFactory`] that opens [`FakeSink`]s and keeps a handle to each, so tests can inspect
/// what the engine did (how many sinks it opened, and what each one played).
pub struct FakeSinkFactory {
    mode: Mode,
    handles: Mutex<Vec<FakeSinkHandle>>,
    releases: Mutex<u32>,
    /// Errors the next opens fail with, oldest first.
    open_failures: Mutex<VecDeque<String>>,
    handler: Mutex<Option<ReleaseHandler>>,
}

impl FakeSinkFactory {
    /// Sinks that play every write immediately.
    pub fn autoplay() -> Arc<Self> {
        Arc::new(Self::with_mode(Mode::Autoplay))
    }

    /// Sinks that block on a full queue until the test advances them.
    pub fn blocking() -> Arc<Self> {
        Arc::new(Self::with_mode(Mode::Blocking))
    }

    fn with_mode(mode: Mode) -> Self {
        Self {
            mode,
            handles: Mutex::new(Vec::new()),
            releases: Mutex::new(0),
            open_failures: Mutex::new(VecDeque::new()),
            handler: Mutex::new(None),
        }
    }

    /// One handle per sink opened so far, oldest first.
    pub fn handles(&self) -> Vec<FakeSinkHandle> {
        self.handles.lock().unwrap().clone()
    }

    /// How many times the engine gave the card back.
    pub fn release_count(&self) -> u32 {
        *self.releases.lock().unwrap()
    }

    /// Makes the next open fail with `message`, as a refused reservation would.
    pub fn fail_next_open(&self, message: &str) {
        self.open_failures.lock().unwrap().push_back(message.to_string());
    }

    /// Another program asking for the card, as the D-Bus side would relay it. Blocks until the
    /// engine answers, and returns whether it gave the card up. `false` too if no engine is
    /// listening.
    pub fn request_release(&self, by: Option<&str>, priority: i32) -> bool {
        let handler = self.handler.lock().unwrap().clone();
        handler.is_some_and(|handler| handler(ReleaseRequest { by: by.map(str::to_string), priority }))
    }
}

impl SinkFactory for FakeSinkFactory {
    fn open(&self, spec: SourceSpec) -> Result<Box<dyn AudioSink>> {
        if let Some(message) = self.open_failures.lock().unwrap().pop_front() {
            bail!("{message}");
        }
        let (sink, handle) = FakeSink::new(spec);
        match self.mode {
            Mode::Autoplay => handle.set_autoplay(true),
            Mode::Blocking => handle.set_blocking(true),
        }
        self.handles.lock().unwrap().push(handle);
        Ok(Box::new(sink))
    }

    fn release(&self) {
        *self.releases.lock().unwrap() += 1;
    }

    fn on_release_request(&self, handler: ReleaseHandler) {
        *self.handler.lock().unwrap() = Some(handler);
    }
}

/// A [`DeviceReserver`] that records what was reserved and released, with scripted outcomes.
pub struct FakeReserver {
    log: Arc<Mutex<Vec<String>>>,
    /// Outcomes of the next acquires, oldest first; once empty, acquires are granted (`took_over`
    /// false). `Ok(took_over)` grants the card.
    script: Mutex<VecDeque<Result<bool, ReserveError>>>,
}

impl FakeReserver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { log: Arc::new(Mutex::new(Vec::new())), script: Mutex::new(VecDeque::new()) })
    }

    pub fn script(&self, outcome: Result<bool, ReserveError>) {
        self.script.lock().unwrap().push_back(outcome);
    }

    /// `acquire N` / `release N`, in order.
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

struct FakeReservation {
    card: u32,
    took_over: bool,
    log: Arc<Mutex<Vec<String>>>,
}

impl Reservation for FakeReservation {
    fn took_over(&self) -> bool {
        self.took_over
    }
}

impl Drop for FakeReservation {
    fn drop(&mut self) {
        self.log.lock().unwrap().push(format!("release {}", self.card));
    }
}

impl DeviceReserver for FakeReserver {
    fn acquire(&self, card: u32, _device_name: &str) -> std::result::Result<Box<dyn Reservation>, ReserveError> {
        let outcome = self.script.lock().unwrap().pop_front().unwrap_or(Ok(false));
        let took_over = outcome?;
        self.log.lock().unwrap().push(format!("acquire {card}"));
        Ok(Box::new(FakeReservation { card, took_over, log: self.log.clone() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: SourceSpec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };

    /// `frames` stereo frames whose samples are unique and increasing, so any loss, duplication
    /// or reordering shows up in an equality check.
    fn ramp(from_frame: usize, frames: usize) -> Vec<i32> {
        (from_frame * 2..(from_frame + frames) * 2).map(|i| i as i32).collect()
    }

    fn write_all(sink: &mut FakeSink, samples: &[i32]) {
        let mut offset = 0;
        while offset < samples.len() {
            offset += sink.write(&samples[offset..]).unwrap() * SPEC.channels as usize;
        }
    }

    #[test]
    fn write_accepts_at_most_one_period() {
        let (mut sink, _handle) = FakeSink::with_sizes(SPEC, 100, 1000);
        assert_eq!(sink.write(&ramp(0, 250)).unwrap(), 100);
        assert_eq!(sink.write(&ramp(0, 30)).unwrap(), 30);
    }

    #[test]
    fn written_audio_is_queued_until_the_dac_plays_it() {
        let (mut sink, handle) = FakeSink::with_sizes(SPEC, 100, 1000);
        write_all(&mut sink, &ramp(0, 250));
        assert_eq!(sink.delay_frames().unwrap(), 250);
        assert!(handle.played().is_empty());

        handle.advance(100);
        assert_eq!(handle.played(), ramp(0, 100));
        assert_eq!(sink.delay_frames().unwrap(), 150);
    }

    #[test]
    fn overflowing_the_queue_plays_the_oldest_audio_like_a_blocking_write() {
        let (mut sink, handle) = FakeSink::with_sizes(SPEC, 100, 200);
        write_all(&mut sink, &ramp(0, 500));
        assert_eq!(sink.delay_frames().unwrap(), 200);
        assert_eq!(handle.played(), ramp(0, 300));
    }

    #[test]
    fn pause_then_resume_yields_an_identical_stream() {
        let (mut sink, handle) = FakeSink::with_sizes(SPEC, 100, 1000);
        write_all(&mut sink, &ramp(0, 300));
        handle.advance(120);

        sink.pause().unwrap();
        assert!(handle.is_paused());
        handle.advance(1000); // time passes; the DAC must not move while paused
        assert_eq!(handle.played(), ramp(0, 120));
        assert_eq!(sink.delay_frames().unwrap(), 180);

        sink.resume().unwrap();
        write_all(&mut sink, &ramp(300, 100));
        sink.drain().unwrap();
        assert_eq!(handle.played(), ramp(0, 400));
    }

    #[test]
    fn writing_while_paused_is_rejected() {
        let (mut sink, _handle) = FakeSink::new(SPEC);
        sink.pause().unwrap();
        assert!(sink.write(&ramp(0, 10)).is_err());
    }

    #[test]
    fn flush_discards_queued_audio_and_unpauses() {
        let (mut sink, handle) = FakeSink::with_sizes(SPEC, 100, 1000);
        write_all(&mut sink, &ramp(0, 300));
        handle.advance(50);
        sink.pause().unwrap();

        sink.flush().unwrap();
        assert_eq!(sink.delay_frames().unwrap(), 0);
        assert!(!handle.is_paused());
        assert_eq!(handle.flush_count(), 1);

        write_all(&mut sink, &ramp(1000, 50));
        sink.drain().unwrap();
        let mut expected = ramp(0, 50);
        expected.extend(ramp(1000, 50));
        assert_eq!(handle.played(), expected, "flushed audio must never be heard");
    }

    #[test]
    fn drain_plays_everything_queued() {
        let (mut sink, handle) = FakeSink::with_sizes(SPEC, 100, 1000);
        write_all(&mut sink, &ramp(0, 250));
        sink.drain().unwrap();
        assert_eq!(handle.played(), ramp(0, 250));
        assert_eq!(handle.drain_count(), 1);
        assert_eq!(handle.queued_frames(), 0);
    }

    #[test]
    fn autoplay_plays_writes_immediately() {
        let (mut sink, handle) = FakeSink::with_sizes(SPEC, 100, 1000);
        handle.set_autoplay(true);
        write_all(&mut sink, &ramp(0, 250));
        assert_eq!(sink.delay_frames().unwrap(), 0);
        assert_eq!(handle.played(), ramp(0, 250));
    }

    #[test]
    fn handle_observes_a_sink_that_moved_to_another_thread() {
        let (mut sink, handle) = FakeSink::with_sizes(SPEC, 100, 1000);
        handle.set_autoplay(true);
        std::thread::spawn(move || write_all(&mut sink, &ramp(0, 100))).join().unwrap();
        assert_eq!(handle.played(), ramp(0, 100));
    }

    #[test]
    fn blocking_write_waits_until_the_dac_makes_room() {
        let (mut sink, handle) = FakeSink::with_sizes(SPEC, 100, 200);
        handle.set_blocking(true);
        write_all(&mut sink, &ramp(0, 200)); // exactly fills the queue

        let writer = std::thread::spawn(move || {
            write_all(&mut sink, &ramp(200, 100));
            sink
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(!writer.is_finished(), "the write must be blocked on a full queue");
        assert_eq!(handle.queued_frames(), 200);

        handle.advance(100);
        let mut sink = writer.join().unwrap();
        sink.drain().unwrap();
        assert_eq!(handle.played(), ramp(0, 300));
    }

    #[test]
    fn factory_opens_sinks_in_the_requested_mode_and_keeps_their_handles() {
        let factory = FakeSinkFactory::autoplay();
        let mut sink = factory.open(SPEC).unwrap();
        sink.write(&ramp(0, 10)).unwrap();
        assert_eq!(factory.handles().len(), 1);
        assert_eq!(factory.handles()[0].played(), ramp(0, 10));

        factory.open(SPEC).unwrap();
        assert_eq!(factory.handles().len(), 2);
    }
}
