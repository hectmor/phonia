//! Pure MPD (DASH manifest) parsing helpers.
//!
//! Everything here is a pure function over an XML string / URL, with no network or file I/O,
//! so it is fully unit-testable. `tidlers`'s own `DashManifest` (see
//! `tidlers::client::models::track::playback::DashManifest`) does not expose a segment count and
//! does not resolve relative URLs against `<BaseURL>`, so `tidal.rs` decodes the raw manifest
//! itself and hands the XML text to `parse_mpd` here instead of relying on that type.

use anyhow::{Context, Result, anyhow, bail};
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use std::time::Duration;

/// Decodes an attribute's raw bytes as UTF-8 and unescapes XML entities (`&amp;` -> `&`, etc.).
/// TIDAL's manifests routinely put pre-signed CDN URLs (with `&`-separated query parameters,
/// escaped as `&amp;` per the XML spec) in `initialization`/`media` attributes; reading the raw
/// bytes without this step silently corrupts those URLs, breaking the CDN's signature check.
fn attr_value(a: &quick_xml::events::attributes::Attribute) -> Result<String> {
    Ok(a.normalized_value(XmlVersion::Implicit1_0)
        .context("unescaping an XML attribute value")?
        .into_owned())
}

/// Everything needed to download the init segment and all media segments of a single
/// DASH (ISO/MP4) audio representation.
#[derive(Debug, Clone, PartialEq)]
pub struct DashSegments {
    /// Absolute URL of the initialization segment.
    pub init_url: String,
    /// Absolute URL template for media segments; still contains the literal `$Number$`
    /// placeholder, substitute it with [`segment_url`].
    pub media_url_template: String,
    /// First segment number (`startNumber`, defaults to 1 per the DASH spec).
    pub start_number: u32,
    /// Total number of media segments, if it could be determined from the manifest.
    /// `None` means the caller must download sequentially until a request fails.
    pub segment_count: Option<u32>,
    /// The `codecs` attribute of the `<Representation>` element (e.g. `"flac"`), if present.
    pub codecs: Option<String>,
    /// Ticks per second of the segment timing below (`SegmentTemplate@timescale`).
    pub timescale: u64,
    /// When each segment starts, which is what seeking needs.
    pub timing: SegmentTiming,
    /// `mediaPresentationDuration`, when the manifest has one we could read.
    pub presentation_duration: Option<Duration>,
}

/// When the segments of a representation start, in `timescale` ticks from the start of the track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentTiming {
    /// The manifest says nothing usable about timing, so seeking is not possible.
    Unknown,
    /// Every segment lasts `ticks` (`SegmentTemplate@duration`); the last one may be shorter.
    Constant { ticks: u64 },
    /// A `<SegmentTimeline>`, flattened: `starts[i]` is when segment `i` starts and `end` when the
    /// last one ends. Repeats (`r=`), gaps (`t=`) and `presentationTimeOffset` are already
    /// applied, so lookup is a binary search.
    Explicit { starts: Vec<u64>, end: u64 },
}

/// The segment containing a point in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentAt {
    /// The `$Number$` to request.
    pub number: u32,
    /// Position in the list, counting from zero regardless of `startNumber`.
    pub index: u32,
    /// Where the segment starts, at or before the requested time.
    pub start: Duration,
}

impl DashSegments {
    /// Builds the absolute URL for media segment `number` (as used by `SegmentTemplate`'s
    /// `$Number$` substitution).
    pub fn segment_url(&self, number: u32) -> String {
        self.media_url_template
            .replace("$Number$", &number.to_string())
    }

    /// The inclusive range of segment numbers to download, when the count is known.
    pub fn segment_range(&self) -> Option<std::ops::RangeInclusive<u32>> {
        self.segment_range_from(self.start_number)
    }

    /// Like [`DashSegments::segment_range`], starting at segment `first`. Empty if `first` is
    /// past the last segment.
    pub fn segment_range_from(&self, first: u32) -> Option<std::ops::RangeInclusive<u32>> {
        self.segment_count
            .map(|count| first..=(self.start_number + count.saturating_sub(1)))
    }

