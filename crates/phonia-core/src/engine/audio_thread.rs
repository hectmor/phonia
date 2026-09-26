//! The audio thread: owns the sink and the decoder and runs the engine's state machine.
//!
//! It is a plain OS thread, never a tokio task: writing to the sink and reading a
//! `SegmentStream` both block, and blocking a runtime worker would stall everything else on it.
//! Everything that can be slow (asking TIDAL for a track) runs on the runtime and comes back as a
//! message, so this loop has a single place to wait.

use super::supplier::{Advance, LoadedTrack, Peek, SeekMode, TrackMedia, TrackSupplier};
use super::types::{
    Command, EndReason, Event, OutputState, ReleaseReason, SeekTarget, State, Status, TrackMeta,
    TrackRef,
};
use super::{PREFETCH_LEAD, PREVIOUS_RESTART_AFTER};
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
    /// The first chunk of audio, already decoded, for a track opened ahead of its turn (empty
    /// otherwise): it proves the track decodes, and is what gets written first.
    first_chunk: Vec<i32>,
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
            first_chunk: Vec::new(),
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
        first_chunk: Vec::new(),
    })
}

/// Opens the track that comes after the current one and decodes its first chunk, so that it is
/// known to play and can be written the instant the current one ends.
async fn prefetch(supplier: Arc<dyn TrackSupplier>, track: TrackRef) -> Result<Prepared> {
    let loaded = supplier.open_ahead(track).await?;
    let mut prepared = prepare(loaded).await?;
    tokio::task::spawn_blocking(move || {
        let mut chunk = Vec::new();
        if !prepared.source.next_chunk_into(&mut chunk)? {
            chunk.clear();
        }
        prepared.first_chunk = chunk;
        Ok(prepared)
    })
    .await
    .map_err(|error| anyhow!("decoding the next track was interrupted: {error}"))?
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
    /// The track opened ahead of its turn is ready, or could not be.
    Prefetched {
        generation: u64,
        result: Result<Box<Prepared>, String>,
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
    pub gapless: bool,
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
        outgoing: None,
        next: None,
        prefetch_generation: 0,
        waiting_until: None,
        adopting: false,
        polling: false,
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
    /// Frames of the track before this one that are counted in `frames_written`: this track was
    /// joined to it without a gap, so the sink's counter runs on across the boundary.
    lead_in: u64,
    /// The source has nothing more to give; what is left is to see the track out.
    source_done: bool,
}

/// The track that was playing when its successor was joined to it, until the listener has heard
/// its last frame. Everything reported to the outside is about this track in the meantime.
struct Outgoing {
    meta: TrackMeta,
    spec: SourceSpec,
    duration: Option<Duration>,
    /// Where in the track its last written frame ends.
    end_position: Duration,
    /// What the listener has heard of it.
    position: Duration,
}

/// The track opened ahead of its turn.
struct Next {
    track: TrackRef,
    generation: u64,
    task: JoinHandle<()>,
    phase: NextPhase,
}

enum NextPhase {
    Opening,
    Ready(Box<Prepared>),
    Failed,
}

/// How much earlier than the last audio of a track ends the wait for the next one is given up.
const WAIT_MARGIN: Duration = Duration::from_millis(50);

/// How often the audio thread looks again while it waits for something without a message to say
/// it is done (the listener reaching a boundary, a track opening).
const POLL: Duration = Duration::from_millis(10);

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
    /// Set while the next track was joined to the current one and the listener has not yet heard
    /// the end of the previous track (see [`Outgoing`]).
    outgoing: Option<Outgoing>,
    /// The next track, opened ahead of its turn.
    next: Option<Next>,
    /// Bumped for each track opened ahead, so a late answer for an abandoned one is recognised.
    prefetch_generation: u64,
    /// The current track ended and the next is still opening: waiting for it, until this moment.
    waiting_until: Option<Instant>,
    /// The wait was given up, the sink drained and the engine is `Loading`: the track still
    /// opening will be started as it arrives, instead of asking for it a second time.
    adopting: bool,
    /// Something is being waited for: look again soon instead of writing.
    polling: bool,
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
            let msg = if self.state == State::Playing && self.polling {
                // Waiting for something that sends no message: look again in a moment.
                match self.ctx.rx.recv_timeout(POLL) {
                    Ok(msg) => Some(msg),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else if self.state == State::Playing {
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
            Msg::Prefetched { generation, result } => self.prefetched(generation, result),
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
        self.polling = false;
        self.check_crossing();
        let Some(playing) = self.current.as_mut() else {
            return Ok(());
        };
        if playing.source_done {
            return self.source_finished();
        }
        let Some(sink) = self.sink.as_mut() else {
            return Err(anyhow!("playing without an audio output"));
        };
        let Some(source) = playing.source.as_mut() else {
            return Err(anyhow!("playing without a source"));
        };

        if playing.offset >= playing.pending.len() {
            if !source.next_chunk_into(&mut playing.pending)? {
                playing.source_done = true;
                return self.source_finished();
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
            self.maybe_prefetch();
        }
        Ok(())
    }

    /// What the listener has actually heard: everything handed to the device except what it has
    /// not played yet. Counting only what was written would run ahead by the device's buffer.
    ///
    /// While a track that was joined to the one before it has not started to be heard, that is
    /// the position in the one before.
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
        if let Some(outgoing) = &self.outgoing {
            let remaining = playing.lead_in.saturating_sub(heard);
            return outgoing
                .end_position
                .saturating_sub(frames_to_duration(remaining, playing.spec.sample_rate));
        }
        playing.base
            + frames_to_duration(
                heard.saturating_sub(playing.lead_in),
                playing.spec.sample_rate,
            )
    }

    fn emit_position(&mut self) {
        let position = self.heard_position();
        let Some(playing) = self.current.as_mut() else {
            return;
        };
        playing.last_position_at = Instant::now();
        if let Some(outgoing) = self.outgoing.as_mut() {
            outgoing.position = position;
            let duration = outgoing.duration;
            self.publish_status();
            self.emit(Event::Position { position, duration });
            return;
        }
        playing.position = position;
        let duration = playing.duration;
        self.publish_status();
        self.emit(Event::Position { position, duration });
    }

    /// Drains the sink and ends the current track as completed. `false` if the drain failed.
    fn finish_track_completed(&mut self) -> bool {
        if let Some(sink) = self.sink.as_mut()
            && let Err(error) = sink.drain()
        {
            self.failed(error.context("draining the audio output"));
            return false;
        }
        self.emit_position();
        if let Some(playing) = self.current.take() {
            self.emit(Event::TrackEnded {
                meta: playing.meta,
                reason: EndReason::Completed,
            });
        }
        true
    }

    /// The track ended and nothing was joined to it: drain, and load what comes next.
    fn track_completed(&mut self) {
        if self.finish_track_completed() {
            self.request_load(LoadTarget::Advance(Advance::Auto));
        }
    }

    // ---- gapless ------------------------------------------------------------------------

    /// The source of the current track has nothing more to give.
    fn source_finished(&mut self) -> Result<()> {
        if self.outgoing.is_some() {
            // This track ended before the listener heard the end of the one before it, which
            // only happens with a track shorter than the device's buffer. One boundary at a
            // time: wait for that one.
            self.polling = true;
            return Ok(());
        }
        if self.ctx.gapless {
            self.end_of_source();
        } else {
            self.track_completed();
        }
        Ok(())
    }

    /// The current track has been written in full. If the next one is (or is about to be) open,
    /// joins it on without a gap; otherwise the old way: drain, then load.
    fn end_of_source(&mut self) {
        let Peek::Next(wanted) = self.ctx.supplier.peek(Advance::Auto) else {
            self.drop_next();
            self.track_completed();
            return;
        };
        if self.next.as_ref().is_some_and(|next| next.track != wanted) {
            self.drop_next();
        }
        if self.next.is_none() {
            // The length was not known, so nothing was opened ahead: it starts now, and the
            // sink still holds up to a buffer of audio to cover it.
            self.start_prefetch(wanted);
        }
        match self.next.as_ref().map(|next| &next.phase) {
            Some(NextPhase::Ready(_)) => self.join_next(),
            Some(NextPhase::Opening) => self.wait_for_next(),
            Some(NextPhase::Failed) | None => {
                // Opening it ahead failed; asking again the ordinary way is the one retry.
                self.drop_next();
                self.track_completed();
            }
        }
    }

    /// The next track is ready. With the sink's format, it is written right after the last frame
    /// of the current one; with another, the sink is drained and reopened for it.
    fn join_next(&mut self) {
        let Some(Next {
            phase: NextPhase::Ready(prepared),
            ..
        }) = self.next.take()
        else {
            return;
        };
        self.waiting_until = None;
        let same_format = self
            .sink
            .as_ref()
            .is_some_and(|sink| sink.spec() == prepared.source.spec());
        if same_format {
            self.handover(*prepared);
        } else if self.finish_track_completed() {
            self.start_opened_ahead(*prepared);
        }
    }

    /// The next track is still opening while the last audio of this one plays out. Waits until
    /// that audio is nearly gone; then gives up, drains, and starts the next track as it arrives.
    fn wait_for_next(&mut self) {
        let deadline = match self.waiting_until {
            Some(deadline) => deadline,
            None => {
                let queued = self
                    .sink
                    .as_mut()
                    .map_or(0, |sink| sink.delay_frames().unwrap_or(0));
                let rate = self.current.as_ref().map_or(1, |p| p.spec.sample_rate);
                let deadline =
                    Instant::now() + frames_to_duration(queued, rate).saturating_sub(WAIT_MARGIN);
                self.waiting_until = Some(deadline);
                deadline
            }
        };
        if Instant::now() < deadline {
            self.polling = true;
            return;
        }
        crate::note!("the next track was not ready in time: not gapless");
        self.waiting_until = None;
        if self.finish_track_completed() {
            self.adopting = true;
            self.set_state(State::Loading);
        }
    }

    /// Writes `next` right after the last frame of the current track, which keeps playing out of
    /// the device's buffer. Nothing is drained, flushed or reopened.
    fn handover(&mut self, next: Prepared) {
        let Some(previous) = self.current.take() else {
            return;
        };
        let spec = previous.spec;
        let end_position = previous.base
            + frames_to_duration(
                previous.frames_written.saturating_sub(previous.lead_in),
                spec.sample_rate,
            );
        let Prepared {
            meta,
            source,
            seek,
            start,
            first_chunk,
        } = next;
        let duration = meta.duration.or_else(|| source.duration());
        self.outgoing = Some(Outgoing {
            meta: previous.meta,
            spec,
            duration: previous.duration,
            end_position,
            position: previous.position,
        });
        self.current = Some(Playing {
            meta,
            spec,
            source: Some(source),
            seek,
            pending: first_chunk,
            offset: 0,
            // One continuous stream of samples: what the sink still holds of the previous track
            // is followed by this one.
            recent: previous.recent,
            sink_paused: previous.sink_paused,
            frames_written: previous.frames_written,
            base: start,
            duration,
            position: start,
            last_position_at: previous.last_position_at,
            lead_in: previous.frames_written,
            source_done: false,
        });
        self.check_crossing();
    }

    /// Whether the listener has now heard the last of the previous track, and if so, moves on:
    /// the previous track ends and the new one starts, at the moment it is actually heard.
    fn check_crossing(&mut self) {
        if self.outgoing.is_none() {
            return;
        }
        let (Some(playing), Some(sink)) = (self.current.as_ref(), self.sink.as_mut()) else {
            return;
        };
        let queued = sink.delay_frames().unwrap_or(0);
        if playing.frames_written.saturating_sub(queued) < playing.lead_in {
            return;
        }
        let Some(outgoing) = self.outgoing.take() else {
            return;
        };
        self.emit(Event::Position {
            position: outgoing.end_position,
            duration: outgoing.duration,
        });
        self.emit(Event::TrackEnded {
            meta: outgoing.meta,
            reason: EndReason::Completed,
        });

        let Some(track) = self.current.as_ref().map(|p| p.meta.track.clone()) else {
            return;
        };
        if !self.ctx.supplier.started(&track) {
            // It was removed from the queue while it waited: skip what was written of it.
            self.current = None;
            self.flush_sink();
            self.request_load(LoadTarget::Advance(Advance::Next));
            return;
        }
        if let Some(playing) = self.current.as_ref() {
            let (meta, spec) = (playing.meta.clone(), playing.spec);
            self.publish_status();
            self.emit(Event::TrackStarted {
                meta,
                spec,
                gapless: true,
            });
        }
        self.emit_position();
    }

    /// Starts opening the track that follows the current one, if it is time: the last stretch of
    /// the current track begins. Also checks that what was opened is still what comes next.
    fn maybe_prefetch(&mut self) {
        if !self.ctx.gapless || self.state != State::Playing || self.outgoing.is_some() {
            return;
        }
        let Some(playing) = self.current.as_ref() else {
            return;
        };
        let peeked = self.ctx.supplier.peek(Advance::Auto);
        let remaining = playing.duration.map(|duration| {
            let played = playing.base
                + frames_to_duration(
                    playing.frames_written.saturating_sub(playing.lead_in),
                    playing.spec.sample_rate,
                );
            duration.saturating_sub(played)
        });
        if let Some(next) = &self.next {
            // The queue changed, or the listener went back: what was opened is no longer wanted.
            let changed = peeked != Peek::Next(next.track.clone());
            let too_early = remaining.is_some_and(|remaining| remaining > PREFETCH_LEAD * 3 / 2);
            if changed || too_early {
                self.drop_next();
            } else {
                return;
            }
        }
        if let (Peek::Next(track), Some(remaining)) = (peeked, remaining)
            && remaining <= PREFETCH_LEAD
        {
            self.start_prefetch(track);
        }
    }

    /// Opens `track` in the background, to be played after the current one.
    fn start_prefetch(&mut self, track: TrackRef) {
        self.prefetch_generation += 1;
        let generation = self.prefetch_generation;
        let supplier = self.ctx.supplier.clone();
        let tx = self.ctx.tx.clone();
        let wanted = track.clone();
        let task = self.ctx.rt.spawn(async move {
            let result = prefetch(supplier, wanted)
                .await
                .map(Box::new)
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(Msg::Prefetched { generation, result });
        });
        self.next = Some(Next {
            track,
            generation,
            task,
            phase: NextPhase::Opening,
        });
    }

    /// Forgets the track that was being opened ahead, and stops opening it.
    fn drop_next(&mut self) {
        if let Some(next) = self.next.take() {
            next.task.abort();
        }
        self.waiting_until = None;
        self.adopting = false;
    }

    /// The track opened ahead is ready, or failed.
    fn prefetched(&mut self, generation: u64, result: Result<Box<Prepared>, String>) {
        let Some(next) = self.next.as_mut() else {
            return;
        };
        if next.generation != generation {
            return;
        }
        match result {
            Ok(prepared) if self.adopting => {
                // The wait was given up: this is the track the engine is loading.
                self.next = None;
                self.adopting = false;
                self.start_opened_ahead(*prepared);
            }
            Ok(prepared) => next.phase = NextPhase::Ready(prepared),
            Err(_) if self.adopting => {
                // Opening it failed: ask for it once more, the ordinary way.
                self.next = None;
                self.adopting = false;
                self.request_load(LoadTarget::Advance(Advance::Auto));
            }
            Err(_) => next.phase = NextPhase::Failed,
        }
    }

    /// Starts a track that was opened ahead, now that its turn has come and nothing is playing.
    fn start_opened_ahead(&mut self, prepared: Prepared) {
        if !self.ctx.supplier.started(&prepared.meta.track) {
            // Removed from the queue meanwhile: move on to what follows.
            self.request_load(LoadTarget::Advance(Advance::Next));
            return;
        }
        if let Err(error) = self.start_track(prepared) {
            self.fail(error);
        }
    }

    fn start_track(&mut self, track: Prepared) -> Result<()> {
        let Prepared {
            meta,
            source,
            seek,
            start,
            first_chunk,
        } = track;
        self.waiting_until = None;
        self.polling = false;

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
            pending: first_chunk,
            offset: 0,
            recent: Vec::new(),
            sink_paused: false,
            frames_written: 0,
            base: start,
            duration,
            position: start,
            last_position_at: Instant::now(),
            lead_in: 0,
            source_done: false,
        });
        self.emit(Event::TrackStarted {
            meta,
            spec,
            gapless: false,
        });
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
        if self.outgoing.is_some() {
            self.seek_during_handover(target);
            return;
        }
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

    /// A seek while the next track is already written behind the one still being heard: it is the
    /// track being heard that moves. Its source is spent, so it is opened again at the target, like
    /// a stream that can't rewind; the next track, never announced, is dropped.
    fn seek_during_handover(&mut self, target: SeekTarget) {
        let from = self.heard_position();
        let at = match target {
            SeekTarget::Absolute(at) => at,
            SeekTarget::Forward(by) => from.saturating_add(by),
            SeekTarget::Backward(by) => from.saturating_sub(by),
        };
        let was_paused = self.state == State::Paused;
        let Some(outgoing) = self.outgoing.take() else {
            return;
        };
        self.current = None;
        self.drop_next();
        if outgoing.duration.is_some_and(|duration| at >= duration) {
            self.flush_sink();
            self.emit(Event::TrackEnded {
                meta: outgoing.meta,
                reason: EndReason::Completed,
            });
            self.request_load(LoadTarget::Advance(Advance::Auto));
            return;
        }
        self.current = Some(Playing {
            meta: outgoing.meta,
            spec: outgoing.spec,
            source: None,
            seek: SeekMode::Reopen,
            pending: Vec::new(),
            offset: 0,
            recent: Vec::new(),
            sink_paused: false,
            frames_written: 0,
            base: at,
            duration: outgoing.duration,
            position: at,
            last_position_at: Instant::now(),
            lead_in: 0,
            source_done: false,
        });
        self.seek_reopen(at, was_paused);
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
        self.waiting_until = None;
        if let Some(playing) = self.current.as_mut() {
            playing.base = position;
            playing.frames_written = 0;
            playing.pending.clear();
            playing.offset = 0;
            playing.recent.clear();
            playing.position = position;
            playing.lead_in = 0;
            playing.source_done = false;
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
        if let Some(outgoing) = self.outgoing.take() {
            // The next track was already written behind this one, but it was never announced: to
            // the listener the track that is still playing is the one that is cut short.
            self.current = None;
            self.drop_next();
            self.flush_sink();
            self.emit(Event::TrackEnded {
                meta: outgoing.meta,
                reason: EndReason::Interrupted,
            });
            return;
        }
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
        self.drop_next();
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
        self.drop_next();
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
        self.drop_next();
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
                self.drop_next();
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
        // Whether the listener has just heard the end of the previous track decides how the
        // counters are rebased.
        self.check_crossing();
        let joined = self.outgoing.is_some();
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
        let heard = playing.frames_written - keep as u64;
        if joined {
            // Still hearing the end of the previous track: what was set aside is its tail and
            // the head of this one, so what remains of the lead-in is the tail.
            playing.lead_in = playing.lead_in.saturating_sub(heard);
        } else {
            playing.base += frames_to_duration(
                heard.saturating_sub(playing.lead_in),
                playing.spec.sample_rate,
            );
            playing.lead_in = 0;
        }
        playing.frames_written = 0;
        playing.recent.clear();
        playing.sink_paused = false;
        playing.position = playing.base;
    }

    fn fail(&mut self, error: anyhow::Error) {
        self.emit(Event::Error {
            message: format!("{error:#}"),
        });
        if let Some(outgoing) = self.outgoing.take() {
            self.emit(Event::TrackEnded {
                meta: outgoing.meta,
                reason: EndReason::Failed,
            });
            self.current = None;
        }
        if let Some(playing) = self.current.take() {
            self.emit(Event::TrackEnded {
                meta: playing.meta,
                reason: EndReason::Failed,
            });
        }
        self.drop_next();
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
        // Whatever was being opened ahead is stale once another track is asked for.
        self.drop_next();
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
        // Until the listener hears the joined track, everything is still about the one before.
        let (track, spec, position, duration) = match (&self.outgoing, playing) {
            (Some(outgoing), _) => (
                Some(outgoing.meta.clone()),
                Some(outgoing.spec),
                outgoing.position,
                outgoing.duration,
            ),
            (None, Some(p)) => (Some(p.meta.clone()), Some(p.spec), p.position, p.duration),
            (None, None) => (None, None, Duration::ZERO, None),
        };
        self.ctx.status.send_replace(Status {
            state: self.state,
            track,
            spec,
            position,
            duration,
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
