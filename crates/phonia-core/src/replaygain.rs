//! ReplayGain: TIDAL's own loudness measurements, and (from here on) what phonia does with them.
//!
//! This module currently holds only the data type that carries TIDAL's loudness/peak numbers from
//! a [`crate::tidal::PlaybackInfo`] into a [`crate::engine::TrackMeta`]. The decision logic (when
//! to use the track's own number versus the album's, how much gain that becomes, and how a shared
//! sink applies it) is added in later parts of issue #30.

use crate::tidal::PlaybackInfo;
use serde::Deserialize;
use std::fmt;

/// TIDAL's loudness measurement for a track, and (when TIDAL has one) for the album it's on.
///
/// `track_db` is always present when this exists at all: TIDAL's `playbackinfopostpaywall` either
/// sends a track gain or doesn't send loudness data at all. The other three fields are `None`
/// whenever TIDAL omits them (no peak reported for the track, or the track isn't on an album TIDAL
/// measured).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    pub track_db: f32,
    pub track_peak: Option<f32>,
    pub album_db: Option<f32>,
    pub album_peak: Option<f32>,
}

impl Loudness {
    /// Reads TIDAL's four loudness fields off a [`PlaybackInfo`]; `None` if TIDAL sent no track
    /// gain (older responses, or a track TIDAL hasn't measured).
    pub fn from_playback_info(info: &PlaybackInfo) -> Option<Loudness> {
        Some(Loudness {
            track_db: info.track_replay_gain? as f32,
            track_peak: info.track_peak_amplitude.map(|peak| peak as f32),
            album_db: info.album_replay_gain.map(|gain| gain as f32),
            album_peak: info.album_peak_amplitude.map(|peak| peak as f32),
        })
    }
}

/// How phonia decides whether, and to what, ReplayGain is applied. File-only (`[playback]
/// replaygain` in `config.toml`), no command-line flag: this changes what a sample actually
/// sounds like, so it is a deliberate, written-down choice, not something to flip per run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Never applied. The default: phonia alters no sample until asked to.
    #[default]
    Off,
    /// Always the track's own gain.
    Track,
    /// Always the album's gain (falling back to the track's own when TIDAL has none for the
    /// album, rather than applying nothing).
    Album,
    /// The album's gain when the track sits next to another entry of the same album in play
    /// order and shuffle is off; the track's own gain otherwise.
    Auto,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::Off => "off",
            Mode::Track => "track",
            Mode::Album => "album",
            Mode::Auto => "auto",
        })
    }
}

/// Which of a track's two possible gains was actually used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Track,
    Album,
}

/// The gain [`choose`] decided on for one track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppliedGain {
    pub kind: Kind,
    /// The gain to apply, in dB, after peak-based clip protection.
    pub db: f32,
}

impl AppliedGain {
    /// The same gain as a linear amplitude multiplier, for scaling samples directly.
    pub fn linear(&self) -> f32 {
        10f32.powf(self.db / 20.0)
    }
}

/// Caps a positive (boost) gain so the track's own true peak never clips; a cut (zero or
/// negative) is never touched, whatever the peak says. A missing peak is treated as "could clip
/// at any boost," capping one at 0 dB rather than trusting it to be safe.
fn clip_cap(db: f32, peak: Option<f32>) -> f32 {
    if db <= 0.0 {
        return db;
    }
    match peak {
        Some(peak) if peak > 0.0 => db.min(-20.0 * peak.log10()),
        _ => db.min(0.0),
    }
}

