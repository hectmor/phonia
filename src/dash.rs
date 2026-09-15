//! Pure MPD (DASH manifest) parsing helpers.
//!
//! Everything here is a pure function over an XML string / URL, with no network or file I/O,
//! so it is fully unit-testable. `tidlers`'s own `DashManifest` (see
//! `tidlers::client::models::track::playback::DashManifest`) does not expose a segment count and
//! does not resolve relative URLs against `<BaseURL>`, so `tidal.rs` decodes the raw manifest
//! itself and hands the XML text to `parse_mpd` here instead of relying on that type.

use anyhow::{Context, Result, anyhow, bail};
use quick_xml::Reader;
use quick_xml::events::Event;

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
}

impl DashSegments {
    /// Builds the absolute URL for media segment `number` (as used by `SegmentTemplate`'s
    /// `$Number$` substitution).
    pub fn segment_url(&self, number: u32) -> String {
        self.media_url_template.replace("$Number$", &number.to_string())
    }

    /// The inclusive range of segment numbers to download, when the count is known.
    pub fn segment_range(&self) -> Option<std::ops::RangeInclusive<u32>> {
        self.segment_count
            .map(|count| self.start_number..=(self.start_number + count.saturating_sub(1)))
    }
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
        .ok_or_else(|| anyhow!("duración ISO8601 inválida (falta el prefijo PT): {s}"))?;

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
            other => bail!("carácter inesperado '{other}' en duración ISO8601: {s}"),
        }
    }
    Ok(secs)
}

fn parse_component(num: &str, whole: &str) -> Result<f64> {
    num.parse::<f64>()
        .with_context(|| format!("no se pudo parsear el número '{num}' en duración ISO8601: {whole}"))
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

    let mut in_segment_timeline = false;
    let mut timeline_count: Option<u64> = None;
    let mut codecs: Option<String> = None;

    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .context("error leyendo el XML del manifiesto MPD")?
        {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                match e.name().as_ref() {
                    b"MPD" => {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"mediaPresentationDuration" {
                                media_presentation_duration =
                                    Some(String::from_utf8_lossy(&a.value).to_string());
                            }
                        }
                    }
                    b"Representation" => {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"codecs" {
                                codecs = Some(String::from_utf8_lossy(&a.value).to_string());
                            }
                        }
                    }
                    b"SegmentTemplate" => {
                        for a in e.attributes().flatten() {
                            let value = String::from_utf8_lossy(&a.value).to_string();
                            match a.key.as_ref() {
                                b"initialization" => init_url = Some(value),
                                b"media" => media_url = Some(value),
                                b"timescale" => timescale = value.parse().ok(),
                                b"duration" => duration = value.parse().ok(),
                                b"startNumber" => start_number = value.parse().unwrap_or(1),
                                _ => {}
                            }
                        }
                    }
                    b"SegmentTimeline" => {
                        in_segment_timeline = true;
                        timeline_count = Some(0);
                    }
                    b"S" if in_segment_timeline => {
                        let mut r: u64 = 0;
                        for a in e.attributes().flatten() {
                            if a.key.as_ref() == b"r" {
                                let value = String::from_utf8_lossy(&a.value).to_string();
                                r = value.parse().unwrap_or(0);
                            }
                        }
                        *timeline_count.get_or_insert(0) += 1 + r;
                    }
                    b"BaseURL" => {
                        pending_base_url = true;
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if pending_base_url {
                    let text = t
                        .decode()
                        .context("error decodificando texto de <BaseURL>")?
                        .into_owned();
                    if !text.is_empty() {
                        base_url = Some(text);
                    }
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

    let init_url = init_url.ok_or_else(|| anyhow!("el manifiesto MPD no tiene <SegmentTemplate initialization=...>"))?;
    let media_url = media_url.ok_or_else(|| anyhow!("el manifiesto MPD no tiene <SegmentTemplate media=...>"))?;

    let segment_count = if let Some(count) = timeline_count {
        Some(u32::try_from(count).context("el número de segmentos de <SegmentTimeline> es demasiado grande")?)
    } else if let (Some(mpd_duration), Some(timescale), Some(duration)) =
        (media_presentation_duration.as_deref(), timescale, duration)
    {
        let total_secs = parse_iso8601_duration_secs(mpd_duration)?;
        let count = (total_secs * timescale as f64 / duration as f64).ceil();
        Some(count as u32)
    } else {
        None
    };

    Ok(DashSegments {
        init_url: join_url(base_url.as_deref(), &init_url),
        media_url_template: join_url(base_url.as_deref(), &media_url),
        start_number,
        segment_count,
        codecs,
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
        assert_eq!(parsed.segment_url(3), "https://sp-ad-cf.audio.tidal.com/mediatracks/abc/chunk-3.m4s");
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
}
