//! The Home section: the Continue row, then one block per favorites/folders list the library
//! already has, each capped and closed with a "See all" row (see `home::rows`).

use super::browse::folder_entry_text;
use super::first_visible;
use crate::app::State;
use crate::home::{self, Row};
use crate::theme::Theme;
use phonia_ipc::fmt;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

pub fn draw(state: &State, theme: &Theme, focused: bool, frame: &mut Frame, area: Rect) {
    let rows = home::rows(state);
    let selected = home::display_index_of(&rows, state.home_cursor.selected());
    let height = usize::from(area.height);
    let start = first_visible(selected.unwrap_or(0), height);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(height.max(1))
        .map(|(index, row)| line_for(row, focused && Some(index) == selected, theme))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn line_for(row: &Row, selected: bool, theme: &Theme) -> Line<'static> {
    let text = match row {
        Row::Continue(continue_) => continue_.text().to_string(),
        Row::Spacer => String::new(),
        Row::Header(tab) => tab.title().to_string(),
        Row::Album(album) => fmt::album(album),
        Row::Artist(artist) => fmt::artist(artist),
        Row::Entry(entry) => folder_entry_text(entry),
        Row::Track(track) => fmt::track(track),
        Row::Played(played) => fmt::track_name(
            played.title.as_deref(),
            played.artist.as_deref(),
            Some(&played.source),
        ),
        Row::SeeAll { total, .. } => format!("See all ({total}) \u{2192}"),
    };
    let style = if selected {
        theme.selected
    } else if matches!(row, Row::Header(_)) {
        theme.title
    } else {
        theme.text
    };
    Line::styled(text, style)
}
