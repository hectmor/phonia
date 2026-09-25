//! The daemon proper: an engine and a queue, driven by requests and observed through events.
//!
//! Transport-independent: [`Daemon::handle`] answers one request, [`Daemon::subscribe`] gives the
//! ordered stream of events. The server in `server.rs` puts them on a socket.

use crate::convert;
use crate::outputs::Outputs;
use futures_util::StreamExt;
use futures_util::stream;
use phonia_core::control::Controller;
use phonia_core::engine::{self, Command, Engine, TrackOpener};
use phonia_core::openers::{DescribeError, DispatchOpener, Source};
use phonia_core::output::SinkFactory;
use phonia_core::output::alsa::SinkReport;
use phonia_core::queue::{ItemId, Queue, QueueTrack};
use phonia_ipc as ipc;
use phonia_ipc::{AddAt, ErrorCode, NewTrack, Payload, ProtocolError, Reply, Request};
use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc, watch};

/// Completes once the daemon has been asked to stop. (A helper so the guard `wait_for` returns is
/// dropped here, not carried across the caller's awaits, where it would make the future `!Send`.)
pub async fn wait_for_shutdown(signal: &mut watch::Receiver<bool>) {
    let _ = signal.wait_for(|stopping| *stopping).await;
}

/// The most tracks one `queue_add` may carry.
const MAX_ADD: usize = 1000;
/// How many tracks are described at once when adding: enough to be quick over the network, few
/// enough not to hammer TIDAL.
const DESCRIBE_CONCURRENCY: usize = 8;
/// Events held for a slow subscriber before it is told to resync.
const EVENT_BACKLOG: usize = 1024;

pub struct DaemonParts {
    pub sinks: Arc<dyn SinkFactory>,
    pub opener: Arc<DispatchOpener>,
    /// Bit-perfect reports from the sinks, to be announced to clients.
    pub reports: mpsc::UnboundedReceiver<SinkReport>,
    pub engine: engine::Options,
    /// Which output the daemon is on and how to move it.
    pub outputs: Outputs,
}

pub struct Daemon {
    controller: Controller,
    outputs: Outputs,
    opener: Arc<DispatchOpener>,
    events: broadcast::Sender<(u64, ipc::Event)>,
    /// The sequence number of the last event published; only changed under `publish_lock`.
    seq: AtomicU64,
    /// Held while numbering and sending an event, so every subscriber sees the same total order.
    publish_lock: Mutex<()>,
    /// Serializes requests that change things, so compound operations (removing the playing
    /// entry and skipping) can't interleave between clients.
    control_lock: tokio::sync::Mutex<()>,
    shutdown: watch::Sender<bool>,
    info: ipc::ServerInfo,
}

impl Daemon {
    /// Starts the engine and the task that turns everything that happens into ordered events.
    /// Must be called inside a tokio runtime.
    pub fn start(parts: DaemonParts) -> Result<Arc<Daemon>> {
        let queue = Queue::new(parts.opener.clone() as Arc<dyn TrackOpener>);
        let engine = Engine::spawn_with_options(tokio::runtime::Handle::current(), parts.sinks, queue.clone(), parts.engine)?;
        let controller = Controller::new(engine, queue);

        let (events, _) = broadcast::channel(EVENT_BACKLOG);
        let (shutdown, _) = watch::channel(false);
        let daemon = Arc::new(Daemon {
            controller,
            outputs: parts.outputs,
            opener: parts.opener,
            events,
            seq: AtomicU64::new(0),
            publish_lock: Mutex::new(()),
            control_lock: tokio::sync::Mutex::new(()),
            shutdown,
            info: ipc::ServerInfo {
                name: "phoniad".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                pid: std::process::id(),
            },
        });

        tokio::spawn(daemon.clone().fan_in(parts.reports));
        Ok(daemon)
    }

