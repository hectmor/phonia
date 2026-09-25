//! Shared mode: playing through the desktop's sound server (PipeWire, or PulseAudio) instead of
//! taking a sound card for phonia alone.
//!
//! Nothing here is bit-perfect: the server mixes phonia with everything else, converts and, unless
//! the sink runs at the source's rate, resamples. What it buys is that phonia can use outputs that
//! are not `hw:` cards (Bluetooth, HDMI, the laptop's speakers), share a card with other programs,
//! and be moved between them.
//!
//! [`SharedSink`] is the [`AudioSink`] over it. It is generic over a [`Transport`] (the few calls
//! that reach the server: pause, flush, how much it holds) so that everything except talking to
//! a real server is tested with a fake, and the real one lives in [`pulse`].

pub mod pulse;
pub mod ring;

use super::alsa::{ReportHandler, SinkReport};
use super::AudioSink;
use crate::decode::SourceSpec;
use anyhow::{Result, bail};
use ring::Ring;
use std::time::{Duration, Instant};

/// How long a drain waits for the server to play what it was given, beyond what it holds.
const DRAIN_SLACK: Duration = Duration::from_secs(10);
const DRAIN_POLL: Duration = Duration::from_millis(20);

/// What the server says about a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timing {
    /// Bytes the server holds that have not reached the sink yet.
    pub queued_bytes: u64,
    /// Bytes the stream has been given since it began.
    pub write_bytes: u64,
    /// Bytes the sink has taken from the stream since it began.
    pub read_bytes: u64,
    /// How long audio takes from the sink's input to the speaker.
    pub sink_latency: Duration,
}

/// The calls that reach the sound server, from the audio thread.
pub trait Transport: Send {
    /// Stops (or restarts) the server taking audio from the stream. What it holds is kept.
    fn cork(&mut self, corked: bool) -> Result<()>;
    /// Throws away what the server holds.
    fn flush(&mut self) -> Result<()>;
    fn timing(&mut self) -> Result<Timing>;
}

pub struct SharedSink<T: Transport> {
    spec: SourceSpec,
    ring: Ring,
    transport: T,
    /// How much silence goes in front of audio that follows a stop (see `needs_silence`).
    silence_frames: u64,
    /// Where, counted in frames from the start of the stream, the latest silence ends: up to there
    /// what the server plays is not audio the engine wrote, and does not count as yet to be heard.
    silence_end: u64,
    /// The server drops the first stretch of audio after the stream starts, restarts after
    /// running dry, or is flushed. Silence goes in first so that it is what gets dropped.
    needs_silence: bool,
    /// What the server is asked to hold, for [`AudioSink::capacity_frames`].
    server_target_frames: u64,
    /// Told what the sink is doing when the first audio goes in.
    report: Option<(ReportHandler, SinkReport)>,
}

impl<T: Transport> SharedSink<T> {
    /// `ring` already has the silence for the start of the stream (`silence_frames` of it) in it.
    pub fn new(
        spec: SourceSpec,
        ring: Ring,
        transport: T,
        silence_frames: u64,
        server_target_frames: u64,
        report: Option<(ReportHandler, SinkReport)>,
    ) -> Self {
        Self {
            spec,
            ring,
            transport,
            silence_frames,
            silence_end: silence_frames,
            needs_silence: false,
            server_target_frames,
            report,
        }
    }

    /// Puts silence in front of what is about to be written after a stop.
    fn add_silence(&mut self) {
        let waiting = self.ring.queued_frames() as u64;
        if let Ok(timing) = self.transport.timing() {
            self.silence_end = timing.write_bytes / self.frame_bytes() + waiting + self.silence_frames;
        }
        self.ring.push_silence(self.silence_frames as usize);
        self.needs_silence = false;
    }

    fn frame_bytes(&self) -> u64 {
        u64::from(self.spec.channels) * 4
    }
}

impl<T: Transport> Drop for SharedSink<T> {
    fn drop(&mut self) {
        self.ring.close();
    }
}

impl<T: Transport> AudioSink for SharedSink<T> {
    fn spec(&self) -> SourceSpec {
        self.spec
    }

    /// What can be in flight at most (the ring and what the server holds), plus a second for the
    /// latency of the sink, so that the engine keeps enough to replay all of it.
    fn capacity_frames(&self) -> u64 {
        self.ring.capacity_frames() as u64 + self.server_target_frames + u64::from(self.spec.sample_rate)
    }

    fn write(&mut self, samples: &[i32]) -> Result<usize> {
        if self.needs_silence {
            self.add_silence();
        }
        let frames = self.ring.write(samples)?;
        if frames > 0
            && let Some((handler, report)) = self.report.take()
        {
            handler(report);
        }
        Ok(frames)
    }

