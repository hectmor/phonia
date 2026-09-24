//! The playback engine: an audio thread driven by commands, reporting what it does as events.
//!
//! Callers talk to an [`Engine`] handle ([`Engine::send`], [`Engine::subscribe`],
//! [`Engine::status`]); the engine finds tracks through a [`TrackSupplier`] and plays them
//! through sinks from a [`SinkFactory`]. It knows nothing about queues, TIDAL or IPC, which sit
//! on top of it.

mod audio_thread;
mod supplier;
mod types;

#[cfg(test)]
mod tests;

pub use supplier::{Advance, LoadedTrack, SeekMode, TrackMedia, TrackOpener, TrackSupplier};
pub use types::{
    Command, EndReason, Event, OutputState, ReleaseReason, SeekTarget, State, Status, TrackMeta, TrackRef,
};

use std::time::Duration;

/// How often a playing track reports its position.
const POSITION_INTERVAL: Duration = Duration::from_millis(250);

/// How long a pause lasts before the audio device is handed back to the desktop.
pub const RELEASE_AFTER_PAUSE: Duration = Duration::from_secs(10);

/// How long a program asking for the audio device waits for the engine to answer.
const RELEASE_ANSWER_TIMEOUT: Duration = Duration::from_secs(3);

/// `Previous` restarts the current track instead of going back once it has played this long.
pub const PREVIOUS_RESTART_AFTER: Duration = Duration::from_secs(3);

use crate::output::reserve;
use crate::output::{ReleaseRequest, SinkFactory};
use anyhow::{Context as _, Result, anyhow};
use audio_thread::{Context, Msg};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tokio::runtime::Handle;
use tokio::sync::{broadcast, watch};

/// A slow subscriber misses events (`Lagged`) rather than holding the audio thread back.
const EVENT_CAPACITY: usize = 256;

/// Tunables of an [`Engine`].
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// How often a playing track reports its position; zero reports after every write.
    pub position_interval: Duration,
    /// How long a pause lasts before the audio device is handed back to the desktop. `None`
    /// keeps it for as long as the engine has a track; zero hands it back on every pause.
    pub release_after_pause: Option<Duration>,
}

impl Default for Options {
    fn default() -> Self {
        Self { position_interval: POSITION_INTERVAL, release_after_pause: Some(RELEASE_AFTER_PAUSE) }
    }
}

pub struct Engine {
    tx: mpsc::Sender<Msg>,
    events: broadcast::Sender<Event>,
    status: watch::Receiver<Status>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Engine {
    /// Starts the audio thread. `rt` runs the supplier's (possibly slow) `open` calls. Pausing
    /// for [`RELEASE_AFTER_PAUSE`] hands the audio device back.
    pub fn spawn(rt: Handle, sinks: Arc<dyn SinkFactory>, supplier: Arc<dyn TrackSupplier>) -> Result<Self> {
        Self::spawn_with_options(rt, sinks, supplier, Options::default())
    }

    /// Like [`Engine::spawn`], with `options`.
    pub fn spawn_with_options(
        rt: Handle,
        sinks: Arc<dyn SinkFactory>,
        supplier: Arc<dyn TrackSupplier>,
        options: Options,
    ) -> Result<Self> {
        Self::spawn_inner(rt, sinks, supplier, options)
    }

    /// Like [`Engine::spawn`], with a custom interval between position reports; a zero interval
    /// reports after every write, which tests use to observe positions deterministically.
    fn spawn_inner(
        rt: Handle,
        sinks: Arc<dyn SinkFactory>,
        supplier: Arc<dyn TrackSupplier>,
        options: Options,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (status_tx, status) = watch::channel(Status {
            state: State::Stopped,
            track: None,
            spec: None,
            position: Duration::ZERO,
            duration: None,
            output: OutputState::Closed,
        });

        let request_tx = tx.clone();
        sinks.on_release_request(Arc::new(move |request: ReleaseRequest| {
            // The protocol's rule: only a program that matters more than us gets the device.
            if request.priority <= reserve::PRIORITY {
                return false;
            }
            let (done, answer) = mpsc::channel();
            if request_tx.send(Msg::ReleaseRequested { by: request.by, done }).is_err() {
                return false;
            }
            answer.recv_timeout(RELEASE_ANSWER_TIMEOUT).unwrap_or(false)
        }));

        let ctx = Context {
            rx,
            tx: tx.clone(),
            rt,
            sinks,
            supplier,
            events: events.clone(),
            status: status_tx,
            position_interval: options.position_interval,
            release_after_pause: options.release_after_pause,
        };
        let thread = std::thread::Builder::new()
            .name("phonia-audio".into())
            .spawn(move || audio_thread::run(ctx))
            .context("spawning the audio thread")?;

        Ok(Self { tx, events, status, thread: Mutex::new(Some(thread)) })
    }

    /// Fails only once the engine has shut down.
    pub fn send(&self, command: Command) -> Result<()> {
        self.tx.send(Msg::Command(command)).map_err(|_| anyhow!("the playback engine has shut down"))
    }

    /// Events from now on; subscribe before sending the command whose events you care about.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    /// Stops playback, releases the audio device and waits for the audio thread to exit.
    /// Does nothing if already shut down.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
