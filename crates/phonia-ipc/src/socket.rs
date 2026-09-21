//! Where the daemon's socket lives.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const SOCKET_DIR: &str = "phonia";
pub const SOCKET_FILE: &str = "phoniad.sock";

/// The socket path for the current user: `$XDG_RUNTIME_DIR/phonia/phoniad.sock`, else
/// `/run/user/<uid>/phonia/phoniad.sock`, else `/tmp/phonia-<uid>/phoniad.sock`.
pub fn default_socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    let uid = current_uid();
    socket_path_from(runtime_dir.as_deref(), uid, Path::new(&format!("/run/user/{uid}")).is_dir())
}

/// The path for the given environment, separated from the environment so it can be tested.
pub fn socket_path_from(runtime_dir: Option<&str>, uid: u32, run_user_exists: bool) -> PathBuf {
    match runtime_dir.filter(|dir| Path::new(dir).is_absolute()) {
        Some(dir) => Path::new(dir).join(SOCKET_DIR).join(SOCKET_FILE),
        None if run_user_exists => PathBuf::from(format!("/run/user/{uid}")).join(SOCKET_DIR).join(SOCKET_FILE),
        None => PathBuf::from(format!("/tmp/phonia-{uid}")).join(SOCKET_FILE),
    }
}

/// The user id of this process.
pub fn current_uid() -> u32 {
    std::fs::metadata("/proc/self").map(|metadata| metadata.uid()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_directory_wins() {
        assert_eq!(
            socket_path_from(Some("/run/user/1000"), 1000, true),
            PathBuf::from("/run/user/1000/phonia/phoniad.sock")
        );
    }

    #[test]
    fn a_relative_or_empty_runtime_directory_is_ignored() {
        assert_eq!(socket_path_from(Some("relative"), 1000, true), PathBuf::from("/run/user/1000/phonia/phoniad.sock"));
        assert_eq!(socket_path_from(Some(""), 1000, false), PathBuf::from("/tmp/phonia-1000/phoniad.sock"));
    }

    #[test]
    fn without_a_runtime_directory_it_falls_back_per_user() {
        assert_eq!(socket_path_from(None, 1000, true), PathBuf::from("/run/user/1000/phonia/phoniad.sock"));
        assert_eq!(socket_path_from(None, 1000, false), PathBuf::from("/tmp/phonia-1000/phoniad.sock"));
        assert_ne!(socket_path_from(None, 1001, false), socket_path_from(None, 1000, false), "one per user");
    }

    #[test]
    fn this_process_has_a_user_id_and_a_path() {
        let path = default_socket_path();
        assert!(path.is_absolute() && path.ends_with("phoniad.sock"), "{path:?}");
    }
}