    /// Everything that happens (engine events, queue changes, sink reports) in one place, where
    /// each gets its sequence number.
    async fn fan_in(self: Arc<Self>, mut reports: mpsc::UnboundedReceiver<SinkReport>) {
        let mut engine_events = self.controller.subscribe_events();
        let mut queue_changes = self.controller.queue().subscribe();
        let mut shutdown = self.shutdown.subscribe();
        loop {
            tokio::select! {
                event = engine_events.recv() => match event {
                    Ok(event) => {
                        let queue = self.controller.snapshot();
                        self.publish(|_| convert::event(&event, &queue));
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        self.publish(|seq| {
                            let (status, queue) = self.state();
                            ipc::Event::Resync { skipped, seq, status, queue }
                        });
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                Ok(()) = queue_changes.changed() => {
                    let queue = queue_changes.borrow_and_update().clone();
                    self.publish(|_| ipc::Event::QueueChanged { queue: convert::queue_dto(&queue) });
                }
                Some(report) = reports.recv() => {
                    self.publish(|_| ipc::Event::SinkReport(convert::sink_report(&report)));
                }
                _ = shutdown.changed() => break,
            }
        }
    }

    /// Numbers an event and sends it to every subscriber.
    fn publish(&self, make: impl FnOnce(u64) -> ipc::Event) {
        let _order = self.publish_lock.lock().unwrap();
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        // Nobody subscribed is not an error.
        let _ = self.events.send((seq, make(seq)));
    }

    fn state(&self) -> (ipc::Status, ipc::Queue) {
        let queue = self.controller.snapshot();
        (
            convert::status_dto(&self.controller.status(), &queue, Some(self.outputs.route())),
            convert::queue_dto(&queue),
        )
    }

    /// The state right now, with the sequence number of the last event it includes. A snapshot
    /// may already reflect an event or two beyond that number, which a client just sees twice:
    /// events describe state and applying one again changes nothing.
    pub fn snapshot(&self) -> (u64, ipc::Status, ipc::Queue) {
        let seq = self.seq.load(Ordering::SeqCst);
        let (status, queue) = self.state();
        (seq, status, queue)
    }

    /// The events from now on, each with its sequence number.
    pub fn subscribe(&self) -> broadcast::Receiver<(u64, ipc::Event)> {
        self.events.subscribe()
    }

    pub fn hello(&self) -> ipc::ServerHello {
        ipc::ServerHello { protocol: ipc::PROTOCOL, server: self.info.clone(), capabilities: vec![ipc::CAP_OUTPUT_RELEASE.to_string(), ipc::CAP_OUTPUT_SELECT.to_string()] }
    }

    /// Flips to true when the daemon should stop.
    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    /// Asks everything to stop: tells subscribers, then wakes whoever waits on
    /// [`Daemon::shutdown_signal`].
    pub fn request_shutdown(&self) {
        self.publish(|_| ipc::Event::ShuttingDown);
        self.shutdown.send_replace(true);
    }

    /// Stops playback and releases the audio device, waiting for the audio thread to end.
    pub async fn stop_engine(self: &Arc<Self>) {
        let daemon = self.clone();
        // The audio thread may take up to a buffer to notice; that must not block the runtime.
        let _ = tokio::task::spawn_blocking(move || daemon.controller.shutdown()).await;
    }

    /// Answers one request. `hello`, `subscribe` and `unsubscribe` belong to a connection, not to
    /// the daemon, so the server deals with them.
    pub async fn handle(&self, request: Request) -> Reply {
        match request {
            Request::Status => Reply::Ok(Payload::Status(self.state().0)),
            Request::Queue => Reply::Ok(Payload::Queue(self.state().1)),
            Request::QueueAdd { tracks, at } => self.queue_add(tracks, at).await,
            Request::Outputs => {
                let entries = self.outputs.list().await;
                Reply::Ok(Payload::Outputs {
                    outputs: entries.iter().map(convert::output_info).collect(),
                    current: Some(self.outputs.route().id),
                })
            }
            Request::SetOutput { output } => self.set_output(&output).await,
            Request::Hello { .. } | Request::Subscribe | Request::Unsubscribe => {
                error(ErrorCode::BadRequest, "this request is handled by the connection")
            }
            Request::Unknown => error(ErrorCode::UnknownRequest, "this daemon does not know that request"),
            mutating => {
                let _serial = self.control_lock.lock().await;
                self.change(mutating)
            }
        }
    }

    /// Moves playback to another output, keeping the track and the position. The engine has
    /// moved even if the new output could not be opened (it is paused on the track, ready for a
    /// resume), so the route follows it and clients are told either way.
    async fn set_output(&self, id: &str) -> Reply {
        let _serial = self.control_lock.lock().await;
        let (spec, factory) = match self.outputs.prepare(id) {
            Ok(prepared) => prepared,
            Err(error) => return self::error(ErrorCode::BadRequest, &format!("{error:#}")),
        };
        // Taking a card or opening a Bluetooth speaker can take a while, and the audio thread
        // may be in the middle of a write.
        let switched = tokio::task::block_in_place(|| self.controller.set_output(factory));
        let entries = self.outputs.list().await;
        self.outputs.switched_to(spec, &entries);
        let route = self.outputs.route();
        self.publish(|_| ipc::Event::OutputChanged { route });
        match switched {
            Ok(()) => Reply::Ok(Payload::Ack),
            Err(error) => self::error(ErrorCode::Internal, &format!("{error:#}")),
        }
    }

    /// Names the output the daemon started on the way the list of outputs does.
    pub async fn refresh_route(&self) {
        self.outputs.refresh().await;
    }

    /// Outputs appeared or disappeared: tells subscribers to ask again.
    pub fn outputs_changed(&self) {
        self.publish(|_| ipc::Event::OutputsChanged);
    }

    /// The requests that change something. Called with the control lock held.
    fn change(&self, request: Request) -> Reply {
        let send = |command: Command| match self.controller.send(command) {
            Ok(()) => Reply::Ok(Payload::Ack),
            Err(error) => self::error(ErrorCode::EngineGone, &format!("{error:#}")),
        };
        match request {
            Request::Play { item: None } => send(Command::Play(None)),
            Request::Play { item: Some(id) } => match self.controller.play_item(ItemId(id.0)) {
                Ok(()) => Reply::Ok(Payload::Ack),
                Err(error) => self::error(ErrorCode::NotFound, &format!("{error:#}")),
            },
            Request::Stop => send(Command::Stop),
            Request::Pause => send(Command::Pause),
            Request::Resume => send(Command::Resume),
            Request::TogglePause => send(Command::TogglePause),
            Request::Next => send(Command::Next),
            Request::Previous => send(Command::Previous),
            Request::Release => send(Command::Release),
            Request::Seek { target } => send(Command::Seek(convert::seek_target(target))),
            Request::QueueRemove { ids } => {
                let ids: Vec<ItemId> = ids.iter().map(|id| ItemId(id.0)).collect();
                Reply::Ok(Payload::Removed { count: self.controller.remove(&ids) })
            }
            Request::QueueMove { id, to } => {
                if self.controller.queue().move_to(ItemId(id.0), to) {
                    Reply::Ok(Payload::Ack)
                } else {
                    error(ErrorCode::NotFound, &format!("there is no queue entry {}", id.0))
                }
            }
            Request::QueueClear => {
                self.controller.clear();
                Reply::Ok(Payload::Ack)
            }
            Request::SetShuffle { shuffle } => {
                self.controller.queue().set_shuffle(shuffle);
                Reply::Ok(Payload::Ack)
            }
            Request::SetRepeat { repeat } => {
                self.controller.queue().set_repeat(convert::repeat_from_wire(repeat));
                Reply::Ok(Payload::Ack)
            }
            Request::Shutdown => {
                self.request_shutdown();
                Reply::Ok(Payload::Ack)
            }
            other => error(ErrorCode::Internal, &format!("{other:?} reached the wrong handler")),
        }
    }

    /// Adds tracks. Their titles and lengths are looked up first (several at once, which for TIDAL
    /// is a network call each), and the queue is only touched at the end, so a slow lookup never
    /// holds up other clients. A track that is wrong is refused; one whose details could not be
    /// fetched just now is added without them.
    async fn queue_add(&self, tracks: Vec<NewTrack>, at: AddAt) -> Reply {
        if tracks.len() > MAX_ADD {
            return error(ErrorCode::BadRequest, &format!("at most {MAX_ADD} tracks can be added at once"));
        }

        enum Outcome {
            Ready(Source, phonia_core::openers::SourceInfo),
            Unresolved(Source, String),
            Rejected(String, String),
        }

        let outcomes: Vec<Outcome> = stream::iter(tracks)
            .map(|track| async move {
                let source = match Source::parse(&track.source) {
                    Ok(source) => source,
                    Err(error) => return Outcome::Rejected(track.source, format!("{error:#}")),
                };
                match self.opener.describe(&source).await {
                    Ok(info) => Outcome::Ready(source, info),
                    Err(DescribeError::Invalid(reason)) => Outcome::Rejected(track.source, reason),
                    Err(DescribeError::Unavailable(reason)) => Outcome::Unresolved(source, reason),
                }
            })
            .buffered(DESCRIBE_CONCURRENCY)
            .collect()
            .await;

        let (mut accepted, mut unresolved_reasons, mut rejected) = (Vec::new(), Vec::new(), Vec::new());
        for outcome in outcomes {
            match outcome {
                Outcome::Ready(source, info) => accepted.push((source, info.title, info.duration, None)),
                Outcome::Unresolved(source, reason) => accepted.push((source, None, None, Some(reason))),
                Outcome::Rejected(source, reason) => rejected.push(ipc::Rejected { source, reason }),
            }
        }

        let _serial = self.control_lock.lock().await;
        let tracks: Vec<QueueTrack> = accepted
            .iter()
            .map(|(source, title, duration, _)| QueueTrack {
                source: phonia_core::engine::TrackRef(source.to_wire()),
                title: title.clone(),
                duration: *duration,
            })
            .collect();
        let queue = self.controller.queue();
        let ids = match at {
            AddAt::End => queue.add(tracks),
            AddAt::Next => queue.play_next(tracks),
            AddAt::Index { index } => queue.insert(index, tracks),
        };
        for (id, (_, _, _, reason)) in ids.iter().zip(&accepted) {
            if let Some(reason) = reason {
                unresolved_reasons.push(ipc::Unresolved { id: ipc::ItemId(id.0), reason: reason.clone() });
            }
        }

        Reply::Ok(Payload::Added {
            ids: ids.iter().map(|id| ipc::ItemId(id.0)).collect(),
            rejected,
            unresolved: unresolved_reasons,
        })
    }
}

fn error(code: ErrorCode, message: &str) -> Reply {
    Reply::Err(ProtocolError { code, message: message.to_string() })
}
