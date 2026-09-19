//! The audio thread: owns the sink and the decoder and runs the engine's state machine.
//!
//! It is a plain OS thread, never a tokio task: writing to the sink and reading a
//! `SegmentStream` both block, and blocking a runtime worker would stall everything else on it.
//! Everything that can be slow (asking TIDAL for a track) runs on the runtime and comes back as a
//! message, so this loop has a single place to wait.

use super::supplier::{Advance, LoadedTrack, TrackMedia, TrackSupplier};
use super::types::{Command, EndReason, Event, State, Status, TrackMeta, TrackRef};
use crate::decode::{Decoder, SourceSpec};
use crate::output::{AudioSink, SinkFactory};
use anyhow::{Context as _, Result, anyhow};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use tokio::runtime::Handle;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

/// Frames handed to the sink per chunk when the media is already raw PCM.
const RAW_CHUNK_FRAMES: usize = 4096;

/// Everything the audio thread can be told, from callers and from its own load tasks.
pub(super) enum Msg {
    Command(Command),
    Loaded { generation: u64, track: Box<LoadedTrack> },
    LoadFailed { generation: u64, error: String },
    QueueExhausted { generation: u64 },
    Shutdown,
}

pub(super) struct Context {
    pub rx: Receiver<Msg>,
    pub tx: Sender<Msg>,
    pub rt: Handle,
    pub sinks: Arc<dyn SinkFactory>,
    pub supplier: Arc<dyn TrackSupplier>,
    pub events: broadcast::Sender<Event>,
    pub status: watch::Sender<Status>,
}

