//! The daemon proper: an engine and a queue, driven by requests and observed through events.
//!
//! Transport-independent: [`Daemon::handle`] answers one request, [`Daemon::subscribe`] gives the
//! ordered stream of events. The server in `server.rs` puts them on a socket.

use crate::autoplay;
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
use phonia_core::play_log::{PlayLog, PlaybackSession, SessionTracker};
use phonia_core::queue::{ItemId, Queue, QueueSnapshot, QueueTrack};
use phonia_ipc as ipc;
use phonia_ipc::{AddAt, ErrorCode, NewTrack, Payload, ProtocolError, Reply, Request};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, watch};

/// Completes once the daemon has been asked to stop. (A helper so the guard `wait_for` returns is
/// dropped here, not carried across the caller's awaits, where it would make the future `!Send`.)
pub async fn wait_for_shutdown(signal: &mut watch::Receiver<bool>) {
    let _ = signal.wait_for(|stopping| *stopping).await;
}

/// A track on its way into the queue: what it is, its title, artist, length, cover and album id
/// if known, and why they are not known, if they are not.
type Accepted = (
    Source,
    Option<String>,
    Option<String>,
    Option<std::time::Duration>,
    Option<String>,
    Option<String>,
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

/// How many tracks' lyrics [`LyricsCache`] keeps at once.
const LYRICS_CACHE_CAPACITY: usize = 64;

/// A bounded in-memory cache of lyrics by track id, lost when the daemon restarts. Only `Some`
/// and `None` answers are kept, never a failure: a transient TIDAL error is worth retrying, not
/// caching. Eviction is FIFO, the simplest policy that bounds memory; lyrics are not asked for
/// often enough for something smarter to matter.
struct LyricsCache {
    order: std::collections::VecDeque<String>,
    entries: std::collections::HashMap<String, Option<ipc::Lyrics>>,
}

impl LyricsCache {
    fn new() -> Self {
        Self {
            order: std::collections::VecDeque::new(),
            entries: std::collections::HashMap::new(),
        }
    }

    fn get(&self, id: &str) -> Option<Option<ipc::Lyrics>> {
        self.entries.get(id).cloned()
    }

    fn insert(&mut self, id: String, lyrics: Option<ipc::Lyrics>) {
        if !self.entries.contains_key(&id) {
            self.order.push_back(id.clone());
            if self.order.len() > LYRICS_CACHE_CAPACITY
                && let Some(oldest) = self.order.pop_front()
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(id, lyrics);
    }
}

/// A bit-perfect report, stamped with the route id of the output whose sink produced it (see
/// `convert::sink_report`'s own doc comment for why that has to happen where the sink is built,
/// not later).
pub struct OutputReport {
    pub output: String,
    pub report: SinkReport,
}

pub struct DaemonParts {
    pub sinks: Arc<dyn SinkFactory>,
    pub opener: Arc<DispatchOpener>,
    /// Bit-perfect reports from the sinks, to be announced to clients.
    pub reports: mpsc::UnboundedReceiver<OutputReport>,
    pub engine: engine::Options,
    /// How ReplayGain is chosen; see `phonia_core::replaygain::Mode`. Set once at startup: there
    /// is no runtime way to change it.
    pub replaygain: phonia_core::replaygain::Mode,
    /// The startup value of autoplay; unlike `replaygain`, can be changed at runtime afterward
    /// (`Request::SetAutoplay`).
    pub autoplay: bool,
    /// Which output the daemon is on and how to move it.
    pub outputs: Outputs,
    /// The tiers TIDAL is asked for, if the daemon plays from TIDAL.
    pub quality: Option<Arc<QualityLimits>>,
    /// TIDAL's catalog, if the daemon has a login to browse it with.
    pub catalog: Option<Arc<dyn Catalog>>,
    /// Reports finished plays to TIDAL, if `[tidal] report_plays` is on.
    pub play_log: Option<PlayLog>,
}

pub struct Daemon {
    controller: Controller,
    outputs: Outputs,
    opener: Arc<DispatchOpener>,
    quality: Option<Arc<QualityLimits>>,
    catalog: Option<Arc<dyn Catalog>>,
    /// The tracker's state is only ever touched from `fan_in`, so a plain (non-async) lock is
    /// enough; it just has to be `Sync` to live on `Daemon`.
    play_log: Option<(PlayLog, Mutex<SessionTracker>)>,
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
    /// The verdict for the sink that is open right now, if one has been reported since it opened.
    /// Gated against the rest of a status with `SinkReport::applies_to` before it is shown, since
    /// nothing announces a sink closing or changing on its own.
    last_report: Mutex<Option<ipc::SinkReport>>,
    /// Lyrics already fetched this run, so reopening the same track's lyrics panel does not hit
    /// TIDAL again.
    lyrics_cache: Mutex<LyricsCache>,
    /// Autoplay's own bookkeeping; see `autoplay::Autoplay`. Only ever touched synchronously
    /// (`consider_autoplay`, `run_autoplay`, and the cancel on `Stop`/`QueueClear`), so a plain
    /// lock is enough.
    autoplay: Mutex<autoplay::Autoplay>,
}

impl Daemon {
    /// Starts the engine and the task that turns everything that happens into ordered events.
    /// Must be called inside a tokio runtime.
    pub fn start(parts: DaemonParts) -> Result<Arc<Daemon>> {
        let queue = Queue::new(parts.opener.clone() as Arc<dyn TrackOpener>);
        queue.set_replay_gain(parts.replaygain);
        queue.set_autoplay(parts.autoplay);
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
            play_log: parts
                .play_log
                .map(|play_log| (play_log, Mutex::new(SessionTracker::new()))),
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
            last_report: Mutex::new(None),
            lyrics_cache: Mutex::new(LyricsCache::new()),
            autoplay: Mutex::new(autoplay::Autoplay::default()),
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
    async fn fan_in(self: Arc<Self>, mut reports: mpsc::UnboundedReceiver<OutputReport>) {
        let mut engine_events = self.controller.subscribe_events();
        let mut queue_changes = self.controller.queue().subscribe();
        let mut shutdown = self.shutdown.subscribe();
        loop {
            tokio::select! {
                event = engine_events.recv() => match event {
                    Ok(event) => {
                        // The two events that mean the sink is gone: nothing else announces that,
                        // so the stale verdict has to be dropped here, not left for `applies_to`
                        // to catch (it can only compare against a *new* format or output, not "no
                        // sink at all").
                        if matches!(
                            event,
                            engine::Event::OutputReleased { .. }
                                | engine::Event::StateChanged(engine::State::Stopped)
                        ) {
                            *self.last_report.lock().unwrap() = None;
                        }
                        let queue = self.controller.snapshot();
                        self.note_play_log(&event, &queue);
                        self.publish(|_| convert::event(&event, &queue));
                        match &event {
                            engine::Event::TrackStarted { meta, .. } => {
                                let (tidal_id, _) = resolve_tidal_track(meta, &queue);
                                self.autoplay.lock().unwrap().note_started(tidal_id);
                                self.consider_autoplay();
                            }
                            engine::Event::QueueExhausted => {
                                self.autoplay.lock().unwrap().queue_exhausted(queue.current);
                            }
                            _ => {}
                        }
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
                    // Catches the condition becoming true without a fresh TrackStarted: autoplay
                    // turned on mid-track, repeat switched to off, or the entries after the
                    // current one removed.
                    self.consider_autoplay();
                }
                Some(OutputReport { output, report }) = reports.recv() => {
                    let dto = convert::sink_report(&report, &output);
                    *self.last_report.lock().unwrap() = Some(dto.clone());
                    self.publish(|_| ipc::Event::SinkReport(dto));
                }
                _ = shutdown.changed() => break,
            }
        }
    }

    /// Feeds an engine event to the play-log tracker, if reporting plays is on, and spawns a
    /// send for whatever session it finishes (if any). Resolving the source has to happen here,
    /// against this exact snapshot, not later: a gapless join can advance the queue before a
    /// `TrackEnded` for the track it followed is even converted.
    fn note_play_log(&self, event: &engine::Event, queue: &QueueSnapshot) {
        let Some((play_log, tracker)) = &self.play_log else {
            return;
        };
        let now = now_ms();
        let mut tracker = tracker.lock().unwrap();
        let finished = match event {
            engine::Event::TrackStarted { meta, .. } => {
                let (product_id, quality) = resolve_tidal_track(meta, queue);
                tracker.started(product_id, quality, None, now)
            }
            engine::Event::Position { position, .. } => {
                tracker.position(*position);
                None
            }
            engine::Event::Seeked { position } => {
                tracker.seeked(*position, now);
                None
            }
            engine::Event::StateChanged(engine::State::Paused) => {
                tracker.paused(now);
                None
            }
            engine::Event::StateChanged(engine::State::Playing) => {
                tracker.resumed(now);
                None
            }
            engine::Event::TrackEnded { meta, reason } => {
                tracker.ended(*reason, meta.duration, now)
            }
            _ => None,
        };
        drop(tracker);
        if let Some(session) = finished {
            spawn_report(play_log.clone(), session);
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
        let mut status = convert::status_dto(
            &self.controller.status(),
            &queue,
            Some(self.outputs.route()),
            self.outputs.volume(),
            self.quality_range(),
        );
        status.sink_report = self
            .last_report
            .lock()
            .unwrap()
            .clone()
            .filter(|report| report.applies_to(&status));
        (status, convert::queue_dto(&queue))
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
        // Only a daemon with a login to browse with can search, fetch lyrics, or autoplay
        // (which fetches more tracks from TIDAL to append).
        if self.catalog.is_some() {
            capabilities.push(ipc::CAP_CATALOG.to_string());
            capabilities.push(ipc::CAP_LYRICS.to_string());
            capabilities.push(ipc::CAP_AUTOPLAY.to_string());
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
            Request::Playlists {
                from,
                offset,
                limit,
            } => self.playlists(from, offset, limit).await,
            Request::Library { limit } => self.library(limit).await,
            Request::PlaylistFolder {
                folder,
                offset,
                limit,
            } => self.playlist_folder(folder, offset, limit).await,
            Request::Lyrics { id } => self.lyrics(id).await,
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

    /// A track's lyrics, synced or plain, cached in memory so asking again for the same track
    /// does not hit TIDAL again. Read-only like `search`, so it does not take the control lock;
    /// the server also runs it beside the connection's other requests, in case TIDAL is slow.
    async fn lyrics(&self, id: String) -> Reply {
        let catalog = match self.catalog_or_refuse("show lyrics from") {
            Ok(catalog) => catalog,
            Err(reply) => return reply,
        };
        if let Some(cached) = self.lyrics_cache.lock().unwrap().get(&id) {
            return Reply::Ok(Payload::Lyrics { id, lyrics: cached });
        }
        match catalog.track_lyrics(id.clone()).await {
            Ok(found) => {
                let dto = found.as_ref().map(convert::lyrics);
                self.lyrics_cache
                    .lock()
                    .unwrap()
                    .insert(id.clone(), dto.clone());
                Reply::Ok(Payload::Lyrics { id, lyrics: dto })
            }
            Err(failure) => catalog_failure(&failure),
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
        let page = match &from {
            ipc::AlbumListRef::ArtistAlbums { id } => {
                catalog
                    .artist_albums(id.clone(), AlbumFilter::Albums, offset, limit)
                    .await
            }
            ipc::AlbumListRef::ArtistSingles { id } => {
                catalog
                    .artist_albums(id.clone(), AlbumFilter::EpsAndSingles, offset, limit)
                    .await
            }
            ipc::AlbumListRef::FavoriteAlbums => catalog.favorite_albums(offset, limit).await,
            ipc::AlbumListRef::Unknown => {
                return self::error(
                    ErrorCode::BadRequest,
                    "this daemon does not know that list of albums",
                );
            }
        };
        match page {
            Ok(page) => Reply::Ok(Payload::Albums {
                from,
                page: convert::page(&page, convert::album_summary),
            }),
            Err(failure) => catalog_failure(&failure),
        }
    }

    /// One page of a list of playlists.
    async fn playlists(
        &self,
        from: ipc::PlaylistListRef,
        offset: u32,
        limit: Option<u32>,
    ) -> Reply {
        let catalog = match self.catalog_or_refuse("list playlists from") {
            Ok(catalog) => catalog,
            Err(reply) => return reply,
        };
        let limit = match list_limit(limit, DEFAULT_SEARCH_LIMIT) {
            Ok(limit) => limit,
            Err(reply) => return reply,
        };
        let page = match from {
            ipc::PlaylistListRef::Mine => catalog.my_playlists(offset, limit).await,
            ipc::PlaylistListRef::Unknown => {
                return self::error(
                    ErrorCode::BadRequest,
                    "this daemon does not know that list of playlists",
                );
            }
        };
        match page {
            Ok(page) => Reply::Ok(Payload::Playlists {
                from,
                page: convert::page(&page, convert::playlist_summary),
            }),
            Err(failure) => catalog_failure(&failure),
        }
    }

    /// One page of a playlist folder's own contents, sub-folders and playlists alike. `folder`
    /// is `None` for the root of "My Collection".
    async fn playlist_folder(
        &self,
        folder: Option<String>,
        offset: u32,
        limit: Option<u32>,
    ) -> Reply {
        let catalog = match self.catalog_or_refuse("list a playlist folder from") {
            Ok(catalog) => catalog,
            Err(reply) => return reply,
        };
        let limit = match list_limit(limit, DEFAULT_SEARCH_LIMIT) {
            Ok(limit) => limit,
            Err(reply) => return reply,
        };
        match catalog.playlist_folder(folder.clone(), offset, limit).await {
            Ok(page) => Reply::Ok(Payload::PlaylistFolder {
                folder,
                page: convert::page(&page, convert::folder_entry),
            }),
            Err(failure) => catalog_failure(&failure),
        }
    }

    /// The library: the first page of the user's favorite tracks, of their favorite albums, and
    /// of their own playlists, all asked for at the same time.
    async fn library(&self, limit: Option<u32>) -> Reply {
        let catalog = match self.catalog_or_refuse("show a library from") {
            Ok(catalog) => catalog,
            Err(reply) => return reply,
        };
        let limit = match list_limit(limit, DEFAULT_SEARCH_LIMIT) {
            Ok(limit) => limit,
            Err(reply) => return reply,
        };
        let (favorite_tracks, favorite_albums, my_playlists) = tokio::join!(
            catalog.favorite_tracks(0, limit),
            catalog.favorite_albums(0, limit),
            catalog.my_playlists(0, limit),
        );
        match (favorite_tracks, favorite_albums, my_playlists) {
            (Ok(favorite_tracks), Ok(favorite_albums), Ok(my_playlists)) => {
                Reply::Ok(Payload::Library {
                    favorite_tracks: convert::page(&favorite_tracks, convert::track_summary),
                    favorite_albums: convert::page(&favorite_albums, convert::album_summary),
                    my_playlists: convert::page(&my_playlists, convert::playlist_summary),
                })
            }
            (Err(failure), ..) | (_, Err(failure), _) | (.., Err(failure)) => {
                catalog_failure(&failure)
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
                "this output has no volume to set: it's an exclusive card with no hardware mixer \
                 control of its own. Use the DAC's own knob, or switch to a shared output",
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
            Request::Stop => {
                // An explicit Stop: a fetch already in flight for the entry that was playing
                // must not resume playback once it lands.
                self.autoplay.lock().unwrap().cancel();
                send(Command::Stop)
            }
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
                self.autoplay.lock().unwrap().cancel();
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
            Request::SetAutoplay { autoplay } => {
                self.controller.queue().set_autoplay(autoplay);
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
                Outcome::Ready(source, info) => accepted.push((
                    source,
                    info.title,
                    info.artist,
                    info.duration,
                    info.cover,
                    info.album_id,
                    None,
                )),
                Outcome::Unresolved(source, reason) => {
                    accepted.push((source, None, None, None, None, None, Some(reason)))
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
        let tracks: Vec<QueueTrack> = accepted.iter().map(queue_track).collect();
        let queue = self.controller.queue();
        let ids = match at {
            AddAt::End => queue.add(tracks),
            AddAt::Next => queue.play_next(tracks),
            AddAt::Index { index } => queue.insert(index, tracks),
        };
        for (id, (_, _, _, _, _, _, reason)) in ids.iter().zip(&accepted) {
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
            match accepted_from_catalog(track) {
                Ok(entry) => accepted.push(entry),
                Err(reason) => rejected.push(reason),
            }
        }
        self.add_to_queue(accepted, rejected, at).await
    }

    /// Whether autoplay should act right now, and if so, starts fetching that entry's radio in
    /// the background (`run_autoplay`). Read-only and synchronous: called after every engine
    /// event and queue change in `fan_in`, so it is cheap on the common case where there is
    /// nothing to do.
    fn consider_autoplay(self: &Arc<Self>) {
        let Some(catalog) = self.catalog.clone() else {
            return;
        };
        let Some(entry) = self.controller.queue().autoplay_due() else {
            return;
        };
        // Due only means "the queue agrees nothing follows this entry": a genuinely stopped
        // engine (the user pressed Stop, not a race with `QueueExhausted`) must not autoplay on
        // its own just because it happens to be sitting on a last entry that qualifies.
        if self.controller.status().state == engine::State::Stopped {
            return;
        }
        let mut state = self.autoplay.lock().unwrap();
        let Some(generation) = state.begin(entry) else {
            return;
        };
        let seed = state.seed(&self.controller.snapshot(), entry);
        let Some(seed) = seed else {
            // Nothing TIDAL has played this session: there is nothing to seed a radio from.
            state.finish(generation);
            return;
        };
        drop(state);
        tokio::spawn(self.clone().run_autoplay(catalog, entry, seed, generation));
    }

    /// Fetches `seed`'s radio and, if it is still wanted by the time it answers, appends up to
    /// `autoplay::BATCH` tracks from it. The network call happens without holding `control_lock`,
    /// so it never holds up another client's request; everything after it (the re-check and the
    /// actual queue change) does, the same way adding an album or a playlist already does.
    async fn run_autoplay(
        self: Arc<Self>,
        catalog: Arc<dyn Catalog>,
        entry: ItemId,
        seed: String,
        generation: u64,
    ) {
        let fetched = tokio::time::timeout(
            autoplay::FETCH_TIMEOUT,
            catalog.track_radio(seed, 0, MAX_ITEMS_LIMIT),
        )
        .await;
        let _serial = self.control_lock.lock().await;
        let Some(resume) = self.autoplay.lock().unwrap().finish(generation) else {
            // Superseded (a different entry became due) or cancelled (an explicit Stop or a
            // cleared queue): this answer, whatever it is, is no longer wanted.
            return;
        };
        let radio = match fetched {
            Ok(Ok(page)) => page,
            Ok(Err(failure)) => {
                phonia_core::warn!("autoplay: could not fetch a radio: {failure}");
                return;
            }
            Err(_) => {
                phonia_core::warn!("autoplay: fetching a radio timed out");
                return;
            }
        };
        // Re-validate under control_lock: the user may have added their own tracks, skipped,
        // turned repeat back on or autoplay off, or cleared the queue while this was in flight.
        if self.controller.queue().autoplay_due() != Some(entry) {
            return;
        }
        if self.controller.status().state == engine::State::Stopped && !resume {
            return;
        }
        let snapshot = self.controller.snapshot();
        let tracks: Vec<QueueTrack> = radio
            .items
            .iter()
            .filter_map(|track| accepted_from_catalog(track).ok())
            .map(|accepted| queue_track(&accepted))
            .collect();
        let tracks = autoplay::pick(tracks, &snapshot, autoplay::BATCH);
        if tracks.is_empty() {
            return;
        }
        let ids = self.controller.queue().add(tracks);
        if resume && self.controller.status().state == engine::State::Stopped {
            phonia_core::warn!(
                "autoplay: the queue ran dry before its radio arrived; resuming on it now"
            );
            let _ = self.controller.play_item(ids[0]);
        }
    }
}

/// `track` as an `Accepted` entry ready for the queue, or the reason it is refused: TIDAL lists
/// it but marks it as not streamable where this account is.
fn accepted_from_catalog(track: &tidal_catalog::Track) -> Result<Accepted, ipc::Rejected> {
    let source = Source::Tidal(track.id.clone());
    if !track.streamable {
        return Err(ipc::Rejected {
            source: source.to_wire(),
            reason: format!(
                "{} is listed by TIDAL but cannot be streamed here",
                track.title
            ),
        });
    }
    let artist = (!track.artists.is_empty()).then(|| {
        track
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    });
    let cover = track.album.as_ref().and_then(|album| album.cover.clone());
    let album_id = track.album.as_ref().map(|album| album.id.clone());
    Ok((
        source,
        Some(track.title.clone()),
        artist,
        track.duration,
        cover,
        album_id,
        None,
    ))
}

/// `accepted` as a queue entry.
fn queue_track(accepted: &Accepted) -> QueueTrack {
    let (source, title, artist, duration, cover, album_id, _) = accepted;
    QueueTrack {
        source: phonia_core::engine::TrackRef(source.to_wire()),
        title: title.clone(),
        artist: artist.clone(),
        duration: *duration,
        cover: cover.clone(),
        album_id: album_id.clone(),
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
        ipc::CatalogRef::FavoriteTracks => catalog.favorite_tracks(offset, limit),
        ipc::CatalogRef::TrackRadio { id } => catalog.track_radio(id.clone(), offset, limit),
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

/// The TIDAL track id and delivered quality for an engine track reference, if it is one: `None`
/// for a local file, or a reference the queue no longer has (already removed by the time this
/// runs), since there is nothing to report TIDAL about either way.
fn resolve_tidal_track(
    meta: &engine::TrackMeta,
    queue: &QueueSnapshot,
) -> (Option<String>, Option<phonia_core::config::Quality>) {
    let (_, source) = convert::entry_of(&meta.track, queue);
    let product_id = source
        .and_then(|source| Source::parse(&source).ok())
        .and_then(|source| match source {
            Source::Tidal(id) => Some(id),
            Source::File(_) => None,
        });
    let quality = meta.quality.map(|delivered| delivered.delivered);
    (product_id, quality)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// Sends a finished play in the background, with one retry: never blocks playback or the event
/// fan-out, and a failure here is always best-effort (see `docs/DECISIONS.md` for #120).
fn spawn_report(play_log: PlayLog, session: PlaybackSession) {
    const RETRY_AFTER: Duration = Duration::from_secs(30);
    tokio::spawn(async move {
        if let Err(error) = play_log.send(std::slice::from_ref(&session)).await {
            phonia_core::warn!(
                "phoniad: could not report a play to TIDAL ({error:#}); retrying once in 30s"
            );
            tokio::time::sleep(RETRY_AFTER).await;
            if let Err(error) = play_log.send(std::slice::from_ref(&session)).await {
                phonia_core::warn!("phoniad: retry failed too ({error:#}); giving up on this play");
            }
        }
    });
}

#[cfg(test)]
mod play_log_tests {
    use super::*;
    use phonia_core::config::Quality;
    use phonia_core::engine::{Delivered, TrackMeta, TrackRef};
    use phonia_core::queue::{QueueItem, Repeat};

    fn snapshot() -> QueueSnapshot {
        let item = |id: u64, source: &str| QueueItem {
            id: ItemId(id),
            track: QueueTrack {
                source: TrackRef(source.to_string()),
                title: None,
                artist: None,
                duration: None,
                cover: None,
                album_id: None,
            },
        };
        QueueSnapshot {
            version: 1,
            items: vec![item(7, "file:/m/a.flac"), item(8, "tidal:42")],
            order: vec![ItemId(8), ItemId(7)],
            current: Some(ItemId(8)),
            shuffle: false,
            repeat: Repeat::Off,
            autoplay: false,
        }
    }

    fn meta_for(id: u64, quality: Option<Delivered>) -> TrackMeta {
        TrackMeta {
            track: ItemId(id).track_ref(),
            title: None,
            artist: None,
            duration: None,
            quality,
            cover: None,
            loudness: None,
            gain: None,
        }
    }

    #[test]
    fn a_tidal_track_resolves_its_id_and_delivered_quality() {
        let delivered = Delivered {
            requested: Quality::Hires,
            delivered: Quality::Lossless,
        };
        let meta = meta_for(8, Some(delivered));
        let (id, quality) = resolve_tidal_track(&meta, &snapshot());
        assert_eq!(id, Some("42".to_string()));
        assert_eq!(quality, Some(Quality::Lossless));
    }

    #[test]
    fn a_local_file_has_no_tidal_id_to_report() {
        let meta = meta_for(7, None);
        let (id, _) = resolve_tidal_track(&meta, &snapshot());
        assert_eq!(id, None);
    }

    #[test]
    fn an_item_no_longer_in_the_queue_resolves_to_nothing() {
        let meta = meta_for(999, None);
        let (id, quality) = resolve_tidal_track(&meta, &snapshot());
        assert_eq!(id, None);
        assert_eq!(quality, None);
    }
}
