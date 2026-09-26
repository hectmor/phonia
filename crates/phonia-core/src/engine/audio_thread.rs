//! The audio thread: owns the sink and the decoder and runs the engine's state machine.
//!
//! It is a plain OS thread, never a tokio task: writing to the sink and reading a
//! `SegmentStream` both block, and blocking a runtime worker would stall everything else on it.
//! Everything that can be slow (asking TIDAL for a track) runs on the runtime and comes back as a
//! message, so this loop has a single place to wait.

use super::PREVIOUS_RESTART_AFTER;
use super::supplier::{Advance, LoadedTrack, SeekMode, TrackMedia, TrackSupplier};
use super::types::{
    Command, EndReason, Event, OutputState, ReleaseReason, SeekTarget, State, Status, TrackMeta,
    TrackRef,
};
use crate::decode::{Decoder, SourceSpec, duration_to_frames, frames_to_duration};
use crate::output::{AudioSink, OutputGone, ReleaseHandler, SinkFactory};
use anyhow::{Context as _, Result, anyhow, bail};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

/// Frames handed to the sink per chunk when the media is already raw PCM.
const RAW_CHUNK_FRAMES: usize = 4096;

/// A loaded track whose decoder is already open. Probing a stream (the first segments of a TIDAL
/// track have to arrive) can take a while, so it is done by the task that loads the track, and the
/// audio thread only ever starts a track that is ready.
pub(super) struct Prepared {
    meta: TrackMeta,
    source: Source,
    seek: SeekMode,
    start: Duration,
}

impl Prepared {
    /// A track of raw samples, ready to start, for tests that hand the audio thread a result.
    #[cfg(test)]
    pub(super) fn for_tests(meta: TrackMeta, samples: Vec<i32>, spec: SourceSpec) -> Self {
        Self {
            meta,
            source: Source::Raw {
                samples,
                next: 0,
                spec,
            },
            seek: SeekMode::None,
            start: Duration::ZERO,
        }
    }
}

/// Opens the decoder of a loaded track. Blocks while the stream is probed, so it runs on a
/// blocking thread, never on the audio thread or a runtime worker.
async fn prepare(track: LoadedTrack) -> Result<Prepared> {
    let LoadedTrack {
        meta,
        media,
        seek,
        start,
    } = track;
    let source = tokio::task::spawn_blocking(move || open_source(media))
        .await
        .map_err(|error| anyhow!("opening the decoder was interrupted: {error}"))??;
    Ok(Prepared {
        meta,
        source,
        seek,
        start,
    })
}

fn open_source(media: TrackMedia) -> Result<Source> {
    Ok(match media {
        TrackMedia::Encoded { source, extension } => Source::Decoder(
            Decoder::open_boxed(source, extension.as_deref()).context("opening the decoder")?,
        ),
        TrackMedia::RawPcm { samples, spec } => Source::Raw {
            samples,
            next: 0,
            spec,
        },
    })
}

/// Everything the audio thread can be told, from callers and from its own load tasks.
pub(super) enum Msg {
    Command(Command),
    Loaded {
        generation: u64,
        track: Box<Prepared>,
    },
    LoadFailed {
        generation: u64,
        error: String,
    },
    QueueExhausted {
        generation: u64,
    },
    /// Another program wants the audio device; `done` gets whether the engine gave it up.
    ReleaseRequested {
        by: Option<String>,
        done: Sender<bool>,
    },
    /// Play through another output from now on; `done` gets whether that worked.
    SetOutput {
        sinks: Arc<dyn SinkFactory>,
        done: Sender<Result<(), String>>,
    },
    Shutdown,
}

pub(super) struct Context {
    pub rx: Receiver<Msg>,
    pub tx: Sender<Msg>,
    pub rt: Handle,
    pub sinks: Arc<dyn SinkFactory>,
    /// Answers other programs that ask for the audio device; every output the engine plays on has
    /// it installed.
    pub release_handler: ReleaseHandler,
    pub supplier: Arc<dyn TrackSupplier>,
    pub events: broadcast::Sender<Event>,
    pub status: watch::Sender<Status>,
    pub position_interval: Duration,
    pub release_after_pause: Option<Duration>,
}

