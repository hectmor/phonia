//! What the interface knows, what can happen to it, and how one becomes the other.

use crate::browse::{self as browse, Header, Stack};
use crate::cursor::Cursor;
use crate::keymap::{self, Action, Key, Resolution};
use crate::library::{self, LibraryState, LibraryTab};
use crate::search::{Phase, SearchState, Selected, Tab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use phonia_ipc::{
    CAP_VOLUME, Event, Payload, Queue, Repeat, Request, SeekTarget, ServerInfo, Status, Track,
    Version,
};
use std::time::Duration;

/// How often the interface wakes up on its own (see [`Msg::Tick`]).
pub const TICK: Duration = Duration::from_millis(250);

/// How far `<` and `>` move within the track.
const SEEK_STEP_MS: u64 = 10_000;
/// How much `+` and `-` change the volume, in percent.
const VOLUME_STEP: i16 = 5;

/// The panels the user can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Sidebar,
    Main,
}

/// The things the sidebar lists; the main panel shows the selected one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Queue,
    Search,
    Library,
}

impl Section {
    pub const ALL: [Section; 3] = [Section::Queue, Section::Search, Section::Library];

    pub fn title(self) -> &'static str {
        match self {
            Section::Queue => "Queue",
            Section::Search => "Search",
            Section::Library => "Library",
        }
    }
}

/// How the interface stands with the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Connection {
    /// Before the first answer, and after asking to try again.
    #[default]
    Connecting,
    Connected,
    /// The daemon is not there, or went away; it will be tried again in `retry_in`.
    Disconnected {
        reason: String,
        retry_in: Duration,
    },
    /// What answered is not a phonia daemon this interface can talk to. Not tried again by itself.
    Refused {
        reason: String,
    },
}

/// What the daemon said about itself when the connection opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub info: ServerInfo,
    pub protocol: Version,
    /// The optional features it has (see `phonia_ipc::CAP_*`). Older daemons have fewer.
    pub capabilities: Vec<String>,
}

/// Everything the interface remembers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// Set when the user asked to leave.
    pub quit: bool,
    pub focus: Focus,
    /// The selected row of the sidebar, which is also the section shown.
    pub sidebar: Cursor,
    /// The selected entry of the queue, in queue order.
    pub queue_cursor: Cursor,
    pub help: bool,
    /// How many lines the help is scrolled down, when it is longer than the screen.
    pub help_scroll: usize,
    /// Keys pressed that begin a longer binding (the first `g` of `gg`).
    pub pending: Vec<Key>,
    /// The size of the terminal, columns and rows.
    pub size: (u16, u16),
    pub connection: Connection,
    /// The daemon, once connected; kept after it goes away, so the screen still says what it was.
    pub server: Option<Server>,
    /// What the daemon last said: kept while it is away, and stale until it is back.
    pub status: Option<Status>,
    pub queue: Option<Queue>,
    /// Why the last request sent (a playback key) did not work, until the next one is tried.
    pub last_error: Option<String>,
    /// A line of good news ("Added 12 tracks"), shown in the bar until the next key.
    pub notice: Option<String>,
    pub search: SearchState,
    /// The album and playlist views opened from a search result.
    pub search_views: Stack,
    /// The library: favorite tracks, favorite albums, the user's own playlists. `None` until its
    /// section has been shown once and the daemon has been asked for it.
    pub library: Option<LibraryState>,
    /// The album and playlist views opened from the library.
    pub library_views: Stack,
    /// The number the next view opened is pushed with.
    pub next_serial: u64,
}

impl State {
    pub fn section(&self) -> Section {
        Section::ALL[self.sidebar.selected().min(Section::ALL.len() - 1)]
    }

    /// Whether the daemon has an optional feature. A daemon that is not connected has none, and an
    /// older one lacks the newer features: the keys and panels that need one are only offered
    /// when this says yes.
    pub fn has(&self, capability: &str) -> bool {
        self.connection == Connection::Connected
            && self
                .server
                .as_ref()
                .is_some_and(|server| server.capabilities.iter().any(|c| c == capability))
    }

    /// How many rows half a page is: half the terminal's height, and at least one.
    fn half_page(&self) -> usize {
        usize::from(self.size.1 / 2).max(1)
    }

    /// The cover (or picture) the main panel's current section wants to show: an opened album's
    /// or playlist's cover, an opened artist's picture, or, in the queue section, the currently
    /// playing track's own album cover. `None` with nothing open, nothing playing, or no cover.
    pub fn open_cover(&self) -> Option<(phonia_ipc::image::Kind, &str)> {
        let stack = match self.section() {
            Section::Queue => {
                let cover = self.status.as_ref()?.track.as_ref()?.cover.as_deref()?;
                return Some((phonia_ipc::image::Kind::AlbumCover, cover));
            }
            Section::Search => &self.search_views,
            Section::Library => &self.library_views,
        };
        match stack.top()? {
            browse::View::TrackList(view) => {
                let header = view.header();
                let kind = match header {
                    browse::Header::Album(_) => phonia_ipc::image::Kind::AlbumCover,
                    browse::Header::Playlist(_) => phonia_ipc::image::Kind::PlaylistCover,
                };
                Some((kind, header.cover()?))
            }
            browse::View::Artist(artist) => Some((
                phonia_ipc::image::Kind::ArtistPicture,
                artist.picture.as_deref()?,
            )),
        }
    }
}

/// Something that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Key(KeyEvent),
    /// The terminal has this size now (columns, rows). Also sent once when the interface starts.
    Resize(u16, u16),
    /// The interface woke up by itself, [`TICK`] after the last time.
    Tick,
    /// Connected to the daemon, with the state it is in.
    Connected {
        server: ServerInfo,
        protocol: Version,
        capabilities: Vec<String>,
        status: Status,
        queue: Queue,
    },
    /// The daemon is not there or went away; the next attempt is in `retry_in`.
    Disconnected {
        reason: String,
        retry_in: Duration,
    },
    /// The peer is not a phonia daemon this interface can talk to.
    Refused {
        reason: String,
    },
    /// The daemon announced something.
    Daemon(Event),
    /// A [`Cmd::Send`] did not work.
    RequestFailed {
        reason: String,
    },
    /// The answer to a [`Cmd::Request`]: what came back, or the daemon's reason it did not.
    Response {
        tag: Tag,
        result: Result<Payload, String>,
    },
}

/// Something the interface asks the outside to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    Quit,
    /// Try to connect now, without waiting for the next attempt.
    RetryNow,
    /// Send this request to the daemon; failure comes back as [`Msg::RequestFailed`].
    Send(Request),
    /// Send this request and bring its answer back as a [`Msg::Response`] carrying `tag`, which
    /// says what the answer is for.
    Request {
        tag: Tag,
        request: Request,
    },
}

/// What a request that wants its answer is for, carried out and back so the answer can be put
/// where it belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// A search; the answer counts only if it is for the current generation of the search.
    Search { generation: u64 },
    /// The next page of one list of the results of the current search.
    SearchMore { tab: Tab, generation: u64 },
    /// Tracks added to the queue from a result; if `play`, the first of them starts playing.
    Add { play: bool },
    /// An album or a playlist, whole, added from within its own view; `play_at`, when given, is
    /// which of the tracks added (by position among the ones that were, skipping the ones TIDAL
    /// would not stream) to start playing.
    AddFrom { play_at: Option<usize> },
    /// The view pushed with this number: its tracks, or the next page of them.
    View { serial: u64 },
    /// The request that loads the library.
    Library { generation: u64 },
    /// The next page of one of the library's three lists.
    LibraryMore { tab: LibraryTab, generation: u64 },
}

/// What [`update`] decided.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// Whether the screen shows something different now.
    pub redraw: bool,
    pub commands: Vec<Cmd>,
}

impl Effects {
    fn redraw() -> Self {
        Self {
            redraw: true,
            commands: Vec::new(),
        }
    }

    fn command(command: Cmd) -> Self {
        Self {
            redraw: false,
            commands: vec![command],
        }
    }
}

/// Applies one message to the state, then, unlike a search, the library has no key that starts
/// loading it: it is asked for the moment its section is first shown, whatever brought the state
/// there (a key, or the connection completing while it was already the section shown).
pub fn update(state: &mut State, msg: Msg) -> Effects {
    let mut effects = update_now(state, msg);
    if let Some(request) = maybe_load_library(state) {
        effects.redraw = true;
        effects.commands.push(Cmd::Request {
            tag: Tag::Library { generation: 0 },
            request,
        });
    }
    effects
}

fn update_now(state: &mut State, msg: Msg) -> Effects {
    match msg {
        Msg::Key(event) => on_key(state, event),
        Msg::Resize(columns, rows) => {
            state.size = (columns, rows);
            Effects::redraw()
        }
        Msg::Tick => tick(state),
        Msg::Connected {
            server,
            protocol,
            capabilities,
            status,
            queue,
        } => {
            state.connection = Connection::Connected;
            state.server = Some(Server {
                info: server,
                protocol,
                capabilities,
            });
            state.status = Some(status);
            state.queue = Some(queue);
            clamp_queue_cursor(state);
            Effects::redraw()
        }
        Msg::Disconnected { reason, retry_in } => {
            state.connection = Connection::Disconnected { reason, retry_in };
            state.search.connection_lost();
            state.search_views.connection_lost();
            if let Some(library) = &mut state.library {
                library.connection_lost();
            }
            state.library_views.connection_lost();
            Effects::redraw()
        }
        Msg::Refused { reason } => {
            state.connection = Connection::Refused { reason };
            Effects::redraw()
        }
        Msg::Daemon(event) => on_daemon_event(state, event),
        Msg::RequestFailed { reason } => {
            state.last_error = Some(reason);
            Effects::redraw()
        }
        Msg::Response { tag, result } => on_response(state, tag, result),
    }
}

/// The request that loads the library, the moment its section is first shown: refused, silently,
/// until there is a connection with the catalog to ask (so it is tried again the moment there is
/// one). Once asked, a failure is left as is, the same as a search that is not retried by itself.
fn maybe_load_library(state: &mut State) -> Option<Request> {
    if state.section() != Section::Library
        || state.library.is_some()
        || state.connection != Connection::Connected
        || !state.has(phonia_ipc::CAP_CATALOG)
    {
        return None;
    }
    let (library, request) = LibraryState::new();
    state.library = Some(library);
    Some(request)
}

/// Starts a search for `query`, if there is a daemon that can do it; if there is not, says why
/// where the results would be. An empty query does nothing.
pub fn submit_search(state: &mut State, query: &str) -> Effects {
    let query = query.trim();
    if query.is_empty() {
        return Effects::default();
    }
    if state.connection != Connection::Connected {
        state.search.fail("not connected to phoniad".to_string());
        return Effects::redraw();
    }
    if !state.has(phonia_ipc::CAP_CATALOG) {
        state.search.fail(
            "this phoniad cannot search TIDAL: it needs protocol 1.6 and a TIDAL login (run \
             `phonia login`, then restart it)"
                .to_string(),
        );
        return Effects::redraw();
    }
    let request = state.search.begin(query.to_string());
    Effects {
        redraw: true,
        commands: vec![Cmd::Request {
            tag: Tag::Search {
                generation: state.search.generation,
            },
            request,
        }],
    }
}

/// Puts an answer where its tag says. One for a search that is no longer the current one (a newer
/// search was started, or the connection was lost meanwhile) is dropped.
fn on_response(state: &mut State, tag: Tag, result: Result<Payload, String>) -> Effects {
    match tag {
        Tag::Search { generation } => {
            if generation != state.search.generation {
                return Effects::default();
            }
            match result {
                Ok(payload) => {
                    state.search.finish(payload);
                }
                Err(reason) => state.search.fail(reason),
            }
            Effects::redraw()
        }
        Tag::SearchMore { tab, generation } => {
            if generation != state.search.generation {
                return Effects::default();
            }
            match result {
                Ok(payload) => state.search.add_page(tab, payload),
                Err(reason) => {
                    state.search.page_failed(tab);
                    state.last_error = Some(format!("could not load more results: {reason}"));
                }
            }
            Effects::redraw()
        }
        Tag::Add { play } => on_added(state, result, play.then_some(0)),
        Tag::AddFrom { play_at } => on_added(state, result, play_at),
        Tag::View { serial } => on_view_response(state, serial, result),
        Tag::Library { generation } => {
            let Some(library) = &mut state.library else {
                return Effects::default();
            };
            if generation != library.generation {
                return Effects::default();
            }
            match result {
                Ok(payload) => {
                    library.finish(payload);
                }
                Err(reason) => library.fail(reason),
            }
            Effects::redraw()
        }
        Tag::LibraryMore { tab, generation } => {
            let Some(library) = &mut state.library else {
                return Effects::default();
            };
            if generation != library.generation {
                return Effects::default();
            }
            match result {
                Ok(payload) => library.add_page(tab, payload),
                Err(reason) => {
                    library.page_failed(tab);
                    state.last_error =
                        Some(format!("could not load more of the library: {reason}"));
                }
            }
            Effects::redraw()
        }
    }
}

/// What an add answers with, wherever it came from: how many tracks went in, and which one (if
/// any) to start playing, by its position among the ones that were added.
fn on_added(state: &mut State, result: Result<Payload, String>, play_at: Option<usize>) -> Effects {
    match result {
        Ok(Payload::Added { ids, rejected, .. }) => {
            state.notice = Some(added_notice(ids.len(), rejected.first()));
            match play_at.and_then(|index| ids.get(index)).copied() {
                Some(id) => Effects {
                    redraw: true,
                    commands: vec![Cmd::Send(Request::Play { item: Some(id) })],
                },
                None => Effects::redraw(),
            }
        }
        Ok(_) => {
            state.last_error = Some("the daemon answered something unexpected".to_string());
            Effects::redraw()
        }
        Err(reason) => {
            state.last_error = Some(reason);
            Effects::redraw()
        }
    }
}

/// The answer to opening a view, or to asking it for more tracks: the same request either way, so
/// what tells them apart is whether the view was still loading.
/// A page for one of the artist's three lists failed: only one can be waiting at a time (the
/// interface asks for the next page of the list being looked at), so whichever is marked as
/// loading is the one this failure was for.
fn clear_pending(artist: &mut browse::ArtistView) {
    artist.top_tracks.loading = false;
    artist.albums.loading = false;
    artist.singles.loading = false;
}

/// The view pushed with `serial`, wherever it is: the one opened from a search result, or the one
/// opened from the library — whichever stack actually has it, regardless of which section the user
/// has since moved to.
fn find_view(state: &mut State, serial: u64) -> Option<&mut browse::View> {
    if let Some(view) = state.search_views.find_mut(serial) {
        Some(view)
    } else {
        state.library_views.find_mut(serial)
    }
}

fn on_view_response(state: &mut State, serial: u64, result: Result<Payload, String>) -> Effects {
    match find_view(state, serial) {
        Some(browse::View::TrackList(view)) => {
            let opening = view.phase == browse::Phase::Loading;
            match result {
                Ok(Payload::Tracks { page, .. }) => {
                    if opening {
                        view.tracks = crate::list::Found::from_page(page);
                        view.phase = browse::Phase::Done;
                    } else {
                        view.tracks.append(Some(page));
                    }
                }
                Ok(_) => {
                    let reason = "the daemon answered something unexpected".to_string();
                    if opening {
                        view.phase = browse::Phase::Failed(reason);
                    } else {
                        view.tracks.loading = false;
                        state.last_error = Some(reason);
                    }
                }
                Err(reason) => {
                    if opening {
                        view.phase = browse::Phase::Failed(reason);
                    } else {
                        view.tracks.loading = false;
                        state.last_error = Some(format!("could not load more results: {reason}"));
                    }
                }
            }
        }
        Some(browse::View::Artist(artist)) => {
            let opening = artist.phase == browse::Phase::Loading;
            match result {
                // The one request that opens the page: everything it shows, at once.
                Ok(Payload::Artist {
                    artist: summary,
                    bio,
                    top_tracks,
                    albums,
                    singles,
                }) if opening => {
                    artist.name = summary.name;
                    artist.picture = summary.picture;
                    artist.bio = bio;
                    artist.top_tracks = crate::list::Found::from_page(top_tracks);
                    artist.albums = crate::list::Found::from_page(albums);
                    artist.singles = crate::list::Found::from_page(singles);
                    artist.phase = browse::Phase::Done;
                }
                // A later page of one of its three lists.
                Ok(Payload::Tracks { page, .. }) => artist.top_tracks.append(Some(page)),
                Ok(Payload::Albums {
                    from: phonia_ipc::AlbumListRef::ArtistAlbums { .. },
                    page,
                }) => artist.albums.append(Some(page)),
                Ok(Payload::Albums {
                    from: phonia_ipc::AlbumListRef::ArtistSingles { .. },
                    page,
                }) => artist.singles.append(Some(page)),
                Ok(_) => {
                    let reason = "the daemon answered something unexpected".to_string();
                    if opening {
                        artist.phase = browse::Phase::Failed(reason);
                    } else {
                        clear_pending(artist);
                        state.last_error = Some(reason);
                    }
                }
                Err(reason) => {
                    if opening {
                        artist.phase = browse::Phase::Failed(reason);
                    } else {
                        clear_pending(artist);
                        state.last_error = Some(format!("could not load more results: {reason}"));
                    }
                }
            }
        }
        // Closed, or the answer to a view that is no longer open: nothing to show it in.
        None => return Effects::default(),
    }
    Effects::redraw()
}

/// What to tell about tracks added to the queue: how many, and why one was not, if one was not.
fn added_notice(added: usize, first_refused: Option<&phonia_ipc::Rejected>) -> String {
    let tracks = |n: usize| {
        if n == 1 {
            "1 track".to_string()
        } else {
            format!("{n} tracks")
        }
    };
    match (added, first_refused) {
        (0, None) => "There was nothing to add".to_string(),
        (0, Some(refused)) => format!("Not added: {}", refused.reason),
        (n, None) => format!("Added {}", tracks(n)),
        (n, Some(refused)) => format!("Added {} (not added: {})", tracks(n), refused.reason),
    }
}

