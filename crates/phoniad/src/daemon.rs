//! The daemon proper: an engine and a queue, driven by requests and observed through events.
//!
//! Transport-independent: [`Daemon::handle`] answers one request, [`Daemon::subscribe`] gives the
//! ordered stream of events. The server in `server.rs` puts them on a socket.

use crate::convert;
use crate::outputs::{Outputs, VolumeError};
use anyhow::Result;
use futures_util::StreamExt;
use futures_util::stream;
use phonia_core::catalog::{
    self as tidal_catalog, AlbumFilter, Catalog, MAX_ITEMS_LIMIT, MAX_SEARCH_LIMIT,
};
use phonia_core::control::Controller;
use phonia_core::engine::{self, Command, Engine, TrackOpener};
use phonia_core::openers::{DescribeError, DispatchOpener, QualityLimits, Source};
use phonia_core::output::SinkFactory;
use phonia_core::output::alsa::SinkReport;
use phonia_core::queue::{ItemId, Queue, QueueTrack};
use phonia_ipc as ipc;
use phonia_ipc::{AddAt, ErrorCode, NewTrack, Payload, ProtocolError, Reply, Request};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc, watch};

/// Completes once the daemon has been asked to stop. (A helper so the guard `wait_for` returns is
/// dropped here, not carried across the caller's awaits, where it would make the future `!Send`.)
pub async fn wait_for_shutdown(signal: &mut watch::Receiver<bool>) {
    let _ = signal.wait_for(|stopping| *stopping).await;
}

/// A track on its way into the queue: what it is, its title and length if known, and why they
/// are not known, if they are not.
type Accepted = (
    Source,
    Option<String>,
    Option<std::time::Duration>,
    Option<String>,
);

/// How many results of each kind a search returns when the client does not say.
const DEFAULT_SEARCH_LIMIT: u32 = 50;

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
    /// The tiers TIDAL is asked for, if the daemon plays from TIDAL.
    pub quality: Option<Arc<QualityLimits>>,
    /// TIDAL's catalog, if the daemon has a login to browse it with.
    pub catalog: Option<Arc<dyn Catalog>>,
}

pub struct Daemon {
    controller: Controller,
    outputs: Outputs,
    opener: Arc<DispatchOpener>,
    quality: Option<Arc<QualityLimits>>,
    catalog: Option<Arc<dyn Catalog>>,
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
        let first_sinks = parts.sinks.clone();
        let engine = Engine::spawn_with_options(
            tokio::runtime::Handle::current(),
            parts.sinks,
            queue.clone(),
            parts.engine,
        )?;
        let controller = Controller::new(engine, queue);

        let (events, _) = broadcast::channel(EVENT_BACKLOG);
        let (shutdown, _) = watch::channel(false);
        let daemon = Arc::new(Daemon {
            controller,
            outputs: parts.outputs,
            opener: parts.opener,
            quality: parts.quality,
            catalog: parts.catalog,
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

        // The volume can be changed from the desktop's mixer too; the daemon follows and announces it.
        let weak = Arc::downgrade(&daemon);
        daemon.outputs.on_volume_change(Arc::new(move |volume| {
            if let Some(daemon) = weak.upgrade() {
                daemon.volume_changed_outside(volume);
            }
        }));
        daemon.outputs.attach(first_sinks, false);

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
            convert::status_dto(
                &self.controller.status(),
                &queue,
                Some(self.outputs.route()),
                self.outputs.volume(),
                self.quality_range(),
            ),
            convert::queue_dto(&queue),
        )
    }

    fn quality_range(&self) -> Option<ipc::QualityRange> {
        self.quality.as_ref().map(|limits| ipc::QualityRange {
            max: convert::quality(limits.max()),
            min: convert::quality(limits.min()),
        })
    }

