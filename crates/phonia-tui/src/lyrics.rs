//! A track's lyrics, pulled from TIDAL only while the Lyrics panel is open (see
//! `app::maybe_load_lyrics`), never pushed for every track whether or not anyone is looking.
//!
//! Scrolling (`LyricsState::scroll`) is one model for both kinds of answer TIDAL can give: with no
//! manual scroll yet, synced lyrics follow the line being sung and plain text starts at the top;
//! the moment the user scrolls, that becomes an explicit offset instead, kept until the track
//! changes and a fresh `begin` drops it. This is deliberately not two separate mechanisms (an
//! auto-follow one for synced, a stored-offset one like the help screen's for plain text): one
//! model that can be either computed or overridden covers both without a "most of the time
//! computed, but sometimes not" special case in the view.

use crate::browse::Phase;
use phonia_ipc::{LyricLine, Lyrics, Payload, Request, Track};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsState {
    /// The bare numeric TIDAL id this is for: `Request::Lyrics`'s own `id`, never `tidal:`-prefixed.
    pub id: String,
    /// Counts the one request that loads it, so a late answer for a request since superseded (a
    /// newer track, or a reconnect that asked again) is told apart from the current one.
    pub generation: u64,
    pub phase: Phase,
    /// `None` until `Done`, and still `None` once it is when TIDAL has nothing for this track.
    pub lyrics: Option<Lyrics>,
    /// A scrolled offset in rows, overriding wherever the panel would otherwise show by itself;
    /// see the module doc comment. `None` until the user scrolls, reset by the next `begin`.
    pub scroll: Option<usize>,
}

impl LyricsState {
    /// Lyrics about to be loaded for `id`, and the request that loads them.
    pub fn begin(id: String, generation: u64) -> (Self, Request) {
        (
            Self {
                id: id.clone(),
                generation,
                phase: Phase::Loading,
                lyrics: None,
                scroll: None,
            },
            Request::Lyrics { id },
        )
    }

    /// Takes the daemon's answer. The caller (`app::on_response`) only calls this once it has
    /// already checked the generation still matches, so the `id` the payload echoes back is not
    /// re-checked here -- the same trust `library`/`search` place in their own generation guard.
    pub fn finish(&mut self, payload: Payload) -> bool {
        let Payload::Lyrics { lyrics, .. } = payload else {
            self.fail("the daemon answered something unexpected".to_string());
            return false;
        };
        self.lyrics = lyrics;
        self.phase = Phase::Done;
        true
    }

    pub fn fail(&mut self, reason: String) {
        self.phase = Phase::Failed(reason);
    }

    /// The connection went away: if it was still loading, this will never be answered now, and
    /// late one anyway must not count once reconnected and it is asked again. One already `Done`
    /// is left alone, kept on screen while disconnected, the same as the library's own.
    pub fn connection_lost(&mut self) {
        if self.phase == Phase::Loading {
            self.generation += 1;
            self.phase = Phase::Failed("the connection to the daemon was lost".to_string());
        }
    }
}

/// The bare numeric TIDAL id a track's lyrics are asked with, or `None` for a local file (or one
/// not even playing): `Request::Lyrics`'s own `id`, unlike `source`, is never `tidal:`-prefixed.
pub fn tidal_id(track: &Track) -> Option<&str> {
    track.source.as_deref()?.strip_prefix("tidal:")
}

/// The line being sung at `position_ms`: the last one whose own timestamp has passed. `None`
/// before the first line starts (an intro) or when there are no lines at all. Lines sharing a
/// timestamp (a repeated chorus tag) resolve to the later-indexed one.
pub fn current_line(lines: &[LyricLine], position_ms: u64) -> Option<usize> {
    lines
        .partition_point(|line| line.at_ms <= position_ms)
        .checked_sub(1)
}

/// Where to start showing lines so that `current` sits in the middle of `rows`, among `len` of
/// them in all, clamped so the view never scrolls past the first line or past the end.
pub fn centered_first(current: Option<usize>, len: usize, rows: usize) -> usize {
    let Some(current) = current else {
        return 0;
    };
    current
        .saturating_sub(rows / 2)
        .min(len.saturating_sub(rows))
}