/// Counts down to the next attempt to connect, repainting when the seconds shown change.
fn tick(state: &mut State) -> Effects {
    let Connection::Disconnected { retry_in, .. } = &mut state.connection else {
        return Effects::default();
    };
    let shown = seconds_left(*retry_in);
    *retry_in = retry_in.saturating_sub(TICK);
    if seconds_left(*retry_in) == shown {
        Effects::default()
    } else {
        Effects::redraw()
    }
}

/// The wait as shown to a person: whole seconds, rounded up, so "0" is never shown while waiting.
pub fn seconds_left(retry_in: Duration) -> u64 {
    retry_in.as_millis().div_ceil(1000) as u64
}

fn on_daemon_event(state: &mut State, event: Event) -> Effects {
    match event {
        // A client that fell behind is given the whole state again: it replaces what is held.
        Event::Resync { status, queue, .. } => {
            state.status = Some(status);
            state.queue = Some(queue);
            clamp_queue_cursor(state);
            Effects::redraw()
        }
        Event::QueueChanged { queue } => {
            state.queue = Some(queue);
            clamp_queue_cursor(state);
            Effects::redraw()
        }
        Event::StateChanged { state: new_state } => {
            set_status(state, |status| {
                status.state = new_state;
                // The sink is gone for certain once the engine stops: nothing else says so, and
                // a stopped engine has nothing playing for a stale verdict to be mistaken for.
                if new_state == phonia_ipc::State::Stopped {
                    status.output = phonia_ipc::Output::Closed;
                    status.sink_report = None;
                }
            });
            Effects::redraw()
        }
        Event::Position {
            position_ms,
            duration_ms,
        } => {
            set_status(state, |status| {
                status.position_ms = position_ms;
                status.duration_ms = duration_ms;
            });
            Effects::redraw()
        }
        Event::TrackStarted {
            item_id,
            source,
            title,
            duration_ms,
            spec,
            quality,
            cover,
            ..
        } => {
            set_status(state, |status| {
                status.track = Some(Track {
                    item_id,
                    source,
                    title,
                    duration_ms,
                    quality,
                    cover,
                });
                status.spec = Some(spec);
                // Only reached once a track has actually started, which `start_track` only
                // reports once its sink opened successfully.
                status.output = phonia_ipc::Output::Open;
            });
            Effects::redraw()
        }
        Event::VolumeChanged { percent, muted } => {
            set_status(state, |status| {
                status.volume = Some(phonia_ipc::Volume { percent, muted });
            });
            Effects::redraw()
        }
        Event::OutputChanged { route } => {
            set_status(state, |status| {
                // An exclusive card plays the audio unscaled: it has no volume to show.
                if route.mode == phonia_ipc::OutputMode::Exclusive {
                    status.volume = None;
                }
                status.route = Some(route);
            });
            Effects::redraw()
        }
        Event::Seeked { position_ms } => {
            set_status(state, |status| status.position_ms = position_ms);
            Effects::redraw()
        }
        Event::SeekRejected { reason } => {
            state.last_error = Some(format!("seek rejected: {reason}"));
            Effects::redraw()
        }
        Event::TrackEnded { .. } => {
            set_status(state, |status| {
                status.track = None;
                status.spec = None;
                // Deliberately NOT cleared here: a `SinkReport` is sent once per sink *open*, not
                // once per track, so gapless tracks of the same format share one and it must
                // survive the one that produced it ending.
            });
            Effects::redraw()
        }
        Event::SinkReport(report) => {
            set_status(state, |status| status.sink_report = Some(report));
            Effects::redraw()
        }
        Event::OutputReleased { by, .. } => {
            set_status(state, |status| {
                status.output = phonia_ipc::Output::Released { by };
                status.sink_report = None;
            });
            Effects::redraw()
        }
        Event::OutputAcquired => {
            set_status(state, |status| status.output = phonia_ipc::Output::Open);
            Effects::redraw()
        }
        // The connection closes right after; that is what is shown. Events this version does not
        // know are ignored.
        _ => Effects::default(),
    }
}

/// Keeps the cursor on the queue when the queue got shorter under it.
fn clamp_queue_cursor(state: &mut State) {
    let len = queue_len(state);
    state.queue_cursor.clamp(len);
}

fn queue_len(state: &State) -> usize {
    state.queue.as_ref().map_or(0, |queue| queue.items.len())
}

/// Whether the keys that act on a queue entry mean something now: the queue is on screen and has
/// the focus.
fn in_queue(state: &State) -> bool {
    state.focus == Focus::Main && state.section() == Section::Queue
}

/// Applies a change to the held status, if there is one (there always should be, once connected).
fn set_status(state: &mut State, change: impl FnOnce(&mut Status)) {
    if let Some(status) = &mut state.status {
        change(status);
    }
}

fn on_key(state: &mut State, event: KeyEvent) -> Effects {
    // A notice is read once: the next key takes it away.
    let had_notice = state.notice.take().is_some();
    let mut effects = on_key_now(state, event);
    effects.redraw |= had_notice;
    effects
}

fn on_key_now(state: &mut State, event: KeyEvent) -> Effects {
    let key = Key::from_event(event);
    if state.help {
        return on_key_in_help(state, key);
    }
    // While a search is being typed every key is text or editing, not a command.
    if state.search.editing {
        return on_key_typing(state, event);
    }
    match keymap::resolve(&state.pending, key) {
        Resolution::Pending => {
            state.pending.push(key);
            Effects::default()
        }
        Resolution::None => {
            state.pending.clear();
            Effects::default()
        }
        Resolution::Action(action) => {
            state.pending.clear();
            apply(state, action)
        }
    }
}

/// Keys while the line of the search is being typed. A printable key is a character (so `q` and
/// `j` are letters here), and what edits the line is listed in [`keymap::TYPING`].
fn on_key_typing(state: &mut State, event: KeyEvent) -> Effects {
    let control = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let input = &mut state.search.input;
    match event.code {
        KeyCode::Char('c') if control => return Effects::command(Cmd::Quit),
        KeyCode::Esc => state.search.editing = false,
        KeyCode::Enter => {
            state.search.editing = false;
            let query = state.search.input.text().to_string();
            // Empty, it only stops the typing; otherwise the results are what comes next.
            state.focus = Focus::Main;
            let mut effects = submit_search(state, &query);
            effects.redraw = true;
            return effects;
        }
        KeyCode::Left => input.left(),
        KeyCode::Right => input.right(),
        KeyCode::Home => input.home(),
        KeyCode::End => input.end(),
        KeyCode::Char('a') if control => input.home(),
        KeyCode::Char('e') if control => input.end(),
        KeyCode::Char('w') if control => input.delete_word_back(),
        KeyCode::Char('u') if control => input.delete_to_start(),
        KeyCode::Backspace => input.backspace(),
        KeyCode::Delete => input.delete(),
        // A character, unless it is a chord with Control or Alt that this line has no use for.
        KeyCode::Char(c) if !control && !alt => input.insert(c),
        _ => return Effects::default(),
    }
    Effects::redraw()
}

/// The help covers the screen and answers only to what closes it, to the movement keys, which
/// scroll it when it is longer than the screen, and to `Ctrl-c`, which always quits.
fn on_key_in_help(state: &mut State, key: Key) -> Effects {
    match keymap::resolve(&[], key) {
        Resolution::Action(Action::Quit) if key == Key::ctrl('c') => Effects::command(Cmd::Quit),
        Resolution::Action(Action::Quit | Action::ToggleHelp | Action::CloseHelp) => {
            state.help = false;
            Effects::redraw()
        }
        Resolution::Action(
            action @ (Action::Down
            | Action::Up
            | Action::First
            | Action::Last
            | Action::HalfPageDown
            | Action::HalfPageUp),
        ) => scroll_help(state, action),
        _ => Effects::default(),
    }
}

/// How far the help can scroll: the lines it has beyond those that fit on the screen.
pub fn help_max_scroll(state: &State) -> usize {
    crate::view::help_overflow(state.size.1)
}

fn scroll_help(state: &mut State, action: Action) -> Effects {
    let max = help_max_scroll(state);
    let page = state.half_page();
    let before = state.help_scroll;
    state.help_scroll = match action {
        Action::Down => before + 1,
        Action::Up => before.saturating_sub(1),
        Action::First => 0,
        Action::Last => max,
        Action::HalfPageDown => before + page,
        Action::HalfPageUp => before.saturating_sub(page),
        _ => before,
    }
    .min(max);
    if state.help_scroll == before {
        Effects::default()
    } else {
        Effects::redraw()
    }
}

/// What might have changed visibly, taken before and after handling an action that does not
/// already know on its own whether to redraw: cursor positions (the sidebar, the queue, a search's
/// or the library's own lists, and whatever is open on top of either one's stack), which tab and
/// which view is showing, and the two toggles (the focus, the help).
#[derive(PartialEq)]
struct Snapshot {
    focus: Focus,
    help: bool,
    sidebar: Cursor,
    queue_cursor: Cursor,
    search_tab: Tab,
    search_cursors: [Cursor; 4],
    search_view: Option<u64>,
    library_tab: Option<LibraryTab>,
    library_cursors: Option<[Cursor; 3]>,
    library_view: Option<u64>,
    /// The cursor of whatever is open on top of the current section's stack, since moving within
    /// an opened album, playlist or artist page does not otherwise touch anything above.
    browsing_cursor: Option<Cursor>,
}

fn snapshot(state: &mut State) -> Snapshot {
    Snapshot {
        focus: state.focus,
        help: state.help,
        sidebar: state.sidebar,
        queue_cursor: state.queue_cursor,
        search_tab: state.search.tab,
        search_cursors: state.search.list_cursors(),
        search_view: state.search_views.top_serial(),
        library_tab: state.library.as_ref().map(|library| library.tab),
        library_cursors: state.library.as_ref().map(LibraryState::list_cursors),
        library_view: state.library_views.top_serial(),
        browsing_cursor: browsing_cursor(state),
    }
}

/// The stack of views the section that has one (search or the library) is showing now, if the
/// current section is one of those.
fn active_stack_mut(state: &mut State) -> Option<&mut Stack> {
    match state.section() {
        Section::Search => Some(&mut state.search_views),
        Section::Library => Some(&mut state.library_views),
        _ => None,
    }
}

/// The cursor of whatever is open on top of the current section's stack: an album's or a
/// playlist's tracks, or one of an artist's three lists.
fn browsing_cursor(state: &mut State) -> Option<Cursor> {
    match active_stack_mut(state)?.top_mut()? {
        browse::View::TrackList(view) => Some(view.tracks.cursor),
        browse::View::Artist(artist) => Some(match artist.tracks_or_albums() {
            browse::ListRef::Tracks(found) => found.cursor,
            browse::ListRef::Albums(found) => found.cursor,
        }),
    }
}

fn apply(state: &mut State, action: Action) -> Effects {
    let before = snapshot(state);
    match action {
        Action::Quit => return Effects::command(Cmd::Quit),
        Action::Reconnect => return reconnect(state),
        Action::ToggleHelp => {
            state.help = !state.help;
            state.help_scroll = 0;
        }
        // Only means something while the help is open, which is handled apart.
        Action::CloseHelp => {}
        Action::FocusNext => {
            state.focus = match state.focus {
                Focus::Sidebar => Focus::Main,
                Focus::Main => Focus::Sidebar,
            }
        }
        Action::FocusSidebar => {
            if !pop_view_if_browsing(state) {
                state.focus = Focus::Sidebar;
            }
        }
        Action::Back => {
            pop_view_if_browsing(state);
        }
        Action::FocusMain => state.focus = Focus::Main,
        Action::Section(index) => state.sidebar.select(index, Section::ALL.len()),
        Action::Down
        | Action::Up
        | Action::First
        | Action::Last
        | Action::HalfPageDown
        | Action::HalfPageUp => {
            move_cursor(state, action);
            let commands = load_more_results(state);
            if !commands.is_empty() {
                return Effects {
                    redraw: true,
                    commands,
                };
            }
        }
        Action::TogglePause
        | Action::Next
        | Action::Previous
        | Action::SeekBack
        | Action::SeekForward
        | Action::VolumeUp
        | Action::VolumeDown
        | Action::ToggleMute
        | Action::ToggleShuffle
        | Action::CycleRepeat => return send_playback(state, action),
        Action::Activate => {
            // In the sidebar, Enter opens the section; in the queue, it plays the entry.
            if state.focus == Focus::Sidebar {
                state.focus = Focus::Main;
            } else if state.section() == Section::Search {
                if !state.search_views.is_empty() {
                    return act_in_view(state, action);
                }
                // Enter plays the result under the cursor; with none, it opens the line.
                if state.search.selected().is_some() {
                    return act_on_result(state, action);
                }
                state.search.editing = true;
                return Effects::redraw();
            } else if state.section() == Section::Library {
                return if state.library_views.is_empty() {
                    act_on_library_result(state, action)
                } else {
                    act_in_view(state, action)
                };
            } else {
                return edit_queue(state, action);
            }
        }
        Action::RemoveEntry | Action::MoveEntryDown | Action::MoveEntryUp | Action::ClearQueue => {
            return edit_queue(state, action);
        }
        Action::AddToQueue | Action::AddNext => {
            return match state.section() {
                Section::Search if state.search_views.is_empty() => act_on_result(state, action),
                Section::Search => act_in_view(state, action),
                Section::Library if state.library_views.is_empty() => {
                    act_on_library_result(state, action)
                }
                Section::Library => act_in_view(state, action),
                Section::Queue => Effects::default(),
            };
        }
        Action::StartSearch => {
            // From anywhere: any album or playlist open is left, and typing starts.
            state.search_views = Stack::default();
            let index = Section::ALL
                .iter()
                .position(|section| *section == Section::Search)
                .unwrap_or(0);
            state.sidebar.select(index, Section::ALL.len());
            state.focus = Focus::Main;
            state.search.editing = true;
            return Effects::redraw();
        }
        Action::TabNext | Action::TabPrevious => {
            // An artist page (only ever opened from a search result, never from the library) has
            // three lists to switch between; anything else open (an album, a playlist) has one.
            if let Some(browse::View::Artist(artist)) =
                active_stack_mut(state).and_then(Stack::top_mut)
            {
                let tab = artist.tab;
                artist.tab = if action == Action::TabNext {
                    tab.next()
                } else {
                    tab.previous()
                };
                return if artist.tab == tab {
                    Effects::default()
                } else {
                    Effects::redraw()
                };
            }
            match state.section() {
                Section::Search if state.search_views.is_empty() => {
                    let tab = state.search.tab;
                    state.search.tab = if action == Action::TabNext {
                        tab.next()
                    } else {
                        tab.previous()
                    };
                    return if state.search.tab == tab {
                        Effects::default()
                    } else {
                        Effects::redraw()
                    };
                }
                Section::Library if state.library_views.is_empty() => {
                    let Some(library) = &mut state.library else {
                        return Effects::default();
                    };
                    let tab = library.tab;
                    library.tab = if action == Action::TabNext {
                        tab.next()
                    } else {
                        tab.previous()
                    };
                    return if library.tab == tab {
                        Effects::default()
                    } else {
                        Effects::redraw()
                    };
                }
                // A track list (an album or a playlist) has one list: nothing to switch.
                _ => return Effects::default(),
            }
        }
    }
    if snapshot(state) == before {
        Effects::default()
    } else {
        Effects::redraw()
    }
}

/// Sends a playback request, if there is a connection to send it to; refused, silently, while
/// there is none, rather than queued for later. One that cannot be made (a volume on an exclusive
/// card) is explained instead.
fn send_playback(state: &mut State, action: Action) -> Effects {
    if state.connection != Connection::Connected {
        return Effects::default();
    }
    let had_error = state.last_error.take().is_some();
    match request_for(state, action) {
        Ok(request) => Effects {
            // Nothing visible changes from sending it, except clearing a previous error.
            redraw: had_error,
            commands: vec![Cmd::Send(request)],
        },
        Err(reason) => {
            state.last_error = Some(reason);
            Effects::redraw()
        }
    }
}

/// The request a playback key stands for, given what is known of the daemon now.
fn request_for(state: &State, action: Action) -> Result<Request, String> {
    let volume = || -> Result<phonia_ipc::Volume, String> {
        if !state.has(CAP_VOLUME) {
            return Err(
                "this phoniad is too old to set the volume: restart it after updating".to_string(),
            );
        }
        state
            .status
            .as_ref()
            .and_then(|status| status.volume)
            .ok_or_else(|| {
                "this output has no volume to set: an exclusive card plays the audio unscaled. Use \
                 the DAC's own volume, or switch to a shared output"
                    .to_string()
            })
    };
    let step = |volume: phonia_ipc::Volume, change: i16| Request::SetVolume {
        percent: (i16::from(volume.percent) + change).clamp(0, 100) as u8,
    };
    Ok(match action {
        Action::TogglePause => Request::TogglePause,
        Action::Next => Request::Next,
        Action::Previous => Request::Previous,
        Action::SeekBack => Request::Seek {
            target: SeekTarget::Backward { ms: SEEK_STEP_MS },
        },
        Action::SeekForward => Request::Seek {
            target: SeekTarget::Forward { ms: SEEK_STEP_MS },
        },
        Action::VolumeUp => step(volume()?, VOLUME_STEP),
        Action::VolumeDown => step(volume()?, -VOLUME_STEP),
        Action::ToggleMute => Request::SetMute {
            mute: !volume()?.muted,
        },
        Action::ToggleShuffle => Request::SetShuffle {
            shuffle: !state.queue.as_ref().is_some_and(|queue| queue.shuffle),
        },
        Action::CycleRepeat => Request::SetRepeat {
            repeat: match state.queue.as_ref().map(|queue| queue.repeat) {
                Some(Repeat::Off) | None => Repeat::All,
                Some(Repeat::All) => Repeat::One,
                Some(Repeat::One) => Repeat::Off,
            },
        },
        _ => unreachable!("not a request: {action:?}"),
    })
}

