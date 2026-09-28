//! An album or a playlist, opened to see its tracks.

use super::first_visible;
use crate::app::{Focus, State};
use crate::browse::{Header, Phase, View};
use crate::theme::Theme;
use phonia_ipc::fmt;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

pub fn draw(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let Some(View::TrackList(view)) = state.search_views.top() else {
        return;
    };
    let header = header_lines(view.header(), theme);
    let [header_area, list_area] =
        Layout::vertical([Constraint::Length(header.len() as u16), Constraint::Min(0)]).areas(area);
    frame.render_widget(Paragraph::new(header), header_area);
    let focused = state.focus == Focus::Main;
    let lines = match &view.phase {
        Phase::Loading => vec![Line::styled("Loading...", theme.dim)],
        Phase::Failed(reason) => vec![Line::styled(reason.clone(), theme.error)],
        Phase::Done => track_rows(view, focused, usize::from(list_area.height), theme),
    };
    frame.render_widget(Paragraph::new(lines), list_area);
}

/// The album's or the playlist's own line, plus its copyright when there is one, and a blank line
/// to set the tracks apart.
fn header_lines(header: &Header, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = match header {
        Header::Album(album) => {
            let mut lines = vec![Line::styled(fmt::album(album), theme.accent)];
            if let Some(copyright) = &album.copyright {
                lines.push(Line::styled(copyright.clone(), theme.dim));
            }
            lines
        }
        Header::Playlist(playlist) => vec![Line::styled(fmt::playlist(playlist), theme.accent)],
    };
    lines.push(Line::raw(""));
    lines
}

/// The tracks, numbered as they are on the album or the playlist, the one under the cursor
/// highlighted when the list has the focus and one TIDAL will not stream dimmed.
fn track_rows<'a>(
    view: &crate::browse::TrackListView,
    focused: bool,
    height: usize,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let tracks = &view.tracks;
    if tracks.items.is_empty() {
        return vec![Line::styled("No tracks.", theme.dim)];
    }
    let cursor = tracks.cursor.selected();
    let start = first_visible(cursor, height);
    tracks
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(height.max(1))
        .map(|(index, track)| {
            let number = track.track_number.map_or(index + 1, |n| n as usize);
            let text = format!("{number:>3}. {}", fmt::track_short(track));
            let style = if focused && index == cursor {
                theme.selected
            } else if !track.streamable {
                theme.dim
            } else {
                theme.text
            };
            Line::styled(text, style)
        })
        .collect()
}
