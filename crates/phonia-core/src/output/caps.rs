//! What an ALSA device can actually play: a channel range, and, for every sample rate it accepts
//! at all, which lossless integer formats it accepts at that exact rate.
//!
//! Found by asking the hardware (`output/alsa.rs`'s `probe`), not assumed: a real device often
//! constrains format and rate jointly (24-bit only up to 96 kHz is common on USB DACs), so a
//! format has to be tested at the rate a track actually wants, not in isolation. Everything here
//! is pure data and pure functions, with no `alsa` crate dependency, so it is unit-tested without
//! opening real hardware; `output/alsa.rs` adapts the real `HwParams` API to the closures
//! [`probe_with`] takes.

use crate::decode::SourceSpec;
use std::collections::BTreeMap;
use std::fmt;

/// A lossless integer container [`crate::output::alsa::AlsaSink`] can pack samples into. Phonia's
/// own vocabulary for this, kept separate from `alsa::pcm::Format` so this module needs no `alsa`
/// crate dependency; `output/alsa.rs` maps between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    S16Le,
    S24_3Le,
    S24Le,
    S32Le,
}

impl SampleFormat {
    /// Every format, in the order a track's bit depth is matched against: the narrowest
    /// container that can hold it losslessly wins, so a 16-bit source is never padded into a
    /// wider one than it has to be. `S32Le` is tried before `S24Le` since both are 4-byte
    /// containers but `S32Le` carries the sample in all 32 bits rather than the low 3 bytes of
    /// the slot, which some devices accept more readily.
    pub const ALL: [SampleFormat; 4] = [
        SampleFormat::S16Le,
        SampleFormat::S24_3Le,
        SampleFormat::S32Le,
        SampleFormat::S24Le,
    ];

    /// How many of the container's bits actually carry the sample. Padding a source into a wider
    /// container with zeros is lossless (see `output::alsa`'s packing functions), so a source of
    /// `bits` fits a format whenever this is at least `bits`.
    pub fn significant_bits(self) -> u32 {
        match self {
            SampleFormat::S16Le => 16,
            SampleFormat::S24_3Le | SampleFormat::S24Le => 24,
            SampleFormat::S32Le => 32,
        }
    }

    /// The size of one sample on the wire.
    pub fn container_bytes(self) -> u32 {
        match self {
            SampleFormat::S16Le => 2,
            SampleFormat::S24_3Le => 3,
            SampleFormat::S24Le | SampleFormat::S32Le => 4,
        }
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SampleFormat::S16Le => "S16_LE",
            SampleFormat::S24_3Le => "S24_3LE",
            SampleFormat::S24Le => "S24_LE",
            SampleFormat::S32Le => "S32_LE",
        })
    }
}

/// The sample rates worth asking a device about, beyond whatever a specific track's own rate
/// adds: every rate TIDAL is known to serve.
pub const STANDARD_RATES: [u32; 8] = [
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
];

/// What a device accepts: a channel count range, and, for every rate it accepts at all, the
/// sample formats it accepts at that exact rate. Only a rate that accepts at least one format is
/// present, so an empty map means nothing lossless was found at any rate tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// The fewest and the most channels the device accepts (equal for a fixed-channel device).
    pub channels: (u32, u32),
    pub rates: BTreeMap<u32, Vec<SampleFormat>>,
}

impl Capabilities {
    /// The formats accepted at exactly `rate`, if the device takes that rate at all.
    pub fn formats_at(&self, rate: u32) -> Option<&[SampleFormat]> {
        self.rates.get(&rate).map(Vec::as_slice)
    }

    /// The matrix `phonia probe-device` prints: the channel range, then every accepted rate with
    /// the formats it takes at that rate.
    pub fn to_text(&self) -> String {
        let mut lines = vec![if self.channels.0 == self.channels.1 {
            format!("Channels: {}", self.channels.0)
        } else {
            format!("Channels: {}-{}", self.channels.0, self.channels.1)
        }];
        if self.rates.is_empty() {
            lines.push("No rate was accepted with any lossless format.".to_string());
        } else {
            lines.push("  Rate (Hz)  Formats".to_string());
            for (rate, formats) in &self.rates {
                let formats = formats
                    .iter()
                    .map(SampleFormat::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!("  {rate:>9}  {formats}"));
            }
        }
        lines.join("\n")
    }
}