/// Plain lyrics, split into the rows a terminal shows them as: one per source line, cut rather
/// than wrapped when it is longer than the panel.
pub fn plain_lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

/// Whether the panel's "not synced" notice shows: a plain-only answer, not an empty one.
pub fn shows_notice(lyrics: &Lyrics) -> bool {
    lyrics.lines.is_empty() && lyrics.plain.is_some()
}

/// Whether the panel's provider-credit footer line shows.
pub fn shows_footer(lyrics: &Lyrics) -> bool {
    lyrics.provider.is_some()
}

/// The rows the scrollable body gets, once the notice and the footer (whichever of the two show)
/// have taken theirs from the panel's own total (`view::lyrics_panel_rows`).
pub fn body_rows(lyrics: &Lyrics, panel_rows: usize) -> usize {
    let extra = usize::from(shows_notice(lyrics)) + usize::from(shows_footer(lyrics));
    panel_rows.saturating_sub(extra)
}

/// Where the panel would scroll to by itself, with no manual override (see the module doc
/// comment): centered on the line being sung for synced lyrics, or the top for plain text -- there
/// is nothing in plain text to follow.
pub fn auto_offset(lyrics: &Lyrics, position_ms: u64, rows: usize) -> usize {
    if lyrics.lines.is_empty() {
        0
    } else {
        centered_first(
            current_line(&lyrics.lines, position_ms),
            lyrics.lines.len(),
            rows,
        )
    }
}

