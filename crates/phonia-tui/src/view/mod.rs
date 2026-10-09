//! Painting a state onto the screen.

mod browse;
mod help;
mod home;
mod library;
mod lyrics;
mod search;

/// How far the help can scroll on a screen `rows` tall.
pub fn help_overflow(rows: u16) -> usize {
    help::overflow(rows)
}

/// The rows the Lyrics panel's own body has, inside `state.size`: the main panel, less its
/// border. Shared with `app::scroll_lyrics` so the two cannot disagree about how much there is to
/// scroll (the notice and the footer, when either shows, take their own row out of this in turn --
/// see `lyrics::body_rows`).
pub fn lyrics_panel_rows(state: &State) -> usize {
    let area = areas(Rect::new(0, 0, state.size.0, state.size.1)).main;
    usize::from(area.height.saturating_sub(2))
}

use crate::app::{Connection, Focus, Section, State, seconds_left};
use crate::covers::{self, Covers};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui_image::Image;

/// The columns the sidebar takes: the longest section name and room for the border and the mark.
const SIDEBAR_WIDTH: u16 = 14;
/// The rows of the bar at the bottom: a border and four lines. Always this many, in every
/// connection state, so the main panel's own height never depends on what is playing or whether a
/// signal-path verdict has arrived yet.
const BAR_HEIGHT: u16 = 5;
/// The rows the now-playing header takes when there is no cover beside it to size it by (no
/// picker, no cover id, or not enough room for one): title, artist, quality and a blank
/// separator -- exactly what [`now_playing_header`] can ever produce, text never needing more
/// room the way an image would.
const HEADER_TEXT_ROWS: u16 = 4;

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

