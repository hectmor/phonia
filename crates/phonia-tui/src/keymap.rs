//! The keys, in one table.
//!
//! Every key the interface answers is a row of [`BINDINGS`]. The keys are looked up in it, and the
//! help screen is drawn from it, so the two cannot disagree: a key that works is in the help, and
//! a key in the help works.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    ToggleHelp,
    /// Closes the help, when it is open.
    CloseHelp,
    /// Connects to the daemon now, when it is not connected.
    Reconnect,
    Down,
    Up,
    First,
    Last,
    HalfPageDown,
    HalfPageUp,
    FocusNext,
    FocusSidebar,
    FocusMain,
    /// Goes to the nth section of the sidebar, counting from 0.
    Section(usize),
    TogglePause,
    Next,
    Previous,
    SeekBack,
    SeekForward,
    VolumeUp,
    VolumeDown,
    ToggleMute,
    ToggleShuffle,
    /// Off, then all, then one, then off again.
    CycleRepeat,
    /// Enter: open the section from the sidebar; in the queue, play the selected entry.
    Activate,
    RemoveEntry,
    MoveEntryDown,
    MoveEntryUp,
    ClearQueue,
}

/// A key press, with the modifiers that matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub control: bool,
}

impl Key {
    pub const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            control: false,
        }
    }

    pub const fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            control: true,
        }
    }

    /// Shift is not a modifier here: it is already in the letter (`G` is `G`, whatever the terminal
    /// reports), and it means nothing on `Tab` or `Esc`.
    pub fn from_event(event: KeyEvent) -> Self {
        Self {
            code: event.code,
            control: event.modifiers.contains(KeyModifiers::CONTROL),
        }
    }
}

/// Which panel of the help a binding is listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    General,
    Movement,
    Panels,
    Playback,
    Queue,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::General => "General",
            Group::Movement => "Movement",
            Group::Panels => "Panels",
            Group::Playback => "Playback",
            Group::Queue => "Queue",
        }
    }
}

pub struct Binding {
    /// The keys, one press after the other: `gg` is two.
    pub keys: &'static [Key],
    /// How the help writes them.
    pub label: &'static str,
    pub action: Action,
    pub help: &'static str,
    pub group: Group,
}

const fn c(ch: char) -> Key {
    Key::plain(KeyCode::Char(ch))
}

const fn bind(
    keys: &'static [Key],
    label: &'static str,
    action: Action,
    help: &'static str,
    group: Group,
) -> Binding {
    Binding {
        keys,
        label,
        action,
        help,
        group,
    }
}

/// Every key of the interface. Several rows may have the same action (`j` and the arrow).
pub const BINDINGS: &[Binding] = &[
    bind(&[c('q')], "q", Action::Quit, "quit", Group::General),
    bind(
        &[Key::ctrl('c')],
        "Ctrl-c",
        Action::Quit,
        "quit",
        Group::General,
    ),
    bind(
        &[c('?')],
        "?",
        Action::ToggleHelp,
        "show or hide this help",
        Group::General,
    ),
    bind(
        &[Key::plain(KeyCode::Esc)],
        "Esc",
        Action::CloseHelp,
        "close the help",
        Group::General,
    ),
    bind(
        &[c('R')],
        "R",
        Action::Reconnect,
        "connect to the daemon now",
        Group::General,
    ),
    bind(&[c('j')], "j", Action::Down, "down", Group::Movement),
    bind(
        &[Key::plain(KeyCode::Down)],
        "Down",
        Action::Down,
        "down",
        Group::Movement,
    ),
    bind(&[c('k')], "k", Action::Up, "up", Group::Movement),
    bind(
        &[Key::plain(KeyCode::Up)],
        "Up",
        Action::Up,
        "up",
        Group::Movement,
    ),
    bind(
        &[c('g'), c('g')],
        "gg",
        Action::First,
        "first row",
        Group::Movement,
    ),
    bind(&[c('G')], "G", Action::Last, "last row", Group::Movement),
    bind(
        &[Key::ctrl('d')],
        "Ctrl-d",
        Action::HalfPageDown,
        "half a page down",
        Group::Movement,
    ),
    bind(
        &[Key::ctrl('u')],
        "Ctrl-u",
        Action::HalfPageUp,
        "half a page up",
        Group::Movement,
    ),
    bind(
        &[Key::plain(KeyCode::Tab)],
        "Tab",
        Action::FocusNext,
        "next panel",
        Group::Panels,
    ),
    bind(
        &[c('h')],
        "h",
        Action::FocusSidebar,
        "go to the sidebar",
        Group::Panels,
    ),
    bind(
        &[Key::plain(KeyCode::Left)],
        "Left",
        Action::FocusSidebar,
        "go to the sidebar",
        Group::Panels,
    ),
    bind(
        &[c('l')],
        "l",
        Action::FocusMain,
        "go to the list",
        Group::Panels,
    ),
    bind(
        &[Key::plain(KeyCode::Right)],
        "Right",
        Action::FocusMain,
        "go to the list",
        Group::Panels,
    ),
    bind(
        &[Key::plain(KeyCode::Enter)],
        "Enter",
        Action::Activate,
        "open the section, or play the selected track",
        Group::Panels,
    ),
    bind(&[c('1')], "1", Action::Section(0), "Queue", Group::Panels),
    bind(&[c('2')], "2", Action::Section(1), "Search", Group::Panels),
    bind(&[c('3')], "3", Action::Section(2), "Library", Group::Panels),
    bind(
        &[Key::plain(KeyCode::Char(' '))],
        "Space",
        Action::TogglePause,
        "play or pause",
        Group::Playback,
    ),
    bind(&[c('n')], "n", Action::Next, "next track", Group::Playback),
    bind(
        &[c('p')],
        "p",
        Action::Previous,
        "previous track",
        Group::Playback,
    ),
    bind(
        &[c('<')],
        "<",
        Action::SeekBack,
        "back 10 s",
        Group::Playback,
    ),
    bind(
        &[c('>')],
        ">",
        Action::SeekForward,
        "forward 10 s",
        Group::Playback,
    ),
    bind(
        &[c('+')],
        "+",
        Action::VolumeUp,
        "volume up",
        Group::Playback,
    ),
    bind(
        &[c('=')],
        "=",
        Action::VolumeUp,
        "volume up",
        Group::Playback,
    ),
    bind(
        &[c('-')],
        "-",
        Action::VolumeDown,
        "volume down",
        Group::Playback,
    ),
    bind(
        &[c('m')],
        "m",
        Action::ToggleMute,
        "mute or unmute",
        Group::Playback,
    ),
    bind(
        &[c('s')],
        "s",
        Action::ToggleShuffle,
        "shuffle on or off",
        Group::Playback,
    ),
    bind(
        &[c('d')],
        "d",
        Action::RemoveEntry,
        "remove the selected track",
        Group::Queue,
    ),
    bind(
        &[c('J')],
        "J",
        Action::MoveEntryDown,
        "move the selected track down",
        Group::Queue,
    ),
    bind(
        &[c('K')],
        "K",
        Action::MoveEntryUp,
        "move the selected track up",
        Group::Queue,
    ),
    bind(
        &[c('c'), c('c')],
        "cc",
        Action::ClearQueue,
        "clear the queue",
        Group::Queue,
    ),
    bind(
        &[c('r')],
        "r",
        Action::CycleRepeat,
        "repeat: off, all, one",
        Group::Playback,
    ),
];

