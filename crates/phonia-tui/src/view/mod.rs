//! Painting a state onto the screen.

mod help;
mod search;

/// How far the help can scroll on a screen `rows` tall.
pub fn help_overflow(rows: u16) -> usize {
    help::overflow(rows)
}

use crate::app::{Connection, Focus, Section, State, seconds_left};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// The columns the sidebar takes: the longest section name and room for the border and the mark.
const SIDEBAR_WIDTH: u16 = 14;
/// The rows of the bar at the bottom: a border and three lines.
const BAR_HEIGHT: u16 = 4;

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

fn draw_main(state: &State, theme: &Theme, frame: &mut Frame, area: Rect) {
    let focused = state.focus == Focus::Main;
    // The search has its own layout inside the panel: the line, the tabs and the results.
    if state.section() == Section::Search {
        let block = panel(Section::Search.title(), focused, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        search::draw(state, theme, frame, inner);
        return;
    }
    let (title, lines) = match (state.section(), &state.queue) {
        (Section::Queue, Some(queue)) => {
            let title = format!("Queue ({})", queue.items.len());
            // The border takes a row above and one below.
            let rows = usize::from(area.height.saturating_sub(2));
            let cursor = focused.then(|| state.queue_cursor.selected());
            (title, queue_lines(queue, cursor, rows, theme))
        }
        (Section::Queue, None) => (
            "Queue".to_string(),
            vec![Line::styled("Not connected yet.", theme.dim)],
        ),
        (section, _) => (
            section.title().to_string(),
            vec![Line::styled("Nothing to show yet.", theme.dim)],
        ),
    };
    frame.render_widget(
        Paragraph::new(lines).block(panel(&title, focused, theme)),
        area,
    );
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
            let name = item.title.clone().unwrap_or_else(|| item.source.clone());
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
    let name = status
        .track
        .as_ref()
        .and_then(|track| track.title.clone().or_else(|| track.source.clone()))
        .unwrap_or_else(|| "Nothing playing".to_string());
    let mut text = format!("{}  {name}", state_word(status.state));
    if status.track.is_some() {
        let mut details = Vec::new();
        if let Some(spec) = &status.spec {
            details.push(format!(
                "{}-bit / {}",
                spec.bits_per_sample,
                phonia_ipc::fmt::sample_rate(spec.sample_rate)
            ));
        }
        if let Some(quality) = status.track.as_ref().and_then(|track| track.quality) {
            details.push(phonia_ipc::fmt::stream_quality(&quality));
        }
        if !details.is_empty() {
            text.push_str(&format!("  ({})", details.join(", ")));
        }
    }
    Line::styled(text, theme.text)
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
    if let Some(queue) = &state.queue {
        if queue.shuffle {
            flags.push("shuffle".to_string());
        }
        match queue.repeat {
            phonia_ipc::Repeat::Off => {}
            phonia_ipc::Repeat::All => flags.push("repeat all".to_string()),
            phonia_ipc::Repeat::One => flags.push("repeat one".to_string()),
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
            Line::styled("? help   q quit", theme.dim),
        ],
        Connection::Refused { reason } => vec![
            Line::styled(format!("Cannot use this daemon: {reason}"), theme.error),
            Line::styled("R: try again", theme.dim),
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
        assert!(text.contains("1:05 "), "{text}");
        assert!(text.contains(" 5:43"), "{text}");
        assert!(
            text.contains('█') && text.contains('░'),
            "the progress bar: {text}"
        );
        assert!(text.contains("(16-bit / 44.1 kHz)"), "{text}");
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
            }),
        );
        let text = screen(&state, 120, 12);
        assert!(
            text.contains("(24-bit / 96 kHz, lossless (asked for hires))"),
            "{text}"
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
        update(&mut state, Msg::Resize(100, 60));
        press(&mut state, '?');
        let text = screen(&state, 100, 60);
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
                duration_ms: None,
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
                },
            }),
        );
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
            terminal.draw(|frame| draw(state, &theme, frame)).unwrap();
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
        press(&mut state, '2');
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
            .draw(|frame| draw(&state, &Theme::new(false), frame))
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

    #[test]
    fn the_row_under_the_cursor_is_highlighted_only_when_the_list_has_the_focus_and_no_typing() {
        let theme = Theme::new(false);
        let reversed_rows = |state: &State| {
            let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
            terminal.draw(|frame| draw(state, &theme, frame)).unwrap();
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
}