    fn delay_frames(&mut self) -> Result<u64> {
        let waiting = self.ring.queued_frames() as u64;
        let Ok(timing) = self.transport.timing() else { return Ok(waiting) };

        let frame = self.frame_bytes();
        let unplayed = waiting + timing.queued_bytes / frame;
        let silence_left = self.silence_end.saturating_sub(timing.read_bytes / frame);
        let latency = (timing.sink_latency.as_secs_f64() * f64::from(self.spec.sample_rate)) as u64;
        Ok(unplayed.saturating_sub(silence_left) + latency)
    }

    fn pause(&mut self) -> Result<()> {
        self.transport.cork(true)
    }

    fn resume(&mut self) -> Result<()> {
        self.transport.cork(false)
    }

    fn flush(&mut self) -> Result<()> {
        self.ring.clear();
        // Whatever silence was left went with the flush, and the stream restarts from empty.
        self.silence_end = 0;
        self.needs_silence = true;
        self.transport.flush()?;
        // The stream may have been corked by a pause: flushing leaves it ready for new audio.
        self.transport.cork(false)
    }

    /// Waits until everything written has been played, without ending the stream: the engine
    /// writes the next track to the same sink.
    fn drain(&mut self) -> Result<()> {
        self.transport.cork(false)?;
        let frame = self.frame_bytes();
        let deadline = Instant::now() + DRAIN_SLACK;
        let latency = loop {
            if let Some(why) = self.ring.gone() {
                bail!(super::OutputGone(why));
            }
            let timing = self.transport.timing()?;
            if self.ring.queued_frames() == 0 && timing.queued_bytes < frame {
                break timing.sink_latency;
            }
            if Instant::now() > deadline {
                bail!("the sound server did not finish playing within {} s", DRAIN_SLACK.as_secs());
            }
            std::thread::sleep(DRAIN_POLL);
        };
        // The last frames are still on their way from the sink to the speaker.
        std::thread::sleep(latency);
        // The stream is dry now, and will restart from empty.
        self.needs_silence = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const SPEC: SourceSpec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };

    #[derive(Clone, Default)]
    struct Fake {
        log: Arc<Mutex<Vec<String>>>,
        timing: Arc<Mutex<Timing>>,
        fail_timing: Arc<Mutex<bool>>,
    }

    impl Transport for Fake {
        fn cork(&mut self, corked: bool) -> Result<()> {
            self.log.lock().unwrap().push(format!("cork {corked}"));
            Ok(())
        }
        fn flush(&mut self) -> Result<()> {
            self.log.lock().unwrap().push("flush".into());
            Ok(())
        }
        fn timing(&mut self) -> Result<Timing> {
            if *self.fail_timing.lock().unwrap() {
                bail!("no answer");
            }
            Ok(*self.timing.lock().unwrap())
        }
    }

    fn sink(prefix: u64) -> (SharedSink<Fake>, Fake, Ring) {
        let ring = Ring::new(4_800, 480, 2);
        ring.push_silence(prefix as usize);
        let fake = Fake::default();
        (SharedSink::new(SPEC, ring.clone(), fake.clone(), prefix, 19_200, None), fake, ring)
    }

    fn frames(count: usize) -> Vec<i32> {
        vec![1; count * 2]
    }

    #[test]
    fn a_write_takes_a_period_and_the_delay_is_what_the_ring_and_the_server_hold() {
        let (mut sink, fake, _) = sink(0);
        assert_eq!(sink.write(&frames(1_000)).unwrap(), 480);
        assert_eq!(sink.delay_frames().unwrap(), 480);

        // The server took 200 frames of it and holds them, 100 more reached the sink, and the sink
        // adds 10 ms.
        *fake.timing.lock().unwrap() =
            Timing { queued_bytes: 200 * 8, read_bytes: 100 * 8, sink_latency: Duration::from_millis(10), ..Timing::default() };
        assert_eq!(sink.delay_frames().unwrap(), 480 + 200 + 480);
    }

    #[test]
    fn silence_at_the_start_is_not_audio_waiting_to_be_heard() {
        let (mut sink, fake, _) = sink(1_000);
        sink.write(&frames(480)).unwrap();
        assert_eq!(sink.delay_frames().unwrap(), 480, "the 1000 silent frames don't count");

        // Half of the silence has been played; the rest still is not audio.
        *fake.timing.lock().unwrap() = Timing { queued_bytes: 0, read_bytes: 500 * 8, ..Timing::default() };
        assert_eq!(sink.delay_frames().unwrap(), 1_480 - 500);
        // All of it played and some audio too.
        *fake.timing.lock().unwrap() = Timing { queued_bytes: 0, read_bytes: 1_200 * 8, ..Timing::default() };
        assert_eq!(sink.delay_frames().unwrap(), 1_480, "the ring still holds everything, nothing was pulled");
    }

