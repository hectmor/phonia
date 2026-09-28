//! Turning wire values into short text, shared by every client that prints them.

use crate::dto::{
    AlbumKind, AlbumSummary, ArtistRef, ArtistSummary, PlaylistSummary, StreamQuality, TrackSummary,
};

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

/// `Korn, Jonathan Davis`; empty when there are no artists.
pub fn artists(artists: &[ArtistRef]) -> String {
    artists
        .iter()
        .map(|artist| artist.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The name with its version, `Falling Away from Me (Remastered)`. TIDAL often writes the version
/// into the title as well as giving it apart (`Requiem Mass (Deluxe Edition)` with the version
/// `Deluxe Edition`), and then it is not said twice.
fn titled(title: &str, version: Option<&str>) -> String {
    match version {
        Some(version)
            if !version.is_empty() && !title.to_lowercase().contains(&version.to_lowercase()) =>
        {
            format!("{title} ({version})")
        }
        _ => title.to_string(),
    }
}

/// `1 track`, `14 tracks`.
fn count_of_tracks(count: u32) -> String {
    if count == 1 {
        "1 track".to_string()
    } else {
        format!("{count} tracks")
    }
}

/// `Korn - Here to Stay (Remastered) - Untouchables - 4:31 - hires`, leaving out what is not known.
pub fn track(track: &TrackSummary) -> String {
    let mut parts = Vec::new();
    let artists = artists(&track.artists);
    let title = titled(&track.title, track.version.as_deref());
    parts.push(if artists.is_empty() {
        title
    } else {
        format!("{artists} - {title}")
    });
    if let Some(album) = &track.album {
        parts.push(album.title.clone());
    }
    if let Some(ms) = track.duration_ms {
        parts.push(self::ms(ms));
    }
    if let Some(quality) = track.quality {
        parts.push(quality.to_string());
    }
    if track.explicit {
        parts.push("explicit".to_string());
    }
    if !track.streamable {
        parts.push("not available".to_string());
    }
    parts.join(" - ")
}

/// `Here to Stay (Remastered) - 4:31 - hires`: a track inside a list where its album and artists
/// are already known (the album it is on), so they are not repeated.
pub fn track_short(track: &TrackSummary) -> String {
    let mut parts = vec![titled(&track.title, track.version.as_deref())];
    if let Some(ms) = track.duration_ms {
        parts.push(self::ms(ms));
    }
    if let Some(quality) = track.quality {
        parts.push(quality.to_string());
    }
    if track.explicit {
        parts.push("explicit".to_string());
    }
    if !track.streamable {
        parts.push("not available".to_string());
    }
    parts.join(" - ")
}

/// `Korn - Untouchables - 2002 - 14 tracks - hires`.
pub fn album(album: &AlbumSummary) -> String {
    let mut parts = Vec::new();
    let artists = artists(&album.artists);
    let title = titled(&album.title, album.version.as_deref());
    parts.push(if artists.is_empty() {
        title
    } else {
        format!("{artists} - {title}")
    });
    if let Some(year) = album.release_date.as_deref().and_then(|date| date.get(..4)) {
        parts.push(year.to_string());
    }
    match album.kind {
        Some(AlbumKind::Ep) => parts.push("EP".to_string()),
        Some(AlbumKind::Single) => parts.push("single".to_string()),
        _ => {}
    }
    if let Some(count) = album.track_count {
        parts.push(count_of_tracks(count));
    }
    if let Some(quality) = album.quality {
        parts.push(quality.to_string());
    }
    if album.explicit {
        parts.push("explicit".to_string());
    }
    parts.join(" - ")
}

pub fn artist(artist: &ArtistSummary) -> String {
    artist.name.clone()
}

/// `Nu metal - TIDAL - 40 tracks`.
pub fn playlist(playlist: &PlaylistSummary) -> String {
    let mut parts = vec![playlist.title.clone()];
    if let Some(creator) = &playlist.creator {
        parts.push(creator.clone());
    }
    if let Some(count) = playlist.track_count {
        parts.push(count_of_tracks(count));
    }
    parts.join(" - ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{AlbumRef, Quality};

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

    fn a_track() -> TrackSummary {
        TrackSummary {
            id: "1".into(),
            title: "Here to Stay".into(),
            version: Some("Remastered".into()),
            artists: vec![
                ArtistRef {
                    id: "1".into(),
                    name: "Korn".into(),
                },
                ArtistRef {
                    id: "2".into(),
                    name: "Jonathan Davis".into(),
                },
            ],
            album: Some(AlbumRef {
                id: "9".into(),
                title: "Untouchables".into(),
            }),
            duration_ms: Some(271_000),
            explicit: true,
            track_number: Some(2),
            volume_number: None,
            quality: Some(Quality::Hires),
            streamable: true,
        }
    }

    #[test]
    fn a_track_reads_as_one_line_with_what_is_known() {
        assert_eq!(
            track(&a_track()),
            "Korn, Jonathan Davis - Here to Stay (Remastered) - Untouchables - 4:31 - hires - explicit"
        );
        let bare = TrackSummary {
            id: "2".into(),
            title: "Untitled".into(),
            version: Some(String::new()),
            artists: vec![],
            album: None,
            duration_ms: None,
            explicit: false,
            track_number: None,
            volume_number: None,
            quality: None,
            streamable: false,
        };
        assert_eq!(track(&bare), "Untitled - not available");
    }

    #[test]
    fn a_version_already_in_the_title_is_not_said_twice() {
        assert_eq!(
            titled("Requiem Mass (Deluxe Edition)", Some("Deluxe Edition")),
            "Requiem Mass (Deluxe Edition)"
        );
        assert_eq!(
            titled("Song (live)", Some("Live")),
            "Song (live)",
            "whatever the case"
        );
        assert_eq!(
            titled("Falling Away from Me", Some("Remastered")),
            "Falling Away from Me (Remastered)"
        );
    }

    #[test]
    fn one_track_is_not_one_tracks() {
        assert_eq!(count_of_tracks(1), "1 track");
        assert_eq!(count_of_tracks(0), "0 tracks");
        assert_eq!(count_of_tracks(14), "14 tracks");
    }

    #[test]
    fn a_track_in_a_list_leaves_out_the_artists_and_the_album() {
        assert_eq!(
            track_short(&a_track()),
            "Here to Stay (Remastered) - 4:31 - hires - explicit"
        );
        let mut plain = a_track();
        plain.version = None;
        plain.duration_ms = None;
        plain.quality = None;
        plain.explicit = false;
        plain.streamable = false;
        assert_eq!(track_short(&plain), "Here to Stay - not available");
    }

    #[test]
    fn an_album_shows_its_year_and_size() {
        let album = AlbumSummary {
            id: "9".into(),
            title: "Untouchables".into(),
            version: None,
            artists: vec![ArtistRef {
                id: "1".into(),
                name: "Korn".into(),
            }],
            release_date: Some("2002-06-11".into()),
            track_count: Some(14),
            duration_ms: None,
            explicit: false,
            quality: Some(Quality::Lossless),
            kind: Some(AlbumKind::Album),
            copyright: None,
        };
        assert_eq!(
            super::album(&album),
            "Korn - Untouchables - 2002 - 14 tracks - lossless"
        );
        // An EP or a single says so; an album does not need to.
        let ep = AlbumSummary {
            kind: Some(AlbumKind::Ep),
            ..album.clone()
        };
        assert!(super::album(&ep).contains("2002 - EP - 14 tracks"));
        let single = AlbumSummary {
            kind: Some(AlbumKind::Single),
            ..album
        };
        assert!(super::album(&single).contains("- single -"));
    }

    #[test]
    fn a_playlist_shows_its_creator_and_size() {
        let playlist = PlaylistSummary {
            id: "u".into(),
            title: "Nu metal".into(),
            creator: Some("TIDAL".into()),
            description: None,
            track_count: Some(40),
            duration_ms: None,
        };
        assert_eq!(super::playlist(&playlist), "Nu metal - TIDAL - 40 tracks");
        assert_eq!(
            artist(&ArtistSummary {
                id: "1".into(),
                name: "Korn".into()
            }),
            "Korn"
        );
    }
}