/// Probes `channels` (as given, not tested: the caller reads it from the device) and every rate
/// in [`STANDARD_RATES`] plus `extra_rates`, and, at each rate accepted at all, every
/// [`SampleFormat`]. `test_rate` and `test_format_at` are a device's own `HwParams::test_rate`
/// and a rate-narrowed clone's `test_format` (format and rate are tested jointly, at the same
/// narrowed rate, since a real device can constrain them together), so this needs no real
/// hardware to run against.
pub fn probe_with(
    channels: (u32, u32),
    extra_rates: &[u32],
    mut test_rate: impl FnMut(u32) -> bool,
    mut test_format_at: impl FnMut(u32, SampleFormat) -> bool,
) -> Capabilities {
    let mut wanted: Vec<u32> = STANDARD_RATES.to_vec();
    for &rate in extra_rates {
        if !wanted.contains(&rate) {
            wanted.push(rate);
        }
    }
    let mut rates = BTreeMap::new();
    for rate in wanted {
        if !test_rate(rate) {
            continue;
        }
        let formats: Vec<SampleFormat> = SampleFormat::ALL
            .into_iter()
            .filter(|&format| test_format_at(rate, format))
            .collect();
        if !formats.is_empty() {
            rates.insert(rate, formats);
        }
    }
    Capabilities { channels, rates }
}

/// Why a track does not fit what [`probe_with`] found, in [`choose`]'s own order of checking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// The source's bit depth is not one phonia knows how to pack into any container (0, or more
    /// than 32): not a device limitation, a track that cannot be played at all.
    Depth,
    /// The device does not accept the source's channel count at any rate.
    Channels,
    /// The device accepts the source's channel count, but not its exact sample rate, at all.
    Rate,
    /// The device accepts the rate, but every format it offers there is narrower than the
    /// source's bit depth, so playing it would mean dropping bits.
    DepthAtRate { offered: Vec<SampleFormat> },
}

/// A track a device cannot play losslessly, with what was actually found to decide it: precise
/// enough to tell someone exactly what to expect from their hardware, never a bare "try again".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// The device, for people (`hw:0,0 (DS2)`).
    pub device: String,
    pub source: SourceSpec,
    pub missing: Missing,
    pub caps: Capabilities,
}

impl std::error::Error for Unsupported {}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Unsupported {
            device,
            source,
            missing,
            caps,
        } = self;
        match missing {
            Missing::Depth => write!(
                f,
                "a {}-bit source has no lossless container phonia knows how to use (only 1 to 32 \
                 bits are)",
                source.bits_per_sample
            ),
            Missing::Channels => write!(
                f,
                "{device} accepts {}; this track has {} channel(s)",
                channel_range_text(caps.channels),
                source.channels
            ),
            Missing::Rate => write!(
                f,
                "{device} cannot play {} Hz natively; for {}-bit audio it can do {} Hz. phonia \
                 does not resample in exclusive mode; to hear it resampled, play through the \
                 sound server (`phonia ctl output set shared:default`).",
                source.sample_rate,
                source.bits_per_sample,
                rates_text(rates_fitting(caps, source.bits_per_sample)),
            ),
            Missing::DepthAtRate { offered } => write!(
                f,
                "{device} at {} Hz only takes {}, which would drop bits from a {}-bit source",
                source.sample_rate,
                rates_text_formats(offered),
                source.bits_per_sample
            ),
        }
    }
}

fn channel_range_text(range: (u32, u32)) -> String {
    if range.0 == range.1 {
        format!("{} channel(s)", range.0)
    } else {
        format!("{} to {} channels", range.0, range.1)
    }
}

