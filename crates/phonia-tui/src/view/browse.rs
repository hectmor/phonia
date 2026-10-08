//! An album, a playlist or an artist, opened to see its music.

use super::first_visible;
use crate::app::{Focus, State};
use crate::browse::{ArtistTab, ArtistView, FolderView, Header, Phase, Stack, TrackListView, View};
use crate::covers::{self, Covers};
use crate::theme::Theme;
use phonia_ipc::{AlbumSummary, FolderEntry, TrackSummary, fmt};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui_image::Image;

/// The view on top of `stack` (the one opened from a search result, or from the library).
pub fn draw(
    state: &State,
    stack: &Stack,
    theme: &Theme,
    covers: &Covers,
    frame: &mut Frame,
    area: Rect,
) {
    match stack.top() {
        Some(View::TrackList(view)) => draw_track_list(state, view, theme, covers, frame, area),
        Some(View::Artist(view)) => draw_artist(state, view, theme, covers, frame, area),
        Some(View::Folder(view)) => draw_folder(state, view, theme, frame, area),
        None => {}
    }
}

fn draw_track_list(
    state: &State,
    view: &TrackListView,
    theme: &Theme,
    covers: &Covers,
    frame: &mut Frame,
    area: Rect,
) {
    let header_lines = header_lines(view.header(), theme);
    // The cover's own space is reserved whenever one *could* show here (a picker, a cover id to
    // show, and room enough), whether or not it has actually finished fetching yet: the layout
    // must not jump once it has. An item with no cover id at all reserves nothing, the same as a
    // terminal with no picker.
    let reserved = covers.picker().and_then(|picker| {
        let cells = covers::cover_size(area, picker.font_size())?;
        let url = covers::cover_url(view.header(), cells, picker.font_size())?;
        Some((url, cells))
    });
    let (cover_area, header_area, list_area) = match reserved {
        Some((url, cells)) => {
            let [top, list_area] =
                Layout::vertical([Constraint::Length(cells.height), Constraint::Min(0)])
                    .areas(area);
            let [cover_area, header_area] =
                Layout::horizontal([Constraint::Length(cells.width), Constraint::Min(0)])
                    .areas(top);
            (Some((cover_area, url)), header_area, list_area)
        }
        None => {
            let [header_area, list_area] = Layout::vertical([
                Constraint::Length(header_lines.len() as u16),
                Constraint::Min(0),
            ])
            .areas(area);
            (None, header_area, list_area)
        }
    };
    if let Some((cover_area, url)) = &cover_area
        && let Some(protocol) = covers.ready(url)
    {
        frame.render_widget(Image::new(protocol), *cover_area);
    }
    frame.render_widget(Paragraph::new(header_lines), header_area);
    let focused = state.focus == Focus::Main;
    let lines = match &view.phase {
        Phase::Loading => vec![Line::styled("Loading...", theme.dim)],
        Phase::Failed(reason) => vec![Line::styled(reason.clone(), theme.error)],
        Phase::Done => track_rows(
            &view.tracks.items,
            view.tracks.cursor.selected(),
            focused,
            usize::from(list_area.height),
            theme,
        ),
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
    tracks: &[TrackSummary],
    cursor: usize,
    focused: bool,
    height: usize,
    theme: &Theme,
) -> Vec<Line<'a>> {
    if tracks.is_empty() {
        return vec![Line::styled("No tracks.", theme.dim)];
    }
    let start = first_visible(cursor, height);
    tracks
        .iter()
        .enumerate()
        .skip(start)
        .take(height.max(1))
        .map(|(index, track)| {
            let number = track.track_number.map_or(index + 1, |n| n as usize);
            let text = format!("{number:>3}. {}", fmt::track_short(track));
            row_line(text, index == cursor, focused, !track.streamable, theme)
        })
        .collect()
}

/// An artist's albums or its EPs and singles, numbered from where the page starts.
fn album_rows<'a>(
    albums: &[AlbumSummary],
    cursor: usize,
    focused: bool,
    height: usize,
    theme: &Theme,
) -> Vec<Line<'a>> {
    if albums.is_empty() {
        return vec![Line::styled("No albums.", theme.dim)];
    }
    let start = first_visible(cursor, height);
    albums
        .iter()
        .enumerate()
        .skip(start)
        .take(height.max(1))
        .map(|(index, album)| {
            let text = format!("{:>3}. {}", index + 1, fmt::album(album));
            row_line(text, index == cursor, focused, false, theme)
        })
        .collect()
}

fn row_line<'a>(text: String, selected: bool, focused: bool, dim: bool, theme: &Theme) -> Line<'a> {
    let style = if focused && selected {
        theme.selected
    } else if dim {
        theme.dim
    } else {
        theme.text
    };
    Line::styled(text, style)
}

