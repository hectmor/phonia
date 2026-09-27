//! What the interface knows, what can happen to it, and how one becomes the other.

use crate::cursor::Cursor;
use crate::keymap::{self, Action, Key, Resolution};
use crossterm::event::KeyEvent;

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

/// Everything the interface remembers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// Set when the user asked to leave.
    pub quit: bool,
    pub focus: Focus,
    /// The selected row of the sidebar, which is also the section shown.
    pub sidebar: Cursor,
    pub help: bool,
    /// Keys pressed that begin a longer binding (the first `g` of `gg`).
    pub pending: Vec<Key>,
    /// The size of the terminal, columns and rows.
    pub size: (u16, u16),
}

impl State {
    pub fn section(&self) -> Section {
        Section::ALL[self.sidebar.selected().min(Section::ALL.len() - 1)]
    }

    /// How many rows half a page is: half the terminal's height, and at least one.
    fn half_page(&self) -> usize {
        usize::from(self.size.1 / 2).max(1)
    }
}

/// Something that happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg {
    Key(KeyEvent),
    /// The terminal has this size now (columns, rows). Also sent once when the interface starts.
    Resize(u16, u16),
    /// The interface woke up by itself.
    Tick,
}

/// Something the interface asks the outside to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    Quit,
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
        Msg::Tick => Effects::default(),
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

/// The help covers the screen and answers only to what closes it (and to `Ctrl-c`, which always
/// quits).
fn on_key_in_help(state: &mut State, key: Key) -> Effects {
    match keymap::resolve(&[], key) {
        Resolution::Action(Action::Quit) if key == Key::ctrl('c') => Effects::command(Cmd::Quit),
        Resolution::Action(Action::Quit | Action::ToggleHelp | Action::CloseHelp) => {
            state.help = false;
            Effects::redraw()
        }
        _ => Effects::default(),
    }
}

fn apply(state: &mut State, action: Action) -> Effects {
    let before = (state.focus, state.help, state.sidebar);
    match action {
        Action::Quit => return Effects::command(Cmd::Quit),
        Action::ToggleHelp => state.help = !state.help,
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
    }
    if (state.focus, state.help, state.sidebar) == before {
        Effects::default()
    } else {
        Effects::redraw()
    }
}

/// Moves the cursor of the panel that has the focus. The main panel lists nothing yet.
fn move_cursor(state: &mut State, action: Action) {
    if state.focus != Focus::Sidebar {
        return;
    }
    let len = Section::ALL.len();
    let page = state.half_page();
    match action {
        Action::Down => state.sidebar.down(len),
        Action::Up => state.sidebar.up(),
        Action::First => state.sidebar.first(),
        Action::Last => state.sidebar.last(len),
        Action::HalfPageDown => state.sidebar.page_down(len, page),
        Action::HalfPageUp => state.sidebar.page_up(page),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

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
        assert!(
            !ch(&mut state, 'j').redraw,
            "the keys behind the help are off"
        );
        assert_eq!(state.section(), Section::Queue);
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
}
