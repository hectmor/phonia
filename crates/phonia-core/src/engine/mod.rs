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

pub use supplier::{Advance, LoadedTrack, TrackMedia, TrackSupplier};
pub use types::{Command, EndReason, Event, State, Status, TrackMeta, TrackRef};

use std::time::Duration;

/// How often a playing track reports its position.
const POSITION_INTERVAL: Duration = Duration::from_millis(250);

/// `Previous` restarts the current track instead of going back once it has played this long.
pub const PREVIOUS_RESTART_AFTER: Duration = Duration::from_secs(3);

use crate::output::SinkFactory;
use anyhow::{Context as _, Result, anyhow};
use audio_thread::{Context, Msg};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tokio::runtime::Handle;
use tokio::sync::{broadcast, watch};

/// A slow subscriber misses events (`Lagged`) rather than holding the audio thread back.
const EVENT_CAPACITY: usize = 256;

pub struct Engine {
    tx: mpsc::Sender<Msg>,
    events: broadcast::Sender<Event>,
    status: watch::Receiver<Status>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Engine {
    /// Starts the audio thread. `rt` runs the supplier's (possibly slow) `open` calls.
    pub fn spawn(rt: Handle, sinks: Arc<dyn SinkFactory>, supplier: Arc<dyn TrackSupplier>) -> Result<Self> {
        Self::spawn_with_position_interval(rt, sinks, supplier, POSITION_INTERVAL)
    }

    /// Like [`Engine::spawn`], with a custom interval between position reports; a zero interval
    /// reports after every write, which tests use to observe positions deterministically.
    pub(crate) fn spawn_with_position_interval(
        rt: Handle,
        sinks: Arc<dyn SinkFactory>,
        supplier: Arc<dyn TrackSupplier>,
        position_interval: Duration,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (status_tx, status) = watch::channel(Status {
            state: State::Stopped,
            track: None,
            spec: None,
            position: Duration::ZERO,
            duration: None,
        });

        let ctx = Context {
            rx,
            tx: tx.clone(),
            rt,
            sinks,
            supplier,
            events: events.clone(),
            status: status_tx,
            position_interval,
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