pub(super) fn run(ctx: Context) {
    AudioThread {
        ctx,
        state: State::Stopped,
        generation: 0,
        load_task: None,
        pause_when_ready: false,
        sink: None,
        released: None,
        release_when_ready: None,
        paused_at: None,
        current: None,
    }
    .run();
}

enum LoadTarget {
    Track(TrackRef),
    Advance(Advance),
}

enum Source {
    Decoder(Decoder),
    Raw {
        samples: Vec<i32>,
        next: usize,
        spec: SourceSpec,
    },
}

impl Source {
    fn spec(&self) -> SourceSpec {
        match self {
            Source::Decoder(decoder) => decoder.spec(),
            Source::Raw { spec, .. } => *spec,
        }
    }

    fn duration(&self) -> Option<Duration> {
        match self {
            Source::Decoder(decoder) => decoder.duration(),
            Source::Raw { samples, spec, .. } => Some(frames_to_duration(
                (samples.len() / spec.channels as usize) as u64,
                spec.sample_rate,
            )),
        }
    }

    /// Repositions the source, returning where it actually landed.
    fn seek(&mut self, at: Duration) -> Result<Duration> {
        match self {
            Source::Decoder(decoder) => decoder.seek(at),
            Source::Raw {
                samples,
                next,
                spec,
            } => {
                let total_frames = (samples.len() / spec.channels as usize) as u64;
                let frame = duration_to_frames(at, spec.sample_rate).min(total_frames);
                *next = frame as usize * spec.channels as usize;
                Ok(frames_to_duration(frame, spec.sample_rate))
            }
        }
    }

    /// Drops the next `frames` frames, on top of any drop already pending.
    fn skip_frames(&mut self, frames: u64) {
        match self {
            Source::Decoder(decoder) => decoder.skip_frames(frames),
            Source::Raw {
                samples,
                next,
                spec,
            } => {
                let skipped = (frames as usize).saturating_mul(spec.channels as usize);
                *next = next.saturating_add(skipped).min(samples.len());
            }
        }
    }

    /// Replaces `out` with the next chunk of interleaved samples; `false` at the end.
    fn next_chunk_into(&mut self, out: &mut Vec<i32>) -> Result<bool> {
        match self {
            Source::Decoder(decoder) => decoder.next_chunk_into(out),
            Source::Raw {
                samples,
                next,
                spec,
            } => {
                if *next >= samples.len() {
                    return Ok(false);
                }
                let end = (*next + RAW_CHUNK_FRAMES * spec.channels as usize).min(samples.len());
                out.clear();
                out.extend_from_slice(&samples[*next..end]);
                *next = end;
                Ok(true)
            }
        }
    }
}

/// The track being played and how far into it we are.
struct Playing {
    meta: TrackMeta,
    spec: SourceSpec,
    /// `None` while the track is being reopened for a seek.
    source: Option<Source>,
    seek: SeekMode,
    /// The chunk being written to the sink, of which `offset` samples are already in.
    pending: Vec<i32>,
    offset: usize,
    /// The most recent samples handed to the sink (at least what it can hold queued), so that
    /// what the listener has not heard yet can be played again after the device was closed.
    recent: Vec<i32>,
    /// Whether the sink itself was paused. A track that starts paused never touched the sink,
    /// so there is nothing to resume there.
    sink_paused: bool,
    /// Frames the sink has accepted since the track started or was last repositioned.
    frames_written: u64,
    /// Where in the track the first of those frames sits: zero at the start, the target after a
    /// seek. Position is this plus what has been heard since.
    base: Duration,
    duration: Option<Duration>,
    /// What the listener had heard when the position was last reported.
    position: Duration,
    last_position_at: Instant,
}

struct AudioThread {
    ctx: Context,
    state: State,
    /// Bumped by every load request and by `stop`. A load result carrying an older number
    /// answers a question nobody is asking any more and is dropped.
    generation: u64,
    load_task: Option<JoinHandle<()>>,
    /// A `Pause` arrived while the track was still loading: start it paused.
    pause_when_ready: bool,
    /// Kept open between tracks of the same format, so there is no gap or click.
    sink: Option<Box<dyn AudioSink>>,
    /// The device was handed back while a track is loaded: `Some(who asked)`.
    released: Option<Option<String>>,
    /// A release asked for while a track was loading: done as soon as it starts, paused.
    release_when_ready: Option<Release>,
    /// When the current pause began.
    paused_at: Option<Instant>,
    current: Option<Playing>,
}