    /// The track's length, when the manifest tells us.
    pub fn total_duration(&self) -> Option<Duration> {
        match &self.timing {
            SegmentTiming::Explicit { end, .. } => Some(ticks_to_duration(*end, self.timescale)),
            _ => self.presentation_duration,
        }
    }

    /// The segment that contains `at` (a segment covers `[start, next start)`, so a time exactly
    /// on a boundary belongs to the segment that starts there). `None` when the timing is
    /// unknown or `at` is at or past the end of the track.
    pub fn segment_for_time(&self, at: Duration) -> Option<SegmentAt> {
        let ticks = duration_to_ticks(at, self.timescale);
        let (index, start_ticks) = match &self.timing {
            SegmentTiming::Unknown => return None,
            SegmentTiming::Explicit { starts, end } => {
                if ticks >= *end {
                    return None;
                }
                let index = starts
                    .partition_point(|start| *start <= ticks)
                    .saturating_sub(1);
                (u32::try_from(index).ok()?, *starts.get(index)?)
            }
            SegmentTiming::Constant { ticks: length } => {
                if *length == 0 || self.total_duration().is_some_and(|total| at >= total) {
                    return None;
                }
                let index = u32::try_from(ticks / length).ok()?;
                if self.segment_count.is_some_and(|count| index >= count) {
                    return None;
                }
                (index, u64::from(index) * length)
            }
        };
        Some(SegmentAt {
            number: self.start_number.checked_add(index)?,
            index,
            start: ticks_to_duration(start_ticks, self.timescale),
        })
    }
}

fn ticks_to_duration(ticks: u64, timescale: u64) -> Duration {
    let timescale = timescale.max(1);
    Duration::new(
        ticks / timescale,
        ((ticks % timescale) as u128 * 1_000_000_000 / timescale as u128) as u32,
    )
}

/// Rounds to the nearest tick: `ticks_to_duration` truncates to whole nanoseconds, so truncating
/// here too would turn a segment's exact start time into one tick before it, i.e. the previous
/// segment.
fn duration_to_ticks(duration: Duration, timescale: u64) -> u64 {
    let scaled = duration.as_nanos() * u128::from(timescale.max(1));
    u64::try_from((scaled + 500_000_000) / 1_000_000_000).unwrap_or(u64::MAX)
}

/// One `<S t= d= r=>` element of a `<SegmentTimeline>`.
struct TimelineEntry {
    t: Option<u64>,
    d: u64,
    /// Extra repeats after the first; negative means "until the end of the period".
    r: i64,
}

/// Turns a timeline into segment start times and the end time, minus `presentation_time_offset`.
/// `None` when an entry repeats without a bound, which needs the period length to expand.
fn flatten_timeline(
    entries: &[TimelineEntry],
    presentation_time_offset: u64,
) -> Option<(Vec<u64>, u64)> {
    let (mut starts, mut cursor) = (Vec::new(), 0u64);
    for entry in entries {
        if entry.r < 0 {
            return None;
        }
        if let Some(t) = entry.t {
            cursor = t;
        }
        for _ in 0..=entry.r {
            starts.push(cursor.saturating_sub(presentation_time_offset));
            cursor = cursor.saturating_add(entry.d);
        }
    }
    Some((starts, cursor.saturating_sub(presentation_time_offset)))
}

/// Resolves a (possibly relative) URL against an optional `<BaseURL>`.
///
/// TIDAL's manifests have so far always used absolute `initialization`/`media` URLs, but the
/// DASH spec allows them to be relative to `<BaseURL>`, so we handle both.
fn join_url(base: Option<&str>, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }
    match base {
        Some(base) if base.ends_with('/') => format!("{base}{url}"),
        Some(base) => format!("{base}/{url}"),
        None => url.to_string(),
    }
}

