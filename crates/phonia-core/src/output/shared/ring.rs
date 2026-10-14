//! The buffer between the audio thread and the sound server.
//!
//! The engine writes to a sink with a blocking `write`, but a PulseAudio stream is pulled by the
//! server: it asks for audio when it has room. The ring joins the two. The writer copies whole
//! frames in and waits while the ring is full; the server's callback ([`RingSource`]) takes
//! whatever is there and, when there is nothing, says "not yet" instead of "end of stream", which
//! would close the stream for good.

use crate::output::OutputGone;
use anyhow::{Result, anyhow};
use pulseaudio::PlaybackSource;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

/// A full ring that the server does not draw from for this long means it has stopped listening.
const STALL: Duration = Duration::from_secs(5);
/// How often a blocked writer looks at whether the output is gone.
const WAKE_EVERY: Duration = Duration::from_millis(100);

#[derive(Default)]
struct State {
    bytes: VecDeque<u8>,
    waker: Option<Waker>,
    /// Bytes handed to the server so far.
    pulled: u64,
    /// The output is gone, and why.
    gone: Option<String>,
    closed: bool,
}

struct Inner {
    state: Mutex<State>,
    space: Condvar,
}

#[derive(Clone)]
pub struct Ring {
    inner: Arc<Inner>,
    frame_bytes: usize,
    channels: usize,
    capacity_bytes: usize,
    period_frames: usize,
}

impl Ring {
    /// `capacity_frames` is how much can wait in the ring; `period_frames` how much one `write`
    /// takes at most, so that a command sent meanwhile is noticed within a period.
    pub fn new(capacity_frames: usize, period_frames: usize, channels: usize) -> Self {
        let frame_bytes = channels * 4;
        Self {
            inner: Arc::new(Inner {
                state: Mutex::default(),
                space: Condvar::new(),
            }),
            frame_bytes,
            channels,
            capacity_bytes: capacity_frames * frame_bytes,
            period_frames,
        }
    }

    pub fn capacity_frames(&self) -> usize {
        self.capacity_bytes / self.frame_bytes
    }

    /// Copies up to one period of `samples` (interleaved `i32`) in, waiting for room. Returns the
    /// frames taken. Fails with [`OutputGone`] once the output has gone away or stopped listening.
    pub fn write(&self, samples: &[i32]) -> Result<usize> {
        let frames = (samples.len() / self.channels).min(self.period_frames);
        if frames == 0 {
            return Ok(0);
        }
        let needed = frames * self.frame_bytes;

        let mut state = self.inner.state.lock().unwrap();
        let mut last_progress = (state.pulled, Instant::now());
        loop {
            if let Some(why) = &state.gone {
                return Err(anyhow!(OutputGone(why.clone())));
            }
            if state.bytes.len() + needed <= self.capacity_bytes {
                break;
            }
            if state.pulled != last_progress.0 {
                last_progress = (state.pulled, Instant::now());
            } else if last_progress.1.elapsed() > STALL {
                return Err(anyhow!(OutputGone(
                    "the sound server stopped taking audio".to_string()
                )));
            }
            state = self.inner.space.wait_timeout(state, WAKE_EVERY).unwrap().0;
        }

        state.bytes.extend(
            samples[..frames * self.channels]
                .iter()
                .flat_map(|sample| sample.to_le_bytes()),
        );
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
        Ok(frames)
    }

    /// Adds silence, ignoring the capacity: a stream loses the first frames it is given, and this is
    /// what it loses instead.
    pub fn push_silence(&self, frames: usize) {
        let mut state = self.inner.state.lock().unwrap();
        state
            .bytes
            .extend(std::iter::repeat_n(0u8, frames * self.frame_bytes));
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// Frames waiting to be taken by the server.
    pub fn queued_frames(&self) -> usize {
        self.inner.state.lock().unwrap().bytes.len() / self.frame_bytes
    }

    /// Bytes the server has taken so far.
    pub fn pulled_bytes(&self) -> u64 {
        self.inner.state.lock().unwrap().pulled
    }

    /// Throws away what is waiting.
    pub fn clear(&self) {
        self.inner.state.lock().unwrap().bytes.clear();
        self.inner.space.notify_all();
    }

    /// The output is gone: writers stop waiting and fail with [`OutputGone`].
    pub fn mark_gone(&self, why: &str) {
        let mut state = self.inner.state.lock().unwrap();
        state.gone.get_or_insert_with(|| why.to_string());
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
        self.inner.space.notify_all();
    }

    pub fn gone(&self) -> Option<String> {
        self.inner.state.lock().unwrap().gone.clone()
    }

    /// Nobody uses the ring any more: whatever watches the output can stop.
    pub fn close(&self) {
        self.inner.state.lock().unwrap().closed = true;
        self.inner.space.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.inner.state.lock().unwrap().closed
    }

    /// What the server draws from.
    pub fn source(&self) -> RingSource {
        RingSource { ring: self.clone() }
    }
}

pub struct RingSource {
    ring: Ring,
}

impl PlaybackSource for RingSource {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<usize> {
        let ring = &self.ring;
        let mut state = ring.inner.state.lock().unwrap();
        let take = buf.len().min(state.bytes.len()) / ring.frame_bytes * ring.frame_bytes;
        if take == 0 {
            // Nothing yet. Never `Ready(0)`: that means the stream is over.
            state.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        for (slot, byte) in buf.iter_mut().zip(state.bytes.drain(..take)) {
            *slot = byte;
        }
        state.pulled += take as u64;
        drop(state);
        ring.inner.space.notify_all();
        Poll::Ready(take)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    struct CountingWaker(AtomicUsize);
    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn poll(ring: &Ring, wanted: usize) -> (Poll<usize>, Vec<u8>, Arc<CountingWaker>) {
        let counter = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(counter.clone());
        let mut cx = std::task::Context::from_waker(&waker);
        let mut source = ring.source();
        let mut buf = vec![0u8; wanted];
        let result = Pin::new(&mut source).poll_read(&mut cx, &mut buf);
        if let Poll::Ready(n) = result {
            buf.truncate(n);
        }
        (result, buf, counter)
    }

    fn samples(from: i32, count: usize) -> Vec<i32> {
        (from..from + count as i32).collect()
    }

    #[test]
    fn what_is_written_is_what_the_server_reads_in_order() {
        let ring = Ring::new(100, 10, 2);
        assert_eq!(ring.write(&samples(0, 8)).unwrap(), 4);
        let (result, bytes, _) = poll(&ring, 1000);
        assert_eq!(result, Poll::Ready(32));
        let read: Vec<i32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes(*c))
            .collect();
        assert_eq!(read, samples(0, 8));
        assert_eq!(ring.pulled_bytes(), 32);
    }