/// A request to hand the audio device back.
struct Release {
    reason: ReleaseReason,
    by: Option<String>,
    /// Told whether the device was given up, when another program is waiting for the answer.
    done: Option<Sender<bool>>,
}

impl Release {
    fn answer(self, given_up: bool) {
        if let Some(done) = self.done {
            let _ = done.send(given_up);
        }
    }
}

impl AudioThread {
    fn run(mut self) {
        loop {
            // While playing, poll for messages between writes; otherwise (stopped, loading or
            // paused) sleep until one comes instead of spinning.
            let msg = if self.state == State::Playing {
                match self.ctx.rx.try_recv() {
                    Ok(msg) => Some(msg),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => break,
                }
            } else if let Some(deadline) = self.idle_deadline() {
                match self
                    .ctx
                    .rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(msg) => Some(msg),
                    Err(RecvTimeoutError::Timeout) => {
                        self.release_output(Release {
                            reason: ReleaseReason::Idle,
                            by: None,
                            done: None,
                        });
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match self.ctx.rx.recv() {
                    Ok(msg) => Some(msg),
                    Err(_) => break,
                }
            };

            match msg {
                Some(msg) => {
                    if !self.handle(msg) {
                        break;
                    }
                }
                None => {
                    if let Err(error) = self.play_step() {
                        self.failed(error);
                    }
                }
            }
        }
        self.stop();
    }

    /// When a pause with the device still open has lasted long enough to give the device back.
    fn idle_deadline(&self) -> Option<Instant> {
        if self.state != State::Paused || self.sink.is_none() {
            return None;
        }
        Some(self.paused_at? + self.ctx.release_after_pause?)
    }

    /// Returns `false` when the thread should exit.
    fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Command(Command::Play(Some(track))) => {
                self.interrupt_current();
                self.request_load(LoadTarget::Track(track));
            }
            Msg::Command(Command::Play(None)) => {
                if self.state == State::Stopped {
                    self.request_load(LoadTarget::Advance(Advance::Auto));
                }
            }
            Msg::Command(Command::Next) => {
                self.interrupt_current();
                self.request_load(LoadTarget::Advance(Advance::Next));
            }
            Msg::Command(Command::Previous) => {
                let restart = self.heard_position() >= PREVIOUS_RESTART_AFTER;
                self.interrupt_current();
                self.request_load(LoadTarget::Advance(if restart {
                    Advance::Restart
                } else {
                    Advance::Previous
                }));
            }
            Msg::Command(Command::Stop) => self.stop(),
            Msg::Command(Command::Pause) => self.pause(),
            Msg::Command(Command::Resume) => self.resume(),
            Msg::Command(Command::TogglePause) => match self.state {
                State::Playing | State::Loading | State::Seeking if !self.pause_when_ready => {
                    self.pause()
                }
                _ => self.resume(),
            },
            Msg::Command(Command::Seek(target)) => self.seek(target),
            Msg::Command(Command::Release) => self.release_output(Release {
                reason: ReleaseReason::Command,
                by: None,
                done: None,
            }),
            Msg::ReleaseRequested { by, done } => self.release_output(Release {
                reason: ReleaseReason::Requested,
                by,
                done: Some(done),
            }),
            Msg::Loaded { generation, track } if self.is_awaited(generation) => {
                let result = if self.state == State::Seeking {
                    self.finish_seek(*track)
                } else {
                    self.start_track(*track)
                };
                if let Err(error) = result {
                    self.fail(error);
                }
            }
            Msg::LoadFailed { generation, error } if self.is_awaited(generation) => {
                self.fail(anyhow!(error));
            }
            Msg::QueueExhausted { generation } if self.is_awaited(generation) => {
                self.emit(Event::QueueExhausted);
                self.stop();
            }
            Msg::Loaded { .. } | Msg::LoadFailed { .. } | Msg::QueueExhausted { .. } => {}
            Msg::SetOutput { sinks, done } => self.set_output(sinks, done),
            Msg::Shutdown => {
                self.stop();
                return false;
            }
        }
        true
    }

    fn is_awaited(&self, generation: u64) -> bool {
        matches!(self.state, State::Loading | State::Seeking) && generation == self.generation
    }

    /// Writes at most one period of the current track to the sink.
    fn play_step(&mut self) -> Result<()> {
        let Some(playing) = self.current.as_mut() else {
            return Ok(());
        };
        let Some(sink) = self.sink.as_mut() else {
            return Err(anyhow!("playing without an audio output"));
        };
        let Some(source) = playing.source.as_mut() else {
            return Err(anyhow!("playing without a source"));
        };

        if playing.offset >= playing.pending.len() {
            if !source.next_chunk_into(&mut playing.pending)? {
                self.track_completed();
                return Ok(());
            }
            playing.offset = 0;
        }

        let channels = playing.spec.channels as usize;
        let frames = sink.write(&playing.pending[playing.offset..])?;
        // A sink that takes nothing from a partial frame would otherwise spin forever.
        if frames > 0 {
            let written_to = playing.offset + frames * channels;
            playing
                .recent
                .extend_from_slice(&playing.pending[playing.offset..written_to]);
            let keep = sink.capacity_frames() as usize * channels;
            if playing.recent.len() > keep * 2 {
                playing.recent.drain(..playing.recent.len() - keep);
            }
        }
        playing.offset = if frames == 0 {
            playing.pending.len()
        } else {
            playing.offset + frames * channels
        };
        playing.frames_written += frames as u64;

        if playing.last_position_at.elapsed() >= self.ctx.position_interval {
            self.emit_position();
        }
        Ok(())
    }

    /// What the listener has actually heard: everything handed to the device except what it has
    /// not played yet. Counting only what was written would run ahead by the device's buffer.
    fn heard_position(&mut self) -> Duration {
        let Some(playing) = self.current.as_ref() else {
            return Duration::ZERO;
        };
        // If the device can't say, or was handed back, nothing is queued.
        let queued = self
            .sink
            .as_mut()
            .map_or(0, |sink| sink.delay_frames().unwrap_or(0));
        let heard = playing.frames_written.saturating_sub(queued);
        playing.base + frames_to_duration(heard, playing.spec.sample_rate)
    }

    fn emit_position(&mut self) {
        let position = self.heard_position();
        let Some(playing) = self.current.as_mut() else {
            return;
        };
        playing.position = position;
        playing.last_position_at = Instant::now();
        let duration = playing.duration;
        self.publish_status();
        self.emit(Event::Position { position, duration });
    }

    fn track_completed(&mut self) {
        if let Some(sink) = self.sink.as_mut()
            && let Err(error) = sink.drain()
        {
            self.failed(error.context("draining the audio output"));
            return;
        }
        self.emit_position();
        if let Some(playing) = self.current.take() {
            self.emit(Event::TrackEnded {
                meta: playing.meta,
                reason: EndReason::Completed,
            });
        }
        self.request_load(LoadTarget::Advance(Advance::Auto));
    }

    fn start_track(&mut self, track: Prepared) -> Result<()> {
        let Prepared {
            meta,
            source,
            seek,
            start,
        } = track;

        let spec = source.spec();
        let duration = meta.duration.or_else(|| source.duration());
        if !self.sink.as_ref().is_some_and(|sink| sink.spec() == spec) {
            self.open_sink(spec)?;
        }

        self.current = Some(Playing {
            meta: meta.clone(),
            spec,
            source: Some(source),
            seek,
            pending: Vec::new(),
            offset: 0,
            recent: Vec::new(),
            sink_paused: false,
            frames_written: 0,
            base: start,
            duration,
            position: start,
            last_position_at: Instant::now(),
        });
        self.emit(Event::TrackStarted { meta, spec });
        let state = if self.pause_when_ready {
            State::Paused
        } else {
            State::Playing
        };
        self.pause_when_ready = false;
        self.set_state(state);
        self.emit_position();
        self.release_if_asked();
        Ok(())
    }

    /// Opens the device for `spec`, taking it back from the desktop if it had been handed over.
    fn open_sink(&mut self, spec: SourceSpec) -> Result<()> {
        // The device can only be opened once, so the old configuration has to go first.
        self.sink = None;
        self.sink = Some(
            self.ctx
                .sinks
                .open(spec)
                .context("opening the audio output")?,
        );
        if self.released.take().is_some() {
            // The status first: whoever hears the event and asks for the status must see it.
            self.publish_status();
            self.emit(Event::OutputAcquired);
        }
        Ok(())
    }

    /// Hands the device back as soon as the track that was loading when the request came is
    /// ready (paused).
    fn release_if_asked(&mut self) {
        if self.state == State::Paused
            && let Some(request) = self.release_when_ready.take()
        {
            self.release_output(request);
        }
    }

    fn seek(&mut self, target: SeekTarget) {
        if !matches!(self.state, State::Playing | State::Paused | State::Seeking)
            || self.current.is_none()
        {
            self.reject_seek("nothing is playing");
            return;
        }

        // Relative to what has been heard, which right after another seek is that seek's target,
        // so repeated seeks compose.
        let from = self.heard_position();
        let at = match target {
            SeekTarget::Absolute(at) => at,
            SeekTarget::Forward(by) => from.saturating_add(by),
            SeekTarget::Backward(by) => from.saturating_sub(by),
        };
        let Some((mode, duration)) = self
            .current
            .as_ref()
            .map(|playing| (playing.seek, playing.duration))
        else {
            return;
        };

        if mode == SeekMode::None {
            self.reject_seek("this track can't be seeked");
        } else if duration.is_some_and(|duration| at >= duration) {
            self.seek_past_the_end();
        } else {
            let was_paused = self.state == State::Paused
                || (self.state == State::Seeking && self.pause_when_ready);
            match mode {
                SeekMode::InPlace => self.seek_in_place(at),
                SeekMode::ForwardOnly => self.seek_forward_only(at),
                SeekMode::Reopen => self.seek_reopen(at, was_paused),
                SeekMode::None => unreachable!("rejected above"),
            }
        }
    }

    fn reject_seek(&self, reason: &str) {
        self.emit(Event::SeekRejected {
            reason: reason.to_string(),
        });
    }

    /// Seeking beyond the end ends the track, as in MPRIS, instead of clamping to just before it.
    fn seek_past_the_end(&mut self) {
        self.flush_sink();
        if let Some(playing) = self.current.take() {
            self.emit(Event::TrackEnded {
                meta: playing.meta,
                reason: EndReason::Completed,
            });
        }
        self.request_load(LoadTarget::Advance(Advance::Auto));
    }

    /// The source can be repositioned directly.
    fn seek_in_place(&mut self, at: Duration) {
        self.flush_sink();
        let result = self
            .current
            .as_mut()
            .and_then(|playing| playing.source.as_mut())
            .map(|source| source.seek(at));
        match result {
            Some(Ok(landed)) => self.finish_reposition(landed),
            Some(Err(error)) => self.fail(error.context("seeking in the track")),
            None => {}
        }
    }

    /// The source is one continuous stream: it can skip ahead of what it has already decoded, and
    /// nothing else.
    fn seek_forward_only(&mut self, at: Duration) {
        let Some(playing) = self.current.as_ref() else {
            return;
        };
        let queued_in_chunk =
            playing.pending.len().saturating_sub(playing.offset) / playing.spec.channels as usize;
        let decoded = playing.frames_written + queued_in_chunk as u64;
        let decoded_until = playing.base + frames_to_duration(decoded, playing.spec.sample_rate);
        if at < decoded_until {
            self.reject_seek("this stream can only seek forward, past what is already buffered");
            return;
        }

        let skip = duration_to_frames(at - decoded_until, playing.spec.sample_rate);
        self.flush_sink();
        if let Some(source) = self
            .current
            .as_mut()
            .and_then(|playing| playing.source.as_mut())
        {
            source.skip_frames(skip);
        }
        self.finish_reposition(at);
    }

    /// The stream can't rewind: drop it and ask the supplier to open the track at the target.
    fn seek_reopen(&mut self, at: Duration, was_paused: bool) {
        self.flush_sink();
        let Some(playing) = self.current.as_mut() else {
            return;
        };
        playing.source = None; // dropping the old stream stops its download
        let track = playing.meta.track.clone();
        self.rebase(at);
        self.begin_load(LoadTarget::Track(track), at, State::Seeking, was_paused);
        self.emit(Event::Seeked { position: at });
        self.emit_position();
    }

    /// The listener's position is now `position`, with nothing queued in the device.
    fn rebase(&mut self, position: Duration) {
        if let Some(playing) = self.current.as_mut() {
            playing.base = position;
            playing.frames_written = 0;
            playing.pending.clear();
            playing.offset = 0;
            playing.recent.clear();
            playing.position = position;
            // The flush that always precedes this also unpaused the device; while the engine is
            // paused nothing is written until it resumes, so there is nothing to resume in it.
            playing.sink_paused = false;
        }
    }

    fn finish_reposition(&mut self, position: Duration) {
        self.rebase(position);
        self.emit(Event::Seeked { position });
        self.emit_position();
    }

    /// The reopened track has arrived: skip to the exact target within it and carry on.
    fn finish_seek(&mut self, track: Prepared) -> Result<()> {
        let Prepared {
            mut source,
            seek,
            start,
            ..
        } = track;
        let Some(playing) = self.current.as_mut() else {
            return Ok(());
        };
        if source.spec() != playing.spec {
            bail!("the reopened track has a different format than the one that was playing");
        }

        // The supplier starts at a boundary at or before the target; the rest is skipped here.
        let landed = if start <= playing.base {
            source.skip_frames(duration_to_frames(
                playing.base - start,
                playing.spec.sample_rate,
            ));
            playing.base
        } else {
            start
        };
        playing.source = Some(source);
        playing.seek = seek;
        playing.base = landed;
        playing.position = landed;

        let state = if self.pause_when_ready {
            State::Paused
        } else {
            State::Playing
        };
        self.pause_when_ready = false;
        self.set_state(state);
        self.emit_position();
        self.release_if_asked();
        Ok(())
    }

    fn pause(&mut self) {
        match self.state {
            State::Loading | State::Seeking => self.pause_when_ready = true,
            State::Playing => {
                if let (Some(playing), Some(sink)) = (self.current.as_mut(), self.sink.as_mut()) {
                    if let Err(error) = sink.pause() {
                        self.fail(error.context("pausing the audio output"));
                        return;
                    }
                    playing.sink_paused = true;
                }
                self.set_state(State::Paused);
                self.emit_position();
            }
            State::Paused | State::Stopped => {}
        }
    }

    /// Opens the output again for the track that is loaded, if it is closed. Whoever has it may
    /// refuse, or a chosen output may be gone; nothing is lost either way: the track, the position
    /// and the audio not yet heard are kept, so a later resume can retry.
    fn reopen_output(&mut self) -> Result<()> {
        match self.current.as_ref().map(|playing| playing.spec) {
            Some(spec) if self.sink.is_none() => self.open_sink(spec),
            _ => Ok(()),
        }
    }

    fn resume(&mut self) {
        match self.state {
            State::Loading | State::Seeking => self.pause_when_ready = false,
            State::Paused => {
                if let Err(error) = self.reopen_output() {
                    self.emit(Event::Error {
                        message: format!("{error:#}"),
                    });
                    return;
                }
                if let (Some(playing), Some(sink)) = (self.current.as_mut(), self.sink.as_mut())
                    && playing.sink_paused
                {
                    if let Err(error) = sink.resume() {
                        self.fail(error.context("resuming the audio output"));
                        return;
                    }
                    playing.sink_paused = false;
                }
                self.set_state(State::Playing);
            }
            State::Playing | State::Stopped => {}
        }
    }

    /// Cuts the current track short, discarding whatever is queued in the device so it is never
    /// heard. The sink stays open for the next track.
    fn interrupt_current(&mut self) {
        let Some(playing) = self.current.take() else {
            return;
        };
        self.flush_sink();
        self.emit(Event::TrackEnded {
            meta: playing.meta,
            reason: EndReason::Interrupted,
        });
    }

    /// Throws away everything queued in the device, so it is never heard.
    fn flush_sink(&mut self) {
        if let Some(sink) = self.sink.as_mut()
            && let Err(error) = sink.flush()
        {
            self.emit(Event::Error {
                message: format!("{:#}", error.context("flushing the audio output")),
            });
        }
    }

    /// Stops everything and releases the audio device.
    fn stop(&mut self) {
        self.interrupt_current();
        self.cancel_load();
        self.pause_when_ready = false;
        self.close_output();
        self.set_state(State::Stopped);
    }

    /// Something the audio thread was doing failed. An output that has gone away (a Bluetooth
    /// speaker switched off) is not the engine's failure: it pauses on the track with the position
    /// kept, so that reconnecting the speaker and resuming carries on. Anything else stops it.
    fn failed(&mut self, error: anyhow::Error) {
        let gone = error.downcast_ref::<OutputGone>().is_some();
        if gone && self.current.is_some() && matches!(self.state, State::Playing | State::Paused) {
            self.output_lost(error);
        } else {
            self.fail(error);
        }
    }

    /// The output went away while a track was loaded.
    fn output_lost(&mut self, error: anyhow::Error) {
        self.emit(Event::Error {
            message: format!("{error:#}"),
        });
        // The dead sink can't be paused; the audio it still held is set aside as if it had been.
        if self.state == State::Playing {
            self.set_state(State::Paused);
        }
        self.set_aside_unheard();
        self.sink = None;
        self.ctx.sinks.release();
        self.released = Some(None);
        self.publish_status();
        self.emit(Event::OutputReleased {
            by: None,
            reason: ReleaseReason::Lost,
        });
        self.emit_position();
        self.publish_status();
    }

    /// Plays through `sinks` from now on, keeping the track and the exact position.
    fn set_output(&mut self, sinks: Arc<dyn SinkFactory>, done: Sender<Result<(), String>>) {
        let was_playing = self.state == State::Playing;
        if matches!(self.state, State::Playing | State::Paused) && self.sink.is_some() {
            self.pause();
            if self.state != State::Paused {
                // Pausing failed and stopped the engine; there is nothing left to move.
                self.replace_sinks(sinks);
                let _ = done.send(Ok(()));
                return;
            }
            self.set_aside_unheard();
        }
        // Whatever the old output was holding, and any request for it, is moot now.
        self.sink = None;
        self.released = None;
        if let Some(request) = self.release_when_ready.take() {
            request.answer(true);
        }
        self.replace_sinks(sinks);
        self.publish_status();

        let result = if was_playing && self.state == State::Paused {
            match self.reopen_output() {
                Ok(()) => {
                    self.resume();
                    Ok(())
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    self.emit(Event::Error {
                        message: message.clone(),
                    });
                    Err(message)
                }
            }
        } else {
            Ok(())
        };
        let _ = done.send(result);
    }

    /// Gives the old output back and takes `sinks` in its place.
    fn replace_sinks(&mut self, sinks: Arc<dyn SinkFactory>) {
        self.ctx.sinks.release();
        sinks.on_release_request(self.ctx.release_handler.clone());
        self.ctx.sinks = sinks;
    }

    /// Closes the device for good and gives the card back.
    fn close_output(&mut self) {
        self.sink = None;
        self.released = None;
        if let Some(request) = self.release_when_ready.take() {
            request.answer(true);
        }
        self.ctx.sinks.release();
    }

    /// Hands the audio device back, keeping the track and the exact position: the audio queued
    /// in the device but not yet heard is set aside and played first when the device is taken
    /// again.
    fn release_output(&mut self, request: Release) {
        match self.state {
            State::Loading | State::Seeking => {
                // The track isn't ready: it starts paused and lets go as soon as it does.
                self.pause_when_ready = true;
                if let Some(earlier) = self.release_when_ready.replace(request) {
                    earlier.answer(false);
                }
            }
            State::Stopped => request.answer(true),
            State::Playing | State::Paused => {
                if self.sink.is_none() {
                    request.answer(true);
                    return;
                }
                self.pause();
                if self.state != State::Paused {
                    // Pausing failed and stopped the engine; the device is closed anyway.
                    request.answer(true);
                    return;
                }
                self.set_aside_unheard();
                self.sink = None;
                self.ctx.sinks.release();
                self.released = Some(request.by.clone());
                // The status first: whoever hears the event and asks for the status must see it.
                self.publish_status();
                self.emit(Event::OutputReleased {
                    by: request.by.clone(),
                    reason: request.reason,
                });
                request.answer(true);
            }
        }
    }

    /// Moves what the device holds but the listener has not heard back into `pending`, and
    /// rebases the position on what has been heard.
    fn set_aside_unheard(&mut self) {
        let (Some(playing), Some(sink)) = (self.current.as_mut(), self.sink.as_mut()) else {
            return;
        };
        let channels = playing.spec.channels as usize;
        let queued = sink.delay_frames().unwrap_or(0) as usize;
        let keep = queued
            .min(playing.recent.len() / channels)
            .min(playing.frames_written as usize);

        let mut carried = playing.recent[playing.recent.len() - keep * channels..].to_vec();
        carried.extend_from_slice(&playing.pending[playing.offset.min(playing.pending.len())..]);
        playing.pending = carried;
        playing.offset = 0;
        playing.base += frames_to_duration(
            playing.frames_written - keep as u64,
            playing.spec.sample_rate,
        );
        playing.frames_written = 0;
        playing.recent.clear();
        playing.sink_paused = false;
        playing.position = playing.base;
    }

    fn fail(&mut self, error: anyhow::Error) {
        self.emit(Event::Error {
            message: format!("{error:#}"),
        });
        if let Some(playing) = self.current.take() {
            self.emit(Event::TrackEnded {
                meta: playing.meta,
                reason: EndReason::Failed,
            });
        }
        self.cancel_load();
        self.pause_when_ready = false;
        self.close_output();
        self.set_state(State::Stopped);
    }

    fn cancel_load(&mut self) {
        self.generation += 1;
        if let Some(task) = self.load_task.take() {
            task.abort();
        }
    }

    fn request_load(&mut self, target: LoadTarget) {
        // A pause applies to the track being loaded, not to whichever one replaces it.
        self.begin_load(target, Duration::ZERO, State::Loading, false);
    }

    /// Asks the supplier for a track, from `at` if it can, and waits in `state` for the answer.
    fn begin_load(
        &mut self,
        target: LoadTarget,
        at: Duration,
        state: State,
        pause_when_ready: bool,
    ) {
        self.cancel_load();
        if let Some(pending) = self.release_when_ready.take() {
            // Asking for a track again means the device is wanted.
            pending.answer(false);
        }
        self.pause_when_ready = pause_when_ready;
        self.set_state(state);

        let generation = self.generation;
        let supplier = self.ctx.supplier.clone();
        let tx = self.ctx.tx.clone();
        self.load_task = Some(self.ctx.rt.spawn(async move {
            let track = match target {
                LoadTarget::Track(track) => Some(track),
                LoadTarget::Advance(how) => supplier.advance(how),
            };
            let msg = match track {
                None => Msg::QueueExhausted { generation },
                Some(track) => {
                    let prepared = match supplier.open(track, at).await {
                        Ok(loaded) => prepare(loaded).await,
                        Err(error) => Err(error),
                    };
                    match prepared {
                        Ok(track) => Msg::Loaded {
                            generation,
                            track: Box::new(track),
                        },
                        Err(error) => Msg::LoadFailed {
                            generation,
                            error: format!("{error:#}"),
                        },
                    }
                }
            };
            let _ = tx.send(msg);
        }));
    }

    fn set_state(&mut self, state: State) {
        let changed = self.state != state;
        if changed {
            self.paused_at = (state == State::Paused).then(Instant::now);
        }
        self.state = state;
        self.publish_status();
        if changed {
            self.emit(Event::StateChanged(state));
        }
    }

    fn publish_status(&self) {
        let playing = self.current.as_ref();
        self.ctx.status.send_replace(Status {
            state: self.state,
            track: playing.map(|p| p.meta.clone()),
            spec: playing.map(|p| p.spec),
            position: playing.map_or(Duration::ZERO, |p| p.position),
            duration: playing.and_then(|p| p.duration),
            output: match (&self.sink, &self.released) {
                (Some(_), _) => OutputState::Open,
                (None, Some(by)) => OutputState::Released { by: by.clone() },
                (None, None) => OutputState::Closed,
            },
        });
    }

    fn emit(&self, event: Event) {
        // Nobody listening is not an error.
        let _ = self.ctx.events.send(event);
    }
}
