//! Turning wire values into short text, shared by every client that prints them.

use crate::dto::StreamQuality;

/// `1:30`, or `1:02:05` from an hour on.
pub fn ms(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds % 3600 / 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

/// What TIDAL delivered: the tier, and what was asked for if it was more.
pub fn stream_quality(quality: &StreamQuality) -> String {
    if quality.fell_back() {
        format!("{} (asked for {})", quality.delivered, quality.requested)
    } else {
        quality.delivered.to_string()
    }
}

/// `44.1 kHz`, `96 kHz`; the plain number of hertz for a rate that isn't a round tenth of a kHz.
pub fn sample_rate(hz: u32) -> String {
    match (hz / 1000, hz % 1000) {
        (khz, 0) => format!("{khz} kHz"),
        (khz, rest) if rest % 100 == 0 => format!("{khz}.{} kHz", rest / 100),
        _ => format!("{hz} Hz"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::Quality;

    #[test]
    fn minutes_below_an_hour_seconds_at_two_digits_and_hours_from_3600() {
        assert_eq!(ms(0), "0:00");
        assert_eq!(ms(65_000), "1:05");
        assert_eq!(ms(3_599_000), "59:59");
        assert_eq!(ms(3_600_000), "1:00:00");
        assert_eq!(ms(3_723_000), "1:02:03");
    }

    #[test]
    fn a_rate_reads_in_khz_when_it_is_a_round_tenth() {
        assert_eq!(sample_rate(44_100), "44.1 kHz");
        assert_eq!(sample_rate(96_000), "96 kHz");
        assert_eq!(sample_rate(192_000), "192 kHz");
        assert_eq!(sample_rate(176_400), "176.4 kHz");
        assert_eq!(sample_rate(22_050), "22050 Hz");
    }

    #[test]
    fn what_tidal_delivered_is_said_plainly_and_a_fallback_says_what_was_asked() {
        let same = StreamQuality {
            requested: Quality::Hires,
            delivered: Quality::Hires,
        };
        let fell = StreamQuality {
            requested: Quality::Hires,
            delivered: Quality::Lossless,
        };
        assert_eq!(stream_quality(&same), "hires");
        assert_eq!(stream_quality(&fell), "lossless (asked for hires)");
    }
}