    /// Changes the best tier to ask TIDAL for and tells everyone.
    fn set_max_quality(&self, quality: ipc::Quality) -> Reply {
        let Some(limits) = &self.quality else {
            return self::error(
                ErrorCode::Unsupported,
                "this daemon does not play from TIDAL",
            );
        };
        let Some(tier) = convert::core_quality(quality) else {
            return self::error(ErrorCode::BadRequest, "that quality is not known");
        };
        match limits.set_max(tier) {
            Ok(()) => {
                self.publish(|_| ipc::Event::MaxQualityChanged { quality });
                Reply::Ok(Payload::Ack)
            }
            Err(error) => self::error(ErrorCode::BadRequest, &format!("{error:#}")),
        }
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
        let mut capabilities = vec![
            ipc::CAP_OUTPUT_RELEASE.to_string(),
            ipc::CAP_OUTPUT_SELECT.to_string(),
            ipc::CAP_VOLUME.to_string(),
            ipc::CAP_GAPLESS.to_string(),
            ipc::CAP_QUALITY.to_string(),
        ];
        // Only a daemon with a login to browse with can search.
        if self.catalog.is_some() {
            capabilities.push(ipc::CAP_CATALOG.to_string());
        }
        ipc::ServerHello {
            protocol: ipc::PROTOCOL,
            server: self.info.clone(),
            capabilities,
        }
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
            Request::QueueAddFrom { from, at } => self.queue_add_from(from, at).await,
            Request::Album { id, limit } => self.album(id, limit).await,
            Request::Artist { id, limit } => self.artist(id, limit).await,
            Request::Tracks {
                from,
                offset,
                limit,
            } => self.tracks(from, offset, limit).await,
            Request::Albums {
                from,
                offset,
                limit,
            } => self.albums(from, offset, limit).await,
            Request::Search {
                query,
                kinds,
                offset,
                limit,
            } => self.search(query, kinds, offset, limit).await,
            Request::Outputs => {
                let entries = self.outputs.list().await;
                Reply::Ok(Payload::Outputs {
                    outputs: entries.iter().map(convert::output_info).collect(),
                    current: Some(self.outputs.route().id),
                })
            }
            Request::SetOutput { output } => self.set_output(&output).await,
            Request::Hello { .. } | Request::Subscribe | Request::Unsubscribe => error(
                ErrorCode::BadRequest,
                "this request is handled by the connection",
            ),
            Request::Unknown => error(
                ErrorCode::UnknownRequest,
                "this daemon does not know that request",
            ),
            mutating => {
                let _serial = self.control_lock.lock().await;
                self.change(mutating)
            }
        }
    }

    /// Searches TIDAL. Read-only, so it does not take the control lock; the server also runs it
    /// beside the connection's other requests, because it can take a second or two.
    async fn search(
        &self,
        query: String,
        kinds: Vec<ipc::CatalogKind>,
        offset: u32,
        limit: Option<u32>,
    ) -> Reply {
        let Some(catalog) = &self.catalog else {
            return self::error(
                ErrorCode::Unsupported,
                "this daemon has no TIDAL catalog to search",
            );
        };
        let query = query.trim().to_string();
        if query.is_empty() {
            return self::error(ErrorCode::BadRequest, "there is nothing to search for");
        }
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        if limit == 0 || limit > MAX_SEARCH_LIMIT {
            return self::error(
                ErrorCode::BadRequest,
                &format!("the limit must be between 1 and {MAX_SEARCH_LIMIT}"),
            );
        }
        let mut wanted = Vec::new();
        for kind in &kinds {
            if let Some(kind) = convert::catalog_kind(*kind)
                && !wanted.contains(&kind)
            {
                wanted.push(kind);
            }
        }
        if !kinds.is_empty() && wanted.is_empty() {
            return self::error(
                ErrorCode::BadRequest,
                "this daemon knows none of those kinds",
            );
        }
        match catalog.search(query.clone(), wanted, offset, limit).await {
            Ok(found) => Reply::Ok(Payload::SearchResults {
                query,
                tracks: found
                    .tracks
                    .as_ref()
                    .map(|page| convert::page(page, convert::track_summary)),
                albums: found
                    .albums
                    .as_ref()
                    .map(|page| convert::page(page, convert::album_summary)),
                artists: found
                    .artists
                    .as_ref()
                    .map(|page| convert::page(page, convert::artist_summary)),
                playlists: found
                    .playlists
                    .as_ref()
                    .map(|page| convert::page(page, convert::playlist_summary)),
            }),
            Err(failure) => {
                let (code, message) = convert::catalog_error(&failure);
                self::error(code, &message)
            }
        }
    }

    /// The catalog, or the reply that says the daemon has none.
    // A reply is built once per request and dropped: boxing it would only complicate the callers.
    #[allow(clippy::result_large_err)]
    fn catalog_or_refuse(&self, what: &str) -> Result<&Arc<dyn Catalog>, Reply> {
        self.catalog.as_ref().ok_or_else(|| {
            self::error(
                ErrorCode::Unsupported,
                &format!("this daemon has no TIDAL catalog to {what}"),
            )
        })
    }

    /// An album and the first page of its tracks, asked for at the same time.
    async fn album(&self, id: String, limit: Option<u32>) -> Reply {
        let catalog = match self.catalog_or_refuse("show albums from") {
            Ok(catalog) => catalog,
            Err(reply) => return reply,
        };
        let limit = match list_limit(limit, MAX_ITEMS_LIMIT) {
            Ok(limit) => limit,
            Err(reply) => return reply,
        };
        let (album, tracks) = tokio::join!(
            catalog.album(id.clone()),
            catalog.album_tracks(id, 0, limit)
        );
        match (album, tracks) {
            (Ok(album), Ok(tracks)) => Reply::Ok(Payload::Album {
                album: convert::album_summary(&album),
                tracks: convert::page(&tracks, convert::track_summary),
            }),
            (Err(failure), _) | (_, Err(failure)) => catalog_failure(&failure),
        }
    }

    /// An artist with all that its view shows, asked for at the same time. Only the bio may be
    /// missing without failing the rest: it is a nicety, and TIDAL has none for many artists.
    async fn artist(&self, id: String, limit: Option<u32>) -> Reply {
        let catalog = match self.catalog_or_refuse("show artists from") {
            Ok(catalog) => catalog,
            Err(reply) => return reply,
        };
        let limit = match list_limit(limit, DEFAULT_SEARCH_LIMIT) {
            Ok(limit) => limit,
            Err(reply) => return reply,
        };
        let (artist, bio, top_tracks, albums, singles) = tokio::join!(
            catalog.artist(id.clone()),
            catalog.artist_bio(id.clone()),
            catalog.artist_top_tracks(id.clone(), 0, limit),
            catalog.artist_albums(id.clone(), AlbumFilter::Albums, 0, limit),
            catalog.artist_albums(id, AlbumFilter::EpsAndSingles, 0, limit),
        );
        match (artist, top_tracks, albums, singles) {
            (Ok(artist), Ok(top_tracks), Ok(albums), Ok(singles)) => Reply::Ok(Payload::Artist {
                artist: convert::artist_summary(&artist),
                bio: bio.ok().flatten(),
                top_tracks: convert::page(&top_tracks, convert::track_summary),
                albums: convert::page(&albums, convert::album_summary),
                singles: convert::page(&singles, convert::album_summary),
            }),
            (Err(failure), ..)
            | (_, Err(failure), ..)
            | (_, _, Err(failure), _)
            | (.., Err(failure)) => catalog_failure(&failure),
        }
    }

    /// One page of a list of tracks.
    async fn tracks(&self, from: ipc::CatalogRef, offset: u32, limit: Option<u32>) -> Reply {
        let catalog = match self.catalog_or_refuse("list tracks from") {
            Ok(catalog) => catalog,
            Err(reply) => return reply,
        };
        let limit = match list_limit(limit, DEFAULT_SEARCH_LIMIT) {
            Ok(limit) => limit,
            Err(reply) => return reply,
        };
        let page = match track_page(catalog.as_ref(), &from, offset, limit) {
            Ok(page) => page,
            Err(reply) => return reply,
        };
        match page.await {
            Ok(page) => Reply::Ok(Payload::Tracks {
                from,
                page: convert::page(&page, convert::track_summary),
            }),
            Err(failure) => catalog_failure(&failure),
        }
    }

    /// One page of a list of albums.
    async fn albums(&self, from: ipc::AlbumListRef, offset: u32, limit: Option<u32>) -> Reply {
        let catalog = match self.catalog_or_refuse("list albums from") {
            Ok(catalog) => catalog,
            Err(reply) => return reply,
        };
        let limit = match list_limit(limit, DEFAULT_SEARCH_LIMIT) {
            Ok(limit) => limit,
            Err(reply) => return reply,
        };
        let (id, filter) = match &from {
            ipc::AlbumListRef::ArtistAlbums { id } => (id.clone(), AlbumFilter::Albums),
            ipc::AlbumListRef::ArtistSingles { id } => (id.clone(), AlbumFilter::EpsAndSingles),
            ipc::AlbumListRef::Unknown => {
                return self::error(
                    ErrorCode::BadRequest,
                    "this daemon does not know that list of albums",
                );
            }
        };
        match catalog.artist_albums(id, filter, offset, limit).await {
            Ok(page) => Reply::Ok(Payload::Albums {
                from,
                page: convert::page(&page, convert::album_summary),
            }),
            Err(failure) => catalog_failure(&failure),
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
        let switched = tokio::task::block_in_place(|| self.controller.set_output(factory.clone()));
        let entries = self.outputs.list().await;
        self.outputs.switched_to(spec, &entries, factory);
        let route = self.outputs.route();
        self.publish(|_| ipc::Event::OutputChanged { route });
        // The new output starts at the volume that was set; the clients are told what it is.
        if let Some(volume) = self.outputs.volume() {
            self.publish(|_| ipc::Event::VolumeChanged {
                percent: volume.percent,
                muted: volume.muted,
            });
        }
        match switched {
            Ok(()) => Reply::Ok(Payload::Ack),
            Err(error) => self::error(ErrorCode::Internal, &format!("{error:#}")),
        }
    }

    /// Names the output the daemon started on the way the list of outputs does.
    pub async fn refresh_route(&self) {
        self.outputs.refresh().await;
    }

    /// Changes the volume and tells everyone. Refused for an output that has none to set.
    fn change_volume(&self, change: impl FnOnce(&mut phonia_core::output::Volume)) -> Reply {
        match self.outputs.set_volume(change) {
            Ok(volume) => {
                self.publish(|_| ipc::Event::VolumeChanged {
                    percent: volume.percent,
                    muted: volume.muted,
                });
                Reply::Ok(Payload::Ack)
            }
            Err(VolumeError::Unsupported) => self::error(
                ErrorCode::Unsupported,
                "this output has no volume to set: an exclusive card plays the audio unscaled, so use \
                 the DAC's own volume, or switch to a shared output",
            ),
            Err(VolumeError::Failed(why)) => self::error(ErrorCode::Internal, &why),
        }
    }

    /// The desktop's mixer changed the volume of the stream.
    fn volume_changed_outside(&self, volume: phonia_core::output::Volume) {
        self.outputs.volume_changed_outside(volume);
        self.publish(|_| ipc::Event::VolumeChanged {
            percent: volume.percent,
            muted: volume.muted,
        });
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
            Request::SetVolume { percent } => self.change_volume(|volume| volume.percent = percent),
            Request::SetMute { mute } => self.change_volume(|volume| volume.muted = mute),
            Request::SetMaxQuality { quality } => self.set_max_quality(quality),
            Request::Seek { target } => send(Command::Seek(convert::seek_target(target))),
            Request::QueueRemove { ids } => {
                let ids: Vec<ItemId> = ids.iter().map(|id| ItemId(id.0)).collect();
                Reply::Ok(Payload::Removed {
                    count: self.controller.remove(&ids),
                })
            }
            Request::QueueMove { id, to } => {
                if self.controller.queue().move_to(ItemId(id.0), to) {
                    Reply::Ok(Payload::Ack)
                } else {
                    error(
                        ErrorCode::NotFound,
                        &format!("there is no queue entry {}", id.0),
                    )
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
                self.controller
                    .queue()
                    .set_repeat(convert::repeat_from_wire(repeat));
                Reply::Ok(Payload::Ack)
            }
            Request::Shutdown => {
                self.request_shutdown();
                Reply::Ok(Payload::Ack)
            }
            other => error(
                ErrorCode::Internal,
                &format!("{other:?} reached the wrong handler"),
            ),
        }
    }

    /// Adds tracks. Their titles and lengths are looked up first (several at once, which for TIDAL
    /// is a network call each), and the queue is only touched at the end, so a slow lookup never
    /// holds up other clients. A track that is wrong is refused; one whose details could not be
    /// fetched just now is added without them.
    async fn queue_add(&self, tracks: Vec<NewTrack>, at: AddAt) -> Reply {
        if tracks.len() > MAX_ADD {
            return error(
                ErrorCode::BadRequest,
                &format!("at most {MAX_ADD} tracks can be added at once"),
            );
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

        let (mut accepted, mut rejected) = (Vec::new(), Vec::new());
        for outcome in outcomes {
            match outcome {
                Outcome::Ready(source, info) => {
                    accepted.push((source, info.title, info.duration, None))
                }
                Outcome::Unresolved(source, reason) => {
                    accepted.push((source, None, None, Some(reason)))
                }
                Outcome::Rejected(source, reason) => {
                    rejected.push(ipc::Rejected { source, reason })
                }
            }
        }

        self.add_to_queue(accepted, rejected, at).await
    }

    /// Puts tracks in the queue where `at` says, and answers with what was added. Each has its
    /// title and length if they are known, and the reason they were not, if that is why they lack
    /// them.
    async fn add_to_queue(
        &self,
        accepted: Vec<Accepted>,
        rejected: Vec<ipc::Rejected>,
        at: AddAt,
    ) -> Reply {
        let mut unresolved_reasons = Vec::new();
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
                unresolved_reasons.push(ipc::Unresolved {
                    id: ipc::ItemId(id.0),
                    reason: reason.clone(),
                });
            }
        }

        Reply::Ok(Payload::Added {
            ids: ids.iter().map(|id| ipc::ItemId(id.0)).collect(),
            rejected,
            unresolved: unresolved_reasons,
        })
    }

    /// Adds the tracks of an album or a playlist, listing them from TIDAL page by page. Their
    /// titles and lengths come with the listing, so none of them needs asking about one by one.
    async fn queue_add_from(&self, from: ipc::CatalogRef, at: AddAt) -> Reply {
        let Some(catalog) = &self.catalog else {
            return self::error(
                ErrorCode::Unsupported,
                "this daemon has no TIDAL catalog to add from",
            );
        };
        let mut tracks: Vec<tidal_catalog::Track> = Vec::new();
        loop {
            let page = match track_page(
                catalog.as_ref(),
                &from,
                tracks.len() as u32,
                MAX_ITEMS_LIMIT,
            ) {
                Ok(page) => page,
                Err(reply) => return reply,
            };
            let page = match page.await {
                Ok(page) => page,
                Err(failure) => {
                    let (code, message) = convert::catalog_error(&failure);
                    return self::error(code, &message);
                }
            };
            // Asked before the rest is fetched: no point listing what will not fit.
            if page.total > MAX_ADD as u64 {
                return self::error(
                    ErrorCode::BadRequest,
                    &format!(
                        "that has {} tracks and at most {MAX_ADD} can be added at once",
                        page.total
                    ),
                );
            }
            let got = page.items.len();
            tracks.extend(page.items);
            // An empty page ends it too, so a listing that lies about its total cannot loop.
            if got == 0 || tracks.len() as u64 >= page.total {
                break;
            }
        }

        let (mut accepted, mut rejected) = (Vec::new(), Vec::new());
        for track in &tracks {
            let source = Source::Tidal(track.id.clone());
            if track.streamable {
                let name = match track.artists.first() {
                    Some(artist) => format!("{} - {}", artist.name, track.title),
                    None => track.title.clone(),
                };
                accepted.push((source, Some(name), track.duration, None));
            } else {
                rejected.push(ipc::Rejected {
                    source: source.to_wire(),
                    reason: format!(
                        "{} is listed by TIDAL but cannot be streamed here",
                        track.title
                    ),
                });
            }
        }
        self.add_to_queue(accepted, rejected, at).await
    }
}