pub(super) fn run(ctx: Context) {
    AudioThread {
        ctx,
        state: State::Stopped,
        generation: 0,
        load_task: None,
        sink: None,
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
    Raw { samples: Vec<i32>, next: usize, spec: SourceSpec },
}

impl Source {
    fn spec(&self) -> SourceSpec {
        match self {
            Source::Decoder(decoder) => decoder.spec(),
            Source::Raw { spec, .. } => *spec,
        }
    }

    /// Replaces `out` with the next chunk of interleaved samples; `false` at the end.
    fn next_chunk_into(&mut self, out: &mut Vec<i32>) -> Result<bool> {
        match self {
            Source::Decoder(decoder) => decoder.next_chunk_into(out),
            Source::Raw { samples, next, spec } => {
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
    source: Source,
    /// The chunk being written to the sink, of which `offset` samples are already in.
    pending: Vec<i32>,
    offset: usize,
}

struct AudioThread {
    ctx: Context,
    state: State,
    /// Bumped by every load request and by `stop`. A load result carrying an older number
    /// answers a question nobody is asking any more and is dropped.
    generation: u64,
    load_task: Option<JoinHandle<()>>,
    /// Kept open between tracks of the same format, so there is no gap or click.
    sink: Option<Box<dyn AudioSink>>,
    current: Option<Playing>,
}

impl AudioThread {
    fn run(mut self) {
        loop {
            // While playing, poll for messages between writes; otherwise sleep until one comes.
            let msg = if self.current.is_some() {
                match self.ctx.rx.try_recv() {
                    Ok(msg) => Some(msg),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => break,
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
                        self.fail(error);
                    }
                }
            }
        }
        self.stop();
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
            Msg::Command(Command::Stop) => self.stop(),
            Msg::Loaded { generation, track } if self.is_awaited(generation) => {
                if let Err(error) = self.start_track(*track) {
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
            Msg::Shutdown => {
                self.stop();
                return false;
            }
        }
        true
    }

    fn is_awaited(&self, generation: u64) -> bool {
        self.state == State::Loading && generation == self.generation
    }

    /// Writes at most one period of the current track to the sink.
    fn play_step(&mut self) -> Result<()> {
        let Some(playing) = self.current.as_mut() else { return Ok(()) };
        let Some(sink) = self.sink.as_mut() else { return Err(anyhow!("playing without an audio output")) };

        if playing.offset >= playing.pending.len() {
            if !playing.source.next_chunk_into(&mut playing.pending)? {
                self.track_completed();
                return Ok(());
            }
            playing.offset = 0;
        }

        let channels = playing.source.spec().channels as usize;
        let frames = sink.write(&playing.pending[playing.offset..])?;
        // A sink that takes nothing from a partial frame would otherwise spin forever.
        playing.offset = if frames == 0 { playing.pending.len() } else { playing.offset + frames * channels };
        Ok(())
    }

    fn track_completed(&mut self) {
        if let Some(sink) = self.sink.as_mut()
            && let Err(error) = sink.drain()
        {
            self.fail(error.context("draining the audio output"));
            return;
        }
        if let Some(playing) = self.current.take() {
            self.emit(Event::TrackEnded { meta: playing.meta, reason: EndReason::Completed });
        }
        self.request_load(LoadTarget::Advance(Advance::Auto));
    }

    fn start_track(&mut self, track: LoadedTrack) -> Result<()> {
        let LoadedTrack { meta, media } = track;
        let source = match media {
            TrackMedia::Encoded { source, extension } => {
                Source::Decoder(Decoder::open_boxed(source, extension.as_deref()).context("opening the decoder")?)
            }
            TrackMedia::RawPcm { samples, spec } => Source::Raw { samples, next: 0, spec },
        };

        let spec = source.spec();
        if !self.sink.as_ref().is_some_and(|sink| sink.spec() == spec) {
            // The device can only be opened once, so the old configuration has to go first.
            self.sink = None;
            self.sink = Some(self.ctx.sinks.open(spec).context("opening the audio output")?);
        }

        self.current = Some(Playing { meta: meta.clone(), source, pending: Vec::new(), offset: 0 });
        self.emit(Event::TrackStarted { meta, spec });
        self.set_state(State::Playing);
        Ok(())
    }

    /// Cuts the current track short, discarding whatever is queued in the device so it is never
    /// heard. The sink stays open for the next track.
    fn interrupt_current(&mut self) {
        let Some(playing) = self.current.take() else { return };
        if let Some(sink) = self.sink.as_mut()
            && let Err(error) = sink.flush()
        {
            self.emit(Event::Error { message: format!("{:#}", error.context("flushing the audio output")) });
        }
        self.emit(Event::TrackEnded { meta: playing.meta, reason: EndReason::Interrupted });
    }

    /// Stops everything and releases the audio device.
    fn stop(&mut self) {
        self.interrupt_current();
        self.cancel_load();
        self.sink = None;
        self.set_state(State::Stopped);
    }

    fn fail(&mut self, error: anyhow::Error) {
        self.emit(Event::Error { message: format!("{error:#}") });
        if let Some(playing) = self.current.take() {
            self.emit(Event::TrackEnded { meta: playing.meta, reason: EndReason::Failed });
        }
        self.cancel_load();
        self.sink = None;
        self.set_state(State::Stopped);
    }

    fn cancel_load(&mut self) {
        self.generation += 1;
        if let Some(task) = self.load_task.take() {
            task.abort();
        }
    }

    fn request_load(&mut self, target: LoadTarget) {
        self.cancel_load();
        self.set_state(State::Loading);

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
                Some(track) => match supplier.open(track).await {
                    Ok(track) => Msg::Loaded { generation, track: Box::new(track) },
                    Err(error) => Msg::LoadFailed { generation, error: format!("{error:#}") },
                },
            };
            let _ = tx.send(msg);
        }));
    }

    fn set_state(&mut self, state: State) {
        let changed = self.state != state;
        self.state = state;
        self.ctx.status.send_replace(Status {
            state,
            track: self.current.as_ref().map(|playing| playing.meta.clone()),
            spec: self.current.as_ref().map(|playing| playing.source.spec()),
        });
        if changed {
            self.emit(Event::StateChanged(state));
        }
    }

    fn emit(&self, event: Event) {
        // Nobody listening is not an error.
        let _ = self.ctx.events.send(event);
    }
}
