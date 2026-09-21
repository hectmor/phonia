//! Creating the daemon's socket safely: private directory, private socket, one daemon at a time,
//! and cleanup of a socket left behind by a daemon that died.

use anyhow::{Context, Result, bail};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};

/// Removes the socket file when dropped, so a clean shutdown leaves nothing behind.
pub struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Binds the socket at `path`.
///
/// The directory is created (or checked) to be private to this user, and the socket itself is
/// made readable by the owner only. If something already answers on `path` another daemon is
/// running and this fails; if the file is there but nothing answers, it is a leftover and is
/// replaced.
pub async fn bind(path: &Path) -> Result<(UnixListener, SocketGuard)> {
    check_length(path)?;
    let directory = path.parent().context("the socket path has no directory")?;
    prepare_directory(directory)?;

    if std::fs::symlink_metadata(path).is_ok() {
        match UnixStream::connect(path).await {
            Ok(_) => bail!("phoniad is already running: something answers on {}", path.display()),
            Err(error) if matches!(error.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => {
                std::fs::remove_file(path)
                    .with_context(|| format!("removing the stale socket {}", path.display()))?;
            }
            Err(error) => bail!("cannot use {} for the socket: {error}", path.display()),
        }
    }

    let listener = UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    let guard = SocketGuard { path: path.to_path_buf() };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", path.display()))?;
    Ok((listener, guard))
}

/// A Unix socket's path has to fit in the small buffer the kernel gives it (108 bytes including
/// the terminator on Linux); the error the system gives for one that doesn't is unhelpful.
fn check_length(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    const LIMIT: usize = 107;
    let length = path.as_os_str().as_bytes().len();
    if length > LIMIT {
        bail!(
            "the socket path is too long ({length} bytes, at most {LIMIT}): {}. Use a shorter --socket path",
            path.display()
        );
    }
    Ok(())
}

/// Makes sure `directory` exists, belongs to this user and is closed to everyone else.
fn prepare_directory(directory: &Path) -> Result<()> {
    if !directory.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        return Ok(());
    }
    let metadata = std::fs::metadata(directory).with_context(|| format!("reading {}", directory.display()))?;
    if metadata.uid() != phonia_ipc::socket::current_uid() {
        bail!("{} belongs to another user; refusing to put the socket there", directory.display());
    }
    if metadata.mode() & 0o077 != 0 {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restricting {}", directory.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("phoniad-socket-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o777
    }

    #[tokio::test]
    async fn the_socket_and_its_directory_are_private() {
        let path = scratch("private").join("run").join("phoniad.sock");
        let (_listener, _guard) = bind(&path).await.unwrap();
        assert_eq!(mode(&path), 0o600, "only the owner may connect");
        assert_eq!(mode(path.parent().unwrap()), 0o700, "and only the owner may even look inside");
    }

    #[tokio::test]
    async fn an_open_directory_is_closed_down() {
        let dir = scratch("open");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (_listener, _guard) = bind(&dir.join("phoniad.sock")).await.unwrap();
        assert_eq!(mode(&dir), 0o700);
    }

    #[tokio::test]
    async fn a_second_daemon_is_refused_while_the_first_answers() {
        let path = scratch("twice").join("phoniad.sock");
        let (_listener, _guard) = bind(&path).await.unwrap();
        let error = bind(&path).await.err().unwrap();
        assert!(error.to_string().contains("already running"), "{error:#}");
        assert!(path.exists(), "the running daemon's socket must not be touched");
    }

    #[tokio::test]
    async fn a_socket_left_by_a_dead_daemon_is_replaced() {
        let path = scratch("stale").join("phoniad.sock");
        {
            let (listener, guard) = bind(&path).await.unwrap();
            // A daemon that was killed leaves its socket file behind.
            std::mem::forget(guard);
            drop(listener);
        }
        assert!(path.exists(), "the leftover is there");
        let (_listener, _guard) = bind(&path).await.expect("a stale socket must not block a new daemon");
    }

    #[tokio::test]
    async fn the_socket_is_removed_on_a_clean_stop() {
        let path = scratch("clean").join("phoniad.sock");
        let (listener, guard) = bind(&path).await.unwrap();
        assert!(path.exists());
        drop(listener);
        drop(guard);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_path_too_long_for_a_socket_says_so() {
        let long = scratch("long").join("x".repeat(120)).join("phoniad.sock");
        let error = bind(&long).await.err().unwrap();
        assert!(error.to_string().contains("too long"), "{error:#}");
        assert!(!long.parent().unwrap().exists(), "nothing is created for a path that can't work");
    }

    #[tokio::test]
    async fn a_path_without_a_directory_is_an_error() {
        assert!(bind(Path::new("/")).await.is_err());
    }
}