/// Asks to connect now, if the daemon is not connected.
fn reconnect(state: &mut State) -> Effects {
    match state.connection {
        Connection::Disconnected { .. } | Connection::Refused { .. } => {
            state.connection = Connection::Connecting;
            Effects {
                redraw: true,
                commands: vec![Cmd::RetryNow],
            }
        }
        Connection::Connecting | Connection::Connected => Effects::default(),
    }
}

/// Asks for the next page of the list being looked at (a search's or the library's own, or
/// whatever is open on top of either one's stack), when the cursor has come near the end of what
/// is loaded and there is more.
fn load_more_results(state: &mut State) -> Vec<Cmd> {
    if state.focus != Focus::Main || state.search.editing {
        return Vec::new();
    }
    if !matches!(state.section(), Section::Search | Section::Library) {
        return Vec::new();
    }
    if let Some(serial) = active_stack_mut(state).and_then(|stack| stack.top_serial()) {
        let Some(view) = active_stack_mut(state).and_then(Stack::top_mut) else {
            return Vec::new();
        };
        let request = match view {
            browse::View::TrackList(view) if view.phase == browse::Phase::Done => {
                view.tracks.next_offset().map(|offset| Request::Tracks {
                    from: view.of.clone(),
                    offset,
                    limit: Some(crate::search::PAGE_SIZE),
                })
            }
            browse::View::Artist(artist) if artist.phase == browse::Phase::Done => {
                let id = artist.id.clone();
                let tab = artist.tab;
                match artist.tracks_or_albums() {
                    browse::ListRef::Tracks(found) => {
                        found.next_offset().map(|offset| Request::Tracks {
                            from: phonia_ipc::CatalogRef::ArtistTopTracks { id },
                            offset,
                            limit: Some(crate::search::PAGE_SIZE),
                        })
                    }
                    browse::ListRef::Albums(found) => {
                        found.next_offset().map(|offset| Request::Albums {
                            from: if tab == browse::ArtistTab::Albums {
                                phonia_ipc::AlbumListRef::ArtistAlbums { id }
                            } else {
                                phonia_ipc::AlbumListRef::ArtistSingles { id }
                            },
                            offset,
                            limit: Some(crate::search::PAGE_SIZE),
                        })
                    }
                }
            }
            _ => None,
        };
        return match request {
            Some(request) => vec![Cmd::Request {
                tag: Tag::View { serial },
                request,
            }],
            None => Vec::new(),
        };
    }
    match state.section() {
        Section::Search => {
            let tab = state.search.tab;
            match state.search.next_page(tab) {
                Some(request) => vec![Cmd::Request {
                    tag: Tag::SearchMore {
                        tab,
                        generation: state.search.generation,
                    },
                    request,
                }],
                None => Vec::new(),
            }
        }
        Section::Library => {
            let Some(library) = &mut state.library else {
                return Vec::new();
            };
            let tab = library.tab;
            let generation = library.generation;
            match library.next_page(tab) {
                Some(request) => vec![Cmd::Request {
                    tag: Tag::LibraryMore { tab, generation },
                    request,
                }],
                None => Vec::new(),
            }
        }
        Section::Queue => Vec::new(),
    }
}

/// Enter, `a` and `A` on the result under the cursor: play it now, add it to the end of the
/// queue, or add it after the track playing. A track, an album or a playlist can be added; an
/// artist has no tracks to add until the artist view exists.
fn act_on_result(state: &mut State, action: Action) -> Effects {
    if state.section() != Section::Search
        || state.focus != Focus::Main
        || state.search.editing
        || state.search.phase != Phase::Done
    {
        return Effects::default();
    }
    let Some(selected) = state.search.selected() else {
        return Effects::default();
    };
    // Enter opens an album or a playlist, to see its tracks, instead of playing it whole.
    if action == Action::Activate {
        match selected {
            Selected::Album(album) => {
                let album = album.clone();
                return open_track_list(
                    state,
                    phonia_ipc::CatalogRef::Album {
                        id: album.id.clone(),
                    },
                    Header::Album(album),
                );
            }
            Selected::Playlist(playlist) => {
                let playlist = playlist.clone();
                return open_track_list(
                    state,
                    phonia_ipc::CatalogRef::Playlist {
                        id: playlist.id.clone(),
                    },
                    Header::Playlist(playlist),
                );
            }
            Selected::Artist(artist) => {
                return open_artist_view(
                    state,
                    artist.id.clone(),
                    artist.name.clone(),
                    artist.picture.clone(),
                );
            }
            Selected::Track(_) => {}
        }
    }
    let play = action == Action::Activate;
    // Playing goes after the track playing and starts the first added; `a` goes to the end.
    let at = if action == Action::AddToQueue {
        phonia_ipc::AddAt::End
    } else {
        phonia_ipc::AddAt::Next
    };
    let request = match selected {
        Selected::Track(track) => {
            if !track.streamable {
                state.last_error = Some(format!("{} is not available where you are", track.title));
                return Effects::redraw();
            }
            match phonia_ipc::source::tidal(&track.id) {
                Ok(source) => Request::QueueAdd {
                    tracks: vec![phonia_ipc::NewTrack { source }],
                    at,
                },
                Err(reason) => {
                    state.last_error = Some(reason);
                    return Effects::redraw();
                }
            }
        }
        Selected::Album(album) => Request::QueueAddFrom {
            from: phonia_ipc::CatalogRef::Album {
                id: album.id.clone(),
            },
            at,
        },
        Selected::Playlist(playlist) => Request::QueueAddFrom {
            from: phonia_ipc::CatalogRef::Playlist {
                id: playlist.id.clone(),
            },
            at,
        },
        // Enter opened the page above; a/A here add its most listened to tracks, whole.
        Selected::Artist(artist) => Request::QueueAddFrom {
            from: phonia_ipc::CatalogRef::ArtistTopTracks {
                id: artist.id.clone(),
            },
            at,
        },
    };
    send_add(state, request, play)
}

/// Enter, `a` and `A` on the row under the cursor of the library's own lists (not one already
/// opened from it): a favorite album or a playlist opens with Enter, the same as from a search
/// result. A favorite track is different from a track in an opened album: Enter plays just that
/// one track, not the whole list from there on — a favorites list has no natural queue order and
/// can run into the thousands, so queuing it whole from here would be an easy way to end up with
/// an enormous, unwanted queue. `a`/`A` always act on the one row: add it (a track), or add it
/// whole (an album or a playlist).
fn act_on_library_result(state: &mut State, action: Action) -> Effects {
    if state.section() != Section::Library || state.focus != Focus::Main {
        return Effects::default();
    }
    let Some(library) = &state.library else {
        return Effects::default();
    };
    if library.phase != browse::Phase::Done {
        return Effects::default();
    }
    let Some(selected) = library.selected() else {
        return Effects::default();
    };
    if action == Action::Activate {
        match selected {
            library::Selected::Album(album) => {
                let album = album.clone();
                return open_track_list(
                    state,
                    phonia_ipc::CatalogRef::Album {
                        id: album.id.clone(),
                    },
                    Header::Album(album),
                );
            }
            library::Selected::Playlist(playlist) => {
                let playlist = playlist.clone();
                return open_track_list(
                    state,
                    phonia_ipc::CatalogRef::Playlist {
                        id: playlist.id.clone(),
                    },
                    Header::Playlist(playlist),
                );
            }
            library::Selected::Track(_) => {}
        }
    }
    let play = action == Action::Activate;
    let at = if action == Action::AddToQueue {
        phonia_ipc::AddAt::End
    } else {
        phonia_ipc::AddAt::Next
    };
    let request = match selected {
        // Enter here plays just this one track: see the doc comment above for why.
        library::Selected::Track(track) => {
            if !track.streamable {
                state.last_error = Some(format!("{} is not available where you are", track.title));
                return Effects::redraw();
            }
            match phonia_ipc::source::tidal(&track.id) {
                Ok(source) => Request::QueueAdd {
                    tracks: vec![phonia_ipc::NewTrack { source }],
                    at,
                },
                Err(reason) => {
                    state.last_error = Some(reason);
                    return Effects::redraw();
                }
            }
        }
        library::Selected::Album(album) => Request::QueueAddFrom {
            from: phonia_ipc::CatalogRef::Album {
                id: album.id.clone(),
            },
            at,
        },
        library::Selected::Playlist(playlist) => Request::QueueAddFrom {
            from: phonia_ipc::CatalogRef::Playlist {
                id: playlist.id.clone(),
            },
            at,
        },
    };
    send_add(state, request, play)
}

/// Sends a track, an album, a playlist or an artist's tracks to the queue: connected, and with the
/// catalog if the request needs it (adding a whole album, playlist or artist does).
fn send_add(state: &mut State, request: Request, play: bool) -> Effects {
    if state.connection != Connection::Connected {
        state.last_error = Some("not connected to phoniad".to_string());
        return Effects::redraw();
    }
    let needs_the_catalog = matches!(request, Request::QueueAddFrom { .. });
    if needs_the_catalog && !state.has(phonia_ipc::CAP_CATALOG) {
        state.last_error =
            Some("this phoniad cannot add albums or playlists: it needs protocol 1.6".to_string());
        return Effects::redraw();
    }
    state.last_error = None;
    Effects {
        redraw: true,
        commands: vec![Cmd::Request {
            tag: Tag::Add { play },
            request,
        }],
    }
}

/// Moves the cursor of the panel that has the focus. The main panel lists nothing yet.
fn move_cursor(state: &mut State, action: Action) {
    let page = state.half_page();
    let queue_len = queue_len(state);
    let search_tab = state.search.tab;
    let library_tab = state.library.as_ref().map(|library| library.tab);
    let browsing = state.focus == Focus::Main
        && matches!(state.section(), Section::Search | Section::Library)
        && active_stack_mut(state).is_some_and(|stack| !stack.is_empty());
    let (len, cursor) = if browsing {
        let Some(view) = active_stack_mut(state).and_then(Stack::top_mut) else {
            return;
        };
        match view {
            browse::View::TrackList(view) => (view.tracks.items.len(), &mut view.tracks.cursor),
            browse::View::Artist(artist) => match artist.tracks_or_albums() {
                browse::ListRef::Tracks(found) => (found.items.len(), &mut found.cursor),
                browse::ListRef::Albums(found) => (found.items.len(), &mut found.cursor),
            },
        }
    } else {
        match (state.focus, state.section()) {
            (Focus::Sidebar, _) => (Section::ALL.len(), &mut state.sidebar),
            (Focus::Main, Section::Queue) => (queue_len, &mut state.queue_cursor),
            (Focus::Main, Section::Search) => {
                let (cursor, len) = state.search.list_of(search_tab);
                (len, cursor)
            }
            (Focus::Main, Section::Library) => {
                let Some(library) = &mut state.library else {
                    return;
                };
                let (cursor, len) = library.list_of(library_tab.unwrap_or_default());
                (len, cursor)
            }
        }
    };
    match action {
        Action::Down => cursor.down(len),
        Action::Up => cursor.up(),
        Action::First => cursor.first(),
        Action::Last => cursor.last(len),
        Action::HalfPageDown => cursor.page_down(len, page),
        Action::HalfPageUp => cursor.page_up(page),
        _ => {}
    }
}

/// Closes the view on top, if the section with the focus is one with a stack and has one open.
fn pop_view_if_browsing(state: &mut State) -> bool {
    state.focus == Focus::Main && active_stack_mut(state).is_some_and(Stack::pop)
}

/// Opens an album or a playlist: pushes it in a loading state (its header is already known from
/// the result it came from) and asks for its first page of tracks.
/// Fails, saying why, unless there is a connection and it can browse the catalog. Shared by
/// whatever wants to open or add from a track list, an album, an artist...
fn require_browsable(state: &mut State, what: &str) -> bool {
    if state.connection != Connection::Connected {
        state.last_error = Some("not connected to phoniad".to_string());
        return false;
    }
    if !state.has(phonia_ipc::CAP_CATALOG) {
        state.last_error = Some(format!(
            "this phoniad cannot show {what}: it needs protocol 1.6"
        ));
        return false;
    }
    true
}

/// The next number to push a view with.
fn next_serial(state: &mut State) -> u64 {
    let serial = state.next_serial;
    state.next_serial += 1;
    serial
}

fn open_artist_view(
    state: &mut State,
    id: String,
    name: String,
    picture: Option<String>,
) -> Effects {
    if !require_browsable(state, "artists") {
        return Effects::redraw();
    }
    let serial = next_serial(state);
    state.search_views.push(
        serial,
        browse::View::Artist(browse::ArtistView::new(id.clone(), name, picture)),
    );
    Effects {
        redraw: true,
        commands: vec![Cmd::Request {
            tag: Tag::View { serial },
            request: Request::Artist { id, limit: None },
        }],
    }
}

fn open_track_list(state: &mut State, of: phonia_ipc::CatalogRef, header: Header) -> Effects {
    if !require_browsable(state, "albums or playlists") {
        return Effects::redraw();
    }
    let serial = next_serial(state);
    // Whichever of the two sections is asking: opened onto its own stack.
    let Some(stack) = active_stack_mut(state) else {
        return Effects::default();
    };
    stack.push(
        serial,
        browse::View::TrackList(browse::TrackListView::new(of.clone(), header)),
    );
    Effects {
        redraw: true,
        commands: vec![Cmd::Request {
            tag: Tag::View { serial },
            request: Request::Tracks {
                from: of,
                offset: 0,
                limit: Some(crate::search::PAGE_SIZE),
            },
        }],
    }
}

/// Enter, `a` and `A` on the track under the cursor of an open album or playlist: Enter queues the
/// whole thing right after the track playing and starts at this one (skipping, in the count, any
/// track TIDAL will not stream, since those never reach the queue); `a`/`A` add just this track.
/// The row under the cursor of whatever is open: a track from a track list or from an artist's
/// top tracks, or an album from an artist's albums or singles.
enum Row {
    Track {
        of: phonia_ipc::CatalogRef,
        track: phonia_ipc::TrackSummary,
        index: usize,
    },
    Album(phonia_ipc::AlbumSummary),
}

/// The row under the cursor of the view on top, if it has finished loading and has one.
fn browsed_row(state: &mut State) -> Option<Row> {
    match active_stack_mut(state)?.top_mut()? {
        browse::View::TrackList(view) if view.phase == browse::Phase::Done => {
            let index = view.tracks.cursor.selected();
            view.tracks
                .items
                .get(index)
                .cloned()
                .map(|track| Row::Track {
                    of: view.of.clone(),
                    track,
                    index,
                })
        }
        browse::View::Artist(artist) if artist.phase == browse::Phase::Done => {
            let id = artist.id.clone();
            match artist.tracks_or_albums() {
                browse::ListRef::Tracks(found) => {
                    let index = found.cursor.selected();
                    found.items.get(index).cloned().map(|track| Row::Track {
                        of: phonia_ipc::CatalogRef::ArtistTopTracks { id },
                        track,
                        index,
                    })
                }
                browse::ListRef::Albums(found) => {
                    let index = found.cursor.selected();
                    found.items.get(index).cloned().map(Row::Album)
                }
            }
        }
        _ => None,
    }
}

/// Enter, `a` and `A` on the row under the cursor of an open view: a track behaves as it does
/// inside an album (Enter continues playing from it, `a`/`A` add just it); an album (from an
/// artist's page) opens with Enter, or is added whole with `a`/`A`.
fn act_in_view(state: &mut State, action: Action) -> Effects {
    match browsed_row(state) {
        Some(Row::Track { of, track, index }) => act_on_track_row(state, action, of, track, index),
        Some(Row::Album(album)) if action == Action::Activate => open_track_list(
            state,
            phonia_ipc::CatalogRef::Album {
                id: album.id.clone(),
            },
            Header::Album(album),
        ),
        Some(Row::Album(album)) => send_add(
            state,
            Request::QueueAddFrom {
                from: phonia_ipc::CatalogRef::Album { id: album.id },
                at: if action == Action::AddToQueue {
                    phonia_ipc::AddAt::End
                } else {
                    phonia_ipc::AddAt::Next
                },
            },
            false,
        ),
        None => Effects::default(),
    }
}

/// Enter on a track being browsed (an album, a playlist, or an artist's top tracks) queues the
/// rest of the list right after the track playing and starts at this one, skipping, in the count,
/// any track before it that TIDAL will not stream (so `play_at` still lands on the right one).
/// `a`/`A` add just this track.
fn act_on_track_row(
    state: &mut State,
    action: Action,
    of: phonia_ipc::CatalogRef,
    track: phonia_ipc::TrackSummary,
    index: usize,
) -> Effects {
    if !track.streamable {
        state.last_error = Some(format!("{} is not available where you are", track.title));
        return Effects::redraw();
    }
    match action {
        Action::Activate => {
            // How many tracks before this one in the same list could stream: the daemon leaves
            // the others out when it adds the list, so this is this track's place among the ids
            // that come back.
            let play_at = match state.search_views.top() {
                Some(browse::View::TrackList(view)) => {
                    count_streamable_before(&view.tracks.items, index)
                }
                Some(browse::View::Artist(artist)) => {
                    count_streamable_before(&artist.top_tracks.items, index)
                }
                _ => return Effects::default(),
            };
            send_add_from(state, of, Some(play_at))
        }
        Action::AddToQueue | Action::AddNext => {
            let source = match phonia_ipc::source::tidal(&track.id) {
                Ok(source) => source,
                Err(reason) => {
                    state.last_error = Some(reason);
                    return Effects::redraw();
                }
            };
            let request = Request::QueueAdd {
                tracks: vec![phonia_ipc::NewTrack { source }],
                at: if action == Action::AddToQueue {
                    phonia_ipc::AddAt::End
                } else {
                    phonia_ipc::AddAt::Next
                },
            };
            send_add(state, request, false)
        }
        _ => Effects::default(),
    }
}

/// How many of the tracks before `index` in a loaded list TIDAL will stream: where the track at
/// `index` lands among the ids the daemon answers with, since the rest are left out.
fn count_streamable_before(tracks: &[phonia_ipc::TrackSummary], index: usize) -> usize {
    tracks[..index]
        .iter()
        .filter(|track| track.streamable)
        .count()
}

