//! The library section: the tabs, and the list of what is in the one shown.

use super::browse::folder_entry_text;
use super::first_visible;
use crate::app::{Connection, Focus, State};
use crate::browse::Phase;
use crate::library::{LibraryState, LibraryTab};
use crate::theme::Theme;
use phonia_ipc::fmt;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub fn draw(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let [tabs, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    draw_tabs(state, theme, frame, tabs);
    draw_list(state, theme, frame, list);
}

fn draw_tabs(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let Some(library) = &state.library else {
        return;
    };
    let mut spans = Vec::new();
    for tab in LibraryTab::ALL {
        // The counts are known once it has loaded -- the Playlists tab (the root of the folder
        // tree) loads separately from the other two, so it has its own phase to check.
        let ready = match tab {
            LibraryTab::Playlists => library.playlists_phase == Phase::Done,
            LibraryTab::FavoriteTracks | LibraryTab::FavoriteAlbums => library.phase == Phase::Done,
        };
        let label = if ready {
            format!("{} ({})", tab.title(), library.total(tab))
        } else {
            tab.title().to_string()
        };
        let style = if tab == library.tab {
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
    let Some(library) = &state.library else {
        // Not asked for yet: say why, so this does not just sit on "Loading..." forever when
        // there is nothing to load it from.
        let text = if state.connection != Connection::Connected {
            "Not connected yet."
        } else if !state.has(phonia_ipc::CAP_CATALOG) {
            "This phoniad has no TIDAL login to show a library from."
        } else {
            "Loading..."
        };
        frame.render_widget(Paragraph::new(Line::styled(text, theme.dim)), area);
        return;
    };
    let phase = match library.tab {
        LibraryTab::Playlists => &library.playlists_phase,
        LibraryTab::FavoriteTracks | LibraryTab::FavoriteAlbums => &library.phase,
    };
    let lines = match phase {
        Phase::Loading => vec![Line::styled("Loading...", theme.dim)],
        Phase::Failed(reason) => vec![Line::styled(reason.clone(), theme.error)],
        Phase::Done => rows(state, library, theme, usize::from(area.height)),
    };
    frame.render_widget(Paragraph::new(lines), area);
}

/// The rows of the current tab that fit in `height`, the one under the cursor highlighted when the
/// list has the focus.
fn rows<'a>(state: &State, library: &LibraryState, theme: &Theme, height: usize) -> Vec<Line<'a>> {
    let texts: Vec<String> = match library.tab {
        LibraryTab::FavoriteTracks => library
            .favorite_tracks
            .items
            .iter()
            .map(fmt::track)
            .collect(),
        LibraryTab::FavoriteAlbums => library
            .favorite_albums
            .items
            .iter()
            .map(fmt::album)
            .collect(),
        LibraryTab::Playlists => library
            .playlists
            .items
            .iter()
            .map(folder_entry_text)
            .collect(),
    };
    if texts.is_empty() {
        let what = library.tab.title().to_lowercase();
        return vec![Line::styled(format!("No {what}."), theme.dim)];
    }
    let cursor = match library.tab {
        LibraryTab::FavoriteTracks => library.favorite_tracks.cursor,
        LibraryTab::FavoriteAlbums => library.favorite_albums.cursor,
        LibraryTab::Playlists => library.playlists.cursor,
    }
    .selected();
    let focused = state.focus == Focus::Main;
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
