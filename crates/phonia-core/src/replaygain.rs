//! ReplayGain: TIDAL's own loudness measurements, and (from here on) what phonia does with them.
//!
//! This module currently holds only the data type that carries TIDAL's loudness/peak numbers from
//! a [`crate::tidal::PlaybackInfo`] into a [`crate::engine::TrackMeta`]. The decision logic (when
//! to use the track's own number versus the album's, how much gain that becomes, and how a shared
//! sink applies it) is added in later parts of issue #30.

use crate::tidal::PlaybackInfo;

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
}
