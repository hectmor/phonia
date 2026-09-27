//! Painting a state onto the screen.

mod help;

use crate::app::{Connection, Focus, Section, State, seconds_left};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// The columns the sidebar takes: the longest section name and room for the border and the mark.
const SIDEBAR_WIDTH: u16 = 14;
/// The rows of the bar at the bottom: a border and two lines.
const BAR_HEIGHT: u16 = 3;

/// The three areas of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Areas {
    pub sidebar: Rect,
    pub main: Rect,
    pub bar: Rect,
}

pub fn areas(area: Rect) -> Areas {
    let [top, bar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(BAR_HEIGHT)]).areas(area);
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(top);
    Areas { sidebar, main, bar }
}

pub fn draw(state: &State, theme: &Theme, frame: &mut Frame) {
    let areas = areas(frame.area());
    draw_sidebar(state, theme, frame, areas.sidebar);
    draw_main(state, theme, frame, areas.main);
    draw_bar(state, theme, frame, areas.bar);
    if state.help {
        help::draw(theme, frame);
    }
}

/// The border of a panel: bright when it has the focus.
fn panel<'a>(title: &'a str, focused: bool, theme: &Theme) -> Block<'a> {
    let (border, title_style) = if focused {
        (theme.accent, theme.accent)
    } else {
        (theme.dim, theme.title)
    };
    Block::bordered()
        .border_style(border)
        .title(Span::styled(format!(" {title} "), title_style))
}