    #[test]
    fn after_a_drain_the_next_audio_is_preceded_by_silence_and_the_position_ignores_it() {
        let (mut sink, fake, ring) = sink(1_000);
        ring.clear(); // the server took the silence that opened the stream
        sink.drain().unwrap();
        // The server has been given, and has played, 5000 frames so far.
        *fake.timing.lock().unwrap() = Timing { write_bytes: 5_000 * 8, read_bytes: 5_000 * 8, ..Timing::default() };

        sink.write(&frames(480)).unwrap();
        assert_eq!(ring.queued_frames(), 1_000 + 480, "the silence went in first");

        // The server has now played 500 of the silence and holds the rest, and the audio.
        *fake.timing.lock().unwrap() =
            Timing { write_bytes: 6_480 * 8, read_bytes: 5_500 * 8, queued_bytes: 980 * 8, ..Timing::default() };
        // 500 silent frames are still to play; they are not audio waiting to be heard.
        assert_eq!(sink.delay_frames().unwrap(), 1_480 + 980 - 500);
    }

    #[test]
    fn after_a_flush_the_next_audio_is_preceded_by_silence_too() {
        let (mut sink, _, ring) = sink(1_000);
        ring.clear();
        sink.flush().unwrap();
        sink.write(&frames(480)).unwrap();
        assert_eq!(ring.queued_frames(), 1_000 + 480);
    }

    #[test]
    fn only_the_first_write_after_a_stop_brings_silence() {
        let (mut sink, _, ring) = sink(1_000);
        ring.clear();
        sink.drain().unwrap();
        sink.write(&frames(480)).unwrap();
        sink.write(&frames(480)).unwrap();
        assert_eq!(ring.queued_frames(), 1_000 + 480 + 480);
    }

    #[test]
    fn a_server_that_does_not_answer_leaves_only_what_the_ring_holds() {
        let (mut sink, fake, _) = sink(0);
        sink.write(&frames(100)).unwrap();
        *fake.fail_timing.lock().unwrap() = true;
        assert_eq!(sink.delay_frames().unwrap(), 100);
    }

    #[test]
    fn pause_and_resume_cork_and_uncork_the_stream() {
        let (mut sink, fake, _) = sink(0);
        sink.pause().unwrap();
        sink.resume().unwrap();
        assert_eq!(*fake.log.lock().unwrap(), ["cork true", "cork false"]);
    }

    #[test]
    fn flushing_empties_the_ring_and_the_server_and_leaves_the_stream_running() {
        let (mut sink, fake, ring) = sink(0);
        sink.write(&frames(100)).unwrap();
        sink.pause().unwrap();
        sink.flush().unwrap();
        assert_eq!(ring.queued_frames(), 0);
        assert_eq!(*fake.log.lock().unwrap(), ["cork true", "flush", "cork false"]);
    }

    #[test]
    fn draining_waits_for_the_ring_and_the_server_to_empty_and_keeps_the_stream() {
        let (mut sink, fake, ring) = sink(0);
        sink.write(&frames(100)).unwrap();
        *fake.timing.lock().unwrap() = Timing { queued_bytes: 800, ..Timing::default() };

        let releaser = {
            let (fake, ring) = (fake.clone(), ring.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                ring.clear(); // the server took it
                *fake.timing.lock().unwrap() = Timing::default(); // and played it
            })
        };
        let started = Instant::now();
        sink.drain().unwrap();
        assert!(started.elapsed() >= Duration::from_millis(90), "it waited for the audio to be played");
        releaser.join().unwrap();

        assert_eq!(fake.log.lock().unwrap().first().map(String::as_str), Some("cork false"), "a paused stream never drains");
        assert!(sink.write(&frames(10)).is_ok(), "the sink is still usable afterwards");
    }

    #[test]
    fn a_lost_output_surfaces_from_write_and_drain_as_a_typed_error() {
        let (mut sink, _, ring) = sink(0);
        ring.mark_gone("the speaker went away");
        for error in [sink.write(&frames(10)).unwrap_err(), sink.drain().unwrap_err()] {
            assert_eq!(error.downcast_ref::<super::super::OutputGone>().unwrap().0, "the speaker went away");
        }
    }

    #[test]
    fn the_capacity_covers_the_ring_the_server_and_a_second_of_latency() {
        let (sink, _, _) = sink(0);
        assert_eq!(sink.capacity_frames(), 4_800 + 19_200 + 48_000);
    }

    #[test]
    fn the_report_is_given_when_the_first_audio_goes_in_and_only_once() {
        use crate::output::alsa::ProcReading;
        let ring = Ring::new(4_800, 480, 2);
        let reports = Arc::new(Mutex::new(0));
        let counter = reports.clone();
        let handler: ReportHandler = Arc::new(move |_| *counter.lock().unwrap() += 1);
        let report = SinkReport::new("x".into(), SPEC, "S32LE".into(), ProcReading::NotHw);
        let mut sink = SharedSink::new(SPEC, ring, Fake::default(), 0, 0, Some((handler, report)));

        assert_eq!(*reports.lock().unwrap(), 0);
        sink.write(&frames(10)).unwrap();
        sink.write(&frames(10)).unwrap();
        assert_eq!(*reports.lock().unwrap(), 1);
    }

    #[test]
    fn dropping_the_sink_tells_whatever_watches_the_output_to_stop() {
        let (sink, _, ring) = sink(0);
        drop(sink);
        assert!(ring.is_closed());
    }
}