pub fn draw(state: &State, theme: &Theme, covers: &Covers, frame: &mut Frame) {
    let areas = areas(frame.area());
    draw_sidebar(state, theme, frame, areas.sidebar);
    draw_main(state, theme, covers, frame, areas.main);
    draw_bar(state, theme, frame, areas.bar);
    if state.help {
        help::draw(theme, state.help_scroll, frame);
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

fn draw_main(state: &State, theme: &Theme, covers: &Covers, frame: &mut Frame, area: Rect) {
    let focused = state.focus == Focus::Main;
    // Home, likewise, may have an album, a playlist or a folder opened from one of its blocks.
    if state.section() == Section::Home {
        let crumbs = state.home_views.titles();
        let title = std::iter::once(Section::Home.title())
            .chain(crumbs)
            .collect::<Vec<_>>()
            .join(" \u{203a} ");
        let block = panel(&title, focused, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if state.home_views.is_empty() {
            home::draw(state, theme, focused, frame, inner);
        } else {
            browse::draw(state, &state.home_views, theme, covers, frame, inner);
        }
        return;
    }
    // The search has its own layout inside the panel: the line, the tabs and the results, or an
    // album or a playlist opened from one of them.
    if state.section() == Section::Search {
        let crumbs = state.search_views.titles();
        let title = std::iter::once(Section::Search.title())
            .chain(crumbs)
            .collect::<Vec<_>>()
            .join(" \u{203a} ");
        let block = panel(&title, focused, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if state.search_views.is_empty() {
            search::draw(state, theme, frame, inner);
        } else {
            browse::draw(state, &state.search_views, theme, covers, frame, inner);
        }
        return;
    }
    // The library, likewise, may have an album or a playlist opened from it.
    if state.section() == Section::Library {
        let crumbs = state.library_views.titles();
        let title = std::iter::once(Section::Library.title())
            .chain(crumbs)
            .collect::<Vec<_>>()
            .join(" \u{203a} ");
        let block = panel(&title, focused, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if state.library_views.is_empty() {
            library::draw(state, theme, frame, inner);
        } else {
            browse::draw(state, &state.library_views, theme, covers, frame, inner);
        }
        return;
    }
    if state.section() == Section::Lyrics {
        let title = match state
            .status
            .as_ref()
            .and_then(|status| status.track.as_ref())
        {
            Some(track) => format!(
                "Lyrics \u{203a} {}",
                phonia_ipc::fmt::track_name(track.title.as_deref(), track.artist.as_deref(), None)
            ),
            None => "Lyrics".to_string(),
        };
        let block = panel(&title, focused, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        lyrics::draw(state, theme, frame, inner);
        return;
    }
    // The queue: the currently playing track's own cover, when there is one, above the list.
    let title = match &state.queue {
        Some(queue) => format!("Queue ({})", queue.items.len()),
        None => "Queue".to_string(),
    };
    let block = panel(&title, focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    draw_queue(state, theme, covers, frame, inner, focused);
}

/// The queue's entries, in play order; the currently playing track's own cover shows above the
/// list when there is one, there is room, and the terminal can show one at all.
fn draw_queue(
    state: &State,
    theme: &Theme,
    covers: &Covers,
    frame: &mut Frame,
    area: Rect,
    focused: bool,
) {
    let Some(queue) = &state.queue else {
        frame.render_widget(
            Paragraph::new(vec![Line::styled("Not connected yet.", theme.dim)]),
            area,
        );
        return;
    };
    let track = state
        .status
        .as_ref()
        .and_then(|status| status.track.as_ref());
    let cover = track.and_then(|track| track.cover.as_deref());
    // Same rule as an opened album's or playlist's header (see `browse::draw_track_list`):
    // reserved only when a cover could actually show here, never on whether it has arrived yet.
    let reserved = covers.picker().and_then(|picker| {
        let cells = covers::cover_size(area, picker.font_size())?;
        let url = covers::track_cover_url(cover, cells, picker.font_size())?;
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
            (Some((cover_area, url)), Some(header_area), list_area)
        }
        // No image to size the header by (no picker, no cover, or not enough room for one), but
        // there is still a track to name: reserved on "is one playing", never on whether a cover
        // could have shown -- the same discipline as the cover case above, just without a cover.
        None if track.is_some() && area.height >= HEADER_TEXT_ROWS => {
            let [header_area, list_area] =
                Layout::vertical([Constraint::Length(HEADER_TEXT_ROWS), Constraint::Min(0)])
                    .areas(area);
            (None, Some(header_area), list_area)
        }
        None => (None, None, area),
    };
    if let Some((cover_area, url)) = &cover_area
        && let Some(protocol) = covers.ready(url)
    {
        frame.render_widget(Image::new(protocol), *cover_area);
    }
    if let Some(header_area) = header_area {
        frame.render_widget(
            Paragraph::new(now_playing_header(state, theme)),
            header_area,
        );
    }
    let rows = usize::from(list_area.height);
    let cursor = focused.then(|| state.queue_cursor.selected());
    frame.render_widget(
        Paragraph::new(queue_lines(queue, cursor, rows, theme)),
        list_area,
    );
}

/// The currently playing track: its title (bold, the thing to look at), the artist on its own
/// quieter line beneath it, and the quality tier when it is streamed from TIDAL. Reserved
/// whenever a track is playing, cover or no cover (see `draw_queue`), so this is never called
/// with nothing to show.
fn now_playing_header<'a>(state: &State, theme: &Theme) -> Vec<Line<'a>> {
    let Some(track) = state
        .status
        .as_ref()
        .and_then(|status| status.track.as_ref())
    else {
        return Vec::new();
    };
    let title = track
        .title
        .clone()
        .or_else(|| track.source.clone())
        .unwrap_or_else(|| "?".to_string());
    let mut lines = vec![Line::styled(title, theme.accent)];
    if let Some(artist) = &track.artist {
        lines.push(Line::styled(format!("◉ {artist}"), theme.dim));
    }
    if let Some(quality) = track.quality {
        lines.push(Line::styled(
            phonia_ipc::fmt::stream_quality(&quality),
            theme.dim,
        ));
    }
    lines.push(Line::raw(""));
    lines
}

/// The first row to show so that `cursor` is on screen among `rows`: the top of the list until the
/// cursor would fall off the bottom, then just enough scrolled for it to be the last row.
pub fn first_visible(cursor: usize, rows: usize) -> usize {
    if rows == 0 {
        return 0;
    }
    (cursor + 1).saturating_sub(rows)
}

/// The queue in queue order, as many rows as fit, with the entry playing marked and, when the
/// list has the focus, the one under the cursor highlighted.
fn queue_lines<'a>(
    queue: &phonia_ipc::Queue,
    cursor: Option<usize>,
    rows: usize,
    theme: &Theme,
) -> Vec<Line<'a>> {
    if queue.items.is_empty() {
        return vec![Line::styled("The queue is empty.", theme.dim)];
    }
    let start = first_visible(cursor.unwrap_or(0), rows);
    queue
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(rows.max(1))
        .map(|(index, item)| {
            let current = queue.current == Some(item.id);
            let marker = if current { ">" } else { " " };
            let name = phonia_ipc::fmt::track_name(
                item.title.as_deref(),
                item.artist.as_deref(),
                Some(&item.source),
            );
            let length = item
                .duration_ms
                .map(|ms| format!("  [{}]", phonia_ipc::fmt::ms(ms)))
                .unwrap_or_default();
            let style = if cursor == Some(index) {
                theme.selected
            } else if current {
                theme.accent
            } else {
                theme.text
            };
            Line::styled(format!("{marker} {:>2}. {name}{length}", index + 1), style)
        })
        .collect()
}

/// What is playing: the state, the track, and how it is being delivered.
fn connected_line<'a>(state: &State, theme: &Theme) -> Line<'a> {
    if let Some(reason) = &state.last_error {
        return Line::styled(format!("Could not do that: {reason}"), theme.error);
    }
    if let Some(notice) = &state.notice {
        return Line::styled(notice.clone(), theme.accent);
    }
    let Some(status) = &state.status else {
        return Line::styled("Connected", theme.text);
    };
    let name = match &status.track {
        Some(track) => phonia_ipc::fmt::track_name(
            track.title.as_deref(),
            track.artist.as_deref(),
            track.source.as_deref(),
        ),
        None => "Nothing playing".to_string(),
    };
    // Format, rate and quality move to the signal-path line below, next to the device they
    // actually reached: saying them here too would mean every playing track reads them twice.
    Line::styled(format!("{}  {name}", state_word(status.state)), theme.text)
}

/// The signal path: what is playing, through what format, to which device, with the bit-perfect
/// verdict -- or as much of that as is known yet, down to nothing at all with no connection.
fn signal_line<'a>(state: &State, theme: &Theme, width: u16) -> Line<'a> {
    // The daemon's own last word on why playback just failed (e.g. a track the DAC refuses) takes
    // priority over anything `status` itself says: `TrackEnded` has likely already cleared the
    // track this error was about, so without this check the line would just go blank or show
    // whatever output was last known, losing the one thing worth saying right now.
    if let Some(error) = &state.playback_error {
        let text = truncate(&format!("\u{2716} {error}"), usize::from(width));
        return Line::styled(text, theme.error);
    }
    let Some(status) = &state.status else {
        return Line::raw("");
    };
    if let phonia_ipc::Output::Released { by } = &status.output {
        let text = match by {
            Some(by) => format!("Output released to {by}: resume to take it back"),
            None => "Output released: resume to take it back".to_string(),
        };
        return Line::styled(text, theme.dim);
    }
    let Some(track) = &status.track else {
        let Some(route) = &status.route else {
            return Line::raw("");
        };
        let mode = match route.mode {
            phonia_ipc::OutputMode::Exclusive => "exclusive",
            phonia_ipc::OutputMode::Shared => "shared, not bit-perfect",
            phonia_ipc::OutputMode::Unknown => "?",
        };
        return Line::styled(format!("Output: {} ({mode})", route.description), theme.dim);
    };

    let mut path = match &track.quality {
        Some(quality) => format!("TIDAL {}", phonia_ipc::fmt::stream_quality(quality)),
        None if track
            .source
            .as_deref()
            .is_some_and(|s| s.starts_with("tidal:")) =>
        {
            "TIDAL".to_string()
        }
        None => "file".to_string(),
    };
    if let Some(spec) = &status.spec {
        path.push(' ');
        path.push_str(&format!(
            "{}-bit / {}",
            spec.bits_per_sample,
            phonia_ipc::fmt::sample_rate(spec.sample_rate)
        ));
        if spec.channels != 2 {
            path.push_str(&format!(" {}ch", spec.channels));
        }
    }

    let Some(report) = status
        .sink_report
        .as_ref()
        .filter(|report| report.applies_to(status))
    else {
        let device = status
            .route
            .as_ref()
            .map(|route| format!(" {}", route.description))
            .unwrap_or_default();
        return Line::styled(format!("{path} \u{2192}{device}"), theme.dim);
    };

    let mut route_text = report.negotiated_format.clone();
    if let Some(rate) = report.resampled_to {
        route_text.push(' ');
        route_text.push_str(&phonia_ipc::fmt::sample_rate(rate));
    }
    let device = match &status.route {
        Some(route) if route.description != report.device => {
            format!("{} ({})", route.description, report.device)
        }
        _ => report.device.clone(),
    };
    let full_path = format!("{path} \u{2192} {route_text} \u{2192} {device}");

    let symbol = if report.bit_perfect {
        "\u{2714}"
    } else {
        "\u{2716}"
    };
    let verdict = format!("{symbol} {}", phonia_ipc::fmt::verdict(report));
    let verdict_style = if report.bit_perfect {
        theme.accent
    } else if report.mode == Some(phonia_ipc::OutputMode::Shared) {
        theme.warn
    } else {
        theme.error
    };

    let (fitted_path, fitted_verdict) = fit(&full_path, &verdict, width);
    Line::from(vec![
        Span::styled(fitted_path.clone(), theme.text),
        Span::raw(if fitted_path.is_empty() { "" } else { "  " }),
        Span::styled(fitted_verdict, verdict_style),
    ])
}

/// Fits `path` and `verdict` into `width` columns (separated by two spaces when both are shown):
/// the verdict is kept whole as long as it fits by itself, and `path` is truncated with `…` to
/// make room for it. Only when the verdict alone would not fit is it the one truncated instead.
fn fit(path: &str, verdict: &str, width: u16) -> (String, String) {
    const SEP: usize = 2;
    let width = usize::from(width);
    let verdict_len = verdict.chars().count();
    if path.is_empty() || verdict_len + SEP > width {
        return (String::new(), truncate(verdict, width));
    }
    (
        truncate(path, width - verdict_len - SEP),
        verdict.to_string(),
    )
}

/// `text`, or its first `width - 1` characters plus `…` when it is longer than `width`.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut truncated: String = text.chars().take(width - 1).collect();
    truncated.push('\u{2026}');
    truncated
}

/// `1:05 ████████░░░░░░░░ 5:43`, as wide as `width`; just the times when there is no room or the
/// length of the track is not known.
pub fn progress_line(position_ms: u64, duration_ms: Option<u64>, width: u16) -> String {
    let position = phonia_ipc::fmt::ms(position_ms);
    let Some(duration_ms) = duration_ms.filter(|ms| *ms > 0) else {
        return position;
    };
    let total = phonia_ipc::fmt::ms(duration_ms);
    // The times, and a space on each side of the bar.
    let taken = position.chars().count() + total.chars().count() + 2;
    let room = usize::from(width).saturating_sub(taken);
    if room < 4 {
        return format!("{position} / {total}");
    }
    let done = (u128::from(position_ms.min(duration_ms)) * room as u128 / u128::from(duration_ms))
        as usize;
    format!(
        "{position} {}{} {total}",
        "█".repeat(done),
        "░".repeat(room - done)
    )
}

/// The volume and the queue's modes, when they are not the plain ones.
fn flags(state: &State) -> String {
    let mut flags = Vec::new();
    if let Some(volume) = state.status.as_ref().and_then(|status| status.volume) {
        flags.push(if volume.muted {
            format!("vol {}% (muted)", volume.percent)
        } else {
            format!("vol {}%", volume.percent)
        });
    }
    if let Some(gain) = state.status.as_ref().and_then(|status| {
        status
            .track
            .as_ref()
            .and_then(|track| phonia_ipc::fmt::replay_gain(track, status.route.as_ref()))
    }) {
        flags.push(gain);
    }
    if let Some(queue) = &state.queue {
        if queue.shuffle {
            flags.push("shuffle".to_string());
        }
        match queue.repeat {
            phonia_ipc::Repeat::Off => {}
            phonia_ipc::Repeat::All => flags.push("repeat all".to_string()),
            phonia_ipc::Repeat::One => flags.push("repeat one".to_string()),
        }
        if queue.autoplay {
            flags.push("autoplay".to_string());
        }
    }
    flags.join("   ")
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

const KEYS: &str =
    "Space pause  n/p skip  </> seek  +/- volume  m mute  s shuffle  r repeat  ? help  q quit";

fn draw_bar(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let lines = match &state.connection {
        Connection::Connecting => vec![
            Line::styled("Connecting to phoniad...", theme.dim),
            Line::raw(""),
            Line::raw(""),
            Line::styled("? help   q quit", theme.dim),
        ],
        Connection::Connected => {
            let status = state.status.as_ref();
            let progress = match status {
                Some(status) if status.track.is_some() => {
                    progress_line(status.position_ms, status.duration_ms, area.width)
                }
                _ => String::new(),
            };
            let flags = flags(state);
            vec![
                connected_line(state, theme),
                signal_line(state, theme, area.width),
                Line::styled(progress, theme.accent),
                Line::from(vec![
                    Span::styled(KEYS, theme.dim),
                    Span::styled(
                        if flags.is_empty() {
                            String::new()
                        } else {
                            format!("   | {flags}")
                        },
                        theme.text,
                    ),
                ]),
            ]
        }
        Connection::Disconnected { reason, retry_in } => vec![
            Line::styled(format!("phoniad is not reachable: {reason}"), theme.error),
            Line::styled(
                format!(
                    "Trying again in {} s (R: now). Start the daemon with `phoniad`.",
                    seconds_left(*retry_in)
                ),
                theme.dim,
            ),
            Line::raw(""),
            Line::styled("? help   q quit", theme.dim),
        ],
        Connection::Refused { reason } => vec![
            Line::styled(format!("Cannot use this daemon: {reason}"), theme.error),
            Line::styled("R: try again", theme.dim),
            Line::raw(""),
            Line::styled("? help   q quit", theme.dim),
        ],
    };
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
            .draw(|frame| draw(state, &Theme::new(false), &Covers::disabled(), frame))
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

    fn press_key(state: &mut State, code: KeyCode) -> crate::app::Effects {
        update(state, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    #[test]
    fn the_screen_has_a_sidebar_a_main_panel_and_a_bar() {
        let screen = screen(&State::default(), 60, 12);
        for wanted in [
            "1 Home",
            "2 Queue",
            "3 Search",
            "4 Library",
            "5 Lyrics",
            "Nothing to continue yet",
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
        press(&mut state, '3');
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
                artist: None,
                duration_ms: Some(343_000),
                spec: phonia_ipc::Spec {
                    sample_rate: 44_100,
                    channels: 2,
                    bits_per_sample: 16,
                },
                gapless: false,
                quality: None,
                cover: None,
                replay_gain: None,
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
        assert!(text.contains("1:05 "), "{text}");
        assert!(text.contains(" 5:43"), "{text}");
        assert!(
            text.contains('█') && text.contains('░'),
            "the progress bar: {text}"
        );
        assert!(text.contains("TIDAL 16-bit / 44.1 kHz"), "{text}");
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
    fn the_queue_is_listed_in_queue_order_even_when_shuffled_with_the_current_one_marked() {
        use phonia_ipc::{ItemId, Queue, QueueItem, Repeat};
        let mut state = connected();
        press(&mut state, '2');
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
                            artist: None,
                            duration_ms: Some(65_000),
                            cover: None,
                        },
                        QueueItem {
                            id: ItemId(2),
                            source: "tidal:9".into(),
                            title: None,
                            artist: None,
                            duration_ms: None,
                            cover: None,
                        },
                    ],
                    order: vec![ItemId(2), ItemId(1)],
                    current: Some(ItemId(2)),
                    shuffle: false,
                    repeat: Repeat::Off,
                    autoplay: false,
                },
            }),
        );
        let text = screen(&state, 90, 12);
        // `order` is reversed, as if shuffled; the list is in queue order, which is what
        // `QueueMove` and `ctl queue list` use.
        let first = text.find("A  [1:05]").unwrap();
        let second = text.find("tidal:9").unwrap();
        assert!(first < second, "A is entry 1: {text}");
        assert!(text.contains(">  2. tidal:9"), "{text}");
        assert!(
            text.contains("Queue (2)"),
            "the title counts the entries: {text}"
        );
    }

    #[test]
    fn the_progress_bar_fills_in_proportion_and_fits_the_width() {
        // Halfway through, in a bar 20 wide: the times take 4 + 4 + 2, so 10 cells are left.
        let line = progress_line(30_000, Some(60_000), 20);
        assert_eq!(line, "0:30 █████░░░░░ 1:00");
        assert_eq!(line.chars().count(), 20);
        assert_eq!(progress_line(0, Some(60_000), 20), "0:00 ░░░░░░░░░░ 1:00");
        assert_eq!(
            progress_line(60_000, Some(60_000), 20),
            "1:00 ██████████ 1:00"
        );
    }

    #[test]
    fn a_position_past_the_end_does_not_overflow_the_bar() {
        let line = progress_line(90_000, Some(60_000), 20);
        assert_eq!(line.chars().count(), 20);
        assert!(line.contains("██████████"), "{line}");
    }

    #[test]
    fn without_a_length_or_room_it_is_just_the_times() {
        assert_eq!(progress_line(65_000, None, 80), "1:05");
        assert_eq!(progress_line(65_000, Some(0), 80), "1:05");
        assert_eq!(progress_line(65_000, Some(343_000), 12), "1:05 / 5:43");
        assert_eq!(progress_line(0, Some(60_000), 0), "0:00 / 1:00");
    }

    #[test]
    fn fit_keeps_the_verdict_whole_and_truncates_the_path_to_make_room() {
        let (path, verdict) = fit("a path that is much too long to fit anywhere", "OK", 20);
        assert_eq!(verdict, "OK");
        assert_eq!(path.chars().count() + verdict.chars().count() + 2, 20);
        assert!(path.ends_with('\u{2026}'), "{path}");
    }

    #[test]
    fn fit_leaves_a_short_path_untouched() {
        assert_eq!(
            fit("short", "OK", 80),
            ("short".to_string(), "OK".to_string())
        );
    }

    #[test]
    fn fit_truncates_the_verdict_only_once_it_alone_does_not_fit() {
        let (path, verdict) = fit("path", "a verdict too long for a narrow bar", 10);
        assert_eq!(path, "", "the path gives way entirely first");
        assert_eq!(verdict.chars().count(), 10);
        assert!(verdict.ends_with('\u{2026}'), "{verdict}");
    }

    #[test]
    fn fit_with_no_path_gives_the_verdict_the_whole_width() {
        assert_eq!(fit("", "OK", 10), (String::new(), "OK".to_string()));
    }

    #[test]
    fn the_volume_and_the_queue_modes_show_when_they_are_not_the_plain_ones() {
        use phonia_ipc::{Queue, Repeat, Volume};
        let mut state = connected();
        assert_eq!(
            flags(&state),
            "",
            "an exclusive card and a plain queue: nothing to say"
        );

        state.status.as_mut().unwrap().volume = Some(Volume {
            percent: 72,
            muted: false,
        });
        assert_eq!(flags(&state), "vol 72%");
        state.status.as_mut().unwrap().volume = Some(Volume {
            percent: 72,
            muted: true,
        });
        state.queue = Some(Queue {
            shuffle: true,
            repeat: Repeat::All,
            ..state.queue.clone().unwrap()
        });
        assert_eq!(flags(&state), "vol 72% (muted)   shuffle   repeat all");
        let text = screen(&state, 140, 12);
        assert!(
            text.contains("| vol 72% (muted)   shuffle   repeat all"),
            "{text}"
        );

        state.queue = Some(Queue {
            autoplay: true,
            ..state.queue.clone().unwrap()
        });
        assert_eq!(
            flags(&state),
            "vol 72% (muted)   shuffle   repeat all   autoplay"
        );
    }

    #[test]
    fn a_gain_actually_applied_in_shared_mode_is_shown_next_to_the_volume() {
        use phonia_ipc::{GainKind, OutputMode, ReplayGain, Route, Track};
        let mut state = connected();
        let gained_track = Track {
            item_id: None,
            source: Some("tidal:1".into()),
            title: Some("Song".into()),
            artist: None,
            duration_ms: None,
            quality: None,
            cover: None,
            replay_gain: Some(ReplayGain {
                kind: GainKind::Track,
                millibels: -600,
            }),
        };
        state.status.as_mut().unwrap().track = Some(gained_track.clone());
        assert_eq!(
            flags(&state),
            "",
            "no route yet: never shown without knowing the output is shared"
        );

        state.status.as_mut().unwrap().route = Some(Route {
            id: "exclusive:hw:1,0".into(),
            mode: OutputMode::Exclusive,
            description: "DS2".into(),
        });
        assert_eq!(
            flags(&state),
            "",
            "decided but never applied in exclusive mode: not shown"
        );

        state.status.as_mut().unwrap().route = Some(Route {
            id: "shared:default".into(),
            mode: OutputMode::Shared,
            description: "Speakers".into(),
        });
        assert_eq!(flags(&state), "RG -6.0 dB (track)");
    }

    #[test]
    fn the_controls_are_listed_in_the_bar() {
        let text = screen(&connected(), 140, 12);
        for wanted in [
            "Space pause",
            "n/p skip",
            "</> seek",
            "+/- volume",
            "m mute",
            "s shuffle",
            "r repeat",
        ] {
            assert!(text.contains(wanted), "{wanted:?} missing from:\n{text}");
        }
    }

    #[test]
    fn a_fallback_shows_what_was_asked_for_next_to_the_format() {
        let mut state = connected();
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::TrackStarted {
                item_id: None,
                source: None,
                title: Some("Song".into()),
                artist: None,
                duration_ms: Some(60_000),
                spec: phonia_ipc::Spec {
                    sample_rate: 96_000,
                    channels: 2,
                    bits_per_sample: 24,
                },
                gapless: false,
                quality: Some(phonia_ipc::StreamQuality {
                    requested: phonia_ipc::Quality::Hires,
                    delivered: phonia_ipc::Quality::Lossless,
                }),
                cover: None,
                replay_gain: None,
            }),
        );
        let text = screen(&state, 120, 12);
        assert!(
            text.contains("TIDAL lossless (asked for hires) 24-bit / 96 kHz"),
            "{text}"
        );
    }

    /// A connected state with `track` already started at `spec`, so a `SinkReport` sent next has
    /// something to apply to.
    fn playing(spec: phonia_ipc::Spec) -> State {
        let mut state = connected();
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::TrackStarted {
                item_id: None,
                source: Some("tidal:1".into()),
                title: Some("Aerodynamic".into()),
                artist: None,
                duration_ms: Some(343_000),
                spec,
                gapless: false,
                quality: None,
                cover: None,
                replay_gain: None,
            }),
        );
        state
    }

    const SPEC_96K: phonia_ipc::Spec = phonia_ipc::Spec {
        sample_rate: 96_000,
        channels: 2,
        bits_per_sample: 24,
    };

    fn a_sink_report(bit_perfect: bool, mode: phonia_ipc::OutputMode) -> phonia_ipc::SinkReport {
        phonia_ipc::SinkReport {
            device: "hw:1,0".into(),
            source: SPEC_96K,
            negotiated_format: "S24_3LE".into(),
            bit_perfect,
            problem: if bit_perfect {
                None
            } else {
                Some("the card reports 48000 Hz instead of 96000 Hz".into())
            },
            hw_params: None,
            mode: Some(mode),
            resampled_to: None,
            codec: None,
            lossy: false,
            output: None,
        }
    }

    #[test]
    fn a_bit_perfect_verdict_shows_on_its_own_line() {
        let mut state = playing(SPEC_96K);
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::SinkReport(a_sink_report(
                true,
                phonia_ipc::OutputMode::Exclusive,
            ))),
        );
        let text = screen(&state, 120, 12);
        assert!(text.contains("TIDAL 24-bit / 96 kHz"), "{text}");
        assert!(text.contains("S24_3LE"), "{text}");
        assert!(text.contains("hw:1,0"), "{text}");
        assert!(text.contains("BIT-PERFECT"), "{text}");
    }

    #[test]
    fn a_converted_verdict_names_the_reason() {
        let mut state = playing(SPEC_96K);
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::SinkReport(a_sink_report(
                false,
                phonia_ipc::OutputMode::Exclusive,
            ))),
        );
        let text = screen(&state, 120, 12);
        assert!(
            text.contains("CONVERTED (the card reports 48000 Hz instead of 96000 Hz)"),
            "{text}"
        );
    }

    #[test]
    fn a_shared_mode_verdict_is_not_treated_as_an_error() {
        let mut state = playing(SPEC_96K);
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::SinkReport(a_sink_report(
                false,
                phonia_ipc::OutputMode::Shared,
            ))),
        );
        let text = screen(&state, 120, 12);
        assert!(text.contains("SHARED"), "{text}");
    }

    #[test]
    fn a_released_output_says_so_and_a_track_with_no_report_yet_ends_in_an_arrow() {
        let mut state = playing(SPEC_96K);
        let text = screen(&state, 120, 12);
        assert!(
            text.contains("TIDAL 24-bit / 96 kHz \u{2192}"),
            "no report has arrived yet: {text}"
        );

        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::OutputReleased {
                by: Some("jackd".into()),
                reason: phonia_ipc::ReleaseReason::Requested,
            }),
        );
        let text = screen(&state, 120, 12);
        assert!(
            text.contains("Output released to jackd: resume to take it back"),
            "{text}"
        );
    }

    #[test]
    fn a_playback_error_shows_on_the_signal_path_line_and_a_fresh_track_clears_it() {
        let mut state = playing(SPEC_96K);
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::TrackEnded {
                item_id: None,
                reason: phonia_ipc::EndReason::Failed,
            }),
        );
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::Error {
                message: "hw:1,0 cannot play 352800 Hz natively".into(),
            }),
        );
        let text = screen(&state, 120, 12);
        assert!(
            text.contains("hw:1,0 cannot play 352800 Hz natively"),
            "{text}"
        );

        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::TrackStarted {
                item_id: None,
                source: Some("tidal:2".into()),
                title: Some("Another Song".into()),
                artist: None,
                duration_ms: None,
                spec: SPEC_96K,
                gapless: false,
                quality: None,
                cover: None,
                replay_gain: None,
            }),
        );
        let text = screen(&state, 120, 12);
        assert!(
            !text.contains("cannot play 352800 Hz"),
            "a track starting replaces the stale error: {text}"
        );
    }

    #[test]
    fn a_long_playback_error_is_truncated_not_wrapped_or_cut_off_silently() {
        let mut state = playing(SPEC_96K);
        let long = "hw:1,0 cannot play 352800 Hz natively; for 24-bit audio it can do 44100, \
            48000, 88200, 96000, 176400, 192000 Hz. phonia does not resample in exclusive mode; \
            to hear it resampled, play through the sound server (`phonia ctl output set \
            shared:default`).";
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::Error {
                message: long.into(),
            }),
        );
        let text = screen(&state, 60, 12);
        let line = text
            .lines()
            .find(|line| line.contains("cannot play"))
            .expect("the error is on some line");
        assert!(line.chars().count() <= 60, "{line:?}");
        assert!(line.ends_with('\u{2026}'), "{line:?}");
    }

    #[test]
    fn a_sink_report_for_a_different_output_than_the_one_switched_to_is_not_shown() {
        // The race #28's own daemon-side fix exists for: a report can arrive stamped with the
        // output that is going away, right as `OutputChanged` names the new one. Whichever order
        // they come in, the old device's verdict must never be shown as the new one's.
        let mut state = playing(SPEC_96K);
        let mut report = a_sink_report(true, phonia_ipc::OutputMode::Exclusive);
        report.output = Some("exclusive:hw:1,0".into());
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::SinkReport(report)),
        );
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::OutputChanged {
                route: phonia_ipc::Route {
                    id: "exclusive:hw:2,0".into(),
                    mode: phonia_ipc::OutputMode::Exclusive,
                    description: "Other DAC".into(),
                },
            }),
        );
        let text = screen(&state, 120, 12);
        assert!(!text.contains("BIT-PERFECT"), "{text}");

        // The other order: the route changes first, the stale report for the old output arrives
        // after -- still not shown, since the ids still disagree.
        let mut state = playing(SPEC_96K);
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::OutputChanged {
                route: phonia_ipc::Route {
                    id: "exclusive:hw:2,0".into(),
                    mode: phonia_ipc::OutputMode::Exclusive,
                    description: "Other DAC".into(),
                },
            }),
        );
        let mut report = a_sink_report(true, phonia_ipc::OutputMode::Exclusive);
        report.output = Some("exclusive:hw:1,0".into());
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::SinkReport(report)),
        );
        let text = screen(&state, 120, 12);
        assert!(!text.contains("BIT-PERFECT"), "{text}");
    }

    #[test]
    fn the_quit_key_is_on_the_same_row_in_every_connection_state_and_with_or_without_a_report() {
        // The bar reserves a fixed 4 content rows in every state (connecting, connected,
        // disconnected, refused) so the row the last line of the bar lands on -- "? help q quit"
        // or its equivalent -- never moves, the same discipline #24's covers work established for
        // the main panel.
        let (width, height) = (100, 24);
        let row_of = |text: &str, needle: &str| {
            text.lines()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} not found in:\n{text}"))
        };

        let connecting_row = row_of(&screen(&State::default(), width, height), "q quit");

        let mut disconnected = connected();
        update(
            &mut disconnected,
            Msg::Disconnected {
                reason: "x".into(),
                retry_in: std::time::Duration::from_secs(1),
            },
        );
        let disconnected_row = row_of(&screen(&disconnected, width, height), "q quit");

        let not_playing_row = row_of(&screen(&connected(), width, height), "q quit");

        let mut playing_no_report = playing(SPEC_96K);
        let playing_no_report_row = row_of(&screen(&playing_no_report, width, height), "q quit");

        update(
            &mut playing_no_report,
            Msg::Daemon(phonia_ipc::Event::SinkReport(a_sink_report(
                true,
                phonia_ipc::OutputMode::Exclusive,
            ))),
        );
        let with_report_row = row_of(&screen(&playing_no_report, width, height), "q quit");

        assert_eq!(
            [
                connecting_row,
                disconnected_row,
                not_playing_row,
                playing_no_report_row,
                with_report_row
            ],
            [connecting_row; 5]
        );
    }

    #[test]
    fn on_a_short_terminal_the_help_scrolls_to_show_every_key() {
        let mut state = State::default();
        update(&mut state, Msg::Resize(100, 24));
        press(&mut state, '?');
        let top = screen(&state, 100, 24);
        assert!(top.contains("j/k scroll"), "{top}");
        assert!(top.contains("General"), "{top}");
        assert!(
            !top.contains("delete everything before the cursor"),
            "the last line is cut off at the bottom:\n{top}"
        );

        state.help_scroll = crate::app::help_max_scroll(&state);
        let bottom = screen(&state, 100, 24);
        assert!(
            bottom.contains("delete everything before the cursor"),
            "{bottom}"
        );
        assert!(bottom.contains("While typing a search"), "{bottom}");
        // What was on top has scrolled out of view.
        assert!(!bottom.contains("General"), "{bottom}");
    }

    #[test]
    fn on_a_tall_terminal_the_whole_help_shows_without_scrolling() {
        let mut state = State::default();
        update(&mut state, Msg::Resize(100, 70));
        press(&mut state, '?');
        let text = screen(&state, 100, 70);
        assert!(!text.contains("j/k scroll"), "{text}");
        for wanted in [
            "General",
            "Movement",
            "Panels",
            "Playback",
            "repeat: off, all, one",
        ] {
            assert!(text.contains(wanted), "{wanted:?} missing:\n{text}");
        }
    }

    fn long_queue(count: u64) -> State {
        use phonia_ipc::{ItemId, Queue, QueueItem, Repeat};
        let mut state = connected();
        let items: Vec<QueueItem> = (1..=count)
            .map(|n| QueueItem {
                id: ItemId(n),
                source: format!("file:/t{n}.flac"),
                title: Some(format!("Track {n}")),
                artist: None,
                duration_ms: None,
                cover: None,
            })
            .collect();
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::QueueChanged {
                queue: Queue {
                    version: 2,
                    order: items.iter().map(|item| item.id).collect(),
                    items,
                    current: None,
                    shuffle: false,
                    repeat: Repeat::Off,
                    autoplay: false,
                },
            }),
        );
        press(&mut state, '2');
        state
    }

    #[test]
    fn the_first_visible_row_keeps_the_cursor_on_screen() {
        assert_eq!(first_visible(0, 10), 0);
        assert_eq!(first_visible(9, 10), 0, "the last row that fits");
        assert_eq!(first_visible(10, 10), 1);
        assert_eq!(first_visible(25, 10), 16);
        assert_eq!(first_visible(5, 0), 0, "no rows: nothing to scroll");
    }

    #[test]
    fn a_long_queue_scrolls_to_the_cursor() {
        let mut state = long_queue(40);
        press(&mut state, 'l');
        // 12 rows of terminal leave 6 for the list (borders and the bar take the rest).
        let top = screen(&state, 80, 12);
        assert!(top.contains(" 1. Track 1 "), "{top}");
        assert!(!top.contains("Track 30"), "{top}");

        for _ in 0..30 {
            press(&mut state, 'j');
        }
        let scrolled = screen(&state, 80, 12);
        assert!(
            scrolled.contains("Track 31"),
            "the cursor row is on screen:\n{scrolled}"
        );
        assert!(!scrolled.contains("Track 1 "), "{scrolled}");
    }

    #[test]
    fn the_cursor_row_is_highlighted_only_while_the_list_has_the_focus() {
        let mut state = long_queue(3);
        let theme = Theme::new(false);
        let has_reverse_row = |state: &State| {
            let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
            terminal
                .draw(|frame| draw(state, &theme, &Covers::disabled(), frame))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..14).any(|y| {
                (14..40).any(|x| {
                    buffer[(x, y)]
                        .modifier
                        .contains(ratatui::style::Modifier::REVERSED)
                })
            })
        };
        assert!(
            !has_reverse_row(&state),
            "focus is on the sidebar: no cursor row"
        );
        press(&mut state, 'l');
        assert!(
            has_reverse_row(&state),
            "focus is on the list: the cursor row shows"
        );
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

    // --- The search section ------------------------------------------------------------------

    fn in_the_search() -> State {
        let mut state = State::default();
        update(
            &mut state,
            Msg::Connected {
                server: phonia_ipc::ServerInfo {
                    name: "phoniad".into(),
                    version: "0.1.0".into(),
                    pid: 1,
                },
                protocol: phonia_ipc::Version { major: 1, minor: 6 },
                capabilities: vec!["catalog".into()],
                status: crate::app::tests_support::status(),
                queue: crate::app::tests_support::queue(),
            },
        );
        press(&mut state, '3');
        state
    }

    /// The search of "korn" done, with two tracks and one album found.
    fn with_results() -> State {
        use crate::app::{Tag, submit_search};
        let mut state = in_the_search();
        submit_search(&mut state, "korn");
        let generation = state.search.generation;
        let track = |id: &str, title: &str| phonia_ipc::TrackSummary {
            id: id.into(),
            title: title.into(),
            version: None,
            artists: vec![phonia_ipc::ArtistRef {
                id: "780".into(),
                name: "Korn".into(),
            }],
            album: Some(phonia_ipc::AlbumRef {
                id: "9".into(),
                title: "Issues".into(),
                cover: None,
            }),
            duration_ms: Some(271_000),
            explicit: false,
            track_number: None,
            volume_number: None,
            quality: Some(phonia_ipc::Quality::Hires),
            streamable: true,
        };
        update(
            &mut state,
            Msg::Response {
                tag: Tag::Search { generation },
                result: Ok(phonia_ipc::Payload::SearchResults {
                    query: "korn".into(),
                    tracks: Some(phonia_ipc::Page {
                        items: vec![track("1", "Falling Away from Me"), track("2", "Dead")],
                        total: 123,
                        offset: 0,
                    }),
                    albums: Some(phonia_ipc::Page {
                        items: vec![phonia_ipc::AlbumSummary {
                            id: "9".into(),
                            title: "Issues".into(),
                            version: None,
                            artists: vec![phonia_ipc::ArtistRef {
                                id: "780".into(),
                                name: "Korn".into(),
                            }],
                            release_date: Some("1999-11-16".into()),
                            track_count: Some(16),
                            duration_ms: None,
                            explicit: true,
                            quality: Some(phonia_ipc::Quality::Hires),
                            kind: None,
                            copyright: None,
                            cover: None,
                        }],
                        total: 20,
                        offset: 0,
                    }),
                    artists: Some(phonia_ipc::Page {
                        items: vec![],
                        total: 0,
                        offset: 0,
                    }),
                    playlists: None,
                }),
            },
        );
        state
    }

    #[test]
    fn before_a_search_the_section_invites_to_start_one() {
        let text = screen(&in_the_search(), 80, 14);
        assert!(text.contains("Press / to search TIDAL."), "{text}");
        assert!(
            text.contains("Tracks") && text.contains("Playlists"),
            "{text}"
        );
    }

    #[test]
    fn what_is_typed_shows_after_the_prompt_with_the_cursor_at_its_end() {
        let mut state = in_the_search();
        press(&mut state, '/');
        for c in "korn".chars() {
            press(&mut state, c);
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        terminal
            .draw(|frame| draw(&state, &Theme::new(false), &Covers::disabled(), frame))
            .unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        // The sidebar (14) and the panel's border (1), then "/ " and four letters, on the first
        // row inside the border.
        assert_eq!((cursor.x, cursor.y), (14 + 1 + 2 + 4, 1));
        let text = screen(&state, 80, 14);
        assert!(text.contains("/ korn"), "{text}");
    }

    #[test]
    fn a_search_under_way_says_so() {
        let mut state = in_the_search();
        crate::app::submit_search(&mut state, "korn");
        let text = screen(&state, 80, 14);
        assert!(text.contains("Searching..."), "{text}");
        assert!(text.contains("/ "), "{text}");
    }

    #[test]
    fn a_failure_is_shown_where_the_results_would_be() {
        let mut state = in_the_search();
        crate::app::submit_search(&mut state, "korn");
        let generation = state.search.generation;
        update(
            &mut state,
            Msg::Response {
                tag: crate::app::Tag::Search { generation },
                result: Err("not logged in to TIDAL (no session): run `phonia login`".into()),
            },
        );
        let text = screen(&state, 100, 14);
        assert!(text.contains("run `phonia login`"), "{text}");
    }

    #[test]
    fn the_results_are_listed_with_the_counts_in_the_tabs() {
        let state = with_results();
        let text = screen(&state, 120, 14);
        assert!(text.contains("Tracks (123)"), "{text}");
        assert!(text.contains("Albums (20)"), "{text}");
        assert!(text.contains("Artists (0)"), "{text}");
        assert!(
            text.contains("1. Korn - Falling Away from Me - Issues - 4:31 - hires"),
            "{text}"
        );
        assert!(text.contains("2. Korn - Dead"), "{text}");
    }

    #[test]
    fn another_tab_shows_its_own_list_and_an_empty_one_says_nothing_was_found() {
        let mut state = with_results();
        press(&mut state, ']');
        let text = screen(&state, 120, 14);
        assert!(
            text.contains("1. Korn - Issues - 1999 - 16 tracks - hires - explicit"),
            "{text}"
        );
        assert!(!text.contains("Falling Away from Me"), "{text}");

        press(&mut state, ']');
        let text = screen(&state, 120, 14);
        assert!(text.contains("No artists for \"korn\"."), "{text}");
    }

    fn in_the_library() -> State {
        let mut state = State::default();
        update(
            &mut state,
            Msg::Connected {
                server: phonia_ipc::ServerInfo {
                    name: "phoniad".into(),
                    version: "0.1.0".into(),
                    pid: 1,
                },
                protocol: phonia_ipc::Version { major: 1, minor: 6 },
                capabilities: vec!["catalog".into()],
                status: crate::app::tests_support::status(),
                queue: crate::app::tests_support::queue(),
            },
        );
        press(&mut state, '4');
        state
    }

    /// The library loaded: one favorite track, one favorite album, one playlist.
    fn with_library() -> State {
        use crate::app::Tag;
        let mut state = in_the_library();
        update(
            &mut state,
            Msg::Response {
                tag: Tag::Library { generation: 0 },
                result: Ok(phonia_ipc::Payload::Library {
                    favorite_tracks: phonia_ipc::Page {
                        items: vec![phonia_ipc::TrackSummary {
                            id: "1".into(),
                            title: "Freak On a Leash".into(),
                            version: None,
                            artists: vec![phonia_ipc::ArtistRef {
                                id: "780".into(),
                                name: "Korn".into(),
                            }],
                            album: None,
                            duration_ms: Some(212_000),
                            explicit: false,
                            track_number: None,
                            volume_number: None,
                            quality: Some(phonia_ipc::Quality::Hires),
                            streamable: true,
                        }],
                        total: 42,
                        offset: 0,
                    },
                    favorite_albums: phonia_ipc::Page {
                        items: vec![phonia_ipc::AlbumSummary {
                            id: "9".into(),
                            title: "Issues".into(),
                            version: None,
                            artists: vec![],
                            release_date: Some("1999-11-16".into()),
                            track_count: Some(16),
                            duration_ms: None,
                            explicit: false,
                            quality: None,
                            kind: None,
                            copyright: None,
                            cover: None,
                        }],
                        total: 1,
                        offset: 0,
                    },
                    my_playlists: phonia_ipc::Page {
                        items: vec![],
                        total: 0,
                        offset: 0,
                    },
                }),
            },
        );
        update(
            &mut state,
            Msg::Response {
                tag: Tag::LibraryPlaylists { generation: 0 },
                result: Ok(phonia_ipc::Payload::PlaylistFolder {
                    folder: None,
                    page: phonia_ipc::Page {
                        items: vec![phonia_ipc::FolderEntry::Playlist(
                            phonia_ipc::PlaylistSummary {
                                id: "p-1".into(),
                                title: "Road trip".into(),
                                creator: None,
                                description: None,
                                track_count: Some(10),
                                duration_ms: None,
                                cover: None,
                            },
                        )],
                        total: 1,
                        offset: 0,
                    },
                }),
            },
        );
        update(
            &mut state,
            Msg::Response {
                tag: Tag::LibraryArtists { generation: 0 },
                result: Ok(phonia_ipc::Payload::Artists {
                    from: phonia_ipc::ArtistListRef::FavoriteArtists,
                    page: phonia_ipc::Page {
                        items: vec![phonia_ipc::ArtistSummary {
                            id: "780".into(),
                            name: "Korn".into(),
                            picture: None,
                        }],
                        total: 1,
                        offset: 0,
                    },
                }),
            },
        );
        state
    }

    #[test]
    fn before_the_library_loads_it_says_so() {
        let text = screen(&in_the_library(), 100, 14);
        assert!(text.contains("Loading..."), "{text}");
        assert!(
            text.contains("Favorite tracks") && text.contains("Your playlists"),
            "{text}"
        );
    }

    #[test]
    fn a_library_without_a_catalog_says_why_instead_of_loading_forever() {
        let mut state = State::default();
        update(
            &mut state,
            Msg::Connected {
                server: phonia_ipc::ServerInfo {
                    name: "phoniad".into(),
                    version: "0.1.0".into(),
                    pid: 1,
                },
                protocol: phonia_ipc::Version { major: 1, minor: 6 },
                capabilities: vec![],
                status: crate::app::tests_support::status(),
                queue: crate::app::tests_support::queue(),
            },
        );
        press(&mut state, '4');
        let text = screen(&state, 80, 14);
        assert!(text.contains("no TIDAL login"), "{text}");
    }

    #[test]
    fn the_librarys_lists_are_shown_with_counts_in_the_tabs() {
        let state = with_library();
        let text = screen(&state, 120, 14);
        assert!(text.contains("Favorite tracks (42)"), "{text}");
        assert!(text.contains("Favorite albums (1)"), "{text}");
        assert!(text.contains("Favorite artists (1)"), "{text}");
        assert!(text.contains("Your playlists (1)"), "{text}");
        assert!(text.contains("1. Korn - Freak On a Leash"), "{text}");
    }

    #[test]
    fn the_favorite_artists_tab_lists_them_and_opens_one() {
        use crate::app::Tag;
        let mut state = with_library();
        press(&mut state, 'l');
        press(&mut state, ']'); // favorite albums
        press(&mut state, ']'); // favorite artists
        let text = screen(&state, 120, 14);
        assert!(text.contains("1. Korn"), "{text}");
        let (tag, _) = tagged_request(press_key(&mut state, KeyCode::Enter));
        assert!(matches!(tag, Tag::View { .. }));
        let text = screen(&state, 100, 14);
        assert!(text.contains("Library \u{203a} Korn"), "{text}");
    }

    #[test]
    fn opening_an_album_from_the_library_shows_its_tracks() {
        use crate::app::Tag;
        let mut state = with_library();
        press(&mut state, 'l');
        press(&mut state, ']'); // favorite albums
        let (tag, _) = tagged_request(press_key(&mut state, KeyCode::Enter));
        assert!(matches!(tag, Tag::View { .. }));
        let text = screen(&state, 100, 14);
        assert!(text.contains("Library \u{203a} Issues"), "{text}");
        assert!(text.contains("Loading..."), "{text}");
    }

    #[test]
    fn a_sub_folder_shows_its_own_breadcrumb_and_tells_a_folder_row_from_a_playlist_row() {
        use crate::app::Tag;
        let mut state = in_the_library();
        update(
            &mut state,
            Msg::Response {
                tag: Tag::Library { generation: 0 },
                result: Ok(phonia_ipc::Payload::Library {
                    favorite_tracks: phonia_ipc::Page {
                        items: vec![],
                        total: 0,
                        offset: 0,
                    },
                    favorite_albums: phonia_ipc::Page {
                        items: vec![],
                        total: 0,
                        offset: 0,
                    },
                    my_playlists: phonia_ipc::Page {
                        items: vec![],
                        total: 0,
                        offset: 0,
                    },
                }),
            },
        );
        update(
            &mut state,
            Msg::Response {
                tag: Tag::LibraryPlaylists { generation: 0 },
                result: Ok(phonia_ipc::Payload::PlaylistFolder {
                    folder: None,
                    page: phonia_ipc::Page {
                        items: vec![phonia_ipc::FolderEntry::Folder {
                            id: "f1".into(),
                            name: "Moods".into(),
                            item_count: 1,
                        }],
                        total: 1,
                        offset: 0,
                    },
                }),
            },
        );
        press(&mut state, 'l');
        press(&mut state, ']'); // favorite albums
        press(&mut state, ']'); // favorite artists
        press(&mut state, ']'); // playlists (the root of "My Collection")
        let text = screen(&state, 100, 14);
        assert!(text.contains("Moods/ (1 item)"), "root row: {text}");

        let (tag, request) = tagged_request(press_key(&mut state, KeyCode::Enter));
        assert_eq!(
            request,
            phonia_ipc::Request::PlaylistFolder {
                folder: Some("f1".into()),
                offset: 0,
                limit: Some(crate::library::PAGE_SIZE),
            }
        );
        let text = screen(&state, 100, 14);
        assert!(text.contains("Library \u{203a} Moods"), "{text}");
        assert!(text.contains("Loading..."), "{text}");

        update(
            &mut state,
            Msg::Response {
                tag,
                result: Ok(phonia_ipc::Payload::PlaylistFolder {
                    folder: Some("f1".into()),
                    page: phonia_ipc::Page {
                        items: vec![phonia_ipc::FolderEntry::Playlist(
                            phonia_ipc::PlaylistSummary {
                                id: "p-inside".into(),
                                title: "Dark Jazz".into(),
                                creator: None,
                                description: None,
                                track_count: Some(75),
                                duration_ms: None,
                                cover: None,
                            },
                        )],
                        total: 1,
                        offset: 0,
                    },
                }),
            },
        );
        let text = screen(&state, 100, 14);
        assert!(text.contains("Dark Jazz"), "{text}");
        assert!(
            !text.contains("item)"),
            "no folder row left to show: {text}"
        );
    }

    #[test]
    fn the_row_under_the_cursor_is_highlighted_only_when_the_list_has_the_focus_and_no_typing() {
        let theme = Theme::new(false);
        let reversed_rows = |state: &State| {
            let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
            terminal
                .draw(|frame| draw(state, &theme, &Covers::disabled(), frame))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..14)
                .filter(|y| {
                    (16..60).any(|x| {
                        buffer[(x, *y)]
                            .modifier
                            .contains(ratatui::style::Modifier::REVERSED)
                    })
                })
                .count()
        };
        let mut state = with_results();
        press(&mut state, 'l');
        assert_eq!(reversed_rows(&state), 1, "the list has the focus");
        press(&mut state, '/');
        assert_eq!(reversed_rows(&state), 0, "typing: no row is selected");
        update(
            &mut state,
            Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        press(&mut state, 'h');
        assert_eq!(reversed_rows(&state), 0, "the focus is on the sidebar");
    }

    #[test]
    fn a_narrow_terminal_does_not_break_the_search_screen() {
        let mut state = with_results();
        press(&mut state, '/');
        for (w, h) in [(0, 0), (1, 1), (16, 4), (20, 5), (40, 8)] {
            let _ = screen(&state, w, h);
        }
    }

    #[test]
    fn a_notice_shows_in_the_bar_and_an_error_takes_its_place() {
        let mut state = connected();
        state.notice = Some("Added 12 tracks".into());
        let text = screen(&state, 100, 12);
        assert!(text.contains("Added 12 tracks"), "{text}");
        assert!(
            !text.contains("Nothing playing"),
            "the notice replaces the status line:\n{text}"
        );

        state.last_error = Some("TIDAL has no such item".into());
        let text = screen(&state, 100, 12);
        assert!(
            text.contains("Could not do that: TIDAL has no such item"),
            "{text}"
        );
        assert!(!text.contains("Added 12 tracks"), "{text}");
    }

    fn tagged_request(effects: crate::app::Effects) -> (crate::app::Tag, phonia_ipc::Request) {
        match effects.commands.as_slice() {
            [crate::app::Cmd::Request { tag, request }] => (*tag, request.clone()),
            other => panic!("expected one tagged request, got {other:?}"),
        }
    }

    /// The album from `with_results` opened and loaded with three tracks, the second not
    /// streamable, and the cursor on the first.
    fn open_album_view() -> State {
        use crate::app::Tag;
        let mut state = with_results();
        press(&mut state, 'l'); // give the results the focus, so Enter can open one
        press(&mut state, ']'); // Albums
        let (tag, _) = tagged_request(press_key(&mut state, KeyCode::Enter));
        let Tag::View { serial } = tag else {
            panic!("not a view tag")
        };
        let track =
            |id: &str, title: &str, number: u32, streamable: bool| phonia_ipc::TrackSummary {
                id: id.into(),
                title: title.into(),
                version: None,
                artists: vec![],
                album: None,
                duration_ms: Some(75_000),
                explicit: false,
                track_number: Some(number),
                volume_number: Some(1),
                quality: Some(phonia_ipc::Quality::Hires),
                streamable,
            };
        update(
            &mut state,
            Msg::Response {
                tag: Tag::View { serial },
                result: Ok(phonia_ipc::Payload::Tracks {
                    from: phonia_ipc::CatalogRef::Album { id: "9".into() },
                    page: phonia_ipc::Page {
                        items: vec![
                            track("1", "Dead", 1, true),
                            track("2", "Trash", 2, false),
                            track("3", "4U", 3, true),
                        ],
                        total: 3,
                        offset: 0,
                    },
                }),
            },
        );
        state
    }

    #[test]
    fn opening_an_album_shows_its_header_before_the_tracks_have_loaded() {
        let mut state = with_results();
        press(&mut state, 'l');
        press(&mut state, ']');
        let _ = tagged_request(press_key(&mut state, KeyCode::Enter));
        let text = screen(&state, 100, 14);
        assert!(text.contains("Search \u{203a} Issues"), "{text}");
        assert!(
            text.contains("Korn - Issues"),
            "the header, known at once: {text}"
        );
        assert!(text.contains("Loading..."), "{text}");
    }

    #[test]
    fn an_albums_cover_reserves_room_beside_the_header_and_draws_once_its_ready() {
        use crate::browse::{Header, TrackListView, View};
        use crate::covers::{self, Outcome};
        use ratatui_image::Resize;
        use ratatui_image::picker::Picker;

        let album = phonia_ipc::AlbumSummary {
            id: "9".into(),
            title: "Issues".into(),
            version: None,
            artists: vec![phonia_ipc::ArtistRef {
                id: "780".into(),
                name: "Korn".into(),
            }],
            release_date: None,
            track_count: None,
            duration_ms: None,
            explicit: false,
            quality: None,
            kind: None,
            copyright: None,
            cover: Some("3c6247c7-d0d7-4978-91b1-0bddc13f45b5".into()),
        };
        let mut state = State::default();
        press(&mut state, '3'); // Search
        state.search_views.push(
            0,
            View::TrackList(TrackListView::new(
                phonia_ipc::CatalogRef::Album { id: "9".into() },
                Header::Album(album.clone()),
            )),
        );

        let (width, height) = (100, 30);
        let theme = Theme::new(false);
        let main = areas(Rect::new(0, 0, width, height)).main;
        let inner = panel("x", false, &theme).inner(main);
        let picker = Picker::halfblocks();
        let cells = covers::cover_size(inner, picker.font_size()).unwrap();
        let url = covers::cover_url(&Header::Album(album), cells, picker.font_size()).unwrap();

        let mut covers = Covers::new(Some(picker.clone()));
        covers.start(&url);
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            u32::from(cells.width) * 10,
            u32::from(cells.height) * 20,
            image::Rgb([220, 20, 20]),
        ));
        let protocol = picker
            .new_protocol(image, cells, Resize::default())
            .unwrap();
        covers.finish(url, Outcome::Ready(protocol));

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(&state, &theme, &covers, frame))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        // Somewhere in the reserved area, a cell's style was touched: the plain background
        // everywhere else never is.
        let drawn = (inner.y..inner.y + cells.height).any(|y| {
            (inner.x..inner.x + cells.width).any(|x| {
                let style = buffer[(x, y)].style();
                style.fg.is_some() || style.bg.is_some()
            })
        });
        assert!(drawn, "no cover pixels found in the reserved area");

        // The header's own text is still shown, to the right of the cover, not under it.
        let text = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Korn - Issues"), "{text}");
    }

    #[test]
    fn with_covers_off_or_no_cover_id_the_layout_is_exactly_as_before() {
        use crate::browse::{Header, TrackListView, View};

        let with_cover = |cover: Option<&str>| {
            let mut state = State::default();
            press(&mut state, '3');
            state.search_views.push(
                0,
                View::TrackList(TrackListView::new(
                    phonia_ipc::CatalogRef::Album { id: "9".into() },
                    Header::Album(phonia_ipc::AlbumSummary {
                        id: "9".into(),
                        title: "Issues".into(),
                        version: None,
                        artists: vec![],
                        release_date: None,
                        track_count: None,
                        duration_ms: None,
                        explicit: false,
                        quality: None,
                        kind: None,
                        copyright: None,
                        cover: cover.map(str::to_string),
                    }),
                )),
            );
            state
        };

        // Covers enabled, but this item has no cover id.
        let no_cover_id = screen_with(
            &with_cover(None),
            &Covers::new(Some(ratatui_image::picker::Picker::halfblocks())),
            100,
            30,
        );
        // This item has a cover id, but covers are off.
        let covers_off = screen_with(
            &with_cover(Some("3c6247c7-d0d7-4978-91b1-0bddc13f45b5")),
            &Covers::disabled(),
            100,
            30,
        );
        // Both lay out identically to a plain `Covers::disabled()` screen: the header's own text
        // starts at the same column either way, so nothing reserved room for a cover in either.
        assert_eq!(no_cover_id, screen(&with_cover(None), 100, 30));
        assert_eq!(covers_off, screen(&with_cover(Some("x")), 100, 30));
    }

    /// Like [`screen`], but with the given [`Covers`] instead of a disabled one.
    fn screen_with(state: &State, covers: &Covers, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(state, &Theme::new(false), covers, frame))
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

    #[test]
    fn an_open_album_lists_its_tracks_numbered_with_the_unstreamable_one_dimmed() {
        let state = open_album_view();
        let text = screen(&state, 100, 14);
        assert!(text.contains("1. Dead"), "{text}");
        assert!(text.contains("2. Trash"), "{text}");
        assert!(text.contains("3. 4U"), "{text}");
        assert!(
            !text.contains("Korn, Jonathan"),
            "the row leaves out the artists it already knows: {text}"
        );
    }

    #[test]
    fn the_selected_track_in_an_open_album_is_highlighted() {
        let theme = Theme::new(false);
        let reversed_rows = |state: &State| {
            let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
            terminal
                .draw(|frame| draw(state, &theme, &Covers::disabled(), frame))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..14)
                .filter(|y| {
                    (16..60).any(|x| {
                        buffer[(x, *y)]
                            .modifier
                            .contains(ratatui::style::Modifier::REVERSED)
                    })
                })
                .count()
        };
        let state = open_album_view();
        assert_eq!(reversed_rows(&state), 1);
    }

    #[test]
    fn a_narrow_terminal_does_not_break_the_album_view() {
        let state = open_album_view();
        for (w, h) in [(0, 0), (1, 1), (16, 4), (20, 5), (40, 8)] {
            let _ = screen(&state, w, h);
        }
    }

    /// A search of "korn" that found one artist, opened, and loaded: two top tracks, one album.
    fn open_artist_view() -> State {
        use crate::app::{Tag, submit_search};
        let mut state = in_the_search();
        submit_search(&mut state, "korn");
        let generation = state.search.generation;
        update(
            &mut state,
            Msg::Response {
                tag: Tag::Search { generation },
                result: Ok(phonia_ipc::Payload::SearchResults {
                    query: "korn".into(),
                    tracks: None,
                    albums: None,
                    artists: Some(phonia_ipc::Page {
                        items: vec![phonia_ipc::ArtistSummary {
                            id: "780".into(),
                            name: "Korn".into(),
                            picture: None,
                        }],
                        total: 1,
                        offset: 0,
                    }),
                    playlists: None,
                }),
            },
        );
        press(&mut state, 'l');
        press(&mut state, ']');
        press(&mut state, ']'); // Artists
        let (tag, _) = tagged_request(press_key(&mut state, KeyCode::Enter));
        let Tag::View { serial } = tag else {
            panic!("not a view tag")
        };
        let track = |id: &str, title: &str| phonia_ipc::TrackSummary {
            id: id.into(),
            title: title.into(),
            version: None,
            artists: vec![],
            album: None,
            duration_ms: Some(75_000),
            explicit: false,
            track_number: None,
            volume_number: None,
            quality: Some(phonia_ipc::Quality::Hires),
            streamable: true,
        };
        update(
            &mut state,
            Msg::Response {
                tag: Tag::View { serial },
                result: Ok(phonia_ipc::Payload::Artist {
                    artist: phonia_ipc::ArtistSummary {
                        id: "780".into(),
                        name: "Korn".into(),
                        picture: None,
                    },
                    bio: Some("A nu metal band.\nMore about it.".into()),
                    top_tracks: phonia_ipc::Page {
                        items: vec![track("1", "Freak On a Leash"), track("2", "Blind")],
                        total: 300,
                        offset: 0,
                    },
                    albums: phonia_ipc::Page {
                        items: vec![phonia_ipc::AlbumSummary {
                            id: "9".into(),
                            title: "Issues".into(),
                            version: None,
                            artists: vec![],
                            release_date: Some("1999-11-16".into()),
                            track_count: Some(16),
                            duration_ms: None,
                            explicit: true,
                            quality: Some(phonia_ipc::Quality::Hires),
                            kind: None,
                            copyright: None,
                            cover: None,
                        }],
                        total: 35,
                        offset: 0,
                    },
                    singles: phonia_ipc::Page {
                        items: vec![],
                        total: 0,
                        offset: 0,
                    },
                }),
            },
        );
        state
    }

    #[test]
    fn an_artist_page_shows_its_name_bio_and_tabs_with_counts() {
        let state = open_artist_view();
        let text = screen(&state, 100, 14);
        assert!(text.contains("Korn"), "{text}");
        assert!(text.contains("A nu metal band."), "{text}");
        assert!(
            !text.contains("More about it."),
            "only the first line of the bio: {text}"
        );
        assert!(text.contains("Top tracks (300)"), "{text}");
        assert!(text.contains("Albums (35)"), "{text}");
        assert!(text.contains("EPs & singles (0)"), "{text}");
        assert!(text.contains("Freak On a Leash"), "{text}");
    }

    #[test]
    fn an_artists_picture_reserves_room_beside_the_header_and_draws_once_its_ready() {
        use crate::browse::{ArtistView, View};
        use crate::covers::{self, Outcome};
        use ratatui_image::Resize;
        use ratatui_image::picker::Picker;

        let mut state = State::default();
        press(&mut state, '3'); // Search
        state.search_views.push(
            0,
            View::Artist(ArtistView::new(
                "780".into(),
                "Korn".into(),
                Some("ca8a29d3-efcd-4cd2-8dea-a376e1c64b1e".into()),
            )),
        );

        let (width, height) = (100, 30);
        let theme = Theme::new(false);
        let main = areas(Rect::new(0, 0, width, height)).main;
        let inner = panel("x", false, &theme).inner(main);
        let picker = Picker::halfblocks();
        let cells = covers::cover_size(inner, picker.font_size()).unwrap();
        let url = covers::picture_url(
            Some("ca8a29d3-efcd-4cd2-8dea-a376e1c64b1e"),
            cells,
            picker.font_size(),
        )
        .unwrap();

        let mut covers = Covers::new(Some(picker.clone()));
        covers.start(&url);
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            u32::from(cells.width) * 10,
            u32::from(cells.height) * 20,
            image::Rgb([20, 20, 220]),
        ));
        let protocol = picker
            .new_protocol(image, cells, Resize::default())
            .unwrap();
        covers.finish(url, Outcome::Ready(protocol));

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(&state, &theme, &covers, frame))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        let drawn = (inner.y..inner.y + cells.height).any(|y| {
            (inner.x..inner.x + cells.width).any(|x| {
                let style = buffer[(x, y)].style();
                style.fg.is_some() || style.bg.is_some()
            })
        });
        assert!(drawn, "no picture pixels found in the reserved area");

        let text = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Korn"), "{text}");
    }

    #[test]
    fn an_artist_with_no_picture_or_with_covers_off_lays_out_as_before() {
        use crate::browse::{ArtistView, View};

        let with_picture = |picture: Option<&str>| {
            let mut state = State::default();
            press(&mut state, '3');
            state.search_views.push(
                0,
                View::Artist(ArtistView::new(
                    "780".into(),
                    "Korn".into(),
                    picture.map(str::to_string),
                )),
            );
            state
        };
        let no_picture = screen_with(
            &with_picture(None),
            &Covers::new(Some(ratatui_image::picker::Picker::halfblocks())),
            100,
            30,
        );
        let covers_off = screen_with(
            &with_picture(Some("ca8a29d3-efcd-4cd2-8dea-a376e1c64b1e")),
            &Covers::disabled(),
            100,
            30,
        );
        assert_eq!(no_picture, screen(&with_picture(None), 100, 30));
        assert_eq!(covers_off, screen(&with_picture(Some("x")), 100, 30));
    }

    #[test]
    fn the_now_playing_tracks_cover_reserves_room_above_the_queue_and_draws_once_its_ready() {
        use crate::covers::{self, Outcome};
        use ratatui_image::Resize;
        use ratatui_image::picker::Picker;

        let cover_id = "3c6247c7-d0d7-4978-91b1-0bddc13f45b5";
        let mut state = connected();
        press(&mut state, '2');
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::TrackStarted {
                item_id: None,
                source: Some("tidal:1".into()),
                title: Some("Aerodynamic".into()),
                artist: None,
                duration_ms: Some(343_000),
                spec: phonia_ipc::Spec {
                    sample_rate: 44_100,
                    channels: 2,
                    bits_per_sample: 16,
                },
                gapless: false,
                quality: None,
                cover: Some(cover_id.into()),
                replay_gain: None,
            }),
        );

        let (width, height) = (100, 30);
        let theme = Theme::new(false);
        let main = areas(Rect::new(0, 0, width, height)).main;
        let inner = panel("x", false, &theme).inner(main);
        let picker = Picker::halfblocks();
        let cells = covers::cover_size(inner, picker.font_size()).unwrap();
        let url = covers::track_cover_url(Some(cover_id), cells, picker.font_size()).unwrap();

        let mut covers = Covers::new(Some(picker.clone()));
        covers.start(&url);
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            u32::from(cells.width) * 10,
            u32::from(cells.height) * 20,
            image::Rgb([20, 220, 20]),
        ));
        let protocol = picker
            .new_protocol(image, cells, Resize::default())
            .unwrap();
        covers.finish(url, Outcome::Ready(protocol));

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(&state, &theme, &covers, frame))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        let drawn = (inner.y..inner.y + cells.height).any(|y| {
            (inner.x..inner.x + cells.width).any(|x| {
                let style = buffer[(x, y)].style();
                style.fg.is_some() || style.bg.is_some()
            })
        });
        assert!(drawn, "no cover pixels found in the reserved area");

        let text = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Aerodynamic"), "{text}");
        assert!(
            text.contains("Queue (0)"),
            "the list is still shown below the cover: {text}"
        );
    }

    #[test]
    fn with_covers_off_or_nothing_playing_the_queue_layout_is_exactly_as_before() {
        let not_playing = connected();
        let playing_with_a_cover = || {
            let mut state = connected();
            update(
                &mut state,
                Msg::Daemon(phonia_ipc::Event::TrackStarted {
                    item_id: None,
                    source: Some("tidal:1".into()),
                    title: Some("Aerodynamic".into()),
                    artist: None,
                    duration_ms: None,
                    spec: phonia_ipc::Spec {
                        sample_rate: 44_100,
                        channels: 2,
                        bits_per_sample: 16,
                    },
                    gapless: false,
                    quality: None,
                    cover: Some("3c6247c7-d0d7-4978-91b1-0bddc13f45b5".into()),
                    replay_gain: None,
                }),
            );
            state
        };
        let picker = Some(ratatui_image::picker::Picker::halfblocks());

        // Covers enabled, but nothing playing (and so nothing with a cover id).
        let nothing_playing = screen_with(&not_playing, &Covers::new(picker.clone()), 100, 30);
        assert_eq!(nothing_playing, screen(&not_playing, 100, 30));

        // Something playing with a cover id, but covers are off.
        let covers_off = screen_with(&playing_with_a_cover(), &Covers::disabled(), 100, 30);
        assert_eq!(covers_off, screen(&playing_with_a_cover(), 100, 30));
    }

    #[test]
    fn the_title_and_artist_show_above_the_queue_even_with_no_cover_at_all() {
        let mut state = connected();
        press(&mut state, '2');
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::TrackStarted {
                item_id: None,
                source: Some("tidal:1".into()),
                title: Some("Sultans of Swing".into()),
                artist: Some("Dire Straits".into()),
                duration_ms: None,
                spec: phonia_ipc::Spec {
                    sample_rate: 44_100,
                    channels: 2,
                    bits_per_sample: 16,
                },
                gapless: false,
                quality: None,
                cover: None,
                replay_gain: None,
            }),
        );
        // Covers disabled entirely: no picker, so a cover could never show here either way.
        let text = screen(&state, 100, 30);
        assert!(text.contains("Sultans of Swing"), "{text}");
        assert!(text.contains("◉ Dire Straits"), "{text}");
        assert!(
            text.contains("Queue (0)"),
            "the list is still shown below the header: {text}"
        );
    }

    #[test]
    fn switching_the_artists_tab_shows_its_albums() {
        let mut state = open_artist_view();
        press(&mut state, ']');
        let text = screen(&state, 100, 14);
        assert!(text.contains("Issues"), "{text}");
        assert!(!text.contains("Freak On a Leash"), "{text}");
    }

    #[test]
    fn an_empty_list_says_so() {
        let mut state = open_artist_view();
        press(&mut state, ']');
        press(&mut state, ']'); // EPs & singles, empty
        let text = screen(&state, 100, 14);
        assert!(text.contains("No albums."), "{text}");
    }

    #[test]
    fn a_narrow_terminal_does_not_break_the_artist_view() {
        let state = open_artist_view();
        for (w, h) in [(0, 0), (1, 1), (16, 4), (20, 5), (40, 8)] {
            let _ = screen(&state, w, h);
        }
    }

    // --- The Lyrics section --------------------------------------------------------------

    fn in_the_lyrics(source: &str) -> State {
        let mut playing = crate::app::tests_support::status();
        playing.track = Some(phonia_ipc::Track {
            item_id: None,
            source: Some(source.to_string()),
            title: Some("Song".into()),
            artist: None,
            duration_ms: Some(300_000),
            quality: None,
            cover: None,
            replay_gain: None,
        });
        let mut state = State::default();
        update(
            &mut state,
            Msg::Connected {
                server: phonia_ipc::ServerInfo {
                    name: "phoniad".into(),
                    version: "0.1.0".into(),
                    pid: 1,
                },
                protocol: phonia_ipc::Version {
                    major: 1,
                    minor: 10,
                },
                capabilities: vec!["catalog".into(), "lyrics".into()],
                status: playing,
                queue: crate::app::tests_support::queue(),
            },
        );
        press(&mut state, '5');
        state
    }

    fn answer_lyrics(state: &mut State, id: &str, lyrics: Option<phonia_ipc::Lyrics>) {
        update(
            state,
            Msg::Response {
                tag: crate::app::Tag::Lyrics { generation: 1 },
                result: Ok(phonia_ipc::Payload::Lyrics {
                    id: id.to_string(),
                    lyrics,
                }),
            },
        );
    }

    #[test]
    fn before_an_answer_the_lyrics_panel_says_loading() {
        let state = in_the_lyrics("tidal:9");
        let text = screen(&state, 80, 14);
        assert!(text.contains("Loading..."), "{text}");
    }

    #[test]
    fn a_local_file_has_no_lyrics_to_ask_for() {
        let state = in_the_lyrics("file:/a.flac");
        let text = screen(&state, 80, 14);
        assert!(text.contains("this track is a local file"), "{text}");
    }

    #[test]
    fn no_lyrics_at_all_says_so_plainly() {
        let mut state = in_the_lyrics("tidal:518338");
        answer_lyrics(&mut state, "518338", None);
        let text = screen(&state, 80, 14);
        assert!(
            text.contains("TIDAL has no lyrics for this track."),
            "{text}"
        );
    }

    #[test]
    fn synced_lyrics_mark_the_current_line_and_show_the_provider() {
        let mut state = in_the_lyrics("tidal:9");
        answer_lyrics(
            &mut state,
            "9",
            Some(phonia_ipc::Lyrics {
                lines: vec![
                    phonia_ipc::LyricLine {
                        at_ms: 0,
                        text: "intro".into(),
                    },
                    phonia_ipc::LyricLine {
                        at_ms: 1_000,
                        text: "verse one".into(),
                    },
                    phonia_ipc::LyricLine {
                        at_ms: 2_000,
                        text: "verse two".into(),
                    },
                ],
                plain: None,
                right_to_left: false,
                provider: Some("MUSIXMATCH".into()),
            }),
        );
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::Position {
                position_ms: 1_500,
                duration_ms: Some(300_000),
            }),
        );
        let text = screen(&state, 80, 14);
        assert!(text.contains("> verse one"), "{text}");
        assert!(text.contains("  verse two"), "{text}");
        assert!(text.contains("Lyrics via MUSIXMATCH"), "{text}");
    }

    #[test]
    fn plain_lyrics_show_a_not_synced_notice() {
        let mut state = in_the_lyrics("tidal:9");
        answer_lyrics(
            &mut state,
            "9",
            Some(phonia_ipc::Lyrics {
                lines: vec![],
                plain: Some("some words".into()),
                right_to_left: false,
                provider: None,
            }),
        );
        let text = screen(&state, 80, 14);
        assert!(text.contains("Not synced to the music"), "{text}");
        assert!(text.contains("some words"), "{text}");
    }

    #[test]
    fn a_long_synced_list_scrolls_to_keep_the_current_line_on_screen() {
        let mut state = in_the_lyrics("tidal:9");
        let lines: Vec<phonia_ipc::LyricLine> = (0..40)
            .map(|n| phonia_ipc::LyricLine {
                at_ms: n * 1_000,
                text: format!("line {n}"),
            })
            .collect();
        answer_lyrics(
            &mut state,
            "9",
            Some(phonia_ipc::Lyrics {
                lines,
                plain: None,
                right_to_left: false,
                provider: None,
            }),
        );
        update(
            &mut state,
            Msg::Daemon(phonia_ipc::Event::Position {
                position_ms: 30_000,
                duration_ms: Some(60_000),
            }),
        );
        let text = screen(&state, 80, 14);
        assert!(text.contains("> line 30"), "{text}");
        assert!(
            !text.contains("line 0 "),
            "the early lines have scrolled off: {text}"
        );
    }

    #[test]
    fn right_to_left_lyrics_are_right_aligned() {
        let mut state = in_the_lyrics("tidal:9");
        answer_lyrics(
            &mut state,
            "9",
            Some(phonia_ipc::Lyrics {
                lines: vec![phonia_ipc::LyricLine {
                    at_ms: 0,
                    text: "שלום".into(),
                }],
                plain: None,
                right_to_left: true,
                provider: None,
            }),
        );
        let text = screen(&state, 40, 14);
        let line = text
            .lines()
            .find(|l| l.contains('ש'))
            .expect("the line is on screen");
        // Right-aligned: the marker sits in the right half of the row, not flush against the
        // sidebar the way a left-aligned line's would.
        let marker = line.chars().position(|c| c == '>').expect("the marker");
        let total = line.chars().count();
        assert!(
            marker * 2 > total,
            "expected the text pushed toward the right: {line:?} (marker at {marker} of {total})"
        );
    }

    fn connected_with_catalog() -> State {
        let mut state = State::default();
        update(
            &mut state,
            Msg::Connected {
                server: phonia_ipc::ServerInfo {
                    name: "phoniad".into(),
                    version: "0.1.0".into(),
                    pid: 1,
                },
                protocol: phonia_ipc::Version { major: 1, minor: 6 },
                capabilities: vec!["catalog".into()],
                status: crate::app::tests_support::status(),
                queue: crate::app::tests_support::queue(),
            },
        );
        state
    }

    /// Connecting asks for the library at once since Home (the default section) reads it too --
    /// left on Home, not moved to Library, so these answers land on its own blocks.
    fn with_library_from_home() -> State {
        use crate::app::Tag;
        let mut state = connected_with_catalog();
        update(
            &mut state,
            Msg::Response {
                tag: Tag::Library { generation: 0 },
                result: Ok(phonia_ipc::Payload::Library {
                    favorite_tracks: phonia_ipc::Page {
                        items: vec![phonia_ipc::TrackSummary {
                            id: "1".into(),
                            title: "Freak On a Leash".into(),
                            version: None,
                            artists: vec![phonia_ipc::ArtistRef {
                                id: "780".into(),
                                name: "Korn".into(),
                            }],
                            album: None,
                            duration_ms: Some(212_000),
                            explicit: false,
                            track_number: None,
                            volume_number: None,
                            quality: None,
                            streamable: true,
                        }],
                        total: 42,
                        offset: 0,
                    },
                    favorite_albums: phonia_ipc::Page {
                        items: vec![phonia_ipc::AlbumSummary {
                            id: "9".into(),
                            title: "Issues".into(),
                            version: None,
                            artists: vec![],
                            release_date: None,
                            track_count: None,
                            duration_ms: None,
                            explicit: false,
                            quality: None,
                            kind: None,
                            copyright: None,
                            cover: None,
                        }],
                        total: 7,
                        offset: 0,
                    },
                    my_playlists: phonia_ipc::Page {
                        items: vec![],
                        total: 0,
                        offset: 0,
                    },
                }),
            },
        );
        update(
            &mut state,
            Msg::Response {
                tag: Tag::LibraryPlaylists { generation: 0 },
                result: Ok(phonia_ipc::Payload::PlaylistFolder {
                    folder: None,
                    page: phonia_ipc::Page {
                        items: vec![phonia_ipc::FolderEntry::Folder {
                            id: "f".into(),
                            name: "Moods".into(),
                            item_count: 3,
                        }],
                        total: 1,
                        offset: 0,
                    },
                }),
            },
        );
        state
    }

    #[test]
    fn home_shows_only_the_continue_row_before_the_library_has_loaded() {
        let text = screen(&connected_with_catalog(), 100, 14);
        assert!(text.contains("Nothing to continue yet"), "{text}");
        assert!(!text.contains("Favorite albums"), "{text}");
        assert!(!text.contains("See all"), "{text}");
    }

    #[test]
    fn home_shows_the_continue_row_and_each_blocks_see_all_row_once_the_library_is_in() {
        let text = screen(&with_library_from_home(), 100, 30);
        assert!(text.contains("Nothing to continue yet"), "{text}");
        assert!(text.contains("Favorite albums"), "{text}");
        assert!(text.contains("Issues"), "{text}");
        assert!(text.contains("See all (7)"), "{text}");
        assert!(text.contains("Your playlists"), "{text}");
        assert!(text.contains("Moods"), "{text}");
        assert!(text.contains("See all (1)"), "{text}");
        assert!(text.contains("Favorite tracks"), "{text}");
        assert!(text.contains("Freak On a Leash"), "{text}");
        assert!(text.contains("See all (42)"), "{text}");
    }

    #[test]
    fn moving_down_on_home_reaches_the_first_album_row_and_highlights_only_it() {
        let theme = Theme::new(false);
        let reversed_row = |state: &State| {
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal
                .draw(|frame| draw(state, &theme, &Covers::disabled(), frame))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..30).find(|y| {
                (16..90).any(|x| {
                    buffer[(x, *y)]
                        .modifier
                        .contains(ratatui::style::Modifier::REVERSED)
                })
            })
        };
        let mut state = with_library_from_home();
        press(&mut state, 'l');
        assert_eq!(state.home_cursor.selected(), 0, "the Continue row first");
        press(&mut state, 'j');
        assert_eq!(state.home_cursor.selected(), 1, "the first album next");
        let row = reversed_row(&state).expect("one row highlighted");
        let text = screen(&state, 100, 30);
        let line = text.lines().nth(row as usize).unwrap_or("");
        assert!(line.contains("Issues"), "{line}");
    }
}
