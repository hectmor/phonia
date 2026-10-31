//! The search section: the line being typed, the tabs, and the list of what was found.

use super::keep_in_view;
use super::{TabLabel, tabs_line};
use crate::app::{Focus, State};
use crate::lyrics;
use crate::search::{Phase, Tab};
use crate::theme::Theme;
use phonia_ipc::fmt;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// The columns taken before the text of the line: `/ `.
const PROMPT: usize = 2;

pub fn draw(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) -> Option<usize> {
    let [input, tabs, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    draw_input(state, theme, frame, input);
    draw_tabs(state, theme, frame, tabs);
    draw_list(state, theme, frame, list)
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
    let tabs: Vec<TabLabel> = Tab::ALL
        .into_iter()
        .map(|tab| TabLabel {
            name: tab.title(),
            // The counts are known once the results are in.
            count: (search.phase == Phase::Done).then(|| search.total(tab)),
            selected: tab == search.tab,
        })
        .collect();
    frame.render_widget(Paragraph::new(tabs_line(&tabs, area.width, theme)), area);
}

fn draw_list(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) -> Option<usize> {
    let search = &state.search;
    let (lines, shown) = match &search.phase {
        Phase::Idle => (
            vec![Line::styled("Press / to search TIDAL.", theme.dim)],
            None,
        ),
        Phase::Searching => (vec![Line::styled("Searching...", theme.dim)], None),
        Phase::Failed(reason) => (vec![Line::styled(reason.clone(), theme.error)], None),
        Phase::Done => rows(state, theme, usize::from(area.height)),
    };
    frame.render_widget(Paragraph::new(lines), area);
    shown
}

/// The rows of the current tab that fit in `height`, the one under the cursor highlighted when the
/// list has the focus, and the scroll offset they were drawn at (`None` on an empty tab).
fn rows<'a>(state: &State, theme: &Theme, height: usize) -> (Vec<Line<'a>>, Option<usize>) {
    let search = &state.search;
    let texts: Vec<String> = match search.tab {
        Tab::Tracks => search.tracks.items.iter().map(fmt::track).collect(),
        Tab::Albums => search.albums.items.iter().map(fmt::album).collect(),
        Tab::Artists => search.artists.items.iter().map(fmt::artist).collect(),
        Tab::Playlists => search.playlists.items.iter().map(fmt::playlist).collect(),
    };
    if texts.is_empty() {
        let what = search.tab.title().to_lowercase();
        return (
            vec![Line::styled(
                format!("No {what} for {:?}.", search.query),
                theme.dim,
            )],
            None,
        );
    }
    let cursor = match search.tab {
        Tab::Tracks => search.tracks.cursor,
        Tab::Albums => search.albums.cursor,
        Tab::Artists => search.artists.cursor,
        Tab::Playlists => search.playlists.cursor,
    };
    let selected = cursor.selected();
    let start = keep_in_view(cursor.scroll(), selected, texts.len(), height);
    let focused = state.focus == Focus::Main && !search.editing;
    // Artists and playlists have nothing in `status.track` to compare against, so only Tracks (by
    // TIDAL id) and Albums (by the album's own cover id, which a track carries too) ever mark a
    // row current.
    let playing = state
        .status
        .as_ref()
        .and_then(|status| status.track.as_ref());
    let now_playing_track = playing.and_then(lyrics::tidal_id);
    let now_playing_album_cover = playing.and_then(|track| track.cover.as_deref());
    let lines = texts
        .into_iter()
        .enumerate()
        .skip(start)
        .take(height.max(1))
        .map(|(index, text)| {
            let current = match search.tab {
                Tab::Tracks => now_playing_track.is_some_and(|id| {
                    search
                        .tracks
                        .items
                        .get(index)
                        .is_some_and(|track| track.id == id)
                }),
                Tab::Albums => now_playing_album_cover.is_some_and(|cover| {
                    search
                        .albums
                        .items
                        .get(index)
                        .is_some_and(|album| album.cover.as_deref() == Some(cover))
                }),
                Tab::Artists | Tab::Playlists => false,
            };
            // Same marker as the queue's own current entry (see `queue_lines`), so the track or
            // album playing stands out even on a row the cursor also happens to be on.
            let marker = if current { ">" } else { " " };
            let style = if focused && index == selected {
                theme.selected
            } else if current {
                theme.accent
            } else {
                theme.text
            };
            Line::styled(format!("{marker}{:>4}. {text}", index + 1), style)
        })
        .collect();
    (lines, Some(start))
}