/// Decides the gain to apply for one track, given the mode in force and whether it sits in an
/// album context (see [`Mode::Auto`]). `None` means apply no gain at all (`Mode::Off`).
pub fn choose(loudness: &Loudness, mode: Mode, same_album_neighbor: bool) -> Option<AppliedGain> {
    let want_album = match mode {
        Mode::Off => return None,
        Mode::Track => false,
        Mode::Album => true,
        Mode::Auto => same_album_neighbor,
    };
    let (kind, db, peak) = if want_album && let Some(album_db) = loudness.album_db {
        (
            Kind::Album,
            album_db,
            loudness.album_peak.or(loudness.track_peak),
        )
    } else {
        (Kind::Track, loudness.track_db, loudness.track_peak)
    };
    Some(AppliedGain {
        kind,
        db: clip_cap(db, peak),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tidal::ManifestKind;

    fn playback_info() -> PlaybackInfo {
        PlaybackInfo {
            track_id: 1,
            audio_mode: "STEREO".into(),
            audio_quality: "LOSSLESS".into(),
            manifest_mime_type: "application/vnd.tidal.bts".into(),
            bit_depth: None,
            sample_rate: None,
            track_replay_gain: None,
            track_peak_amplitude: None,
            album_replay_gain: None,
            album_peak_amplitude: None,
            manifest: ManifestKind::Json {
                url: "http://cdn/a.flac".into(),
                codecs: "flac".into(),
            },
        }
    }

    #[test]
    fn no_track_gain_means_no_loudness_at_all() {
        let info = playback_info();
        assert_eq!(Loudness::from_playback_info(&info), None);
    }

    #[test]
    fn a_track_gain_with_nothing_else_leaves_the_rest_none() {
        let info = PlaybackInfo {
            track_replay_gain: Some(-6.5),
            ..playback_info()
        };
        let loudness = Loudness::from_playback_info(&info).unwrap();
        assert_eq!(loudness.track_db, -6.5);
        assert_eq!(loudness.track_peak, None);
        assert_eq!(loudness.album_db, None);
        assert_eq!(loudness.album_peak, None);
    }

    #[test]
    fn all_four_fields_come_through() {
        let info = PlaybackInfo {
            track_replay_gain: Some(-6.5),
            track_peak_amplitude: Some(0.98),
            album_replay_gain: Some(-7.2),
            album_peak_amplitude: Some(0.99),
            ..playback_info()
        };
        let loudness = Loudness::from_playback_info(&info).unwrap();
        assert_eq!(loudness.track_db, -6.5);
        assert_eq!(loudness.track_peak, Some(0.98));
        assert_eq!(loudness.album_db, Some(-7.2));
        assert_eq!(loudness.album_peak, Some(0.99));
    }

    fn loudness(
        track_db: f32,
        track_peak: Option<f32>,
        album: Option<(f32, Option<f32>)>,
    ) -> Loudness {
        Loudness {
            track_db,
            track_peak,
            album_db: album.map(|(db, _)| db),
            album_peak: album.and_then(|(_, peak)| peak),
        }
    }

    #[test]
    fn mode_parses_from_lowercase_text() {
        #[derive(Deserialize)]
        struct Wrapper {
            mode: Mode,
        }
        let parse =
            |text: &str| toml::from_str::<Wrapper>(&format!("mode = \"{text}\"")).map(|w| w.mode);
        assert_eq!(parse("off"), Ok(Mode::Off));
        assert_eq!(parse("track"), Ok(Mode::Track));
        assert_eq!(parse("album"), Ok(Mode::Album));
        assert_eq!(parse("auto"), Ok(Mode::Auto));
        assert!(parse("Auto").is_err());
    }

    #[test]
    fn off_applies_nothing_whatever_the_context() {
        let loud = loudness(-6.0, Some(0.9), Some((-7.0, Some(0.9))));
        assert_eq!(choose(&loud, Mode::Off, true), None);
        assert_eq!(choose(&loud, Mode::Off, false), None);
    }

    #[test]
    fn track_mode_always_uses_the_track_gain() {
        let loud = loudness(-6.0, Some(0.9), Some((-7.0, Some(0.95))));
        let applied = choose(&loud, Mode::Track, true).unwrap();
        assert_eq!(applied.kind, Kind::Track);
        assert_eq!(applied.db, -6.0);
    }

    #[test]
    fn album_mode_uses_the_album_gain_when_there_is_one() {
        let loud = loudness(-6.0, Some(0.9), Some((-7.0, Some(0.95))));
        let applied = choose(&loud, Mode::Album, false).unwrap();
        assert_eq!(applied.kind, Kind::Album);
        assert_eq!(applied.db, -7.0);
    }

    #[test]
    fn album_mode_falls_back_to_track_when_tidal_has_no_album_gain() {
        let loud = loudness(-6.0, Some(0.9), None);
        let applied = choose(&loud, Mode::Album, false).unwrap();
        assert_eq!(applied.kind, Kind::Track, "honestly reports what was used");
        assert_eq!(applied.db, -6.0);
    }

    #[test]
    fn auto_mode_uses_album_gain_only_next_to_the_same_album() {
        let loud = loudness(-6.0, Some(0.9), Some((-7.0, Some(0.95))));
        assert_eq!(choose(&loud, Mode::Auto, true).unwrap().kind, Kind::Album);
        assert_eq!(choose(&loud, Mode::Auto, false).unwrap().kind, Kind::Track);
    }

    #[test]
    fn a_boost_is_capped_at_the_peak_given_headroom() {
        // A peak of 0.5 leaves ~6.02 dB of headroom before clipping.
        let loud = loudness(10.0, Some(0.5), None);
        let applied = choose(&loud, Mode::Track, false).unwrap();
        assert!((applied.db - 6.0206).abs() < 0.01, "{}", applied.db);
    }

    #[test]
    fn a_boost_with_no_known_peak_is_capped_at_zero() {
        let loud = loudness(10.0, None, None);
        let applied = choose(&loud, Mode::Track, false).unwrap();
        assert_eq!(applied.db, 0.0);
    }

    #[test]
    fn a_cut_is_never_touched_by_the_cap_however_small_the_peak() {
        let loud = loudness(-3.0, Some(0.01), None);
        let applied = choose(&loud, Mode::Track, false).unwrap();
        assert_eq!(applied.db, -3.0);
    }

    #[test]
    fn a_boost_within_headroom_is_not_capped() {
        let loud = loudness(2.0, Some(0.5), None);
        let applied = choose(&loud, Mode::Track, false).unwrap();
        assert_eq!(applied.db, 2.0);
    }

    #[test]
    fn linear_matches_the_familiar_decibel_points() {
        assert!(
            (AppliedGain {
                kind: Kind::Track,
                db: 0.0
            }
            .linear()
                - 1.0)
                .abs()
                < 1e-6
        );
        assert!(
            (AppliedGain {
                kind: Kind::Track,
                db: -6.0206
            }
            .linear()
                - 0.5)
                .abs()
                < 1e-3
        );
        assert!(
            (AppliedGain {
                kind: Kind::Track,
                db: 6.0206
            }
            .linear()
                - 2.0)
                .abs()
                < 1e-3
        );
    }
}