fn draw_sidebar(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let focused = state.focus == Focus::Sidebar;
    let lines: Vec<Line> = Section::ALL
        .iter()
        .enumerate()
        .map(|(index, section)| {
            let selected = index == state.sidebar.selected();
            // The selected section is marked even when the focus is elsewhere, so it is clear
            // what the main panel is showing.
            let style = match (selected, focused) {
                (true, true) => theme.selected,
                (true, false) => theme.accent,
                (false, _) => theme.text,
            };
            Line::styled(format!(" {} {}", index + 1, section.title()), style)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(panel("phonia", focused, theme)),
        area,
    );
}

fn draw_main(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let focused = state.focus == Focus::Main;
    let block = panel(state.section().title(), focused, theme);
    let lines = match (state.section(), &state.queue) {
        (Section::Queue, Some(queue)) => queue_lines(queue, theme),
        (Section::Queue, None) => vec![Line::styled("Not connected yet.", theme.dim)],
        _ => vec![Line::styled("Nothing to show yet.", theme.dim)],
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn queue_lines<'a>(queue: &phonia_ipc::Queue, theme: &Theme) -> Vec<Line<'a>> {
    if queue.items.is_empty() {
        return vec![Line::styled("The queue is empty.", theme.dim)];
    }
    queue
        .order
        .iter()
        .filter_map(|id| queue.items.iter().find(|item| item.id == *id))
        .enumerate()
        .map(|(index, item)| {
            let current = queue.current == Some(item.id);
            let marker = if current { ">" } else { " " };
            let name = item.title.clone().unwrap_or_else(|| item.source.clone());
            let length = item
                .duration_ms
                .map(|ms| format!("  [{}]", phonia_ipc::fmt::ms(ms)))
                .unwrap_or_default();
            let style = if current { theme.accent } else { theme.text };
            Line::styled(format!("{marker} {:>2}. {name}{length}", index + 1), style)
        })
        .collect()
}

fn connected_line<'a>(state: &State, theme: &Theme) -> Line<'a> {
    if let Some(reason) = &state.last_error {
        return Line::styled(format!("Could not do that: {reason}"), theme.error);
    }
    let Some(status) = &state.status else {
        return Line::styled("Connected", theme.text);
    };
    let name = status
        .track
        .as_ref()
        .and_then(|track| track.title.clone().or_else(|| track.source.clone()))
        .unwrap_or_else(|| "Nothing playing".to_string());
    let position = if status.track.is_some() {
        let total = status
            .duration_ms
            .map(|ms| format!(" / {}", phonia_ipc::fmt::ms(ms)))
            .unwrap_or_default();
        format!("  {}{total}", phonia_ipc::fmt::ms(status.position_ms))
    } else {
        String::new()
    };
    Line::styled(
        format!("{}  {name}{position}", state_word(status.state)),
        theme.text,
    )
}

fn state_word(state: phonia_ipc::State) -> &'static str {
    match state {
        phonia_ipc::State::Stopped => "Stopped",
        phonia_ipc::State::Loading => "Loading",
        phonia_ipc::State::Playing => "Playing",
        phonia_ipc::State::Paused => "Paused",
        phonia_ipc::State::Seeking => "Seeking",
    }
}

fn draw_bar(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let (first, second) = match &state.connection {
        Connection::Connecting => (
            Line::styled("Connecting to phoniad...", theme.dim),
            "? help   q quit".to_string(),
        ),
        Connection::Connected => (
            connected_line(state, theme),
            "Space pause   n next   p prev   ? help   q quit".to_string(),
        ),
        Connection::Disconnected { reason, retry_in } => (
            Line::styled(format!("phoniad is not reachable: {reason}"), theme.error),
            format!(
                "Trying again in {} s (R: now). Start the daemon with `phoniad`.   ? help   q quit",
                seconds_left(*retry_in)
            ),
        ),
        Connection::Refused { reason } => (
            Line::styled(format!("Cannot use this daemon: {reason}"), theme.error),
            "R: try again   ? help   q quit".to_string(),
        ),
    };
    let lines = vec![first, Line::styled(second, theme.dim)];
    frame.render_widget(
        Paragraph::new(lines).block(Block::new().borders(Borders::TOP).border_style(theme.dim)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Msg, update};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// The screen as text, one row per line.
    pub(crate) fn screen(state: &State, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(state, &Theme::new(false), frame))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(state: &mut State, c: char) {
        update(
            state,
            Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }

    #[test]
    fn the_screen_has_a_sidebar_a_main_panel_and_a_bar() {
        let screen = screen(&State::default(), 60, 12);
        for wanted in [
            "1 Queue",
            "2 Search",
            "3 Library",
            "Not connected yet",
            "Connecting",
            "? help",
        ] {
            assert!(
                screen.contains(wanted),
                "{wanted:?} missing from:\n{screen}"
            );
        }
    }

    #[test]
    fn the_main_panel_is_titled_with_the_selected_section() {
        let mut state = State::default();
        press(&mut state, '2');
        let screen = screen(&state, 60, 12);
        assert!(screen.contains(" Search "), "{screen}");
    }

    fn connected() -> State {
        let mut state = State::default();
        update(
            &mut state,
            Msg::Connected {
                server: phonia_ipc::ServerInfo {
                    name: "phoniad".into(),
                    version: "0.1.0".into(),
                    pid: 1,
                },
                protocol: phonia_ipc::Version { major: 1, minor: 5 },
                capabilities: vec![],
                status: crate::app::tests_support::status(),
                queue: crate::app::tests_support::queue(),
            },
        );
        state
    }

    #[test]
    fn the_bar_says_how_the_connection_stands() {
        let text = screen(&connected(), 90, 12);
        assert!(text.contains("Stopped"), "{text}");
        assert!(text.contains("Nothing playing"), "{text}");
        assert!(text.contains("Space pause"), "{text}");

        let mut state = connected();
        update(
            &mut state,
            Msg::Disconnected {
                reason: "connection refused".into(),
                retry_in: std::time::Duration::from_millis(2500),
            },
        );
        let text = screen(&state, 120, 12);
        assert!(
            text.contains("phoniad is not reachable: connection refused"),
            "{text}"
        );
        assert!(text.contains("Trying again in 3 s (R: now)"), "{text}");
        assert!(text.contains("`phoniad`"), "{text}");

        update(
            &mut state,
            Msg::Refused {
                reason: "the daemon speaks protocol 2.0".into(),
            },
        );
        let text = screen(&state, 120, 12);
        assert!(
            text.contains("Cannot use this daemon: the daemon speaks protocol 2.0"),
            "{text}"
        );
        assert!(text.contains("R: try again"), "{text}");
    }

    #[test]
    fn a_playing_track_and_position_show_in_the_bar() {
        let mut state = connected();
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::TrackStarted {
                item_id: None,
                source: Some("tidal:1".into()),
                title: Some("Aerodynamic".into()),
                duration_ms: Some(343_000),
                spec: phonia_ipc::Spec {
                    sample_rate: 44_100,
                    channels: 2,
                    bits_per_sample: 16,
                },
                gapless: false,
                quality: None,
            }),
        );
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::StateChanged {
                state: phonia_ipc::State::Playing,
            }),
        );
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::Position {
                position_ms: 65_000,
                duration_ms: Some(343_000),
            }),
        );
        let text = screen(&state, 90, 12);
        assert!(text.contains("Playing"), "{text}");
        assert!(text.contains("Aerodynamic"), "{text}");
        assert!(text.contains("1:05 / 5:43"), "{text}");
    }

    #[test]
    fn a_request_error_replaces_the_playback_line_until_the_next_key() {
        let mut state = connected();
        update(
            &mut state,
            Msg::RequestFailed {
                reason: "the connection to the daemon was closed".into(),
            },
        );
        let text = screen(&state, 90, 12);
        assert!(text.contains("Could not do that"), "{text}");
    }

    #[test]
    fn the_queue_is_listed_in_play_order_with_the_current_one_marked() {
        use phonia_ipc::{ItemId, Queue, QueueItem, Repeat};
        let mut state = connected();
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::QueueChanged {
                queue: Queue {
                    version: 2,
                    items: vec![
                        QueueItem {
                            id: ItemId(1),
                            source: "file:/a.flac".into(),
                            title: Some("A".into()),
                            duration_ms: Some(65_000),
                        },
                        QueueItem {
                            id: ItemId(2),
                            source: "tidal:9".into(),
                            title: None,
                            duration_ms: None,
                        },
                    ],
                    order: vec![ItemId(2), ItemId(1)],
                    current: Some(ItemId(2)),
                    shuffle: false,
                    repeat: Repeat::Off,
                },
            }),
        );
        let text = screen(&state, 90, 12);
        let a = text.find("tidal:9").unwrap();
        let b = text.find("A  [1:05]").unwrap();
        assert!(
            a < b,
            "tidal:9 (order[0]) should be listed before A: {text}"
        );
        assert!(text.contains(">  1. tidal:9"), "{text}");
    }

    #[test]
    fn the_areas_do_not_overlap_and_fill_the_screen() {
        let area = Rect::new(0, 0, 80, 24);
        let a = areas(area);
        assert_eq!(a.sidebar.width, SIDEBAR_WIDTH);
        assert_eq!(a.sidebar.width + a.main.width, area.width);
        assert_eq!(a.sidebar.height + a.bar.height, area.height);
        assert_eq!(a.bar.y, a.sidebar.height);
        assert_eq!(a.main.x, a.sidebar.right());
    }

    #[test]
    fn terminals_too_small_for_the_layout_do_not_panic() {
        let mut state = State::default();
        for (w, h) in [(0, 0), (1, 1), (5, 2), (13, 3), (14, 3), (20, 4)] {
            let _ = screen(&state, w, h);
            state.help = true;
            let _ = screen(&state, w, h);
            state.help = false;
        }
    }

    #[test]
    fn the_help_is_painted_over_the_screen_when_it_is_open() {
        let mut state = State::default();
        press(&mut state, '?');
        let screen = screen(&state, 70, 30);
        assert!(screen.contains("Movement"), "{screen}");
    }
}
