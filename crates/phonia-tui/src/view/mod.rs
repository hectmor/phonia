//! Painting a state onto the screen.

mod help;

use crate::app::{Focus, Section, State};
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
    draw_bar(theme, frame, areas.bar);
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
    frame.render_widget(
        Paragraph::new(Line::styled("Nothing to show yet.", theme.dim)).block(block),
        area,
    );
}

fn draw_bar(theme: &Theme, frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::styled("Not connected to phoniad", theme.dim),
        Line::styled("? help   q quit", theme.dim),
    ];
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
            "Nothing to show yet",
            "Not connected",
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