/// How far the content can scroll past its own top.
pub fn max_scroll(lyrics: &Lyrics, rows: usize) -> usize {
    let len = if lyrics.lines.is_empty() {
        plain_lines(lyrics.plain.as_deref().unwrap_or("")).len()
    } else {
        lyrics.lines.len()
    };
    len.saturating_sub(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(at_ms: u64, text: &str) -> LyricLine {
        LyricLine {
            at_ms,
            text: text.to_string(),
        }
    }

    fn synced(lines: Vec<LyricLine>) -> Lyrics {
        Lyrics {
            lines,
            plain: None,
            right_to_left: false,
            provider: None,
        }
    }

    fn plain(text: &str) -> Lyrics {
        Lyrics {
            lines: vec![],
            plain: Some(text.to_string()),
            right_to_left: false,
            provider: None,
        }
    }

    #[test]
    fn begin_starts_loading_with_no_lyrics_yet() {
        let (state, request) = LyricsState::begin("9".to_string(), 3);
        assert_eq!(state.id, "9");
        assert_eq!(state.generation, 3);
        assert_eq!(state.phase, Phase::Loading);
        assert_eq!(state.lyrics, None);
        assert_eq!(state.scroll, None);
        assert_eq!(request, Request::Lyrics { id: "9".into() });
    }

    #[test]
    fn finish_sets_done_whether_or_not_tidal_had_lyrics() {
        let (mut state, _) = LyricsState::begin("9".into(), 0);
        let lyrics = synced(vec![line(0, "a")]);
        assert!(state.finish(Payload::Lyrics {
            id: "9".into(),
            lyrics: Some(lyrics.clone()),
        }));
        assert_eq!(state.phase, Phase::Done);
        assert_eq!(state.lyrics, Some(lyrics));

        let (mut state, _) = LyricsState::begin("518338".into(), 0);
        assert!(state.finish(Payload::Lyrics {
            id: "518338".into(),
            lyrics: None,
        }));
        assert_eq!(state.phase, Phase::Done);
        assert_eq!(
            state.lyrics, None,
            "an instrumental: done, but nothing to show"
        );
    }

    #[test]
    fn an_unexpected_answer_is_a_failure() {
        let (mut state, _) = LyricsState::begin("9".into(), 0);
        assert!(!state.finish(Payload::Ack));
        assert!(matches!(state.phase, Phase::Failed(_)));
    }

    #[test]
    fn losing_the_connection_fails_it_only_if_still_loading() {
        let (mut state, _) = LyricsState::begin("9".into(), 0);
        state.connection_lost();
        assert!(matches!(state.phase, Phase::Failed(_)));
        assert_eq!(
            state.generation, 1,
            "a late answer for generation 0 is now stale"
        );

        let (mut state, _) = LyricsState::begin("9".into(), 0);
        state.finish(Payload::Lyrics {
            id: "9".into(),
            lyrics: None,
        });
        state.connection_lost();
        assert_eq!(state.phase, Phase::Done, "already done: left as is");
        assert_eq!(state.generation, 0);
    }

    #[test]
    fn tidal_id_strips_the_prefix_and_a_local_file_has_none() {
        let track = |source: Option<&str>| Track {
            item_id: None,
            source: source.map(str::to_string),
            title: None,
            artist: None,
            duration_ms: None,
            quality: None,
            cover: None,
            replay_gain: None,
        };
        assert_eq!(tidal_id(&track(Some("tidal:233059491"))), Some("233059491"));
        assert_eq!(tidal_id(&track(Some("file:/a.flac"))), None);
        assert_eq!(tidal_id(&track(None)), None);
    }

    #[test]
    fn current_line_is_the_last_one_whose_timestamp_has_passed() {
        let lines = [line(0, "intro"), line(1_000, "a"), line(2_000, "b")];
        assert_eq!(current_line(&lines, 0), Some(0), "exactly on a timestamp");
        assert_eq!(current_line(&lines, 500), Some(0), "between two lines");
        assert_eq!(current_line(&lines, 2_500), Some(2), "after the last line");
        assert_eq!(current_line(&[], 0), None, "no lines at all");
    }

    #[test]
    fn before_the_first_line_nothing_is_current_yet() {
        let lines = [line(1_000, "a")];
        assert_eq!(current_line(&lines, 500), None);
    }

    #[test]
    fn repeated_timestamps_resolve_to_the_later_indexed_line() {
        let lines = [line(1_000, "first"), line(1_000, "second")];
        assert_eq!(current_line(&lines, 1_000), Some(1));
    }

    #[test]
    fn centered_first_keeps_the_current_line_in_the_middle() {
        assert_eq!(centered_first(None, 100, 10), 0, "nothing current: top");
        assert_eq!(centered_first(Some(50), 100, 10), 45);
        assert_eq!(
            centered_first(Some(2), 100, 10),
            0,
            "near the start: clamped"
        );
        assert_eq!(
            centered_first(Some(98), 100, 10),
            90,
            "near the end: clamped to the last page"
        );
        assert_eq!(centered_first(Some(5), 10, 20), 0, "more rows than lines");
    }

    #[test]
    fn plain_lines_splits_on_newlines() {
        assert_eq!(plain_lines("a\nb\nc"), vec!["a", "b", "c"]);
        assert_eq!(plain_lines(""), Vec::<&str>::new());
    }

    #[test]
    fn the_notice_and_the_footer_show_exactly_when_their_content_is_there() {
        assert!(!shows_notice(&synced(vec![line(0, "a")])));
        assert!(shows_notice(&plain("some words")));
        assert!(!shows_footer(&plain("some words")));
        let mut credited = plain("some words");
        credited.provider = Some("MUSIXMATCH".into());
        assert!(shows_footer(&credited));
    }

    #[test]
    fn body_rows_leaves_room_for_the_notice_and_the_footer() {
        let mut lyrics = plain("some words");
        assert_eq!(body_rows(&lyrics, 10), 9, "a notice, no footer");
        lyrics.provider = Some("X".into());
        assert_eq!(body_rows(&lyrics, 10), 8, "both");
        assert_eq!(body_rows(&synced(vec![line(0, "a")]), 10), 10, "neither");
    }

    #[test]
    fn auto_offset_follows_the_current_line_or_sits_at_the_top_for_plain_text() {
        let lines = (0..100).map(|n| line(n * 1_000, "x")).collect();
        let found = synced(lines);
        assert_eq!(auto_offset(&found, 50_000, 10), 45);
        assert_eq!(auto_offset(&plain("a\nb\nc"), 0, 10), 0);
    }

    #[test]
    fn max_scroll_counts_whichever_content_is_there() {
        let lines = (0..30).map(|n| line(n * 1_000, "x")).collect();
        assert_eq!(max_scroll(&synced(lines), 10), 20);
        assert_eq!(
            max_scroll(&plain("a\nb\nc"), 10),
            0,
            "fewer lines than rows"
        );
    }
}
