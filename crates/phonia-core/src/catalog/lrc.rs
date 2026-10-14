//! Parsing TIDAL's `subtitles` field: synced lyrics, in the LRC format.
//!
//! A line looks like `[01:02.34]Some words`, one or more timestamp tags followed by the text
//! they belong to. TIDAL's own writer also uses metadata tags (`[ar:]`, `[ti:]`, `[length:]`...)
//! that carry no timing and are ignored here, and an `[offset:±ms]` tag that shifts every
//! timestamp from that point on -- the one tag, besides a timestamp itself, this parser acts on.

use super::LyricLine;
use std::time::Duration;

/// Parses LRC text into time-ordered lines. A line with no recognized timestamp tag (metadata,
/// or just malformed) is skipped rather than failing the whole parse -- one bad line should never
/// cost the rest of the lyrics. A timestamp with nothing after it is kept as an empty line, not
/// dropped: TIDAL uses that to mark an instrumental gap, which a caller wants to know is "between
/// lines", not "still showing the previous one".
pub fn parse(text: &str) -> Vec<LyricLine> {
    let mut offset = Duration::ZERO;
    let mut negative_offset = false;
    let mut lines = Vec::new();

    for raw in text.lines() {
        let raw = raw.trim_end_matches('\r');
        let (tags, rest) = leading_tags(raw);
        if tags.is_empty() {
            continue;
        }

        let mut timestamps = Vec::new();
        for tag in &tags {
            if let Some(value) = tag.strip_prefix("offset:") {
                if let Ok(ms) = value.parse::<i64>() {
                    negative_offset = ms < 0;
                    offset = Duration::from_millis(ms.unsigned_abs());
                }
                continue;
            }
            if let Some(at) = parse_timestamp(tag) {
                timestamps.push(at);
            }
        }
        if timestamps.is_empty() {
            continue;
        }

        let text = rest.trim().to_string();
        for at in timestamps {
            let at = if negative_offset {
                at.saturating_sub(offset)
            } else {
                at + offset
            };
            lines.push(LyricLine {
                at,
                text: text.clone(),
            });
        }
    }

    lines.sort_by_key(|line| line.at);
    lines
}

/// The `[...]` tags a line starts with, and what follows them.
fn leading_tags(line: &str) -> (Vec<&str>, &str) {
    let mut rest = line;
    let mut tags = Vec::new();
    while let Some(after_bracket) = rest.strip_prefix('[') {
        let Some(end) = after_bracket.find(']') else {
            break;
        };
        tags.push(&after_bracket[..end]);
        rest = &after_bracket[end + 1..];
    }
    (tags, rest)
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss.xxx`; `None` for anything else (a metadata tag like `ar:Name`).
fn parse_timestamp(tag: &str) -> Option<Duration> {
    let (minutes, seconds) = tag.split_once(':')?;
    let minutes: u64 = minutes.parse().ok()?;
    let seconds: f64 = seconds.parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(minutes as f64 * 60.0 + seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn plain_centisecond_and_millisecond_timestamps_all_parse() {
        let lrc = "[00:12.34]Two digits\n[00:12.345]Three digits\n[00:08]No fraction";
        let lines = parse(lrc);
        assert_eq!(lines.len(), 3);
        let at = |text: &str| lines.iter().find(|l| l.text == text).unwrap().at;
        assert_eq!(at("No fraction"), ms(8_000));
        assert_eq!(at("Two digits"), ms(12_340));
        assert_eq!(at("Three digits"), ms(12_345));
    }

    #[test]
    fn several_timestamps_on_one_line_each_get_their_own_entry() {
        let lines = parse("[00:12.00][01:30.00]Chorus");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].at, ms(12_000));
        assert_eq!(lines[1].at, ms(90_000));
        assert_eq!(lines[0].text, "Chorus");
        assert_eq!(lines[1].text, "Chorus");
    }

    #[test]
    fn metadata_tags_are_ignored_and_carry_no_line() {
        let lrc = "[ar:Dire Straits]\n[ti:Sultans of Swing]\n[length:05:48]\n[00:01.00]First line";
        let lines = parse(lrc);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "First line");
    }

    #[test]
    fn a_positive_offset_shifts_every_later_timestamp_forward() {
        let lines = parse("[offset:500]\n[00:10.00]A\n[00:20.00]B");
        assert_eq!(lines[0].at, ms(10_500));
        assert_eq!(lines[1].at, ms(20_500));
    }

    #[test]
    fn a_negative_offset_shifts_every_later_timestamp_back() {
        let lines = parse("[offset:-500]\n[00:10.00]A");
        assert_eq!(lines[0].at, ms(9_500));
    }

    #[test]
    fn a_timestamp_with_nothing_after_it_is_an_empty_line_not_dropped() {
        let lines = parse("[00:05.00]\n[00:10.00]Singing again");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "");
        assert_eq!(lines[1].text, "Singing again");
    }

    #[test]
    fn crlf_line_endings_are_handled() {
        let lines = parse("[00:01.00]A\r\n[00:02.00]B\r\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "B");
    }

    #[test]
    fn lines_come_out_sorted_by_time_even_if_the_source_was_not() {
        let lrc = "[00:30.00]Second\n[00:05.00]First";
        let lines = parse(lrc);
        assert_eq!(lines[0].text, "First");
        assert_eq!(lines[1].text, "Second");
    }

    #[test]
    fn a_line_with_no_recognizable_tag_is_skipped_without_losing_the_rest() {
        let lrc = "not a tag at all\n[not-a-timestamp]Still skipped\n[00:01.00]Kept";
        let lines = parse(lrc);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Kept");
    }

    #[test]
    fn an_empty_string_parses_to_no_lines() {
        assert_eq!(parse(""), Vec::new());
    }
}