/// The size of a page a client asks for: `default` when it does not say, and between 1 and 100
/// when it does.
// See `catalog_or_refuse`: the reply is the point, and it is built once.
#[allow(clippy::result_large_err)]
fn list_limit(limit: Option<u32>, default: u32) -> Result<u32, Reply> {
    match limit.unwrap_or(default) {
        0 => Err(error(ErrorCode::BadRequest, "the limit must be at least 1")),
        n if n > MAX_ITEMS_LIMIT => Err(error(
            ErrorCode::BadRequest,
            &format!("the limit must be at most {MAX_ITEMS_LIMIT}"),
        )),
        n => Ok(n),
    }
}

/// The catalog's failure as clients are told of it.
fn catalog_failure(failure: &tidal_catalog::CatalogError) -> Reply {
    let (code, message) = convert::catalog_error(failure);
    error(code, &message)
}

/// Asks the catalog for a page of the list of tracks `from` names, or says that this daemon does
/// not know that list.
#[allow(clippy::result_large_err)]
fn track_page(
    catalog: &dyn Catalog,
    from: &ipc::CatalogRef,
    offset: u32,
    limit: u32,
) -> Result<
    futures_util::future::BoxFuture<
        'static,
        Result<tidal_catalog::Page<tidal_catalog::Track>, tidal_catalog::CatalogError>,
    >,
    Reply,
> {
    Ok(match from {
        ipc::CatalogRef::Album { id } => catalog.album_tracks(id.clone(), offset, limit),
        ipc::CatalogRef::Playlist { id } => catalog.playlist_tracks(id.clone(), offset, limit),
        ipc::CatalogRef::ArtistTopTracks { id } => {
            catalog.artist_top_tracks(id.clone(), offset, limit)
        }
        ipc::CatalogRef::Unknown => {
            return Err(error(
                ErrorCode::BadRequest,
                "this daemon does not know how to list that",
            ));
        }
    })
}

fn error(code: ErrorCode, message: &str) -> Reply {
    Reply::Err(ProtocolError {
        code,
        message: message.to_string(),
    })
}
