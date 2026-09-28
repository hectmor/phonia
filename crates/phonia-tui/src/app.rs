//! What the interface knows, what can happen to it, and how one becomes the other.

use crate::cursor::Cursor;
use crate::keymap::{self, Action, Key, Resolution};
use crate::search::SearchState;
use crossterm::event::KeyEvent;
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
    pub search: SearchState,
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

/// Applies one message to the state.
pub fn update(state: &mut State, msg: Msg) -> Effects {
    match msg {
        Msg::Key(key) => on_key(state, Key::from_event(key)),
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
            set_status(state, |status| status.state = new_state);
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
            ..
        } => {
            set_status(state, |status| {
                status.track = Some(Track {
                    item_id,
                    source,
                    title,
                    duration_ms,
                    quality,
                });
                status.spec = Some(spec);
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
            });
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

fn on_key(state: &mut State, key: Key) -> Effects {
    if state.help {
        return on_key_in_help(state, key);
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

fn apply(state: &mut State, action: Action) -> Effects {
    let before = (state.focus, state.help, state.sidebar, state.queue_cursor);
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
        Action::FocusSidebar => state.focus = Focus::Sidebar,
        Action::FocusMain => state.focus = Focus::Main,
        Action::Section(index) => state.sidebar.select(index, Section::ALL.len()),
        Action::Down
        | Action::Up
        | Action::First
        | Action::Last
        | Action::HalfPageDown
        | Action::HalfPageUp => move_cursor(state, action),
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
            } else {
                return edit_queue(state, action);
            }
        }
        Action::RemoveEntry | Action::MoveEntryDown | Action::MoveEntryUp | Action::ClearQueue => {
            return edit_queue(state, action);
        }
    }
    if (state.focus, state.help, state.sidebar, state.queue_cursor) == before {
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

/// Moves the cursor of the panel that has the focus. The main panel lists nothing yet.
fn move_cursor(state: &mut State, action: Action) {
    let page = state.half_page();
    let (len, cursor) = match (state.focus, state.section()) {
        (Focus::Sidebar, _) => (Section::ALL.len(), &mut state.sidebar),
        (Focus::Main, Section::Queue) => (queue_len(state), &mut state.queue_cursor),
        // The other sections list nothing yet.
        (Focus::Main, _) => return,
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
}