/// The rates, in order, at which `caps` offers at least one format wide enough for `bits`.
fn rates_fitting(caps: &Capabilities, bits: u32) -> Vec<u32> {
    caps.rates
        .iter()
        .filter(|(_, formats)| {
            formats
                .iter()
                .any(|format| format.significant_bits() >= bits)
        })
        .map(|(&rate, _)| rate)
        .collect()
}

fn rates_text(rates: Vec<u32>) -> String {
    rates
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn rates_text_formats(formats: &[SampleFormat]) -> String {
    formats
        .iter()
        .map(SampleFormat::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The tightest lossless container `caps` offers for `source`, or precisely what is missing.
/// Checked in the order a cause is most useful to know about: an invalid depth is wrong
/// regardless of any device, a channel mismatch and a missing rate are each one fact about the
/// device, and a depth that does not fit at an otherwise-accepted rate is the most specific.
pub fn choose(source: SourceSpec, caps: &Capabilities) -> Result<SampleFormat, Missing> {
    if source.bits_per_sample == 0 || source.bits_per_sample > 32 {
        return Err(Missing::Depth);
    }
    if source.channels < caps.channels.0 || source.channels > caps.channels.1 {
        return Err(Missing::Channels);
    }
    let Some(formats) = caps.formats_at(source.sample_rate) else {
        return Err(Missing::Rate);
    };
    formats
        .iter()
        .copied()
        .find(|format| format.significant_bits() >= source.bits_per_sample)
        .ok_or_else(|| Missing::DepthAtRate {
            offered: formats.to_vec(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn significant_bits_and_container_size_match_the_wire_format() {
        assert_eq!(SampleFormat::S16Le.significant_bits(), 16);
        assert_eq!(SampleFormat::S16Le.container_bytes(), 2);
        assert_eq!(SampleFormat::S24_3Le.significant_bits(), 24);
        assert_eq!(SampleFormat::S24_3Le.container_bytes(), 3);
        assert_eq!(SampleFormat::S24Le.significant_bits(), 24);
        assert_eq!(SampleFormat::S24Le.container_bytes(), 4);
        assert_eq!(SampleFormat::S32Le.significant_bits(), 32);
        assert_eq!(SampleFormat::S32Le.container_bytes(), 4);
    }

    #[test]
    fn sample_format_reads_like_alsa_does() {
        assert_eq!(SampleFormat::S16Le.to_string(), "S16_LE");
        assert_eq!(SampleFormat::S24_3Le.to_string(), "S24_3LE");
        assert_eq!(SampleFormat::S24Le.to_string(), "S24_LE");
        assert_eq!(SampleFormat::S32Le.to_string(), "S32_LE");
    }

    /// A device that accepts everything: every standard rate, every format, at every rate.
    #[test]
    fn a_device_that_accepts_everything_reports_every_standard_rate_and_format() {
        let caps = probe_with((2, 2), &[], |_rate| true, |_rate, _format| true);
        assert_eq!(caps.rates.len(), STANDARD_RATES.len());
        for &rate in &STANDARD_RATES {
            assert_eq!(caps.formats_at(rate), Some(SampleFormat::ALL.as_slice()));
        }
    }

    /// A rate the device refuses outright never appears, even if formats would otherwise test
    /// true (the real `HwParams::test_rate`/`test_format` pairing never asks that question, but
    /// the probe's own loop must not either).
    #[test]
    fn a_refused_rate_is_absent_even_if_formats_are_not_tested_at_it() {
        let caps = probe_with((2, 2), &[], |rate| rate != 352_800, |_rate, _format| true);
        assert!(caps.formats_at(352_800).is_none());
        assert!(caps.formats_at(192_000).is_some());
    }

    /// The joint constraint real USB DACs have: 24-bit formats only up to 96 kHz, 16-bit at every
    /// rate. This is the scenario the planning investigation found worth pinning.
    #[test]
    fn formats_are_tested_jointly_with_the_rate_not_independently() {
        let caps = probe_with(
            (2, 2),
            &[],
            |_rate| true,
            |rate, format| match format {
                SampleFormat::S16Le => true,
                _ => rate <= 96_000,
            },
        );
        assert_eq!(
            caps.formats_at(192_000),
            Some([SampleFormat::S16Le].as_slice()),
            "only 16-bit survives above 96 kHz on this fake device"
        );
        assert_eq!(caps.formats_at(96_000), Some(SampleFormat::ALL.as_slice()));
    }

    /// A non-standard rate (a track's own, say), passed as `extra_rates`, is probed too, and does
    /// not duplicate an already-standard one.
    #[test]
    fn extra_rates_are_probed_once_each() {
        let mut asked = Vec::new();
        let caps = probe_with(
            (2, 2),
            &[22_050, 48_000],
            |rate| {
                asked.push(rate);
                true
            },
            |_rate, _format| true,
        );
        assert_eq!(asked.iter().filter(|&&r| r == 48_000).count(), 1);
        assert!(caps.formats_at(22_050).is_some());
    }

    /// A rate with no lossless format at all is left out of the map entirely, not kept with an
    /// empty format list.
    #[test]
    fn a_rate_with_no_accepted_format_is_not_recorded() {
        let caps = probe_with((2, 2), &[], |_rate| true, |_rate, _format| false);
        assert!(caps.rates.is_empty());
    }

    #[test]
    fn to_text_is_pinned_for_a_simple_device() {
        let caps = probe_with(
            (2, 2),
            &[],
            |rate| matches!(rate, 44_100 | 48_000),
            |_rate, format| matches!(format, SampleFormat::S16Le | SampleFormat::S24_3Le),
        );
        assert_eq!(
            caps.to_text(),
            "Channels: 2\n  Rate (Hz)  Formats\n      44100  S16_LE, S24_3LE\n      48000  S16_LE, S24_3LE"
        );
    }

    #[test]
    fn to_text_shows_a_channel_range_and_says_when_nothing_was_accepted() {
        let caps = probe_with((1, 8), &[], |_rate| false, |_rate, _format| true);
        assert_eq!(
            caps.to_text(),
            "Channels: 1-8\nNo rate was accepted with any lossless format."
        );
    }

    // ---- choose ------------------------------------------------------------------------------

    fn source(rate: u32, bits: u32, channels: u32) -> SourceSpec {
        SourceSpec {
            sample_rate: rate,
            channels,
            bits_per_sample: bits,
        }
    }

    /// A device that only has `S24_3LE`, at every TIDAL rate.
    fn ds2_like() -> Capabilities {
        probe_with(
            (2, 2),
            &[],
            |_rate| true,
            |_rate, format| format == SampleFormat::S24_3Le,
        )
    }

    #[test]
    fn a_24_bit_source_picks_the_3_byte_container() {
        assert_eq!(
            choose(source(96_000, 24, 2), &ds2_like()),
            Ok(SampleFormat::S24_3Le)
        );
    }

    #[test]
    fn a_16_bit_source_is_padded_into_the_only_container_offered() {
        // Padding into a wider container is lossless (see `SampleFormat::significant_bits`), so a
        // 16-bit source plays on a 24-bit-only device instead of being refused.
        assert_eq!(
            choose(source(96_000, 16, 2), &ds2_like()),
            Ok(SampleFormat::S24_3Le)
        );
    }

    #[test]
    fn a_20_bit_source_also_fits_the_24_bit_container() {
        assert_eq!(
            choose(source(96_000, 20, 2), &ds2_like()),
            Ok(SampleFormat::S24_3Le)
        );
    }

    #[test]
    fn a_24_bit_source_falls_back_to_s32le_when_s24_3le_is_not_offered() {
        let caps = probe_with(
            (2, 2),
            &[],
            |_rate| true,
            |_rate, format| format == SampleFormat::S32Le,
        );
        assert_eq!(
            choose(source(96_000, 24, 2), &caps),
            Ok(SampleFormat::S32Le)
        );
    }

    #[test]
    fn a_rate_the_device_does_not_have_at_all_is_missing_rate() {
        let caps = probe_with(
            (2, 2),
            &[],
            |rate| rate <= 192_000,
            |_rate, format| format == SampleFormat::S24_3Le,
        );
        assert_eq!(choose(source(352_800, 24, 2), &caps), Err(Missing::Rate));
    }

    #[test]
    fn a_channel_count_outside_the_devices_range_is_missing_channels() {
        assert_eq!(
            choose(source(96_000, 24, 6), &ds2_like()),
            Err(Missing::Channels)
        );
    }

    #[test]
    fn an_accepted_rate_with_only_a_narrower_format_is_missing_depth_at_rate() {
        // 192 kHz only in S16_LE: a real joint constraint (24-bit limited to 96 kHz), the 24-bit
        // source cannot be played there without dropping bits.
        let caps = probe_with(
            (2, 2),
            &[],
            |_rate| true,
            |rate, format| format == SampleFormat::S16Le || rate <= 96_000,
        );
        assert_eq!(
            choose(source(192_000, 24, 2), &caps),
            Err(Missing::DepthAtRate {
                offered: vec![SampleFormat::S16Le]
            })
        );
    }

    #[test]
    fn a_depth_of_zero_or_over_32_bits_is_missing_depth_before_anything_about_the_device() {
        let caps = probe_with((2, 2), &[], |_rate| false, |_rate, _format| false);
        assert_eq!(choose(source(96_000, 0, 2), &caps), Err(Missing::Depth));
        assert_eq!(choose(source(96_000, 33, 2), &caps), Err(Missing::Depth));
    }

    // ---- Unsupported::Display, pinned ---------------------------------------------------------

    #[test]
    fn an_unsupported_rate_names_what_the_device_can_do_at_that_depth() {
        let unsupported = Unsupported {
            device: "hw:0,0 (DS2)".into(),
            source: source(352_800, 24, 2),
            missing: Missing::Rate,
            caps: ds2_like(),
        };
        assert_eq!(
            unsupported.to_string(),
            "hw:0,0 (DS2) cannot play 352800 Hz natively; for 24-bit audio it can do 44100, \
             48000, 88200, 96000, 176400, 192000, 352800, 384000 Hz. phonia does not resample in \
             exclusive mode; to hear it resampled, play through the sound server (`phonia ctl \
             output set shared:default`)."
        );
    }

    #[test]
    fn an_unsupported_depth_at_a_supported_rate_names_the_narrower_format_offered() {
        let unsupported = Unsupported {
            device: "hw:0,0 (DS2)".into(),
            source: source(192_000, 24, 2),
            missing: Missing::DepthAtRate {
                offered: vec![SampleFormat::S16Le],
            },
            caps: ds2_like(),
        };
        assert_eq!(
            unsupported.to_string(),
            "hw:0,0 (DS2) at 192000 Hz only takes S16_LE, which would drop bits from a 24-bit \
             source"
        );
    }

    #[test]
    fn unsupported_channels_names_the_devices_own_range() {
        let unsupported = Unsupported {
            device: "hw:0,0 (DS2)".into(),
            source: source(96_000, 24, 6),
            missing: Missing::Channels,
            caps: ds2_like(),
        };
        assert_eq!(
            unsupported.to_string(),
            "hw:0,0 (DS2) accepts 2 channel(s); this track has 6 channel(s)"
        );
    }

    #[test]
    fn unsupported_depth_names_no_device_at_all() {
        let unsupported = Unsupported {
            device: "hw:0,0 (DS2)".into(),
            source: source(96_000, 33, 2),
            missing: Missing::Depth,
            caps: ds2_like(),
        };
        assert_eq!(
            unsupported.to_string(),
            "a 33-bit source has no lossless container phonia knows how to use (only 1 to 32 \
             bits are)"
        );
    }
}