/// Parses an ISO 8601 duration of the form `PT#H#M#.###S` (only the components TIDAL manifests
/// use) into a number of seconds. Any of H/M/S may be omitted.
pub fn parse_iso8601_duration_secs(s: &str) -> Result<f64> {
    let rest = s
        .trim()
        .strip_prefix("PT")
        .ok_or_else(|| anyhow!("invalid ISO8601 duration (missing the PT prefix): {s}"))?;

    let mut secs = 0f64;
    let mut num = String::new();
    for c in rest.chars() {
        match c {
            '0'..='9' | '.' => num.push(c),
            'H' => {
                secs += parse_component(&num, s)? * 3600.0;
                num.clear();
            }
            'M' => {
                secs += parse_component(&num, s)? * 60.0;
                num.clear();
            }
            'S' => {
                secs += parse_component(&num, s)?;
                num.clear();
            }
            other => bail!("unexpected character '{other}' in ISO8601 duration: {s}"),
        }
    }
    Ok(secs)
}

fn parse_component(num: &str, whole: &str) -> Result<f64> {
    num.parse::<f64>()
        .with_context(|| format!("could not parse number '{num}' in ISO8601 duration: {whole}"))
}

/// Parses an MPD (DASH manifest) XML document, extracting the pieces needed to download a
/// single audio representation: init/media segment URLs, `startNumber`, and (if determinable)
/// the total segment count.
pub fn parse_mpd(xml: &str) -> Result<DashSegments> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut base_url: Option<String> = None;
    let mut pending_base_url = false;

    let mut init_url: Option<String> = None;
    let mut media_url: Option<String> = None;
    let mut start_number: u32 = 1;
    let mut timescale: Option<u64> = None;
    let mut duration: Option<u64> = None;
    let mut media_presentation_duration: Option<String> = None;

    let mut presentation_time_offset: u64 = 0;
    let mut in_segment_timeline = false;
    let mut has_timeline = false;
    let mut timeline: Vec<TimelineEntry> = Vec::new();
    let mut codecs: Option<String> = None;

    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .context("error reading the MPD manifest XML")?
        {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"MPD" => {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == b"mediaPresentationDuration" {
                            media_presentation_duration = Some(attr_value(&a)?);
                        }
                    }
                }
                b"Representation" => {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == b"codecs" {
                            codecs = Some(attr_value(&a)?);
                        }
                    }
                }
                b"SegmentTemplate" => {
                    for a in e.attributes().flatten() {
                        match a.key.as_ref() {
                            b"initialization" => init_url = Some(attr_value(&a)?),
                            b"media" => media_url = Some(attr_value(&a)?),
                            b"timescale" => timescale = attr_value(&a)?.parse().ok(),
                            b"duration" => duration = attr_value(&a)?.parse().ok(),
                            b"startNumber" => start_number = attr_value(&a)?.parse().unwrap_or(1),
                            b"presentationTimeOffset" => {
                                presentation_time_offset = attr_value(&a)?.parse().unwrap_or(0)
                            }
                            _ => {}
                        }
                    }
                }
                b"SegmentTimeline" => {
                    in_segment_timeline = true;
                    has_timeline = true;
                }
                b"S" if in_segment_timeline => {
                    let mut entry = TimelineEntry {
                        t: None,
                        d: 0,
                        r: 0,
                    };
                    for a in e.attributes().flatten() {
                        match a.key.as_ref() {
                            b"t" => entry.t = attr_value(&a)?.parse().ok(),
                            b"d" => entry.d = attr_value(&a)?.parse().unwrap_or(0),
                            b"r" => entry.r = attr_value(&a)?.parse().unwrap_or(0),
                            _ => {}
                        }
                    }
                    timeline.push(entry);
                }
                b"BaseURL" => {
                    pending_base_url = true;
                }
                _ => {}
            },
            Event::Text(t) => {
                if pending_base_url {
                    // Entities (e.g. `&amp;` in a pre-signed URL's query string) arrive as
                    // separate `GeneralRef` events, not inline here, so this text fragment never
                    // contains an unresolved entity; it only needs a charset decode.
                    let text = t.decode().context("error decoding <BaseURL> text")?;
                    base_url.get_or_insert_with(String::new).push_str(&text);
                }
            }
            Event::GeneralRef(r) if pending_base_url => {
                let dest = base_url.get_or_insert_with(String::new);
                if let Some(ch) = r
                    .resolve_char_ref()
                    .context("resolving a character reference in <BaseURL>")?
                {
                    dest.push(ch);
                } else {
                    let name = r.decode().context("decoding an entity in <BaseURL>")?;
                    let resolved = quick_xml::escape::resolve_predefined_entity(&name)
                        .ok_or_else(|| anyhow!("unknown XML entity in <BaseURL>: &{name};"))?;
                    dest.push_str(resolved);
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"BaseURL" => pending_base_url = false,
                b"SegmentTimeline" => in_segment_timeline = false,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }

    let init_url = init_url
        .ok_or_else(|| anyhow!("the MPD manifest has no <SegmentTemplate initialization=...>"))?;
    let media_url =
        media_url.ok_or_else(|| anyhow!("the MPD manifest has no <SegmentTemplate media=...>"))?;

    let count_from_duration = |segment_ticks: u64| -> Result<Option<u32>> {
        let (Some(mpd_duration), Some(timescale)) =
            (media_presentation_duration.as_deref(), timescale)
        else {
            return Ok(None);
        };
        let total_secs = parse_iso8601_duration_secs(mpd_duration)?;
        Ok(Some(
            (total_secs * timescale as f64 / segment_ticks as f64).ceil() as u32,
        ))
    };

    let (segment_count, timing) = match (
        has_timeline,
        flatten_timeline(&timeline, presentation_time_offset),
    ) {
        (true, Some((starts, end))) => {
            let count = u32::try_from(starts.len())
                .context("the <SegmentTimeline> segment count is too large")?;
            (Some(count), SegmentTiming::Explicit { starts, end })
        }
        // An unbounded repeat: every segment lasts the entry's `d`, and the period says how many.
        (true, None) => match timeline.first().map(|entry| entry.d).filter(|d| *d > 0) {
            Some(ticks) => (
                count_from_duration(ticks)?,
                SegmentTiming::Constant { ticks },
            ),
            None => (None, SegmentTiming::Unknown),
        },
        (false, _) => match duration.filter(|d| *d > 0) {
            Some(ticks) => (
                count_from_duration(ticks)?,
                SegmentTiming::Constant { ticks },
            ),
            None => (None, SegmentTiming::Unknown),
        },
    };

    let presentation_duration = media_presentation_duration
        .as_deref()
        .and_then(|value| parse_iso8601_duration_secs(value).ok())
        .and_then(|secs| Duration::try_from_secs_f64(secs).ok());

    Ok(DashSegments {
        init_url: join_url(base_url.as_deref(), &init_url),
        media_url_template: join_url(base_url.as_deref(), &media_url),
        start_number,
        segment_count,
        codecs,
        timescale: timescale.unwrap_or(1),
        timing,
        presentation_duration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MPD_WITH_TIMELINE: &str = r#"
        <MPD mediaPresentationDuration="PT0H0M0.000S">
          <Period>
            <AdaptationSet mimeType="audio/mp4">
              <Representation codecs="flac" bandwidth="9216000">
                <BaseURL>https://sp-ad-cf.audio.tidal.com/mediatracks/abc/</BaseURL>
                <SegmentTemplate initialization="init.mp4" media="chunk-$Number$.m4s" timescale="96000" startNumber="1">
                  <SegmentTimeline>
                    <S d="192000" r="10"/>
                    <S d="100000"/>
                  </SegmentTimeline>
                </SegmentTemplate>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>
    "#;

    const MPD_WITH_DURATION_ONLY: &str = r#"
        <MPD mediaPresentationDuration="PT3M25.123S">
          <Period>
            <AdaptationSet mimeType="audio/mp4">
              <Representation codecs="flac" bandwidth="9216000">
                <BaseURL>https://audio.example.com/track/</BaseURL>
                <SegmentTemplate initialization="init.mp4" media="chunk-$Number$.m4s" timescale="44100" duration="88200" startNumber="1"/>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>
    "#;

    #[test]
    fn ampersands_in_signed_urls_are_unescaped() {
        // Regression test: TIDAL's real manifests put pre-signed CDN URLs (with `&`-separated
        // query parameters, escaped as `&amp;` per the XML spec) in `initialization`/`media`.
        // Reading the raw attribute bytes without unescaping corrupts the URL's signature,
        // which the CDN then rejects with a 404/403.
        let xml = r#"
            <MPD>
              <Period>
                <AdaptationSet mimeType="audio/mp4">
                  <Representation codecs="flac">
                    <BaseURL>https://cdn.example.com/track/?a=1&amp;b=2/</BaseURL>
                    <SegmentTemplate initialization="init.mp4?x=1&amp;y=2" media="chunk-$Number$.m4s?x=1&amp;y=2" startNumber="1"/>
                  </Representation>
                </AdaptationSet>
              </Period>
            </MPD>
        "#;
        let parsed = parse_mpd(xml).expect("should parse");
        assert_eq!(
            parsed.init_url,
            "https://cdn.example.com/track/?a=1&b=2/init.mp4?x=1&y=2"
        );
        assert_eq!(
            parsed.media_url_template,
            "https://cdn.example.com/track/?a=1&b=2/chunk-$Number$.m4s?x=1&y=2"
        );
    }

    #[test]
    fn segment_timeline_counts_s_elements_with_repeat() {
        let parsed = parse_mpd(MPD_WITH_TIMELINE).expect("should parse");
        // <S d="192000" r="10"/> -> 1 + 10 = 11, plus <S d="100000"/> -> 1 = 12 total.
        assert_eq!(parsed.segment_count, Some(12));
    }

    #[test]
    fn relative_urls_are_resolved_against_base_url() {
        let parsed = parse_mpd(MPD_WITH_TIMELINE).expect("should parse");
        assert_eq!(
            parsed.init_url,
            "https://sp-ad-cf.audio.tidal.com/mediatracks/abc/init.mp4"
        );
        assert_eq!(
            parsed.media_url_template,
            "https://sp-ad-cf.audio.tidal.com/mediatracks/abc/chunk-$Number$.m4s"
        );
        assert_eq!(parsed.start_number, 1);
        assert_eq!(
            parsed.segment_url(3),
            "https://sp-ad-cf.audio.tidal.com/mediatracks/abc/chunk-3.m4s"
        );
        assert_eq!(parsed.codecs.as_deref(), Some("flac"));
    }

    #[test]
    fn duration_and_timescale_fallback_computes_segment_count() {
        let parsed = parse_mpd(MPD_WITH_DURATION_ONLY).expect("should parse");
        // 205.123s * 44100 / 88200 = 102.5615 -> ceil -> 103.
        assert_eq!(parsed.segment_count, Some(103));
        assert_eq!(parsed.init_url, "https://audio.example.com/track/init.mp4");
    }

    #[test]
    fn absolute_urls_are_left_untouched_even_without_base_url() {
        let xml = r#"
            <MPD>
              <Period>
                <AdaptationSet mimeType="audio/mp4">
                  <Representation codecs="flac">
                    <SegmentTemplate initialization="https://cdn.example.com/init.mp4" media="https://cdn.example.com/chunk-$Number$.m4s" startNumber="0"/>
                  </Representation>
                </AdaptationSet>
              </Period>
            </MPD>
        "#;
        let parsed = parse_mpd(xml).expect("should parse");
        assert_eq!(parsed.init_url, "https://cdn.example.com/init.mp4");
        assert_eq!(parsed.start_number, 0);
        assert_eq!(parsed.segment_count, None);
    }

    #[test]
    fn parse_iso8601_duration_handles_minutes_and_fractional_seconds() {
        let secs = parse_iso8601_duration_secs("PT3M25.123S").expect("should parse");
        assert!((secs - 205.123).abs() < 1e-9);
    }

    #[test]
    fn parse_iso8601_duration_handles_seconds_only() {
        let secs = parse_iso8601_duration_secs("PT96S").expect("should parse");
        assert!((secs - 96.0).abs() < 1e-9);
    }

    #[test]
    fn parse_iso8601_duration_rejects_missing_prefix() {
        assert!(parse_iso8601_duration_secs("3M25S").is_err());
    }

    #[test]
    fn segment_range_uses_start_number_and_count() {
        let parsed = parse_mpd(MPD_WITH_TIMELINE).expect("should parse");
        assert_eq!(parsed.segment_range(), Some(1..=12));
    }

    #[test]
    fn missing_segment_template_is_an_error() {
        let xml = "<MPD><Period><AdaptationSet mimeType=\"audio/mp4\"/></Period></MPD>";
        assert!(parse_mpd(xml).is_err());
    }

    /// The shape of a real TIDAL HiRes manifest (24-bit/192 kHz, 5:48.68): 87 equal segments and
    /// a shorter last one.
    const MPD_TIDAL_LIKE: &str = r#"
        <MPD mediaPresentationDuration="PT5M48.68S">
          <Period><AdaptationSet><Representation codecs="flac">
            <SegmentTemplate timescale="192000" initialization="https://cdn/0.mp4" media="https://cdn/$Number$.mp4" startNumber="1">
              <SegmentTimeline><S d="765952" r="86"/><S d="308736"/></SegmentTimeline>
            </SegmentTemplate>
          </Representation></AdaptationSet></Period>
        </MPD>
    "#;

    fn at(secs: f64) -> Duration {
        Duration::from_secs_f64(secs)
    }

    #[test]
    fn timeline_gives_the_track_length_and_segment_count() {
        let dash = parse_mpd(MPD_TIDAL_LIKE).unwrap();
        assert_eq!(dash.segment_count, Some(88));
        assert_eq!(dash.total_duration(), Some(Duration::from_millis(348_680)));
    }

    #[test]
    fn segment_for_time_finds_the_containing_segment() {
        let dash = parse_mpd(MPD_TIDAL_LIKE).unwrap();
        let first = dash.segment_for_time(Duration::ZERO).unwrap();
        assert_eq!(
            (first.number, first.index, first.start),
            (1, 0, Duration::ZERO)
        );

        // 30 s is 7 full segments of 3.9893 s in, so it falls in segment 8 (index 7).
        let mid = dash.segment_for_time(at(30.0)).unwrap();
        assert_eq!((mid.number, mid.index), (8, 7));
        assert_eq!(mid.start, ticks_to_duration(7 * 765_952, 192_000));
        assert!(mid.start <= at(30.0));

        let last = dash.segment_for_time(at(348.0)).unwrap();
        assert_eq!((last.number, last.index), (88, 87));
    }

    #[test]
    fn a_time_exactly_on_a_boundary_belongs_to_the_segment_that_starts_there() {
        let dash = parse_mpd(MPD_TIDAL_LIKE).unwrap();
        // The boundary is not a whole number of nanoseconds, which is what once put it one tick
        // into the previous segment.
        let boundary = ticks_to_duration(765_952, 192_000);
        assert_eq!(dash.segment_for_time(boundary).unwrap().number, 2);
        assert_eq!(
            dash.segment_for_time(boundary - Duration::from_millis(1))
                .unwrap()
                .number,
            1
        );
    }

    #[test]
    fn segment_for_time_is_none_at_or_past_the_end() {
        let dash = parse_mpd(MPD_TIDAL_LIKE).unwrap();
        assert!(dash.segment_for_time(at(348.6)).is_some());
        assert_eq!(dash.segment_for_time(Duration::from_millis(348_680)), None);
        assert_eq!(dash.segment_for_time(at(1_000.0)), None);
    }

    #[test]
    fn constant_duration_segments_are_found_by_division() {
        // 2 s segments over 205.123 s: 103 of them, the last one partial.
        let dash = parse_mpd(MPD_WITH_DURATION_ONLY).unwrap();
        assert_eq!(dash.timing, SegmentTiming::Constant { ticks: 88_200 });

        let first = dash.segment_for_time(Duration::ZERO).unwrap();
        assert_eq!((first.number, first.start), (1, Duration::ZERO));
        let second = dash.segment_for_time(at(2.0)).unwrap();
        assert_eq!((second.number, second.start), (2, at(2.0)));
        let last = dash.segment_for_time(at(205.0)).unwrap();
        assert_eq!((last.number, last.start), (103, at(204.0)));

        assert_eq!(
            dash.segment_for_time(at(205.2)),
            None,
            "past the end of the last, partial, segment"
        );
    }

    #[test]
    fn timeline_gaps_and_repeats_are_flattened() {
        let xml = r#"
            <MPD><Period><AdaptationSet><Representation>
              <SegmentTemplate timescale="100" initialization="i" media="m$Number$">
                <SegmentTimeline><S t="0" d="100" r="1"/><S t="500" d="100"/></SegmentTimeline>
              </SegmentTemplate>
            </Representation></AdaptationSet></Period></MPD>"#;
        let dash = parse_mpd(xml).unwrap();
        assert_eq!(
            dash.timing,
            SegmentTiming::Explicit {
                starts: vec![0, 100, 500],
                end: 600
            }
        );
        assert_eq!(dash.segment_count, Some(3));

        assert_eq!(dash.segment_for_time(at(1.5)).unwrap().index, 1);
        let in_the_gap = dash.segment_for_time(at(3.0)).unwrap();
        assert_eq!(
            (in_the_gap.index, in_the_gap.start),
            (1, at(1.0)),
            "a gap belongs to the segment before it"
        );
        assert_eq!(dash.segment_for_time(at(5.0)).unwrap().index, 2);
        assert_eq!(dash.segment_for_time(at(6.0)), None);
    }

    #[test]
    fn segment_numbers_follow_start_number() {
        let xml = r#"
            <MPD><Period><AdaptationSet><Representation>
              <SegmentTemplate timescale="100" initialization="i" media="m$Number$" startNumber="5">
                <SegmentTimeline><S d="100" r="3"/></SegmentTimeline>
              </SegmentTemplate>
            </Representation></AdaptationSet></Period></MPD>"#;
        let dash = parse_mpd(xml).unwrap();
        let found = dash.segment_for_time(at(2.5)).unwrap();
        assert_eq!((found.number, found.index), (7, 2));
        assert_eq!(dash.segment_range(), Some(5..=8));
        assert_eq!(dash.segment_range_from(7), Some(7..=8));
        assert!(dash.segment_range_from(9).unwrap().is_empty());
    }

    #[test]
    fn presentation_time_offset_is_subtracted_from_the_timeline() {
        let xml = r#"
            <MPD><Period><AdaptationSet><Representation>
              <SegmentTemplate timescale="100" initialization="i" media="m$Number$" presentationTimeOffset="1000">
                <SegmentTimeline><S t="1000" d="100" r="1"/></SegmentTimeline>
              </SegmentTemplate>
            </Representation></AdaptationSet></Period></MPD>"#;
        let dash = parse_mpd(xml).unwrap();
        assert_eq!(
            dash.timing,
            SegmentTiming::Explicit {
                starts: vec![0, 100],
                end: 200
            }
        );
        assert_eq!(dash.segment_for_time(at(1.5)).unwrap().index, 1);
    }

    #[test]
    fn an_unbounded_repeat_falls_back_to_constant_segments() {
        let xml = r#"
            <MPD mediaPresentationDuration="PT10S"><Period><AdaptationSet><Representation>
              <SegmentTemplate timescale="100" initialization="i" media="m$Number$">
                <SegmentTimeline><S d="100" r="-1"/></SegmentTimeline>
              </SegmentTemplate>
            </Representation></AdaptationSet></Period></MPD>"#;
        let dash = parse_mpd(xml).unwrap();
        assert_eq!(dash.timing, SegmentTiming::Constant { ticks: 100 });
        assert_eq!(dash.segment_count, Some(10));
        assert_eq!(dash.segment_for_time(at(9.5)).unwrap().number, 10);
    }

    #[test]
    fn without_timing_information_seeking_is_impossible() {
        let xml = r#"<MPD><Period><AdaptationSet><Representation>
              <SegmentTemplate initialization="i" media="m$Number$"/>
            </Representation></AdaptationSet></Period></MPD>"#;
        let dash = parse_mpd(xml).unwrap();
        assert_eq!(dash.timing, SegmentTiming::Unknown);
        assert_eq!(dash.total_duration(), None);
        assert_eq!(dash.segment_for_time(at(1.0)), None);
    }

    #[test]
    fn the_last_tidal_segment_range_can_resume_from_the_middle() {
        let dash = parse_mpd(MPD_TIDAL_LIKE).unwrap();
        assert_eq!(dash.segment_range_from(30), Some(30..=88));
    }
}
