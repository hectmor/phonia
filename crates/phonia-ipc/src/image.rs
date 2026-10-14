//! Turning a TIDAL cover/picture id into an actual image URL, at whatever size fits.
//!
//! TIDAL serves cover art from a public CDN that needs no login: the id (a UUID) becomes a path
//! by turning its dashes into slashes, and only a handful of fixed pixel sizes exist per kind of
//! artwork — asking for any other size is a 404/403. [`url`] picks the smallest of them that is at
//! least the caller's `min_px`, or the largest there is if none is big enough, so a terminal cell
//! never gets handed an image too small to look right.

/// What kind of artwork an id is for: each has its own fixed set of sizes TIDAL serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    AlbumCover,
    ArtistPicture,
    PlaylistCover,
}

impl Kind {
    /// The pixel sizes TIDAL actually serves this kind of artwork at, smallest first. `AlbumCover`
    /// and `ArtistPicture` are confirmed against the real CDN, every size, including that one more
    /// than the largest genuinely fails (`a_real_cover_and_a_real_picture_are_served_at_every_claimed_size`).
    /// `PlaylistCover` is the set TIDAL's other clients ask for, not yet confirmed against a real
    /// id here, for lack of one on this account (which has no playlists of its own).
    fn sizes(self) -> &'static [u32] {
        match self {
            Kind::AlbumCover => &[80, 160, 320, 640, 750, 1280],
            Kind::ArtistPicture => &[160, 320, 480, 750],
            Kind::PlaylistCover => &[160, 320, 480, 640, 750, 1080],
        }
    }
}

/// The URL for `id` (a TIDAL cover/picture id, a UUID) of this `kind`, at the smallest size that
/// is at least `min_px`, or the largest size there is if none is big enough. `None` if `id` is
/// empty or isn't shaped like one (only hex digits and dashes) — a client should show no cover
/// rather than ask the CDN for a path built from whatever a field happened to contain.
pub fn url(kind: Kind, id: &str, min_px: u32) -> Option<String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return None;
    }
    let sizes = kind.sizes();
    let size = sizes
        .iter()
        .find(|&&size| size >= min_px)
        .copied()
        .unwrap_or_else(|| *sizes.last().expect("every kind has at least one size"));
    let path = id.replace('-', "/");
    Some(format!(
        "https://resources.tidal.com/images/{path}/{size}x{size}.jpg"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "3c6247c7-d0d7-4978-91b1-0bddc13f45b5";

    #[test]
    fn the_id_becomes_the_url_path_with_slashes_instead_of_dashes() {
        let url = url(Kind::AlbumCover, ID, 320).unwrap();
        assert_eq!(
            url,
            "https://resources.tidal.com/images/3c6247c7/d0d7/4978/91b1/0bddc13f45b5/320x320.jpg"
        );
    }

    #[test]
    fn the_smallest_size_at_least_as_big_as_asked_is_picked() {
        assert!(url(Kind::AlbumCover, ID, 1).unwrap().contains("/80x80.jpg"));
        assert!(
            url(Kind::AlbumCover, ID, 80)
                .unwrap()
                .contains("/80x80.jpg")
        );
        assert!(
            url(Kind::AlbumCover, ID, 81)
                .unwrap()
                .contains("/160x160.jpg")
        );
        assert!(
            url(Kind::AlbumCover, ID, 300)
                .unwrap()
                .contains("/320x320.jpg")
        );
    }

    #[test]
    fn asking_bigger_than_any_size_gets_the_largest_one_there_is() {
        assert!(
            url(Kind::AlbumCover, ID, 100_000)
                .unwrap()
                .contains("/1280x1280.jpg")
        );
    }

    #[test]
    fn each_kind_has_its_own_sizes() {
        assert!(
            url(Kind::ArtistPicture, ID, 1)
                .unwrap()
                .contains("/160x160.jpg")
        );
        assert!(
            url(Kind::PlaylistCover, ID, 1000)
                .unwrap()
                .contains("/1080x1080.jpg")
        );
    }

    #[test]
    fn an_empty_or_malformed_id_has_no_url() {
        assert_eq!(url(Kind::AlbumCover, "", 320), None);
        assert_eq!(url(Kind::AlbumCover, "not a uuid!", 320), None);
        assert_eq!(url(Kind::AlbumCover, "../../etc/passwd", 320), None);
    }

    /// Every size `Kind::sizes` claims for album covers and artist pictures, checked against the
    /// real CDN with two ids known to be real (from `phonia-core`'s own `--ignored` catalog tests,
    /// against this account's real TIDAL login): Korn's "Issues" cover and Korn's own picture.
    /// Needs no TIDAL login itself, only network: the CDN is public. Playlist covers are not
    /// checked here for lack of a known real id (this account has no playlists of its own); the
    /// sizes for that kind are still the ones TIDAL's other clients ask for, just unconfirmed here.
    /// Run with `--ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs network"]
    async fn a_real_cover_and_a_real_picture_are_served_at_every_claimed_size() {
        const ALBUM_COVER: &str = "3c6247c7-d0d7-4978-91b1-0bddc13f45b5";
        const ARTIST_PICTURE: &str = "ca8a29d3-efcd-4cd2-8dea-a376e1c64b1e";
        let client = reqwest::Client::new();
        for (kind, id) in [
            (Kind::AlbumCover, ALBUM_COVER),
            (Kind::ArtistPicture, ARTIST_PICTURE),
        ] {
            for &size in kind.sizes() {
                let url = url(kind, id, size).unwrap();
                let response = client.get(&url).send().await.unwrap();
                let status = response.status();
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                println!("{kind:?} {size}: {status} {content_type} ({url})");
                assert!(status.is_success(), "{url} answered {status}");
                assert_eq!(content_type, "image/jpeg", "{url}");
            }
            // A size bigger than the largest claimed is expected to fail: confirms the list is
            // not just "sizes that happen to work" but the actual, complete set. Built directly,
            // bypassing `url()`, which would otherwise just clamp back down to the largest size.
            let too_big = kind.sizes().last().unwrap() + 1;
            let oversized = format!(
                "https://resources.tidal.com/images/{}/{too_big}x{too_big}.jpg",
                id.replace('-', "/")
            );
            let status = client.get(&oversized).send().await.unwrap().status();
            assert!(
                !status.is_success(),
                "{oversized} answered {status}: is {too_big} actually a real size for {kind:?}?"
            );
        }
    }
}
