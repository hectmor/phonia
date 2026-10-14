//! The text form of a track's source, as it appears in the queue and in requests.
//!
//! `file:/absolute/path.flac` for a local file, `tidal:12345678` for a TIDAL track. The daemon
//! is the authority on what is valid; these helpers only build well-formed strings, so a client
//! doesn't need the audio libraries just to name a track.

use std::path::Path;

/// The source for a local file, or why it can't be one. The path must be absolute (a relative one
/// means nothing to a daemon with another working directory) and valid UTF-8.
pub fn file(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err(format!("{path:?} is not an absolute path"));
    }
    match path.to_str() {
        Some(text) if !text.contains('\0') => Ok(format!("file:{text}")),
        _ => Err(format!("{path:?} is not valid text")),
    }
}

/// The source for a TIDAL track, whose id is a number.
pub fn tidal(id: &str) -> Result<String, String> {
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("a TIDAL track id is a number, got {id:?}"));
    }
    Ok(format!("tidal:{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_sources() {
        assert_eq!(
            file(Path::new("/music/a b.flac")).unwrap(),
            "file:/music/a b.flac"
        );
        assert_eq!(
            file(Path::new("/música/ñ.flac")).unwrap(),
            "file:/música/ñ.flac"
        );
        assert_eq!(tidal("12345678").unwrap(), "tidal:12345678");
    }

    #[test]
    fn malformed_sources_are_refused() {
        assert!(file(Path::new("relative.flac")).is_err());
        assert!(file(Path::new("/a\0b")).is_err());
        for bad in ["", "abc", "12 3", "-1", "1.5"] {
            assert!(tidal(bad).is_err(), "{bad:?}");
        }
    }
}