/// What a key press turned into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// A binding is complete.
    Action(Action),
    /// The keys so far begin a longer binding (the first `g` of `gg`): wait for the next.
    Pending,
    /// Nothing starts with these keys.
    None,
}

/// Looks up `pending` (the keys already pressed and waiting) followed by `key`.
pub fn resolve(pending: &[Key], key: Key) -> Resolution {
    let mut typed = pending.to_vec();
    typed.push(key);
    let mut longer = false;
    for binding in BINDINGS {
        if binding.keys == typed.as_slice() {
            return Resolution::Action(binding.action);
        }
        longer |= binding.keys.starts_with(&typed);
    }
    if longer {
        Resolution::Pending
    } else {
        Resolution::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_key_resolves_at_once() {
        assert_eq!(resolve(&[], c('j')), Resolution::Action(Action::Down));
        assert_eq!(resolve(&[], c('G')), Resolution::Action(Action::Last));
        assert_eq!(
            resolve(&[], Key::ctrl('d')),
            Resolution::Action(Action::HalfPageDown)
        );
    }

    #[test]
    fn gg_waits_for_the_second_g() {
        assert_eq!(resolve(&[], c('g')), Resolution::Pending);
        assert_eq!(
            resolve(&[c('g')], c('g')),
            Resolution::Action(Action::First)
        );
        assert_eq!(resolve(&[c('g')], c('x')), Resolution::None);
    }

    #[test]
    fn an_unbound_key_resolves_to_nothing() {
        assert_eq!(resolve(&[], c('x')), Resolution::None);
        assert_eq!(resolve(&[], Key::ctrl('q')), Resolution::None);
        assert_eq!(resolve(&[], Key::plain(KeyCode::F(5))), Resolution::None);
    }

    #[test]
    fn a_key_with_control_is_not_the_same_key_without_it() {
        assert_ne!(resolve(&[], Key::ctrl('c')), resolve(&[], c('c')));
    }

    #[test]
    fn shift_is_ignored_because_the_letter_already_carries_it() {
        let event = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(
            resolve(&[], Key::from_event(event)),
            Resolution::Action(Action::Last)
        );
    }

    #[test]
    fn no_two_bindings_share_keys_and_none_is_the_start_of_another() {
        for (i, a) in BINDINGS.iter().enumerate() {
            for b in &BINDINGS[i + 1..] {
                assert!(
                    !a.keys.starts_with(b.keys) && !b.keys.starts_with(a.keys),
                    "{} and {} clash",
                    a.label,
                    b.label
                );
            }
        }
    }
}
