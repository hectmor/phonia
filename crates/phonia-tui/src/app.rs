//! What the interface knows, what can happen to it, and how one becomes the other.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Everything the interface remembers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// Set when the user asked to leave.
    pub quit: bool,
}

/// Something that happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg {
    Key(KeyEvent),
    /// The terminal changed size: nothing to update, but the screen must be painted again.
    Resize,
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
pub fn update(_state: &mut State, msg: Msg) -> Effects {
    match msg {
        Msg::Key(key) => on_key(key),
        Msg::Resize => Effects::redraw(),
        Msg::Tick => Effects::default(),
    }
}

fn on_key(key: KeyEvent) -> Effects {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('q') if !control => Effects::command(Cmd::Quit),
        KeyCode::Char('c') if control => Effects::command(Cmd::Quit),
        _ => Effects::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Msg {
        Msg::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn q_and_control_c_quit() {
        let mut state = State::default();
        for msg in [
            key(KeyCode::Char('q'), KeyModifiers::NONE),
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            assert_eq!(update(&mut state, msg).commands, vec![Cmd::Quit]);
        }
    }

    #[test]
    fn other_keys_do_nothing_yet() {
        let mut state = State::default();
        for msg in [
            key(KeyCode::Char('x'), KeyModifiers::NONE),
            key(KeyCode::Char('q'), KeyModifiers::CONTROL),
            key(KeyCode::Enter, KeyModifiers::NONE),
        ] {
            assert_eq!(update(&mut state, msg), Effects::default());
        }
        assert_eq!(state, State::default());
    }

    #[test]
    fn a_resize_repaints_and_a_tick_does_not() {
        let mut state = State::default();
        assert!(update(&mut state, Msg::Resize).redraw);
        assert!(!update(&mut state, Msg::Tick).redraw);
    }
}
