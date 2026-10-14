//! The Lyrics panel: synced lines following the line being sung, or plain text, whichever TIDAL
//! answered with (see `app::maybe_load_lyrics` and `lyrics.rs` for how the answer gets here).

use crate::app::{Connection, State};
use crate::browse::Phase;
use crate::lyrics::{self, LyricsState};
use phonia_ipc::{LyricLine, Lyrics};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

const NOTICE: &str = "Not synced to the music: TIDAL only has plain text for this track";
const NO_LYRICS: &str = "TIDAL has no lyrics for this track.";
const LOCAL_FILE: &str = "Lyrics come from TIDAL: this track is a local file.";
const RETRY_HINT: &str = "Leave and come back to this panel to try again.";

pub fn draw(state: &State, theme: &crate::theme::Theme, frame: &mut Frame, area: Rect) {
    let Some(status) = &state.status else {
        render(frame, area, "Not connected yet.", theme.dim);
        return;
    };
    let Some(track) = &status.track else {
        render(frame, area, "Nothing is playing.", theme.dim);
        return;
    };
    let Some(id) = lyrics::tidal_id(track) else {
        render(frame, area, LOCAL_FILE, theme.dim);
        return;
    };
    // A matching answer for this exact track -- loading, failed or done -- takes priority over
    // the state of the connection right now, the same way the library keeps showing what it
    // already loaded after a disconnect: only when there is nothing (yet) for this id does the
    // connection itself explain why.
    if let Some(current) = state.lyrics.as_ref().filter(|current| current.id == id) {
        draw_current(state, current, status.position_ms, theme, frame, area);
        return;
    }
    let text = if state.connection != Connection::Connected {
        "Not connected yet."
    } else if !state.has(phonia_ipc::CAP_CATALOG) {
        "This phoniad has no TIDAL login to show lyrics from."
    } else if !state.has(phonia_ipc::CAP_LYRICS) {
        "This phoniad cannot show lyrics: it needs protocol 1.10 (restart it after updating)."
    } else {
        "Loading..."
    };
    render(frame, area, text, theme.dim);
}

fn draw_current(
    state: &State,
    current: &LyricsState,
    position_ms: u64,
    theme: &crate::theme::Theme,
    frame: &mut Frame,
    area: Rect,
) {
    match &current.phase {
        Phase::Loading => render(frame, area, "Loading...", theme.dim),
        Phase::Failed(reason) => {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(reason.clone(), theme.error),
                    Line::styled(RETRY_HINT, theme.dim),
                ]),
                area,
            );
        }
        Phase::Done => match &current.lyrics {
            None => render(frame, area, NO_LYRICS, theme.dim),
            Some(lyrics) if lyrics.lines.is_empty() && lyrics.plain.is_none() => {
                render(frame, area, NO_LYRICS, theme.dim);
            }
            Some(lyrics) => draw_done(state, current, lyrics, position_ms, theme, frame, area),
        },
    }
}

fn draw_done(
    state: &State,
    current: &LyricsState,
    lyrics: &Lyrics,
    position_ms: u64,
    theme: &crate::theme::Theme,
    frame: &mut Frame,
    area: Rect,
) {
    let notice = lyrics::shows_notice(lyrics);
    let footer = lyrics::shows_footer(lyrics);
    let mut constraints = Vec::new();
    if notice {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));
    if footer {
        constraints.push(Constraint::Length(1));
    }
    let areas = Layout::vertical(constraints).split(area);
    let mut next = 0;
    if notice {
        frame.render_widget(Paragraph::new(Line::styled(NOTICE, theme.dim)), areas[next]);
        next += 1;
    }
    let body_area = areas[next];
    next += 1;
    let rows = usize::from(body_area.height);
    let panel_rows = crate::view::lyrics_panel_rows(state);
    let body_rows = lyrics::body_rows(lyrics, panel_rows);
    let max = lyrics::max_scroll(lyrics, body_rows);
    let lines = if lyrics.lines.is_empty() {
        let text = lyrics.plain.as_deref().unwrap_or("");
        let offset = current.scroll.unwrap_or(0).min(max);
        draw_plain(text, lyrics.right_to_left, offset, rows, theme)
    } else {
        let first = current
            .scroll
            .unwrap_or_else(|| lyrics::auto_offset(lyrics, position_ms, body_rows))
            .min(max);
        draw_synced(
            &lyrics.lines,
            lyrics.right_to_left,
            position_ms,
            first,
            rows,
            theme,
        )
    };
    frame.render_widget(Paragraph::new(lines), body_area);
    if footer {
        let provider = lyrics.provider.as_deref().unwrap_or("");
        frame.render_widget(
            Paragraph::new(Line::styled(format!("Lyrics via {provider}"), theme.dim)),
            areas[next],
        );
    }
}

/// The synced lines that fit in `rows`, starting at `first`: the line being sung marked `>` and in
/// `theme.accent`, the ones already sung dimmed, the ones still to come in the plain text style.
fn draw_synced<'a>(
    lines: &'a [LyricLine],
    rtl: bool,
    position_ms: u64,
    first: usize,
    rows: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'a>> {
    let current = lyrics::current_line(lines, position_ms);
    lines
        .iter()
        .enumerate()
        .skip(first)
        .take(rows.max(1))
        .map(|(index, line)| {
            let is_current = Some(index) == current;
            let marker = if is_current { ">" } else { " " };
            let style = match current {
                Some(cur) if index == cur => theme.accent,
                Some(cur) if index < cur => theme.dim,
                _ => theme.text,
            };
            styled_line(format!("{marker} {}", line.text), style, rtl)
        })
        .collect()
}

/// The plain-text lines that fit in `rows`, starting at `offset`.
fn draw_plain<'a>(
    text: &'a str,
    rtl: bool,
    offset: usize,
    rows: usize,
    theme: &crate::theme::Theme,
) -> Vec<Line<'a>> {
    lyrics::plain_lines(text)
        .into_iter()
        .skip(offset)
        .take(rows.max(1))
        .map(|line| styled_line(line.to_string(), theme.text, rtl))
        .collect()
}

fn styled_line<'a>(text: String, style: Style, rtl: bool) -> Line<'a> {
    let line = Line::styled(text, style);
    if rtl {
        line.alignment(Alignment::Right)
    } else {
        line
    }
}

fn render(frame: &mut Frame, area: Rect, text: &str, style: Style) {
    frame.render_widget(Paragraph::new(Line::styled(text, style)), area);
}
