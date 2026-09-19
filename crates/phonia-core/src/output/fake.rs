//! An in-memory [`AudioSink`] for tests: no hardware, no timing.
//!
//! It models a device with a bounded queue. Written audio sits in the queue until it is "played",
//! either explicitly with [`FakeSinkHandle::advance`] or automatically when the queue overflows
//! (which is what a blocking `writei` does: it waits for the DAC to consume audio). Everything
//! that reaches the "DAC" is recorded, so tests can assert on the exact sample stream, including
//! across pauses, flushes and seeks.

use super::AudioSink;
use crate::decode::SourceSpec;
use anyhow::{Result, bail};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

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
    flushes: u32,
    drains: u32,
}

pub struct FakeSink {
    spec: SourceSpec,
    period_frames: usize,
    capacity_frames: usize,
    state: Arc<Mutex<State>>,
}

/// A view onto a [`FakeSink`] that stays usable from the test thread while the sink itself has
/// been moved into the audio thread.
#[derive(Clone)]
pub struct FakeSinkHandle {
    spec: SourceSpec,
    state: Arc<Mutex<State>>,
}

impl FakeSink {
    /// A sink with a 1024-frame period and a 4096-frame queue, in manual mode: nothing is played
    /// until [`FakeSinkHandle::advance`] is called (or the queue overflows).
    pub fn new(spec: SourceSpec) -> (Self, FakeSinkHandle) {
        Self::with_sizes(spec, DEFAULT_PERIOD_FRAMES, DEFAULT_CAPACITY_FRAMES)
    }

    pub fn with_sizes(spec: SourceSpec, period_frames: usize, capacity_frames: usize) -> (Self, FakeSinkHandle) {
        assert!(period_frames > 0 && capacity_frames >= period_frames);
        let state = Arc::new(Mutex::new(State::default()));
        let handle = FakeSinkHandle { spec, state: state.clone() };
        (Self { spec, period_frames, capacity_frames, state }, handle)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

impl FakeSinkHandle {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
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

    fn write(&mut self, samples: &[i32]) -> Result<usize> {
        let channels = self.spec.channels as usize;
        let frames = (samples.len() / channels).min(self.period_frames);
        let samples = &samples[..frames * channels];

        let mut state = self.lock();
        if state.paused {
            bail!("write() called on a paused sink");
        }

        if state.autoplay {
            state.played.extend_from_slice(samples);
            return Ok(frames);
        }

        // A real blocking write waits for the DAC to consume audio when the queue is full.
        let overflow = (state.queued.len() + samples.len()).saturating_sub(self.capacity_frames * channels);
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
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        let mut state = self.lock();
        state.queued.clear();
        state.paused = false;
        state.flushes += 1;
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        let mut state = self.lock();
        state.paused = false;
        let drained: Vec<i32> = state.queued.drain(..).collect();
        state.played.extend(drained);
        state.drains += 1;
        Ok(())
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
}