fn draw_artist(
    state: &State,
    view: &ArtistView,
    theme: &Theme,
    covers: &Covers,
    frame: &mut Frame,
    area: Rect,
) {
    let header = artist_header_lines(view, theme);
    // Same rule as an album's or a playlist's header (see `draw_track_list`): reserved only when
    // a picture could actually show here, never on whether it has arrived yet. The tabs stay full
    // width, under the picture and the text alike.
    let reserved = covers.picker().and_then(|picker| {
        let cells = covers::cover_size(area, picker.font_size())?;
        let url = covers::picture_url(view.picture.as_deref(), cells, picker.font_size())?;
        Some((url, cells))
    });
    let (cover_area, header_area, tabs_area, list_area) = match reserved {
        Some((url, cells)) => {
            let [top, tabs_area, list_area] = Layout::vertical([
                Constraint::Length(cells.height),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .areas(area);
            let [cover_area, header_area] =
                Layout::horizontal([Constraint::Length(cells.width), Constraint::Min(0)])
                    .areas(top);
            (Some((cover_area, url)), header_area, tabs_area, list_area)
        }
        None => {
            let [header_area, tabs_area, list_area] = Layout::vertical([
                Constraint::Length(header.len() as u16),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .areas(area);
            (None, header_area, tabs_area, list_area)
        }
    };
    if let Some((cover_area, url)) = &cover_area
        && let Some(protocol) = covers.ready(url)
    {
        frame.render_widget(Image::new(protocol), *cover_area);
    }
    frame.render_widget(Paragraph::new(header), header_area);
    if view.phase != Phase::Loading {
        frame.render_widget(Paragraph::new(tabs_line(view, theme)), tabs_area);
    }
    let focused = state.focus == Focus::Main;
    let lines = match &view.phase {
        Phase::Loading => vec![Line::styled("Loading...", theme.dim)],
        Phase::Failed(reason) => vec![Line::styled(reason.clone(), theme.error)],
        Phase::Done => match view.tab {
            ArtistTab::TopTracks => track_rows(
                &view.top_tracks.items,
                view.top_tracks.cursor.selected(),
                focused,
                usize::from(list_area.height),
                theme,
            ),
            ArtistTab::Albums => album_rows(
                &view.albums.items,
                view.albums.cursor.selected(),
                focused,
                usize::from(list_area.height),
                theme,
            ),
            ArtistTab::Singles => album_rows(
                &view.singles.items,
                view.singles.cursor.selected(),
                focused,
                usize::from(list_area.height),
                theme,
            ),
        },
    };
    frame.render_widget(Paragraph::new(lines), list_area);
}

/// A playlist folder: its own name, then its contents (sub-folders first, then playlists, the
/// order TIDAL's own API already sorts them in -- nothing to re-sort here).
fn draw_folder(state: &State, view: &FolderView, theme: &Theme, frame: &mut Frame, area: Rect) {
    let [header_area, list_area] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(view.title().to_string(), theme.accent),
            Line::raw(""),
        ]),
        header_area,
    );
    let focused = state.focus == Focus::Main;
    let lines = match &view.phase {
        Phase::Loading => vec![Line::styled("Loading...", theme.dim)],
        Phase::Failed(reason) => vec![Line::styled(reason.clone(), theme.error)],
        Phase::Done => folder_rows(
            &view.entries.items,
            view.entries.cursor.selected(),
            focused,
            usize::from(list_area.height),
            theme,
        ),
    };
    frame.render_widget(Paragraph::new(lines), list_area);
}

/// A folder's own entries: a sub-folder ends with `/` and how many entries it has; a playlist
/// prints exactly as it does everywhere else.
fn folder_rows<'a>(
    entries: &[FolderEntry],
    cursor: usize,
    focused: bool,
    height: usize,
    theme: &Theme,
) -> Vec<Line<'a>> {
    if entries.is_empty() {
        return vec![Line::styled("Nothing in this folder.", theme.dim)];
    }
    let start = first_visible(cursor, height);
    entries
        .iter()
        .enumerate()
        .skip(start)
        .take(height.max(1))
        .map(|(index, entry)| {
            let text = format!("{:>3}. {}", index + 1, folder_entry_text(entry));
            row_line(text, index == cursor, focused, false, theme)
        })
        .collect()
}

/// The one line a folder entry shows: a sub-folder's name with a trailing `/` and its item
/// count, or a playlist the same way search and the library already print one. Shared with the
/// library section's own Playlists tab (its root level), which shows the same entries inline.
pub(crate) fn folder_entry_text(entry: &FolderEntry) -> String {
    match entry {
        FolderEntry::Folder {
            name, item_count, ..
        } => {
            let noun = if *item_count == 1 { "item" } else { "items" };
            format!("{name}/ ({item_count} {noun})")
        }
        FolderEntry::Playlist(playlist) => fmt::playlist(playlist),
        FolderEntry::Unknown => "(an entry this version does not know)".to_string(),
    }
}

/// The artist's name, a line of its bio when there is one, and a blank line to set the tabs apart.
fn artist_header_lines(view: &ArtistView, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(view.name.clone(), theme.accent)];
    if let Some(bio) = &view.bio {
        let first_line = bio.lines().next().unwrap_or(bio);
        lines.push(Line::styled(first_line.to_string(), theme.dim));
    }
    lines.push(Line::raw(""));
    lines
}

/// The three tabs, with a count once they have loaded.
fn tabs_line<'a>(view: &ArtistView, theme: &Theme) -> Line<'a> {
    let mut spans = Vec::new();
    for tab in ArtistTab::ALL {
        let count = match tab {
            ArtistTab::TopTracks => view.top_tracks.total,
            ArtistTab::Albums => view.albums.total,
            ArtistTab::Singles => view.singles.total,
        };
        let style = if tab == view.tab {
            theme.accent
        } else {
            theme.dim
        };
        spans.push(Span::styled(format!("{} ({count})", tab.title()), style));
        spans.push(Span::raw("   "));
    }
    Line::from(spans)
}