/// Queues `of` (an album, a playlist, or an artist's top tracks), whole, right after the track
/// playing, and asks to play the one at `play_at` among the tracks that made it in, if any.
fn send_add_from(state: &mut State, of: phonia_ipc::CatalogRef, play_at: Option<usize>) -> Effects {
    if !require_browsable(state, "albums, playlists or artists") {
        return Effects::redraw();
    }
    state.last_error = None;
    Effects {
        redraw: true,
        commands: vec![Cmd::Request {
            tag: Tag::AddFrom { play_at },
            request: Request::QueueAddFrom {
                from: of,
                at: phonia_ipc::AddAt::Next,
            },
        }],
    }
}

/// The queue's edits and Enter, on the selected entry: sent while connected and looking at the
/// queue, and refused silently otherwise, like the playback keys.
fn edit_queue(state: &mut State, action: Action) -> Effects {
    if state.connection != Connection::Connected || !in_queue(state) {
        return Effects::default();
    }
    let Some(queue) = &state.queue else {
        return Effects::default();
    };
    let index = state.queue_cursor.selected();
    let len = queue.items.len();
    let selected = queue.items.get(index).map(|item| item.id);
    let request = match action {
        Action::Activate => selected.map(|item| Request::Play { item: Some(item) }),
        Action::RemoveEntry => selected.map(|id| Request::QueueRemove { ids: vec![id] }),
        Action::ClearQueue => (len > 0).then_some(Request::QueueClear),
        // The entry moves one place, and the cursor goes with it.
        Action::MoveEntryDown if index + 1 < len => {
            selected.map(|id| Request::QueueMove { id, to: index + 1 })
        }
        Action::MoveEntryUp if index > 0 => {
            selected.map(|id| Request::QueueMove { id, to: index - 1 })
        }
        _ => None,
    };
    let Some(request) = request else {
        return Effects::default();
    };
    let had_error = state.last_error.take().is_some();
    match action {
        Action::MoveEntryDown => state.queue_cursor.select(index + 1, len),
        Action::MoveEntryUp => state.queue_cursor.select(index - 1, len),
        Action::ClearQueue => state.queue_cursor.first(),
        _ => {}
    }
    Effects {
        // The cursor moving with an entry is a change to show; sending alone is not.
        redraw: had_error
            || matches!(
                action,
                Action::MoveEntryDown | Action::MoveEntryUp | Action::ClearQueue
            ),
        commands: vec![Cmd::Send(request)],
    }
}

/// Values for tests, here and in the views.
#[cfg(test)]
pub(crate) mod tests_support {
    use phonia_ipc::{Output, Queue, Repeat, State, Status};

    pub fn status() -> Status {
        Status {
            state: State::Stopped,
            track: None,
            spec: None,
            position_ms: 0,
            duration_ms: None,
            output: Output::Closed,
            route: None,
            volume: None,
            quality_range: None,
            sink_report: None,
        }
    }