    #[test]
    fn a_write_takes_at_most_one_period() {
        let ring = Ring::new(100, 10, 2);
        assert_eq!(ring.write(&samples(0, 100)).unwrap(), 10);
        assert_eq!(ring.queued_frames(), 10);
    }

    #[test]
    fn the_server_is_told_to_wait_not_that_the_stream_ended() {
        let ring = Ring::new(100, 10, 2);
        let (result, _, counter) = poll(&ring, 64);
        assert_eq!(result, Poll::Pending);
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);

        ring.write(&samples(0, 4)).unwrap();
        assert_eq!(
            counter.0.load(Ordering::SeqCst),
            1,
            "writing wakes the server's side"
        );
    }

    #[test]
    fn the_server_only_takes_whole_frames() {
        let ring = Ring::new(100, 10, 2);
        ring.write(&samples(0, 8)).unwrap();
        let (result, _, _) = poll(&ring, 30); // 3.75 frames of 8 bytes
        assert_eq!(result, Poll::Ready(24));
    }

    #[test]
    fn a_full_ring_blocks_the_writer_until_the_server_reads() {
        let ring = Ring::new(10, 10, 2);
        assert_eq!(ring.write(&samples(0, 20)).unwrap(), 10); // fills it
        let writer = {
            let ring = ring.clone();
            std::thread::spawn(move || ring.write(&samples(100, 20)).unwrap())
        };
        std::thread::sleep(Duration::from_millis(150));
        assert!(!writer.is_finished(), "no room: the writer waits");

        let (result, _, _) = poll(&ring, 1000);
        assert_eq!(result, Poll::Ready(80));
        assert_eq!(writer.join().unwrap(), 10, "the reader made room");
    }

    #[test]
    fn a_writer_waiting_for_room_is_released_with_an_error_when_the_output_goes() {
        let ring = Ring::new(10, 10, 2);
        ring.write(&samples(0, 20)).unwrap();
        let writer = {
            let ring = ring.clone();
            std::thread::spawn(move || ring.write(&samples(0, 20)))
        };
        std::thread::sleep(Duration::from_millis(100));
        ring.mark_gone("the speaker went away");

        let error = writer.join().unwrap().unwrap_err();
        let gone = error.downcast_ref::<OutputGone>().expect("a typed error");
        assert_eq!(gone.0, "the speaker went away");
        assert!(ring.write(&samples(0, 2)).is_err(), "and it stays gone");
        assert_eq!(ring.gone().as_deref(), Some("the speaker went away"));
    }

    #[test]
    fn clearing_throws_away_what_was_not_taken_and_makes_room() {
        let ring = Ring::new(10, 10, 2);
        ring.write(&samples(0, 20)).unwrap();
        ring.clear();
        assert_eq!(ring.queued_frames(), 0);
        assert_eq!(ring.write(&samples(0, 20)).unwrap(), 10);
    }

    #[test]
    fn silence_is_added_beyond_the_capacity() {
        let ring = Ring::new(10, 10, 2);
        ring.push_silence(25);
        assert_eq!(ring.queued_frames(), 25);
        let (result, bytes, _) = poll(&ring, 1000);
        assert_eq!(result, Poll::Ready(200));
        assert!(bytes.iter().all(|b| *b == 0));
    }

    #[test]
    fn closing_is_visible_to_whoever_watches() {
        let ring = Ring::new(10, 10, 2);
        assert!(!ring.is_closed());
        ring.close();
        assert!(ring.is_closed());
    }
}
