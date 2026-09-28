//! The search section: the line being typed, the tabs, and the list of what was found.

use super::first_visible;
use crate::app::{Focus, State};
use crate::search::{Phase, Tab};
use crate::theme::Theme;
use phonia_ipc::fmt;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// The columns taken before the text of the line: `/ `.
const PROMPT: usize = 2;

pub fn draw(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let [input, tabs, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    draw_input(state, theme, frame, input);
    draw_tabs(state, theme, frame, tabs);
    draw_list(state, theme, frame, list);
}

fn draw_input(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let search = &state.search;
    let room = usize::from(area.width).saturating_sub(PROMPT);
    let (shown, column) = search.input.window(room);
    let (prompt_style, text_style) = if search.editing {
        (theme.accent, theme.text)
    } else {
        (theme.dim, theme.dim)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/ ", prompt_style),
            Span::styled(shown, text_style),
        ])),
        area,
    );
    if search.editing && room > 0 {
        frame.set_cursor_position(Position::new(area.x + (PROMPT + column) as u16, area.y));
    }
}

fn draw_tabs(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let search = &state.search;
    let mut spans = Vec::new();
    for tab in Tab::ALL {
        // The counts are known once the results are in.
        let label = if search.phase == Phase::Done {
            format!("{} ({})", tab.title(), search.total(tab))
        } else {
            tab.title().to_string()
        };
        let style = if tab == search.tab {
            theme.accent
        } else {
            theme.dim
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw("   "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_list(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let search = &state.search;
    let lines = match &search.phase {
        Phase::Idle => vec![Line::styled("Press / to search TIDAL.", theme.dim)],
        Phase::Searching => vec![Line::styled("Searching...", theme.dim)],
        Phase::Failed(reason) => vec![Line::styled(reason.clone(), theme.error)],
        Phase::Done => rows(state, theme, usize::from(area.height)),
    };
    frame.render_widget(Paragraph::new(lines), area);
}

/// The rows of the current tab that fit in `height`, the one under the cursor highlighted when the
/// list has the focus.
fn rows<'a>(state: &State, theme: &Theme, height: usize) -> Vec<Line<'a>> {
    let search = &state.search;
    let texts: Vec<String> = match search.tab {
        Tab::Tracks => search.tracks.items.iter().map(fmt::track).collect(),
        Tab::Albums => search.albums.items.iter().map(fmt::album).collect(),
        Tab::Artists => search.artists.items.iter().map(fmt::artist).collect(),
        Tab::Playlists => search.playlists.items.iter().map(fmt::playlist).collect(),
    };
    if texts.is_empty() {
        let what = search.tab.title().to_lowercase();
        return vec![Line::styled(
            format!("No {what} for {:?}.", search.query),
            theme.dim,
        )];
    }
    let cursor = match search.tab {
        Tab::Tracks => search.tracks.cursor,
        Tab::Albums => search.albums.cursor,
        Tab::Artists => search.artists.cursor,
        Tab::Playlists => search.playlists.cursor,
    }
    .selected();
    let focused = state.focus == Focus::Main && !search.editing;
    texts
        .into_iter()
        .enumerate()
        .skip(first_visible(cursor, height))
        .take(height.max(1))
        .map(|(index, text)| {
            let style = if focused && index == cursor {
                theme.selected
            } else {
                theme.text
            };
            Line::styled(format!("{:>4}. {text}", index + 1), style)
        })
        .collect()
}