    pub fn queue() -> Queue {
        Queue {
            version: 1,
            items: Vec::new(),
            order: Vec::new(),
            current: None,
            shuffle: false,
            repeat: Repeat::Off,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::time::Duration;
    use tests_support::{queue, status};

    fn connected_msg(capabilities: &[&str]) -> Msg {
        Msg::Connected {
            server: ServerInfo {
                name: "phoniad".into(),
                version: "0.1.0".into(),
                pid: 7,
            },
            protocol: Version { major: 1, minor: 5 },
            capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
            status: status(),
            queue: queue(),
        }
    }

    fn disconnected(state: &mut State, retry_in: Duration) {
        update(
            state,
            Msg::Disconnected {
                reason: "connection refused".into(),
                retry_in,
            },
        );
    }

    fn press(state: &mut State, code: KeyCode) -> Effects {
        update(state, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn ch(state: &mut State, c: char) -> Effects {
        press(state, KeyCode::Char(c))
    }

    fn ctrl(state: &mut State, c: char) -> Effects {
        update(
            state,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
        )
    }

    #[test]
    fn q_and_control_c_quit() {
        let mut state = State::default();
        assert_eq!(ch(&mut state, 'q').commands, vec![Cmd::Quit]);
        assert_eq!(ctrl(&mut state, 'c').commands, vec![Cmd::Quit]);
    }

    #[test]
    fn j_and_k_move_through_the_sections_and_stop_at_the_ends() {
        let mut state = State::default();
        assert_eq!(state.section(), Section::Queue);
        assert!(!ch(&mut state, 'k').redraw, "already at the top");
        assert!(ch(&mut state, 'j').redraw);
        assert_eq!(state.section(), Section::Search);
        ch(&mut state, 'j');
        ch(&mut state, 'j');
        assert_eq!(state.section(), Section::Library);
        assert!(!ch(&mut state, 'j').redraw, "already at the bottom");
    }

    #[test]
    fn the_arrows_do_what_j_and_k_do() {
        let mut state = State::default();
        press(&mut state, KeyCode::Down);
        assert_eq!(state.section(), Section::Search);
        press(&mut state, KeyCode::Up);
        assert_eq!(state.section(), Section::Queue);
    }

    #[test]
    fn gg_goes_to_the_top_and_capital_g_to_the_bottom() {
        let mut state = State::default();
        ch(&mut state, 'G');
        assert_eq!(state.section(), Section::Library);
        assert!(
            !ch(&mut state, 'g').redraw,
            "half a binding changes nothing"
        );
        assert_eq!(state.pending.len(), 1);
        assert!(ch(&mut state, 'g').redraw);
        assert_eq!(state.section(), Section::Queue);
        assert!(state.pending.is_empty());
    }

    #[test]
    fn a_g_followed_by_something_else_is_dropped() {
        let mut state = State::default();
        ch(&mut state, 'G');
        ch(&mut state, 'g');
        ch(&mut state, 'x');
        assert!(state.pending.is_empty());
        assert_eq!(state.section(), Section::Library, "nothing moved");
    }

    #[test]
    fn half_pages_move_by_half_the_height_and_at_least_one_row() {
        let mut state = State::default();
        update(&mut state, Msg::Resize(80, 40));
        ctrl(&mut state, 'd');
        assert_eq!(state.section(), Section::Library, "20 rows is past the end");
        ctrl(&mut state, 'u');
        assert_eq!(state.section(), Section::Queue);

        update(&mut state, Msg::Resize(80, 1));
        ctrl(&mut state, 'd');
        assert_eq!(state.section(), Section::Search, "one row at least");
    }

    #[test]
    fn the_numbers_jump_to_a_section() {
        let mut state = State::default();
        ch(&mut state, '3');
        assert_eq!(state.section(), Section::Library);
        ch(&mut state, '2');
        assert_eq!(state.section(), Section::Search);
        ch(&mut state, '1');
        assert_eq!(state.section(), Section::Queue);
    }

    #[test]
    fn tab_h_and_l_move_the_focus() {
        let mut state = State::default();
        assert_eq!(state.focus, Focus::Sidebar);
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.focus, Focus::Main);
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.focus, Focus::Sidebar);
        ch(&mut state, 'l');
        assert_eq!(state.focus, Focus::Main);
        ch(&mut state, 'h');
        assert_eq!(state.focus, Focus::Sidebar);
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.focus, Focus::Main);
    }

    #[test]
    fn movement_keys_act_on_the_panel_with_the_focus() {
        let mut state = State::default();
        ch(&mut state, 'l');
        assert!(
            !ch(&mut state, 'j').redraw,
            "the list is empty: nothing to move"
        );
        assert_eq!(state.section(), Section::Queue, "the sidebar did not move");
    }

    #[test]
    fn the_help_opens_covers_everything_and_closes_again() {
        let mut state = State::default();
        assert!(ch(&mut state, '?').redraw);
        assert!(state.help);
        // Inside the help `j` scrolls it, it does not move the sidebar behind it.
        ch(&mut state, 'j');
        assert_eq!(
            state.section(),
            Section::Queue,
            "the keys behind the help are off"
        );
        assert!(
            ch(&mut state, 's').commands.is_empty(),
            "so are the playback keys"
        );
        assert!(ch(&mut state, '?').redraw);
        assert!(!state.help);

        ch(&mut state, '?');
        assert!(press(&mut state, KeyCode::Esc).redraw);
        assert!(!state.help);
        ch(&mut state, '?');
        assert!(
            ch(&mut state, 'q').commands.is_empty(),
            "q closes the help, it does not quit"
        );
        assert!(!state.help);
    }

    #[test]
    fn control_c_quits_even_from_the_help() {
        let mut state = State::default();
        ch(&mut state, '?');
        assert_eq!(ctrl(&mut state, 'c').commands, vec![Cmd::Quit]);
    }

    #[test]
    fn a_resize_repaints_and_remembers_the_size() {
        let mut state = State::default();
        assert!(update(&mut state, Msg::Resize(100, 30)).redraw);
        assert_eq!(state.size, (100, 30));
    }

    #[test]
    fn a_tick_changes_nothing() {
        let mut state = State::default();
        assert_eq!(update(&mut state, Msg::Tick), Effects::default());
        assert_eq!(state, State::default());
    }

    #[test]
    fn unbound_keys_change_nothing() {
        let mut state = State::default();
        for effects in [ch(&mut state, 'x'), press(&mut state, KeyCode::F(5))] {
            assert_eq!(effects, Effects::default());
        }
        assert_eq!(state, State::default());
    }

    #[test]
    fn connecting_stores_the_daemon_and_what_it_said() {
        let mut state = State::default();
        assert_eq!(state.connection, Connection::Connecting);
        assert!(update(&mut state, connected_msg(&["volume"])).redraw);
        assert_eq!(state.connection, Connection::Connected);
        let server = state.server.as_ref().unwrap();
        assert_eq!(server.info.version, "0.1.0");
        assert_eq!(server.protocol, Version { major: 1, minor: 5 });
        assert_eq!(state.status, Some(status()));
        assert_eq!(state.queue, Some(queue()));
    }

    #[test]
    fn features_are_offered_only_by_a_connected_daemon_that_has_them() {
        let mut state = State::default();
        assert!(!state.has("volume"), "not connected: nothing");
        update(&mut state, connected_msg(&["volume"]));
        assert!(state.has("volume"));
        assert!(
            !state.has("quality"),
            "an older daemon lacks newer features"
        );
        disconnected(&mut state, Duration::from_secs(1));
        assert!(!state.has("volume"), "gone: nothing again");
    }

    #[test]
    fn losing_the_daemon_keeps_what_was_known_but_says_it_is_gone() {
        let mut state = State::default();
        update(&mut state, connected_msg(&[]));
        assert!(
            update(
                &mut state,
                Msg::Disconnected {
                    reason: "the connection to the daemon was closed".into(),
                    retry_in: Duration::from_millis(250),
                }
            )
            .redraw
        );
        assert!(matches!(state.connection, Connection::Disconnected { .. }));
        assert_eq!(state.status, Some(status()), "the last state stays, stale");
        assert!(state.server.is_some());
    }

    #[test]
    fn the_countdown_repaints_only_when_the_seconds_shown_change() {
        let mut state = State::default();
        disconnected(&mut state, Duration::from_millis(2100));
        // 2100 ms shows "3"; 1850 shows "2": repaint. Then 1600, 1350 and 1100 still show "2".
        let repaints: Vec<bool> = (0..5)
            .map(|_| update(&mut state, Msg::Tick).redraw)
            .collect();
        assert_eq!(repaints, [true, false, false, false, true]);
        for _ in 0..20 {
            update(&mut state, Msg::Tick);
        }
        assert!(matches!(
            state.connection,
            Connection::Disconnected { retry_in, .. } if retry_in == Duration::ZERO
        ));
        assert!(
            !update(&mut state, Msg::Tick).redraw,
            "at zero it stays there"
        );
    }

    #[test]
    fn a_wait_is_shown_in_whole_seconds_rounded_up() {
        assert_eq!(seconds_left(Duration::from_millis(250)), 1);
        assert_eq!(seconds_left(Duration::from_millis(1000)), 1);
        assert_eq!(seconds_left(Duration::from_millis(1001)), 2);
        assert_eq!(seconds_left(Duration::ZERO), 0);
    }

    #[test]
    fn r_asks_to_connect_now_only_when_there_is_no_connection() {
        let mut state = State::default();
        assert_eq!(
            ch(&mut state, 'R'),
            Effects::default(),
            "already connecting"
        );

        disconnected(&mut state, Duration::from_secs(4));
        let effects = ch(&mut state, 'R');
        assert_eq!(effects.commands, vec![Cmd::RetryNow]);
        assert!(effects.redraw);
        assert_eq!(state.connection, Connection::Connecting);

        update(
            &mut state,
            Msg::Refused {
                reason: "no".into(),
            },
        );
        assert_eq!(ch(&mut state, 'R').commands, vec![Cmd::RetryNow]);

        update(&mut state, connected_msg(&[]));
        assert_eq!(
            ch(&mut state, 'R'),
            Effects::default(),
            "connected: nothing to do"
        );
    }

    #[test]
    fn a_resync_replaces_what_was_held() {
        let mut state = State::default();
        update(&mut state, connected_msg(&[]));
        let mut newer = status();
        newer.position_ms = 42_000;
        let effects = update(
            &mut state,
            Msg::Daemon(Event::Resync {
                skipped: 300,
                seq: 99,
                status: newer.clone(),
                queue: queue(),
            }),
        );
        assert!(effects.redraw);
        assert_eq!(state.status, Some(newer));
    }

    #[test]
    fn a_queue_change_replaces_the_queue_and_unknown_events_change_nothing() {
        let mut state = State::default();
        update(&mut state, connected_msg(&[]));
        let mut changed = queue();
        changed.version = 2;
        assert!(
            update(
                &mut state,
                Msg::Daemon(Event::QueueChanged {
                    queue: changed.clone()
                })
            )
            .redraw
        );
        assert_eq!(state.queue, Some(changed));

        let before = state.clone();
        for event in [Event::Unknown, Event::ShuttingDown, Event::OutputsChanged] {
            assert_eq!(update(&mut state, Msg::Daemon(event)), Effects::default());
        }
        assert_eq!(state, before);
    }

    fn playing_with_volume(volume: Option<phonia_ipc::Volume>, capabilities: &[&str]) -> State {
        let mut state = State::default();
        update(&mut state, connected_msg(capabilities));
        state.status.as_mut().unwrap().volume = volume;
        state
    }

    fn sent(effects: Effects) -> Request {
        match effects.commands.as_slice() {
            [Cmd::Send(request)] => request.clone(),
            other => panic!("expected one request, got {other:?}"),
        }
    }

    #[test]
    fn the_seek_keys_move_ten_seconds_either_way() {
        let mut state = playing_with_volume(None, &[]);
        assert_eq!(
            sent(ch(&mut state, '>')),
            Request::Seek {
                target: SeekTarget::Forward { ms: 10_000 }
            }
        );
        assert_eq!(
            sent(ch(&mut state, '<')),
            Request::Seek {
                target: SeekTarget::Backward { ms: 10_000 }
            }
        );
    }

    #[test]
    fn the_volume_keys_step_five_percent_within_zero_and_a_hundred() {
        let volume = |percent| {
            Some(phonia_ipc::Volume {
                percent,
                muted: false,
            })
        };
        let mut state = playing_with_volume(volume(50), &["volume"]);
        assert_eq!(
            sent(ch(&mut state, '+')),
            Request::SetVolume { percent: 55 }
        );
        assert_eq!(
            sent(ch(&mut state, '=')),
            Request::SetVolume { percent: 55 }
        );
        assert_eq!(
            sent(ch(&mut state, '-')),
            Request::SetVolume { percent: 45 }
        );

        let mut state = playing_with_volume(volume(98), &["volume"]);
        assert_eq!(
            sent(ch(&mut state, '+')),
            Request::SetVolume { percent: 100 }
        );
        let mut state = playing_with_volume(volume(3), &["volume"]);
        assert_eq!(sent(ch(&mut state, '-')), Request::SetVolume { percent: 0 });
    }

    #[test]
    fn mute_flips_what_the_output_has_now() {
        let mut state = playing_with_volume(
            Some(phonia_ipc::Volume {
                percent: 40,
                muted: false,
            }),
            &["volume"],
        );
        assert_eq!(sent(ch(&mut state, 'm')), Request::SetMute { mute: true });
        state.status.as_mut().unwrap().volume = Some(phonia_ipc::Volume {
            percent: 40,
            muted: true,
        });
        assert_eq!(sent(ch(&mut state, 'm')), Request::SetMute { mute: false });
    }

    #[test]
    fn an_output_without_a_volume_says_why_instead_of_sending() {
        let mut state = playing_with_volume(None, &["volume"]);
        for key in ['+', '-', 'm'] {
            let effects = ch(&mut state, key);
            assert!(effects.commands.is_empty(), "{key}");
            assert!(effects.redraw, "{key}");
            assert!(
                state
                    .last_error
                    .as_deref()
                    .unwrap()
                    .contains("no volume to set"),
                "{key}: {:?}",
                state.last_error
            );
        }
    }

    #[test]
    fn a_daemon_without_the_volume_capability_says_it_is_too_old() {
        let mut state = playing_with_volume(
            Some(phonia_ipc::Volume {
                percent: 40,
                muted: false,
            }),
            &[],
        );
        let effects = ch(&mut state, '+');
        assert!(effects.commands.is_empty());
        assert!(state.last_error.as_deref().unwrap().contains("too old"));
    }

    #[test]
    fn shuffle_flips_and_repeat_goes_around() {
        let mut state = playing_with_volume(None, &[]);
        assert_eq!(
            sent(ch(&mut state, 's')),
            Request::SetShuffle { shuffle: true }
        );
        state.queue.as_mut().unwrap().shuffle = true;
        assert_eq!(
            sent(ch(&mut state, 's')),
            Request::SetShuffle { shuffle: false }
        );

        let mut seen = Vec::new();
        for _ in 0..3 {
            let Request::SetRepeat { repeat } = sent(ch(&mut state, 'r')) else {
                panic!("not a repeat request");
            };
            state.queue.as_mut().unwrap().repeat = repeat;
            seen.push(repeat);
        }
        assert_eq!(seen, [Repeat::All, Repeat::One, Repeat::Off]);
    }

    #[test]
    fn none_of_the_control_keys_do_anything_without_a_connection() {
        let mut state = State::default();
        for key in ['<', '>', '+', '-', 'm', 's', 'r'] {
            assert_eq!(ch(&mut state, key), Effects::default(), "{key}");
        }
        assert_eq!(state.last_error, None);
    }

    #[test]
    fn the_volume_the_output_and_the_position_follow_the_daemon() {
        let mut state = playing_with_volume(None, &["volume"]);
        update(
            &mut state,
            Msg::Daemon(Event::VolumeChanged {
                percent: 30,
                muted: true,
            }),
        );
        assert_eq!(
            state.status.as_ref().unwrap().volume,
            Some(phonia_ipc::Volume {
                percent: 30,
                muted: true
            })
        );

        // Switching to an exclusive card takes the volume away; a shared output keeps it.
        let route = |mode| phonia_ipc::Route {
            id: "x".into(),
            mode,
            description: "x".into(),
        };
        update(
            &mut state,
            Msg::Daemon(Event::OutputChanged {
                route: route(phonia_ipc::OutputMode::Shared),
            }),
        );
        assert!(state.status.as_ref().unwrap().volume.is_some());
        update(
            &mut state,
            Msg::Daemon(Event::OutputChanged {
                route: route(phonia_ipc::OutputMode::Exclusive),
            }),
        );
        assert_eq!(state.status.as_ref().unwrap().volume, None);

        update(
            &mut state,
            Msg::Daemon(Event::Seeked {
                position_ms: 90_000,
            }),
        );
        assert_eq!(state.status.as_ref().unwrap().position_ms, 90_000);
    }

    fn a_sink_report(device: &str) -> phonia_ipc::SinkReport {
        phonia_ipc::SinkReport {
            device: device.to_string(),
            source: phonia_ipc::Spec {
                sample_rate: 96_000,
                channels: 2,
                bits_per_sample: 24,
            },
            negotiated_format: "S24_3LE".into(),
            bit_perfect: true,
            problem: None,
            hw_params: None,
            mode: Some(phonia_ipc::OutputMode::Exclusive),
            resampled_to: None,
            codec: None,
            lossy: false,
            output: Some("exclusive:hw:1,0".into()),
        }
    }

    #[test]
    fn a_sink_report_shows_up_in_the_status() {
        let mut state = playing_with_volume(None, &[]);
        update(
            &mut state,
            Msg::Daemon(Event::SinkReport(a_sink_report("hw:1,0"))),
        );
        assert_eq!(
            state.status.as_ref().unwrap().sink_report,
            Some(a_sink_report("hw:1,0"))
        );
    }

    #[test]
    fn the_verdict_survives_a_track_ending_but_not_the_engine_stopping() {
        // A SinkReport is sent once per sink open, not once per track: a gapless album of the
        // same format shares one, so TrackEnded must not blank it out from under the next track.
        let mut state = playing_with_volume(None, &[]);
        update(
            &mut state,
            Msg::Daemon(Event::SinkReport(a_sink_report("hw:1,0"))),
        );
        update(
            &mut state,
            Msg::Daemon(Event::TrackEnded {
                item_id: None,
                reason: phonia_ipc::EndReason::Completed,
            }),
        );
        assert!(
            state.status.as_ref().unwrap().sink_report.is_some(),
            "a track ending does not mean the sink closed"
        );

        // The engine actually stopping is the one event that means the sink is gone for certain.
        update(
            &mut state,
            Msg::Daemon(Event::StateChanged {
                state: phonia_ipc::State::Stopped,
            }),
        );
        let status = state.status.as_ref().unwrap();
        assert_eq!(status.sink_report, None);
        assert_eq!(status.output, phonia_ipc::Output::Closed);
    }

    #[test]
    fn releasing_the_output_clears_the_verdict_and_acquiring_it_reopens_it() {
        let mut state = playing_with_volume(None, &[]);
        update(
            &mut state,
            Msg::Daemon(Event::SinkReport(a_sink_report("hw:1,0"))),
        );

        update(
            &mut state,
            Msg::Daemon(Event::OutputReleased {
                by: Some("jackd".into()),
                reason: phonia_ipc::ReleaseReason::Requested,
            }),
        );
        let status = state.status.as_ref().unwrap();
        assert_eq!(
            status.output,
            phonia_ipc::Output::Released {
                by: Some("jackd".into())
            }
        );
        assert_eq!(
            status.sink_report, None,
            "the released sink's verdict is gone with it"
        );

        update(&mut state, Msg::Daemon(Event::OutputAcquired));
        assert_eq!(
            state.status.as_ref().unwrap().output,
            phonia_ipc::Output::Open
        );
    }

    #[test]
    fn a_track_starting_means_the_output_is_open() {
        let mut state = State::default();
        update(&mut state, connected_msg(&[]));
        state.status.as_mut().unwrap().output = phonia_ipc::Output::Released { by: None };
        update(
            &mut state,
            Msg::Daemon(Event::TrackStarted {
                item_id: None,
                source: Some("tidal:1".into()),
                title: Some("Song".into()),
                duration_ms: None,
                spec: phonia_ipc::Spec {
                    sample_rate: 44_100,
                    channels: 2,
                    bits_per_sample: 16,
                },
                gapless: false,
                quality: None,
                cover: None,
            }),
        );
        assert_eq!(
            state.status.as_ref().unwrap().output,
            phonia_ipc::Output::Open
        );
    }

    #[test]
    fn a_rejected_seek_is_explained_until_the_next_key() {
        let mut state = playing_with_volume(None, &[]);
        assert!(
            update(
                &mut state,
                Msg::Daemon(Event::SeekRejected {
                    reason: "this track can't seek".into()
                })
            )
            .redraw
        );
        assert_eq!(
            state.last_error.as_deref(),
            Some("seek rejected: this track can't seek")
        );
        ch(&mut state, '>');
        assert_eq!(state.last_error, None, "the next command clears it");
    }

    #[test]
    fn a_help_longer_than_the_screen_scrolls_and_stops_at_its_ends() {
        let mut state = State::default();
        update(&mut state, Msg::Resize(80, 24));
        ch(&mut state, '?');
        let max = help_max_scroll(&state);
        assert!(max > 0, "the help does not fit in 24 rows");

        assert!(!ch(&mut state, 'k').redraw, "already at the top");
        assert!(ch(&mut state, 'j').redraw);
        assert_eq!(state.help_scroll, 1);
        ch(&mut state, 'G');
        assert_eq!(state.help_scroll, max);
        assert!(!ch(&mut state, 'j').redraw, "already at the bottom");
        ch(&mut state, 'g');
        ch(&mut state, 'g');
        assert_eq!(
            state.help_scroll, max,
            "gg is not a key inside the help, only its first g"
        );
        ctrl(&mut state, 'u');
        assert!(state.help_scroll < max);
    }

    #[test]
    fn a_help_that_fits_has_nothing_to_scroll_and_starts_at_the_top_each_time() {
        let mut state = State::default();
        update(&mut state, Msg::Resize(80, 80));
        ch(&mut state, '?');
        assert_eq!(help_max_scroll(&state), 0);
        assert!(!ch(&mut state, 'j').redraw);

        update(&mut state, Msg::Resize(80, 24));
        ch(&mut state, 'j');
        ch(&mut state, 'j');
        ch(&mut state, '?');
        ch(&mut state, '?');
        assert_eq!(state.help_scroll, 0, "opened again, it starts at the top");
    }

    /// Connected, with a queue of `count` entries (ids 1..=count) and the focus on the queue.
    fn in_the_queue(count: u64) -> State {
        use phonia_ipc::{ItemId, QueueItem};
        let mut state = State::default();
        update(&mut state, connected_msg(&[]));
        let items: Vec<QueueItem> = (1..=count)
            .map(|n| QueueItem {
                id: ItemId(n),
                source: format!("file:/t{n}.flac"),
                title: None,
                duration_ms: None,
                cover: None,
            })
            .collect();
        let mut queue = queue();
        queue.order = items.iter().map(|item| item.id).collect();
        queue.items = items;
        update(&mut state, Msg::Daemon(Event::QueueChanged { queue }));
        ch(&mut state, 'l');
        state
    }

    fn id(n: u64) -> phonia_ipc::ItemId {
        phonia_ipc::ItemId(n)
    }

    #[test]
    fn the_movement_keys_move_the_queue_cursor_when_the_queue_has_the_focus() {
        let mut state = in_the_queue(5);
        ch(&mut state, 'j');
        ch(&mut state, 'j');
        assert_eq!(state.queue_cursor.selected(), 2);
        assert_eq!(state.section(), Section::Queue, "the sidebar did not move");
        ch(&mut state, 'G');
        assert_eq!(state.queue_cursor.selected(), 4);
        ch(&mut state, 'g');
        ch(&mut state, 'g');
        assert_eq!(state.queue_cursor.selected(), 0);
        ch(&mut state, 'k');
        assert_eq!(state.queue_cursor.selected(), 0, "stops at the top");
    }

    #[test]
    fn enter_opens_the_section_from_the_sidebar_and_plays_the_entry_in_the_queue() {
        let mut state = in_the_queue(3);
        ch(&mut state, 'h');
        assert_eq!(state.focus, Focus::Sidebar);
        let effects = press(&mut state, KeyCode::Enter);
        assert_eq!(state.focus, Focus::Main);
        assert!(
            effects.commands.is_empty(),
            "opening the section plays nothing"
        );

        ch(&mut state, 'j');
        assert_eq!(
            sent(press(&mut state, KeyCode::Enter)),
            Request::Play { item: Some(id(2)) }
        );
    }

    #[test]
    fn d_removes_the_selected_entry_and_the_cursor_stays_on_the_list() {
        let mut state = in_the_queue(3);
        ch(&mut state, 'G');
        assert_eq!(
            sent(ch(&mut state, 'd')),
            Request::QueueRemove { ids: vec![id(3)] }
        );
        // The daemon answers with the shorter queue; the cursor is brought back inside it.
        let mut shorter = state.queue.clone().unwrap();
        shorter.items.pop();
        shorter.order.pop();
        update(
            &mut state,
            Msg::Daemon(Event::QueueChanged { queue: shorter }),
        );
        assert_eq!(state.queue_cursor.selected(), 1);
    }

    #[test]
    fn capital_j_and_k_move_the_entry_and_the_cursor_goes_with_it() {
        let mut state = in_the_queue(4);
        ch(&mut state, 'j');
        assert_eq!(
            sent(ch(&mut state, 'J')),
            Request::QueueMove { id: id(2), to: 2 }
        );
        assert_eq!(state.queue_cursor.selected(), 2);
        assert_eq!(
            sent(ch(&mut state, 'K')),
            Request::QueueMove { id: id(3), to: 1 }
        );
        assert_eq!(state.queue_cursor.selected(), 1);
    }

    #[test]
    fn an_entry_cannot_be_moved_past_the_ends() {
        let mut state = in_the_queue(3);
        assert_eq!(ch(&mut state, 'K'), Effects::default(), "already first");
        ch(&mut state, 'G');
        assert_eq!(ch(&mut state, 'J'), Effects::default(), "already last");
        assert_eq!(state.queue_cursor.selected(), 2);
    }

    #[test]
    fn clearing_the_queue_takes_two_presses_of_c() {
        let mut state = in_the_queue(3);
        ch(&mut state, 'j');
        assert_eq!(ch(&mut state, 'c'), Effects::default(), "half a binding");
        assert_eq!(sent(ch(&mut state, 'c')), Request::QueueClear);
        assert_eq!(state.queue_cursor.selected(), 0);

        // Anything in between cancels it: a stray c is never enough.
        let mut state = in_the_queue(3);
        ch(&mut state, 'c');
        ch(&mut state, 'x');
        assert_eq!(ch(&mut state, 'c'), Effects::default());
    }

    #[test]
    fn the_queue_keys_do_nothing_elsewhere() {
        // In the sidebar.
        let mut state = in_the_queue(3);
        ch(&mut state, 'h');
        for key in ['d', 'J', 'K'] {
            assert_eq!(ch(&mut state, key), Effects::default(), "sidebar, {key}");
        }
        ch(&mut state, 'c');
        assert_eq!(ch(&mut state, 'c'), Effects::default(), "sidebar, cc");

        // In another section.
        let mut state = in_the_queue(3);
        ch(&mut state, '2');
        for key in ['d', 'J', 'K'] {
            assert_eq!(ch(&mut state, key), Effects::default(), "search, {key}");
        }

        // On an empty queue.
        let mut state = in_the_queue(0);
        for key in ['d', 'J', 'K'] {
            assert_eq!(ch(&mut state, key), Effects::default(), "empty, {key}");
        }
        assert_eq!(press(&mut state, KeyCode::Enter), Effects::default());
        ch(&mut state, 'c');
        assert_eq!(ch(&mut state, 'c'), Effects::default(), "nothing to clear");
    }

    #[test]
    fn without_a_connection_the_queue_keys_do_nothing() {
        let mut state = in_the_queue(3);
        update(
            &mut state,
            Msg::Disconnected {
                reason: "gone".into(),
                retry_in: Duration::from_secs(1),
            },
        );
        for key in ['d', 'J', 'K'] {
            assert_eq!(ch(&mut state, key), Effects::default(), "{key}");
        }
        assert_eq!(press(&mut state, KeyCode::Enter), Effects::default());
    }

    // --- Searching: what leaves, and where the answer goes -----------------------------------

    fn results_payload(total: u64) -> Payload {
        Payload::SearchResults {
            query: "korn".into(),
            tracks: Some(phonia_ipc::Page {
                items: vec![phonia_ipc::TrackSummary {
                    id: "1".into(),
                    title: "Song".into(),
                    version: None,
                    artists: vec![],
                    album: None,
                    duration_ms: None,
                    explicit: false,
                    track_number: None,
                    volume_number: None,
                    quality: None,
                    streamable: true,
                }],
                total,
                offset: 0,
            }),
            albums: None,
            artists: None,
            playlists: None,
        }
    }

    fn search_request(effects: Effects) -> (Tag, Request) {
        match effects.commands.as_slice() {
            [Cmd::Request { tag, request }] => (*tag, request.clone()),
            other => panic!("expected one tagged request, got {other:?}"),
        }
    }

    #[test]
    fn a_search_leaves_as_a_tagged_request_and_marks_the_search_as_waiting() {
        use crate::search::Phase;
        let mut state = State::default();
        update(&mut state, connected_msg(&["catalog"]));
        let effects = submit_search(&mut state, "  korn  ");
        assert!(effects.redraw);
        let (tag, request) = search_request(effects);
        assert_eq!(tag, Tag::Search { generation: 1 });
        assert_eq!(
            request,
            Request::Search {
                query: "korn".into(),
                kinds: vec![],
                offset: 0,
                limit: Some(50)
            }
        );
        assert_eq!(state.search.phase, Phase::Searching);
        assert_eq!(state.search.query, "korn");
    }

    #[test]
    fn an_empty_search_does_nothing() {
        let mut state = State::default();
        update(&mut state, connected_msg(&["catalog"]));
        assert_eq!(submit_search(&mut state, "   "), Effects::default());
        assert_eq!(state.search.generation, 0);
    }

    #[test]
    fn a_search_without_a_connection_or_a_catalog_says_why_and_sends_nothing() {
        use crate::search::Phase;
        // Not connected.
        let mut state = State::default();
        let effects = submit_search(&mut state, "korn");
        assert!(effects.commands.is_empty() && effects.redraw);
        assert!(matches!(&state.search.phase, Phase::Failed(why) if why.contains("not connected")));

        // Connected to a daemon that cannot search.
        let mut state = State::default();
        update(&mut state, connected_msg(&["volume"]));
        let effects = submit_search(&mut state, "korn");
        assert!(effects.commands.is_empty() && effects.redraw);
        assert!(matches!(&state.search.phase, Phase::Failed(why) if why.contains("cannot search")));
    }

    #[test]
    fn the_answer_to_the_current_search_fills_the_results() {
        use crate::search::Phase;
        let mut state = State::default();
        update(&mut state, connected_msg(&["catalog"]));
        let (tag, _) = search_request(submit_search(&mut state, "korn"));
        let effects = update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(results_payload(123)),
            },
        );
        assert!(effects.redraw);
        assert_eq!(state.search.phase, Phase::Done);
        assert_eq!(state.search.tracks.items.len(), 1);
        assert_eq!(state.search.tracks.total, 123);
    }

    #[test]
    fn a_refusal_becomes_the_failure_shown_where_the_results_would_be() {
        use crate::search::Phase;
        let mut state = State::default();
        update(&mut state, connected_msg(&["catalog"]));
        let (tag, _) = search_request(submit_search(&mut state, "korn"));
        update(
            &mut state,
            Msg::Response {
                tag,
                result: Err("not logged in to TIDAL: run `phonia login`".into()),
            },
        );
        assert_eq!(
            state.search.phase,
            Phase::Failed("not logged in to TIDAL: run `phonia login`".into())
        );
    }

    #[test]
    fn the_answer_to_an_old_search_is_dropped() {
        use crate::search::Phase;
        let mut state = State::default();
        update(&mut state, connected_msg(&["catalog"]));
        let (old, _) = search_request(submit_search(&mut state, "korn"));
        let (new, _) = search_request(submit_search(&mut state, "nu metal"));
        assert_ne!(old, new);

        let effects = update(
            &mut state,
            Msg::Response {
                tag: old,
                result: Ok(results_payload(5)),
            },
        );
        assert_eq!(
            effects,
            Effects::default(),
            "nothing to show: it is not the search asked for"
        );
        assert_eq!(state.search.phase, Phase::Searching);
        assert!(state.search.tracks.items.is_empty());
        assert_eq!(state.search.query, "nu metal");
    }

    #[test]
    fn a_search_waiting_when_the_connection_is_lost_fails_and_its_late_answer_is_dropped() {
        use crate::search::Phase;
        let mut state = State::default();
        update(&mut state, connected_msg(&["catalog"]));
        let (tag, _) = search_request(submit_search(&mut state, "korn"));
        disconnected(&mut state, Duration::from_secs(1));
        assert!(matches!(state.search.phase, Phase::Failed(_)));

        // The answer turns up after all (the request was still on its way): it is not taken.
        update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(results_payload(9)),
            },
        );
        assert!(matches!(state.search.phase, Phase::Failed(_)));
        assert!(state.search.tracks.items.is_empty());
    }

    // --- Typing a search ---------------------------------------------------------------------

    fn typed(state: &mut State, text: &str) {
        for c in text.chars() {
            ch(state, c);
        }
    }

    fn connected_to_a_catalog() -> State {
        let mut state = State::default();
        update(&mut state, connected_msg(&["catalog"]));
        state
    }

    #[test]
    fn slash_opens_the_search_and_starts_typing_from_anywhere() {
        let mut state = connected_to_a_catalog();
        assert_eq!(state.section(), Section::Queue);
        assert_eq!(state.focus, Focus::Sidebar);
        assert!(ch(&mut state, '/').redraw);
        assert_eq!(state.section(), Section::Search);
        assert_eq!(state.focus, Focus::Main);
        assert!(state.search.editing);
    }

    #[test]
    fn while_typing_every_key_is_text_not_a_command() {
        let mut state = connected_to_a_catalog();
        ch(&mut state, '/');
        // `q` would quit and `j` would move: here they are letters.
        let effects = ch(&mut state, 'q');
        assert!(effects.commands.is_empty());
        typed(&mut state, "j nu?/");
        assert_eq!(state.search.input.text(), "qj nu?/");
        assert!(state.search.editing);
        assert_eq!(state.section(), Section::Search, "nothing moved");
        assert!(!state.quit);
    }

    #[test]
    fn escape_stops_typing_and_keeps_the_text_to_edit_again() {
        let mut state = connected_to_a_catalog();
        ch(&mut state, '/');
        typed(&mut state, "korn");
        assert!(press(&mut state, KeyCode::Esc).redraw);
        assert!(!state.search.editing);
        assert_eq!(state.search.input.text(), "korn");

        // The commands work again, and `/` resumes where it was.
        ch(&mut state, '/');
        typed(&mut state, "!");
        assert_eq!(state.search.input.text(), "korn!");
    }

    #[test]
    fn enter_searches_and_leaves_typing_for_the_results() {
        use crate::search::Phase;
        let mut state = connected_to_a_catalog();
        ch(&mut state, '/');
        typed(&mut state, "  nu metal ");
        let effects = press(&mut state, KeyCode::Enter);
        assert!(effects.redraw);
        assert!(!state.search.editing);
        assert_eq!(state.focus, Focus::Main);
        let (tag, request) = search_request(effects);
        assert_eq!(tag, Tag::Search { generation: 1 });
        assert!(matches!(request, Request::Search { ref query, .. } if query == "nu metal"));
        assert_eq!(state.search.phase, Phase::Searching);
    }

    #[test]
    fn enter_on_an_empty_line_only_stops_typing() {
        use crate::search::Phase;
        let mut state = connected_to_a_catalog();
        ch(&mut state, '/');
        let effects = press(&mut state, KeyCode::Enter);
        assert!(effects.commands.is_empty());
        assert!(!state.search.editing);
        assert_eq!(state.search.phase, Phase::Idle);
    }

    #[test]
    fn enter_without_a_connection_says_so_on_the_results() {
        use crate::search::Phase;
        let mut state = State::default();
        ch(&mut state, '/');
        typed(&mut state, "korn");
        let effects = press(&mut state, KeyCode::Enter);
        assert!(effects.commands.is_empty());
        assert!(matches!(state.search.phase, Phase::Failed(_)));
    }

    #[test]
    fn control_c_still_quits_while_typing_and_other_chords_are_not_text() {
        let mut state = connected_to_a_catalog();
        ch(&mut state, '/');
        typed(&mut state, "ab");
        // A chord this line has no use for is not typed.
        ctrl(&mut state, 'x');
        update(
            &mut state,
            Msg::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)),
        );
        assert_eq!(state.search.input.text(), "ab");
        assert_eq!(ctrl(&mut state, 'c').commands, vec![Cmd::Quit]);
    }

    #[test]
    fn the_line_can_be_edited_with_the_usual_keys() {
        let mut state = connected_to_a_catalog();
        ch(&mut state, '/');
        typed(&mut state, "korn untouchables");

        ctrl(&mut state, 'w');
        assert_eq!(state.search.input.text(), "korn ");
        press(&mut state, KeyCode::Backspace);
        assert_eq!(state.search.input.text(), "korn");
        press(&mut state, KeyCode::Home);
        typed(&mut state, "K");
        press(&mut state, KeyCode::Delete);
        assert_eq!(state.search.input.text(), "Korn");
        press(&mut state, KeyCode::Right);
        typed(&mut state, "_");
        assert_eq!(state.search.input.text(), "Ko_rn");
        press(&mut state, KeyCode::End);
        press(&mut state, KeyCode::Left);
        typed(&mut state, "-");
        assert_eq!(state.search.input.text(), "Ko_r-n");
        ctrl(&mut state, 'a');
        ctrl(&mut state, 'u');
        assert_eq!(
            state.search.input.text(),
            "Ko_r-n",
            "nothing before the start"
        );
        ctrl(&mut state, 'e');
        ctrl(&mut state, 'u');
        assert_eq!(state.search.input.text(), "");
    }

    #[test]
    fn the_result_lists_are_switched_with_the_brackets_only_in_the_search() {
        use crate::search::Tab;
        let mut state = connected_to_a_catalog();
        assert_eq!(ch(&mut state, ']'), Effects::default(), "not in the search");
        assert_eq!(state.search.tab, Tab::Tracks);

        ch(&mut state, '/');
        press(&mut state, KeyCode::Esc);
        assert!(ch(&mut state, ']').redraw);
        assert_eq!(state.search.tab, Tab::Albums);
        ch(&mut state, ']');
        ch(&mut state, ']');
        assert_eq!(state.search.tab, Tab::Playlists);
        assert!(!ch(&mut state, ']').redraw, "stops at the last");
        ch(&mut state, '[');
        assert_eq!(state.search.tab, Tab::Artists);
    }

    #[test]
    fn the_movement_keys_walk_the_list_of_the_current_tab() {
        use crate::search::Tab;
        let mut state = connected_to_a_catalog();
        let (tag, _) = search_request({
            ch(&mut state, '/');
            typed(&mut state, "korn");
            press(&mut state, KeyCode::Enter)
        });
        let track = |id: &str| phonia_ipc::TrackSummary {
            id: id.into(),
            title: format!("Song {id}"),
            version: None,
            artists: vec![],
            album: None,
            duration_ms: None,
            explicit: false,
            track_number: None,
            volume_number: None,
            quality: None,
            streamable: true,
        };
        update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(Payload::SearchResults {
                    query: "korn".into(),
                    tracks: Some(phonia_ipc::Page {
                        items: vec![track("1"), track("2"), track("3")],
                        total: 3,
                        offset: 0,
                    }),
                    albums: Some(phonia_ipc::Page {
                        items: vec![],
                        total: 0,
                        offset: 0,
                    }),
                    artists: None,
                    playlists: None,
                }),
            },
        );
        ch(&mut state, 'j');
        ch(&mut state, 'j');
        assert_eq!(state.search.tracks.cursor.selected(), 2);
        ch(&mut state, 'j');
        assert_eq!(
            state.search.tracks.cursor.selected(),
            2,
            "stops at the last row"
        );
        ch(&mut state, 'g');
        ch(&mut state, 'g');
        assert_eq!(state.search.tracks.cursor.selected(), 0);
        ch(&mut state, 'G');
        assert_eq!(state.search.tracks.cursor.selected(), 2);

        // Another tab has its own cursor, and an empty one has nowhere to go.
        ch(&mut state, ']');
        assert_eq!(state.search.tab, Tab::Albums);
        assert_eq!(ch(&mut state, 'j'), Effects::default());
        ch(&mut state, '[');
        assert_eq!(
            state.search.tracks.cursor.selected(),
            2,
            "the first tab kept its place"
        );
    }

    #[test]
    fn enter_in_the_search_list_opens_the_line_for_typing() {
        let mut state = connected_to_a_catalog();
        ch(&mut state, '2');
        ch(&mut state, 'l');
        assert_eq!(state.section(), Section::Search);
        assert!(!state.search.editing);
        assert!(press(&mut state, KeyCode::Enter).redraw);
        assert!(state.search.editing);
    }

    // --- What to do with a result, and more pages ---------------------------------------------

    fn track_row(id: u64) -> phonia_ipc::TrackSummary {
        phonia_ipc::TrackSummary {
            id: id.to_string(),
            title: format!("Song {id}"),
            version: None,
            artists: vec![],
            album: None,
            duration_ms: None,
            explicit: false,
            track_number: None,
            volume_number: None,
            quality: None,
            streamable: true,
        }
    }

    fn page_of<T>(items: Vec<T>, total: u64, offset: u64) -> Option<phonia_ipc::Page<T>> {
        Some(phonia_ipc::Page {
            items,
            total,
            offset,
        })
    }

    /// The results of a search for "korn": `tracks` (of `total` in all), one album, one
    /// playlist and one artist. The focus is on the list, with the daemon able to browse.
    fn with_results(tracks: Vec<phonia_ipc::TrackSummary>, total: u64) -> State {
        let mut state = State::default();
        update(&mut state, connected_msg(&["catalog"]));
        ch(&mut state, '/');
        typed(&mut state, "korn");
        let (tag, _) = search_request(press(&mut state, KeyCode::Enter));
        update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(Payload::SearchResults {
                    query: "korn".into(),
                    tracks: page_of(tracks, total, 0),
                    albums: page_of(
                        vec![phonia_ipc::AlbumSummary {
                            id: "33723912".into(),
                            title: "Issues".into(),
                            version: None,
                            artists: vec![],
                            release_date: None,
                            track_count: Some(16),
                            duration_ms: None,
                            explicit: false,
                            quality: None,
                            kind: None,
                            copyright: None,
                            cover: None,
                        }],
                        1,
                        0,
                    ),
                    artists: page_of(
                        vec![phonia_ipc::ArtistSummary {
                            id: "780".into(),
                            name: "Korn".into(),
                            picture: None,
                        }],
                        1,
                        0,
                    ),
                    playlists: page_of(
                        vec![phonia_ipc::PlaylistSummary {
                            id: "5545fb2d-fd50".into(),
                            title: "Korn Essentials".into(),
                            creator: None,
                            description: None,
                            track_count: Some(25),
                            duration_ms: None,
                            cover: None,
                        }],
                        1,
                        0,
                    ),
                }),
            },
        );
        state
    }

    fn tagged(effects: Effects) -> (Tag, Request) {
        search_request(effects)
    }

    #[test]
    fn enter_on_a_track_adds_it_after_the_one_playing_and_asks_to_play_it() {
        let mut state = with_results(vec![track_row(11), track_row(12)], 2);
        ch(&mut state, 'j');
        let (tag, request) = tagged(press(&mut state, KeyCode::Enter));
        assert_eq!(tag, Tag::Add { play: true });
        assert_eq!(
            request,
            Request::QueueAdd {
                tracks: vec![phonia_ipc::NewTrack {
                    source: "tidal:12".into()
                }],
                at: phonia_ipc::AddAt::Next
            }
        );
    }

    #[test]
    fn a_adds_to_the_end_and_capital_a_right_after_the_playing_track_without_playing() {
        let mut state = with_results(vec![track_row(11)], 1);
        let (tag, request) = tagged(ch(&mut state, 'a'));
        assert_eq!(tag, Tag::Add { play: false });
        assert!(matches!(
            request,
            Request::QueueAdd {
                at: phonia_ipc::AddAt::End,
                ..
            }
        ));
        let (tag, request) = tagged(ch(&mut state, 'A'));
        assert_eq!(tag, Tag::Add { play: false });
        assert!(matches!(
            request,
            Request::QueueAdd {
                at: phonia_ipc::AddAt::Next,
                ..
            }
        ));
    }

    #[test]
    fn a_adds_an_album_or_a_playlist_whole_and_enter_opens_it_instead() {
        let mut state = with_results(vec![track_row(11)], 1);
        ch(&mut state, ']');
        let (tag, request) = tagged(ch(&mut state, 'a'));
        assert_eq!(tag, Tag::Add { play: false });
        assert_eq!(
            request,
            Request::QueueAddFrom {
                from: phonia_ipc::CatalogRef::Album {
                    id: "33723912".into()
                },
                at: phonia_ipc::AddAt::End
            }
        );
        ch(&mut state, ']');
        ch(&mut state, ']');
        let (_, request) = tagged(ch(&mut state, 'A'));
        assert_eq!(
            request,
            Request::QueueAddFrom {
                from: phonia_ipc::CatalogRef::Playlist {
                    id: "5545fb2d-fd50".into()
                },
                at: phonia_ipc::AddAt::Next
            }
        );

        // Enter, on the album, opens it instead of adding it.
        ch(&mut state, '[');
        ch(&mut state, '[');
        let (tag, request) = tagged(press(&mut state, KeyCode::Enter));
        assert_eq!(tag, Tag::View { serial: 0 });
        assert_eq!(
            request,
            Request::Tracks {
                from: phonia_ipc::CatalogRef::Album {
                    id: "33723912".into()
                },
                offset: 0,
                limit: Some(crate::search::PAGE_SIZE)
            }
        );
        assert_eq!(state.search_views.top_serial(), Some(0));
    }

    #[test]
    fn a_and_capital_a_on_an_artist_add_its_top_tracks_and_enter_opens_its_page() {
        let mut state = with_results(vec![track_row(11)], 1);
        ch(&mut state, ']');
        ch(&mut state, ']');
        let (tag, request) = tagged(ch(&mut state, 'a'));
        assert_eq!(tag, Tag::Add { play: false });
        assert_eq!(
            request,
            Request::QueueAddFrom {
                from: phonia_ipc::CatalogRef::ArtistTopTracks { id: "780".into() },
                at: phonia_ipc::AddAt::End
            }
        );

        let (tag, request) = tagged(press(&mut state, KeyCode::Enter));
        assert_eq!(tag, Tag::View { serial: 0 });
        assert_eq!(
            request,
            Request::Artist {
                id: "780".into(),
                limit: None
            }
        );
        let Some(browse::View::Artist(view)) = state.search_views.top() else {
            panic!("no artist view")
        };
        assert_eq!(view.title(), "Korn");
    }

    #[test]
    fn a_track_that_cannot_be_streamed_is_not_added_and_says_why() {
        let mut blocked = track_row(11);
        blocked.streamable = false;
        let mut state = with_results(vec![blocked], 1);
        let effects = press(&mut state, KeyCode::Enter);
        assert!(effects.commands.is_empty());
        assert!(state.last_error.as_deref().unwrap().contains("Song 11"));
    }

    #[test]
    fn acting_on_a_result_without_a_connection_or_the_capability_explains_it() {
        // Not connected.
        let mut state = with_results(vec![track_row(11)], 1);
        disconnected(&mut state, Duration::from_secs(1));
        let effects = ch(&mut state, 'a');
        assert!(effects.commands.is_empty());
        assert!(
            state
                .last_error
                .as_deref()
                .unwrap()
                .contains("not connected")
        );

        // A daemon that can add tracks but not albums.
        let mut state = with_results(vec![track_row(11)], 1);
        update(&mut state, connected_msg(&["volume"]));
        assert!(
            !ch(&mut state, 'a').commands.is_empty(),
            "a track needs no catalog"
        );
        ch(&mut state, ']');
        let effects = ch(&mut state, 'a');
        assert!(effects.commands.is_empty());
        assert!(
            state
                .last_error
                .as_deref()
                .unwrap()
                .contains("albums or playlists")
        );
    }

    #[test]
    fn the_result_keys_do_nothing_unless_the_results_are_in_front_of_you() {
        // Typing: `a` is a letter.
        let mut state = with_results(vec![track_row(11)], 1);
        ch(&mut state, '/');
        assert!(ch(&mut state, 'a').commands.is_empty());
        assert!(state.search.input.text().ends_with('a'));
        press(&mut state, KeyCode::Esc);
        // On the sidebar or in another section.
        ch(&mut state, 'h');
        assert_eq!(ch(&mut state, 'a'), Effects::default());
        ch(&mut state, '1');
        ch(&mut state, 'l');
        assert_eq!(ch(&mut state, 'A'), Effects::default());
        // No results at all.
        let mut idle = State::default();
        update(&mut idle, connected_msg(&["catalog"]));
        ch(&mut idle, '2');
        ch(&mut idle, 'l');
        assert_eq!(ch(&mut idle, 'a'), Effects::default());
    }

    #[test]
    fn the_answer_to_an_add_tells_how_many_and_plays_the_first_when_asked() {
        let added = |n: u64, rejected: Vec<phonia_ipc::Rejected>| {
            Ok(Payload::Added {
                ids: (1..=n).map(phonia_ipc::ItemId).collect(),
                rejected,
                unresolved: vec![],
            })
        };
        let mut state = State::default();
        update(&mut state, connected_msg(&[]));

        let effects = update(
            &mut state,
            Msg::Response {
                tag: Tag::Add { play: true },
                result: added(12, vec![]),
            },
        );
        assert_eq!(state.notice.as_deref(), Some("Added 12 tracks"));
        assert_eq!(
            effects.commands,
            vec![Cmd::Send(Request::Play {
                item: Some(phonia_ipc::ItemId(1))
            })]
        );

        let effects = update(
            &mut state,
            Msg::Response {
                tag: Tag::Add { play: false },
                result: added(1, vec![]),
            },
        );
        assert_eq!(state.notice.as_deref(), Some("Added 1 track"));
        assert!(effects.commands.is_empty(), "not asked to play");

        update(
            &mut state,
            Msg::Response {
                tag: Tag::Add { play: false },
                result: added(
                    3,
                    vec![phonia_ipc::Rejected {
                        source: "tidal:9".into(),
                        reason: "Song 9 is listed by TIDAL but cannot be streamed here".into(),
                    }],
                ),
            },
        );
        assert!(
            state
                .notice
                .as_deref()
                .unwrap()
                .starts_with("Added 3 tracks (not added: Song 9")
        );

        let effects = update(
            &mut state,
            Msg::Response {
                tag: Tag::Add { play: true },
                result: added(0, vec![]),
            },
        );
        assert_eq!(state.notice.as_deref(), Some("There was nothing to add"));
        assert!(effects.commands.is_empty(), "nothing to play");
    }

    #[test]
    fn a_refused_add_becomes_an_error_and_a_notice_is_read_once() {
        let mut state = State::default();
        update(&mut state, connected_msg(&[]));
        update(
            &mut state,
            Msg::Response {
                tag: Tag::Add { play: false },
                result: Err("TIDAL has no such item".into()),
            },
        );
        assert_eq!(state.last_error.as_deref(), Some("TIDAL has no such item"));

        state.notice = Some("Added 2 tracks".into());
        let effects = ch(&mut state, 'x');
        assert!(state.notice.is_none(), "the next key takes it away");
        assert!(effects.redraw, "and the screen shows that");
    }

    fn tracks_payload(from: u64, count: u64, total: u64) -> Payload {
        Payload::SearchResults {
            query: "korn".into(),
            tracks: page_of((from..from + count).map(track_row).collect(), total, from),
            albums: None,
            artists: None,
            playlists: None,
        }
    }

    fn more_request(effects: Effects) -> (Tag, Request) {
        search_request(effects)
    }

    #[test]
    fn coming_near_the_end_of_a_list_that_has_more_asks_for_the_next_page_once() {
        let mut state = with_results((0..50).map(track_row).collect(), 123);
        // Far from the end: nothing to ask.
        for _ in 0..30 {
            assert!(ch(&mut state, 'j').commands.is_empty());
        }
        // Within ten of the end of the fifty loaded.
        let mut asked = None;
        for _ in 0..12 {
            let effects = ch(&mut state, 'j');
            if !effects.commands.is_empty() {
                assert!(asked.is_none(), "asked twice");
                asked = Some(more_request(effects));
            }
        }
        let (tag, request) = asked.expect("the next page was asked for");
        assert!(matches!(
            tag,
            Tag::SearchMore {
                tab: Tab::Tracks,
                ..
            }
        ));
        assert_eq!(
            request,
            Request::Search {
                query: "korn".into(),
                kinds: vec![phonia_ipc::CatalogKind::Tracks],
                offset: 50,
                limit: Some(50)
            }
        );
        assert!(state.search.tracks.loading);
    }

    #[test]
    fn a_page_that_comes_is_added_to_the_list_and_the_next_is_asked_for_in_its_turn() {
        let mut state = with_results((0..50).map(track_row).collect(), 123);
        let (tag, _) = more_request(ch(&mut state, 'G'));
        update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(tracks_payload(50, 50, 123)),
            },
        );
        assert_eq!(state.search.tracks.items.len(), 100);
        assert!(!state.search.tracks.loading);
        assert_eq!(state.search.tracks.items[50].id, "50");
        assert_eq!(state.search.tracks.total, 123);

        let (_, request) = more_request(ch(&mut state, 'G'));
        assert!(matches!(request, Request::Search { offset: 100, .. }));
    }

    #[test]
    fn nothing_more_is_asked_once_everything_is_loaded() {
        let mut state = with_results((0..5).map(track_row).collect(), 5);
        ch(&mut state, 'G');
        assert!(ch(&mut state, 'k').commands.is_empty());
        assert!(!state.search.tracks.loading);
    }

    #[test]
    fn a_late_page_of_an_old_search_is_dropped() {
        let mut state = with_results((0..50).map(track_row).collect(), 123);
        let (old, _) = more_request(ch(&mut state, 'G'));
        // A new search starts before the page comes.
        ch(&mut state, '/');
        typed(&mut state, "!");
        press(&mut state, KeyCode::Enter);
        let effects = update(
            &mut state,
            Msg::Response {
                tag: old,
                result: Ok(tracks_payload(50, 50, 123)),
            },
        );
        assert_eq!(effects, Effects::default());
        assert!(state.search.tracks.items.is_empty());
    }

    #[test]
    fn a_page_that_fails_can_be_asked_for_again_and_says_why() {
        let mut state = with_results((0..50).map(track_row).collect(), 123);
        let (tag, _) = more_request(ch(&mut state, 'G'));
        update(
            &mut state,
            Msg::Response {
                tag,
                result: Err("TIDAL says there were too many requests".into()),
            },
        );
        assert!(!state.search.tracks.loading);
        assert!(
            state
                .last_error
                .as_deref()
                .unwrap()
                .contains("could not load more")
        );
        assert!(
            !ch(&mut state, 'j').commands.is_empty(),
            "it can be asked for again"
        );
    }

    #[test]
    fn an_empty_page_ends_the_list_even_if_the_total_said_more() {
        let mut state = with_results((0..50).map(track_row).collect(), 123);
        let (tag, _) = more_request(ch(&mut state, 'G'));
        update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(tracks_payload(50, 0, 123)),
            },
        );
        assert_eq!(state.search.tracks.total, 50);
        assert!(
            ch(&mut state, 'j').commands.is_empty(),
            "no asking for ever"
        );
    }

    // --- Opening an album or a playlist, and acting inside it -------------------------------

    fn open_album(state: &mut State) -> u64 {
        // From the search results, "Issues" is the only album: open it.
        let (tag, _) = tagged(press(state, KeyCode::Enter));
        let Tag::View { serial } = tag else {
            panic!("not a view tag")
        };
        serial
    }

    /// Three tracks of "Issues", the second not streamable, opened and loaded.
    fn in_an_open_album(streamable_second: bool) -> (State, u64) {
        let mut state = with_results(vec![track_row(11)], 1);
        ch(&mut state, ']'); // Albums
        let serial = open_album(&mut state);
        let mut second = track_row(2);
        second.streamable = streamable_second;
        second.title = "Trash".into();
        let mut tracks = vec![track_row(1), second, track_row(3)];
        tracks[0].title = "Dead".into();
        tracks[2].title = "4U".into();
        for (index, track) in tracks.iter_mut().enumerate() {
            track.track_number = Some(index as u32 + 1);
        }
        update(
            &mut state,
            Msg::Response {
                tag: Tag::View { serial },
                result: Ok(Payload::Tracks {
                    from: phonia_ipc::CatalogRef::Album {
                        id: "33723912".into(),
                    },
                    page: page_of(tracks, 3, 0).unwrap(),
                }),
            },
        );
        (state, serial)
    }

    #[test]
    fn enter_on_an_album_opens_it_with_its_header_known_at_once() {
        use crate::browse::{Header, View};
        let mut state = with_results(vec![track_row(11)], 1);
        ch(&mut state, ']');
        let serial = open_album(&mut state);
        assert_eq!(serial, 0);
        let Some(View::TrackList(view)) = state.search_views.top() else {
            panic!("no view open")
        };
        assert_eq!(view.title(), "Issues");
        assert!(matches!(view.header(), Header::Album(_)));
        assert_eq!(view.phase, crate::browse::Phase::Loading);
    }

    #[test]
    fn the_tracks_of_the_album_fill_the_view_once_they_come() {
        let (state, _) = in_an_open_album(true);
        let Some(crate::browse::View::TrackList(view)) = state.search_views.top() else {
            panic!("no view")
        };
        assert_eq!(view.phase, crate::browse::Phase::Done);
        assert_eq!(view.tracks.items.len(), 3);
        assert_eq!(view.tracks.items[0].title, "Dead");
    }

    #[test]
    fn enter_on_a_track_in_the_album_queues_the_rest_and_plays_the_right_one() {
        let (mut state, _) = in_an_open_album(false); // the 2nd track ("Trash") cannot be streamed
        ch(&mut state, 'j');
        ch(&mut state, 'j'); // the cursor is on the 3rd loaded track ("4U")
        let (tag, request) = tagged(press(&mut state, KeyCode::Enter));
        assert_eq!(
            request,
            Request::QueueAddFrom {
                from: phonia_ipc::CatalogRef::Album {
                    id: "33723912".into()
                },
                at: phonia_ipc::AddAt::Next
            }
        );
        // Of the two tracks before this one, only the 1st ("Dead") could stream: play_at is 1,
        // not 2, since the unstreamable one never reaches the queue.
        assert_eq!(tag, Tag::AddFrom { play_at: Some(1) });

        let added = Payload::Added {
            ids: vec![phonia_ipc::ItemId(10), phonia_ipc::ItemId(11)],
            rejected: vec![],
            unresolved: vec![],
        };
        let effects = update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(added),
            },
        );
        assert_eq!(
            effects.commands,
            vec![Cmd::Send(Request::Play {
                item: Some(phonia_ipc::ItemId(11))
            })],
            "the id at position 1 (0-based), not position 2"
        );
    }

    #[test]
    fn enter_on_a_track_that_cannot_be_streamed_is_refused() {
        let (mut state, _) = in_an_open_album(false);
        ch(&mut state, 'j'); // the unstreamable "Trash"
        let effects = press(&mut state, KeyCode::Enter);
        assert!(effects.commands.is_empty());
        assert!(state.last_error.as_deref().unwrap().contains("Trash"));
    }

    #[test]
    fn a_and_capital_a_add_just_the_track_under_the_cursor() {
        let (mut state, _) = in_an_open_album(true);
        ch(&mut state, 'j');
        let (tag, request) = tagged(ch(&mut state, 'a'));
        assert_eq!(tag, Tag::Add { play: false });
        assert_eq!(
            request,
            Request::QueueAdd {
                tracks: vec![phonia_ipc::NewTrack {
                    source: "tidal:2".into()
                }],
                at: phonia_ipc::AddAt::End
            }
        );
        let (_, request) = tagged(ch(&mut state, 'A'));
        assert!(matches!(
            request,
            Request::QueueAdd {
                at: phonia_ipc::AddAt::Next,
                ..
            }
        ));
    }

    #[test]
    fn h_backspace_and_starting_a_new_search_all_close_the_album() {
        let (mut state, _) = in_an_open_album(true);
        assert!(!state.search_views.is_empty());
        ch(&mut state, 'h');
        assert!(state.search_views.is_empty(), "h closes it");
        assert_eq!(
            state.focus,
            Focus::Main,
            "and stays on the results, not the sidebar"
        );

        let (mut state, _) = in_an_open_album(true);
        press(&mut state, KeyCode::Backspace);
        assert!(state.search_views.is_empty());

        let (mut state, _) = in_an_open_album(true);
        ch(&mut state, '/');
        assert!(
            state.search_views.is_empty(),
            "starting a new search leaves the album open no more"
        );
    }

    #[test]
    fn h_on_the_results_themselves_still_moves_to_the_sidebar() {
        let mut state = with_results(vec![track_row(11)], 1);
        ch(&mut state, 'h');
        assert_eq!(state.focus, Focus::Sidebar);
    }

    #[test]
    fn moving_and_the_brackets_act_on_the_album_not_on_the_search_tabs() {
        let (mut state, _) = in_an_open_album(true);
        let tab_before = state.search.tab;
        assert_eq!(
            ch(&mut state, ']'),
            Effects::default(),
            "no tab switch while browsing"
        );
        assert_eq!(state.search.tab, tab_before);
        let effects = ch(&mut state, 'G');
        let Some(crate::browse::View::TrackList(view)) = state.search_views.top() else {
            panic!("no view")
        };
        assert_eq!(view.tracks.cursor.selected(), 2);
        assert!(
            effects.redraw,
            "moving within an opened view must repaint it, not just move its cursor unseen"
        );
    }

    #[test]
    fn a_long_album_loads_more_tracks_as_the_cursor_nears_the_end() {
        let mut state = with_results(vec![track_row(11)], 1);
        ch(&mut state, ']');
        let serial = open_album(&mut state);
        let tracks: Vec<_> = (0..50).map(track_row).collect();
        update(
            &mut state,
            Msg::Response {
                tag: Tag::View { serial },
                result: Ok(Payload::Tracks {
                    from: phonia_ipc::CatalogRef::Album {
                        id: "33723912".into(),
                    },
                    page: page_of(tracks, 120, 0).unwrap(),
                }),
            },
        );
        let (more_tag, request) = tagged(ch(&mut state, 'G'));
        assert_eq!(more_tag, Tag::View { serial });
        assert_eq!(
            request,
            Request::Tracks {
                from: phonia_ipc::CatalogRef::Album {
                    id: "33723912".into()
                },
                offset: 50,
                limit: Some(crate::search::PAGE_SIZE)
            }
        );
        let more: Vec<_> = (50..100).map(track_row).collect();
        update(
            &mut state,
            Msg::Response {
                tag: more_tag,
                result: Ok(Payload::Tracks {
                    from: phonia_ipc::CatalogRef::Album {
                        id: "33723912".into(),
                    },
                    page: page_of(more, 120, 50).unwrap(),
                }),
            },
        );
        let Some(crate::browse::View::TrackList(view)) = state.search_views.top() else {
            panic!("no view")
        };
        assert_eq!(view.tracks.items.len(), 100);
    }

    #[test]
    fn losing_the_connection_fails_an_album_still_loading() {
        let mut state = with_results(vec![track_row(11)], 1);
        ch(&mut state, ']');
        open_album(&mut state);
        disconnected(&mut state, Duration::from_secs(1));
        let Some(crate::browse::View::TrackList(view)) = state.search_views.top() else {
            panic!("no view")
        };
        assert!(matches!(view.phase, crate::browse::Phase::Failed(_)));
    }

    #[test]
    fn a_response_to_a_view_that_was_already_closed_is_dropped() {
        let (mut state, serial) = in_an_open_album(true);
        ch(&mut state, 'h');
        let effects = update(
            &mut state,
            Msg::Response {
                tag: Tag::View { serial },
                result: Ok(Payload::Tracks {
                    from: phonia_ipc::CatalogRef::Album {
                        id: "33723912".into(),
                    },
                    page: page_of(vec![track_row(9)], 1, 0).unwrap(),
                }),
            },
        );
        assert_eq!(effects, Effects::default());
        assert!(state.search_views.is_empty());
    }

    // --- Opening an artist, and acting inside it ---------------------------------------------

    fn open_artist(state: &mut State) -> u64 {
        ch(state, ']');
        ch(state, ']'); // the Artists tab: "Korn" is the only result
        let (tag, request) = tagged(press(state, KeyCode::Enter));
        let Tag::View { serial } = tag else {
            panic!("not a view tag")
        };
        assert_eq!(
            request,
            Request::Artist {
                id: "780".into(),
                limit: None
            }
        );
        serial
    }

    /// Korn's page, loaded: three top tracks (the second not streamable), two albums, one single.
    fn in_an_open_artist() -> (State, u64) {
        let mut state = with_results(vec![track_row(11)], 1);
        let serial = open_artist(&mut state);
        let mut top = vec![track_row(1), track_row(2), track_row(3)];
        top[0].title = "Freak On a Leash".into();
        top[1].title = "Blind".into();
        top[1].streamable = false;
        top[2].title = "Coming Undone".into();
        let album = |id: &str, title: &str| phonia_ipc::AlbumSummary {
            id: id.into(),
            title: title.into(),
            version: None,
            artists: vec![],
            release_date: None,
            track_count: None,
            duration_ms: None,
            explicit: false,
            quality: None,
            kind: None,
            copyright: None,
            cover: None,
        };
        update(
            &mut state,
            Msg::Response {
                tag: Tag::View { serial },
                result: Ok(Payload::Artist {
                    artist: phonia_ipc::ArtistSummary {
                        id: "780".into(),
                        name: "Korn".into(),
                        picture: None,
                    },
                    bio: Some("A nu metal band.".into()),
                    top_tracks: page_of(top, 300, 0).unwrap(),
                    albums: page_of(
                        vec![album("9", "Issues"), album("10", "Untouchables")],
                        35,
                        0,
                    )
                    .unwrap(),
                    singles: page_of(vec![album("11", "Freak")], 30, 0).unwrap(),
                }),
            },
        );
        (state, serial)
    }

    #[test]
    fn enter_on_an_artist_opens_its_page_with_the_name_known_at_once() {
        let mut state = with_results(vec![track_row(11)], 1);
        let serial = open_artist(&mut state);
        assert_eq!(serial, 0);
        let Some(browse::View::Artist(view)) = state.search_views.top() else {
            panic!("no artist view")
        };
        assert_eq!(view.title(), "Korn");
        assert_eq!(view.phase, browse::Phase::Loading);
    }

    #[test]
    fn the_answer_fills_the_bio_and_the_three_lists() {
        let (state, _) = in_an_open_artist();
        let Some(browse::View::Artist(view)) = state.search_views.top() else {
            panic!("no artist view")
        };
        assert_eq!(view.phase, browse::Phase::Done);
        assert_eq!(view.bio.as_deref(), Some("A nu metal band."));
        assert_eq!(view.top_tracks.items.len(), 3);
        assert_eq!(view.albums.items.len(), 2);
        assert_eq!(view.singles.items[0].title, "Freak");
    }

    #[test]
    fn an_artist_with_no_bio_still_loads() {
        let mut state = with_results(vec![track_row(11)], 1);
        let serial = open_artist(&mut state);
        update(
            &mut state,
            Msg::Response {
                tag: Tag::View { serial },
                result: Ok(Payload::Artist {
                    artist: phonia_ipc::ArtistSummary {
                        id: "780".into(),
                        name: "Korn".into(),
                        picture: None,
                    },
                    bio: None,
                    top_tracks: page_of(vec![], 0, 0).unwrap(),
                    albums: page_of(vec![], 0, 0).unwrap(),
                    singles: page_of(vec![], 0, 0).unwrap(),
                }),
            },
        );
        let Some(browse::View::Artist(view)) = state.search_views.top() else {
            panic!("no artist view")
        };
        assert_eq!(view.phase, browse::Phase::Done);
        assert_eq!(view.bio, None);
    }

    #[test]
    fn the_brackets_switch_the_artists_own_tabs_while_its_page_is_open() {
        let (mut state, _) = in_an_open_artist();
        let Some(browse::View::Artist(view)) = state.search_views.top() else {
            panic!("no artist view")
        };
        assert_eq!(view.tab, browse::ArtistTab::TopTracks);
        assert!(ch(&mut state, ']').redraw);
        let Some(browse::View::Artist(view)) = state.search_views.top() else {
            panic!("no artist view")
        };
        assert_eq!(view.tab, browse::ArtistTab::Albums);
        ch(&mut state, ']');
        let Some(browse::View::Artist(view)) = state.search_views.top() else {
            panic!("no artist view")
        };
        assert_eq!(view.tab, browse::ArtistTab::Singles);
        assert!(!ch(&mut state, ']').redraw, "stops at the last");
        ch(&mut state, '[');
        let Some(browse::View::Artist(view)) = state.search_views.top() else {
            panic!("no artist view")
        };
        assert_eq!(view.tab, browse::ArtistTab::Albums);
        // The search's own tabs, underneath, do not move while the artist's page is open:
        // `open_artist` itself pressed `]` twice to reach the Artists tab before opening one, so
        // that is where the search is left, and stepping back here only moves it one more.
        assert_eq!(state.search.tab, crate::search::Tab::Artists);
        ch(&mut state, 'h');
        ch(&mut state, '[');
        assert_eq!(state.search.tab, crate::search::Tab::Albums);
    }

    #[test]
    fn enter_on_a_top_track_queues_the_rest_and_plays_the_right_one() {
        let (mut state, _) = in_an_open_artist();
        ch(&mut state, 'j'); // "Blind", not streamable
        ch(&mut state, 'j'); // "Coming Undone"
        let (tag, request) = tagged(press(&mut state, KeyCode::Enter));
        assert_eq!(
            request,
            Request::QueueAddFrom {
                from: phonia_ipc::CatalogRef::ArtistTopTracks { id: "780".into() },
                at: phonia_ipc::AddAt::Next
            }
        );
        // "Blind" (not streamable) never reaches the queue, so "Coming Undone" is at position 1.
        assert_eq!(tag, Tag::AddFrom { play_at: Some(1) });
    }

    #[test]
    fn a_and_capital_a_add_just_the_top_track_under_the_cursor() {
        let (mut state, _) = in_an_open_artist();
        let (tag, request) = tagged(ch(&mut state, 'a'));
        assert_eq!(tag, Tag::Add { play: false });
        assert_eq!(
            request,
            Request::QueueAdd {
                tracks: vec![phonia_ipc::NewTrack {
                    source: "tidal:1".into()
                }],
                at: phonia_ipc::AddAt::End
            }
        );
    }

    #[test]
    fn enter_on_an_album_from_the_artists_page_opens_it_and_a_adds_it_whole() {
        let (mut state, _) = in_an_open_artist();
        ch(&mut state, ']'); // Albums
        let (tag, request) = tagged(press(&mut state, KeyCode::Enter));
        let Tag::View { serial: opened } = tag else {
            panic!("not a view tag")
        };
        assert_eq!(
            request,
            Request::Tracks {
                from: phonia_ipc::CatalogRef::Album { id: "9".into() },
                offset: 0,
                limit: Some(crate::search::PAGE_SIZE)
            }
        );
        let Some(browse::View::TrackList(view)) = state.search_views.top() else {
            panic!("not a track list")
        };
        assert_eq!(view.title(), "Issues");
        // The breadcrumb has both: the artist stayed on the stack underneath.
        assert_eq!(state.search_views.titles(), ["Korn", "Issues"]);
        assert_ne!(opened, 0, "a new view, not the artist's own");
        // Back to the artist's page.
        ch(&mut state, 'h');
        assert_eq!(state.search_views.titles(), ["Korn"]);

        ch(&mut state, ']');
        ch(&mut state, ']');
        let (_, request) = tagged(ch(&mut state, 'a'));
        assert_eq!(
            request,
            Request::QueueAddFrom {
                from: phonia_ipc::CatalogRef::Album { id: "11".into() },
                at: phonia_ipc::AddAt::End
            }
        );
    }

    #[test]
    fn each_of_the_three_tabs_loads_more_of_its_own_list() {
        // `G` itself lands the cursor within ten rows of a three-item list's end, so it is `G`,
        // not a `j` after it, that asks for the next page.
        let (mut state, serial) = in_an_open_artist();
        let (tag, request) = tagged(ch(&mut state, 'G'));
        assert_eq!(tag, Tag::View { serial });
        assert_eq!(
            request,
            Request::Tracks {
                from: phonia_ipc::CatalogRef::ArtistTopTracks { id: "780".into() },
                offset: 3,
                limit: Some(crate::search::PAGE_SIZE)
            }
        );

        ch(&mut state, ']'); // Albums
        let (_, request) = tagged(ch(&mut state, 'G'));
        assert_eq!(
            request,
            Request::Albums {
                from: phonia_ipc::AlbumListRef::ArtistAlbums { id: "780".into() },
                offset: 2,
                limit: Some(crate::search::PAGE_SIZE)
            }
        );

        ch(&mut state, ']'); // Singles
        let (_, request) = tagged(ch(&mut state, 'G'));
        assert_eq!(
            request,
            Request::Albums {
                from: phonia_ipc::AlbumListRef::ArtistSingles { id: "780".into() },
                offset: 1,
                limit: Some(crate::search::PAGE_SIZE)
            }
        );
    }

    #[test]
    fn a_page_that_comes_is_added_to_the_right_list() {
        let (mut state, serial) = in_an_open_artist();
        ch(&mut state, ']'); // Albums
        let (tag, _) = tagged(ch(&mut state, 'G'));
        update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(Payload::Albums {
                    from: phonia_ipc::AlbumListRef::ArtistAlbums { id: "780".into() },
                    page: page_of(
                        vec![phonia_ipc::AlbumSummary {
                            id: "12".into(),
                            title: "Follow the Leader".into(),
                            version: None,
                            artists: vec![],
                            release_date: None,
                            track_count: None,
                            duration_ms: None,
                            explicit: false,
                            quality: None,
                            kind: None,
                            copyright: None,
                            cover: None,
                        }],
                        35,
                        2,
                    )
                    .unwrap(),
                }),
            },
        );
        let Some(browse::View::Artist(view)) = state.search_views.top() else {
            panic!("no artist view")
        };
        assert_eq!(view.albums.items.len(), 3);
        assert_eq!(
            tag,
            Tag::View { serial },
            "the same tag opened and paged the page"
        );
    }

    #[test]
    fn losing_the_connection_fails_an_artist_page_still_loading() {
        let mut state = with_results(vec![track_row(11)], 1);
        open_artist(&mut state);
        disconnected(&mut state, Duration::from_secs(1));
        let Some(browse::View::Artist(view)) = state.search_views.top() else {
            panic!("no artist view")
        };
        assert!(matches!(view.phase, browse::Phase::Failed(_)));
    }

    // --- The library ---------------------------------------------------------------------------

    fn library_track(id: &str, title: &str) -> phonia_ipc::TrackSummary {
        let mut track = track_row(0);
        track.id = id.into();
        track.title = title.into();
        track
    }

    fn library_album(id: &str, title: &str) -> phonia_ipc::AlbumSummary {
        phonia_ipc::AlbumSummary {
            id: id.into(),
            title: title.into(),
            version: None,
            artists: vec![],
            release_date: None,
            track_count: None,
            duration_ms: None,
            explicit: false,
            quality: None,
            kind: None,
            copyright: None,
            cover: None,
        }
    }

    fn library_playlist(id: &str, title: &str) -> phonia_ipc::PlaylistSummary {
        phonia_ipc::PlaylistSummary {
            id: id.into(),
            title: title.into(),
            creator: None,
            description: None,
            track_count: None,
            duration_ms: None,
            cover: None,
        }
    }

    /// Connected, with the catalog, having just moved to the Library section: the request that
    /// loads it, already sent (nothing has to be typed, unlike a search).
    fn opening_library() -> (State, Tag) {
        let mut state = State::default();
        update(&mut state, connected_msg(&["catalog"]));
        let effects = ch(&mut state, '3');
        let (tag, request) = search_request(effects);
        assert_eq!(
            request,
            Request::Library {
                limit: Some(crate::library::PAGE_SIZE)
            }
        );
        (state, tag)
    }

    /// The library loaded: two favorite tracks (the second not streamable), one favorite album,
    /// two playlists. The focus is on the list, with the daemon able to browse.
    fn with_library() -> State {
        let (mut state, tag) = opening_library();
        let mut tracks = vec![
            library_track("1", "Freak On a Leash"),
            library_track("2", "Blind"),
        ];
        tracks[1].streamable = false;
        update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(Payload::Library {
                    favorite_tracks: phonia_ipc::Page {
                        items: tracks,
                        total: 2,
                        offset: 0,
                    },
                    favorite_albums: phonia_ipc::Page {
                        items: vec![library_album("9", "Issues")],
                        total: 1,
                        offset: 0,
                    },
                    my_playlists: phonia_ipc::Page {
                        items: vec![
                            library_playlist("p-1", "Road trip"),
                            library_playlist("p-2", "Focus"),
                        ],
                        total: 2,
                        offset: 0,
                    },
                }),
            },
        );
        ch(&mut state, 'l');
        state
    }

    #[test]
    fn moving_to_the_library_asks_for_it_once_and_the_answer_fills_its_three_lists() {
        let (mut state, tag) = opening_library();
        assert_eq!(tag, Tag::Library { generation: 0 });
        assert_eq!(
            state.library.as_ref().unwrap().phase,
            browse::Phase::Loading
        );
        // Selecting it again (it is already the section shown) does not ask a second time.
        assert!(ch(&mut state, '3').commands.is_empty());

        let state = with_library();
        let library = state.library.as_ref().unwrap();
        assert_eq!(library.phase, browse::Phase::Done);
        assert_eq!(library.favorite_tracks.items.len(), 2);
        assert_eq!(library.favorite_albums.items[0].title, "Issues");
        assert_eq!(library.playlists.items.len(), 2);
    }

    #[test]
    fn without_a_connection_or_the_catalog_the_library_is_not_asked_for() {
        let mut state = State::default();
        assert!(ch(&mut state, '3').commands.is_empty());
        assert!(state.library.is_none(), "no connection yet");

        let mut state = State::default();
        update(&mut state, connected_msg(&[]));
        assert!(ch(&mut state, '3').commands.is_empty());
        assert!(state.library.is_none(), "no catalog on this daemon");

        // The moment a connection with the catalog exists, it is asked for.
        update(&mut state, connected_msg(&["catalog"]));
        assert!(state.library.is_some());
    }

    #[test]
    fn losing_the_connection_fails_the_library_if_it_was_still_loading() {
        let (mut state, _) = opening_library();
        disconnected(&mut state, Duration::from_secs(1));
        assert!(matches!(
            state.library.as_ref().unwrap().phase,
            browse::Phase::Failed(_)
        ));

        // One already loaded is untouched.
        let mut state = with_library();
        disconnected(&mut state, Duration::from_secs(1));
        assert_eq!(state.library.as_ref().unwrap().phase, browse::Phase::Done);
    }

    #[test]
    fn the_brackets_switch_the_librarys_own_three_tabs() {
        let mut state = with_library();
        assert_eq!(
            state.library.as_ref().unwrap().tab,
            LibraryTab::FavoriteTracks
        );
        ch(&mut state, ']');
        assert_eq!(
            state.library.as_ref().unwrap().tab,
            LibraryTab::FavoriteAlbums
        );
        ch(&mut state, ']');
        assert_eq!(state.library.as_ref().unwrap().tab, LibraryTab::Playlists);
        assert!(!ch(&mut state, ']').redraw, "already at the last tab");
        ch(&mut state, '[');
        assert_eq!(
            state.library.as_ref().unwrap().tab,
            LibraryTab::FavoriteAlbums
        );
    }

    #[test]
    fn moving_in_the_library_walks_the_current_tabs_list() {
        let mut state = with_library();
        ch(&mut state, 'j');
        assert_eq!(
            state
                .library
                .as_ref()
                .unwrap()
                .favorite_tracks
                .cursor
                .selected(),
            1
        );
        ch(&mut state, ']'); // favorite albums: one row, already there
        assert_eq!(
            state
                .library
                .as_ref()
                .unwrap()
                .favorite_albums
                .cursor
                .selected(),
            0
        );
    }

    #[test]
    fn enter_on_a_favorite_track_plays_just_that_track_not_the_whole_list() {
        // The first row, "Freak On a Leash", is streamable ("Blind" is not).
        let mut state = with_library();
        let (tag, request) = tagged(press(&mut state, KeyCode::Enter));
        assert_eq!(tag, Tag::Add { play: true });
        assert_eq!(
            request,
            Request::QueueAdd {
                tracks: vec![phonia_ipc::NewTrack {
                    source: "tidal:1".into()
                }],
                at: phonia_ipc::AddAt::Next,
            }
        );
    }

    #[test]
    fn a_and_shift_a_on_a_favorite_track_add_just_it_to_the_end_or_after_what_plays() {
        let mut state = with_library();
        let (_, request) = tagged(ch(&mut state, 'a'));
        assert_eq!(
            request,
            Request::QueueAdd {
                tracks: vec![phonia_ipc::NewTrack {
                    source: "tidal:1".into()
                }],
                at: phonia_ipc::AddAt::End,
            }
        );
        let (_, request) = tagged(ch(&mut state, 'A'));
        assert!(matches!(
            request,
            Request::QueueAdd {
                at: phonia_ipc::AddAt::Next,
                ..
            }
        ));
    }

    #[test]
    fn a_favorite_track_that_cannot_stream_is_refused_before_it_reaches_the_daemon() {
        let mut state = with_library();
        ch(&mut state, 'j'); // "Blind", not streamable
        let effects = press(&mut state, KeyCode::Enter);
        assert!(effects.commands.is_empty());
        assert_eq!(
            state.last_error.as_deref(),
            Some("Blind is not available where you are")
        );
    }

    #[test]
    fn enter_on_a_favorite_album_opens_it_like_from_a_search_result() {
        let mut state = with_library();
        ch(&mut state, ']'); // favorite albums
        let (tag, request) = tagged(press(&mut state, KeyCode::Enter));
        let Tag::View { serial } = tag else {
            panic!("not a view tag")
        };
        assert_eq!(
            request,
            Request::Tracks {
                from: phonia_ipc::CatalogRef::Album { id: "9".into() },
                offset: 0,
                limit: Some(crate::search::PAGE_SIZE),
            }
        );
        assert_eq!(state.library_views.top_serial(), Some(serial));
        let Some(browse::View::TrackList(view)) = state.library_views.top() else {
            panic!("no view opened onto the library's own stack")
        };
        assert_eq!(view.header().title(), "Issues");
        assert!(
            state.search_views.is_empty(),
            "opened onto the library, not the search"
        );
    }

    #[test]
    fn a_on_a_favorite_album_adds_it_whole() {
        let mut state = with_library();
        ch(&mut state, ']');
        let (_, request) = tagged(ch(&mut state, 'a'));
        assert_eq!(
            request,
            Request::QueueAddFrom {
                from: phonia_ipc::CatalogRef::Album { id: "9".into() },
                at: phonia_ipc::AddAt::End,
            }
        );
    }

    #[test]
    fn enter_on_one_of_the_users_playlists_opens_it() {
        let mut state = with_library();
        ch(&mut state, ']');
        ch(&mut state, ']'); // playlists
        ch(&mut state, 'j'); // "Focus", the second one
        let (_, request) = tagged(press(&mut state, KeyCode::Enter));
        assert_eq!(
            request,
            Request::Tracks {
                from: phonia_ipc::CatalogRef::Playlist { id: "p-2".into() },
                offset: 0,
                limit: Some(crate::search::PAGE_SIZE),
            }
        );
        let Some(browse::View::TrackList(view)) = state.library_views.top() else {
            panic!("no view")
        };
        assert_eq!(view.header().title(), "Focus");
    }

    #[test]
    fn closing_an_opened_library_view_returns_to_the_librarys_own_lists() {
        let mut state = with_library();
        ch(&mut state, ']');
        press(&mut state, KeyCode::Enter);
        assert!(!state.library_views.is_empty());
        ch(&mut state, 'h');
        assert!(state.library_views.is_empty());
        assert_eq!(
            state.focus,
            Focus::Main,
            "stays on the library, not the sidebar"
        );
    }

    #[test]
    fn a_long_list_of_favorites_loads_more_as_the_cursor_nears_the_end() {
        let (mut state, tag) = opening_library();
        update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(Payload::Library {
                    favorite_tracks: phonia_ipc::Page {
                        items: (0..50)
                            .map(|n| library_track(&n.to_string(), "x"))
                            .collect(),
                        total: 120,
                        offset: 0,
                    },
                    favorite_albums: phonia_ipc::Page {
                        items: vec![],
                        total: 0,
                        offset: 0,
                    },
                    my_playlists: phonia_ipc::Page {
                        items: vec![],
                        total: 0,
                        offset: 0,
                    },
                }),
            },
        );
        ch(&mut state, 'l');
        // `G` puts the cursor at the last loaded row, within ten of the end of what is loaded:
        // close enough for that same move to ask for more.
        let effects = ch(&mut state, 'G');
        let (tag, request) = tagged(effects);
        assert_eq!(
            tag,
            Tag::LibraryMore {
                tab: LibraryTab::FavoriteTracks,
                generation: 0
            }
        );
        assert_eq!(
            request,
            Request::Tracks {
                from: phonia_ipc::CatalogRef::FavoriteTracks,
                offset: 50,
                limit: Some(crate::library::PAGE_SIZE),
            }
        );
    }

    #[test]
    fn the_library_and_the_search_keep_their_own_open_views_apart() {
        let mut library_state = with_library();
        ch(&mut library_state, ']');
        press(&mut library_state, KeyCode::Enter);
        assert!(!library_state.library_views.is_empty());
        assert!(library_state.search_views.is_empty());

        let mut search_state = with_results(vec![track_row(11)], 1);
        ch(&mut search_state, ']'); // Albums
        press(&mut search_state, KeyCode::Enter);
        assert!(!search_state.search_views.is_empty());
        assert!(search_state.library_views.is_empty());
    }
}
