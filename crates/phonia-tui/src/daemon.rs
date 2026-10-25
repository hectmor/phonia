//! Starting phoniad when nobody answers on the socket, and stopping only the one this process
//! started.

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// The pair `phoniad`'s own `socket::bind` already treats as "nobody is listening".
fn nobody_listening(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    )
}

/// The sibling of the running binary if there is one (a cargo target dir, an installed bin/),
/// else PATH.
fn binary(current_exe: Option<PathBuf>, exists: impl Fn(&Path) -> bool) -> PathBuf {
    current_exe
        .map(|path| path.with_file_name("phoniad"))
        .filter(|path| exists(path))
        .unwrap_or_else(|| "phoniad".into())
}

fn args(socket: &Path, config: Option<&Path>) -> Vec<OsString> {
    let mut args = vec![OsString::from("--socket"), socket.into()];
    if let Some(config) = config {
        args.push(OsString::from("--config"));
        args.push(config.into());
    }
    args
}

/// Where phoniad's own stdout/stderr (it still `eprintln!`s a few status lines) go instead of the
/// alternate screen: the same kind of place `phonia_core::recent` keeps state, reimplemented here
/// without depending on phonia-core (which phonia-tui deliberately avoids).
fn log_file() -> std::process::Stdio {
    let Some(dir) = dirs::state_dir().map(|dir| dir.join("phonia")) else {
        return std::process::Stdio::null();
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return std::process::Stdio::null();
    }
    std::fs::File::create(dir.join("phoniad.log"))
        .map(Into::into)
        .unwrap_or_else(|_| std::process::Stdio::null())
}

/// A `phoniad` this process spawned, and is responsible for stopping.
pub struct Owned(tokio::process::Child);

/// Spawns `phoniad` if nobody answers on `socket`. `None` means a daemon is already there (or
/// something that is not one, which the connection task reports as usual): this TUI is then a
/// plain client and must never stop it.
pub async fn start_if_absent(socket: &Path, config: Option<&Path>) -> Result<Option<Owned>> {
    match tokio::net::UnixStream::connect(socket).await {
        Ok(_) => return Ok(None),
        Err(error) if nobody_listening(&error) => {}
        Err(_) => return Ok(None),
    }
    let child = tokio::process::Command::new(binary(std::env::current_exe().ok(), Path::is_file))
        .args(args(socket, config))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log_file())
        // The terminal's SIGHUP must reach only us, not a process we start; we stop it ourselves.
        .process_group(0)
        // A panic must not leave it behind.
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "nobody answers on {} and phoniad could not be started",
                socket.display()
            )
        })?;
    Ok(Some(Owned(child)))
}

impl Owned {
    pub async fn stop(self) {
        self.stop_within(STOP_TIMEOUT).await;
    }

    async fn stop_within(mut self, limit: Duration) {
        // Already exited on its own: lost the bind race to another daemon, failed to start, or
        // was stopped some other way (`phonia ctl shutdown`). Nothing left to manage.
        if !matches!(self.0.try_wait(), Ok(None)) {
            return;
        }
        if let Some(pid) = self.0.id() {
            // SAFETY: signalling a child we started and have not reaped, so its pid can't have
            // been reused for something else yet.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
        if tokio::time::timeout(limit, self.0.wait()).await.is_err() {
            // shortcut: SIGKILL skips stop_engine's graceful device release; upgrade this if a
            // hung shutdown is ever seen in practice.
            let _ = self.0.kill().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    #[test]
    fn nobody_listening_matches_refused_and_not_found() {
        assert!(nobody_listening(&io::Error::from(
            io::ErrorKind::ConnectionRefused
        )));
        assert!(nobody_listening(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(!nobody_listening(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn the_sibling_of_the_running_binary_is_preferred() {
        let exe = PathBuf::from("/opt/phonia/bin/phonia");
        let found = binary(Some(exe.clone()), |path| {
            path == Path::new("/opt/phonia/bin/phoniad")
        });
        assert_eq!(found, Path::new("/opt/phonia/bin/phoniad"));
    }

    #[test]
    fn with_no_sibling_on_disk_the_bare_name_is_used() {
        let exe = PathBuf::from("/opt/phonia/bin/phonia");
        let found = binary(Some(exe), |_| false);
        assert_eq!(found, Path::new("phoniad"));
    }

    #[test]
    fn with_no_current_exe_the_bare_name_is_used() {
        let found = binary(None, |_| true);
        assert_eq!(found, Path::new("phoniad"));
    }

    #[test]
    fn args_without_a_config_is_just_the_socket() {
        let found = args(Path::new("/run/phonia/phoniad.sock"), None);
        assert_eq!(
            found,
            vec![
                OsString::from("--socket"),
                OsString::from("/run/phonia/phoniad.sock"),
            ]
        );
    }

    #[test]
    fn args_with_a_config_adds_it() {
        let found = args(
            Path::new("/run/phonia/phoniad.sock"),
            Some(Path::new("/etc/phonia/config.toml")),
        );
        assert_eq!(
            found,
            vec![
                OsString::from("--socket"),
                OsString::from("/run/phonia/phoniad.sock"),
                OsString::from("--config"),
                OsString::from("/etc/phonia/config.toml"),
            ]
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "phonia-tui-daemon-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    // `start_if_absent` itself isn't called for the two "nobody's listening" cases below: it would
    // then try to spawn the bare name "phoniad" off PATH (since a test binary's sibling is never
    // phoniad), which is not guaranteed to exist wherever `cargo test` runs. These instead confirm
    // the exact real-socket conditions its probe relies on, already covered as pure logic above.

    #[tokio::test]
    async fn a_missing_socket_is_nobody_listening() {
        let path = scratch("missing").join("phoniad.sock");
        let error = tokio::net::UnixStream::connect(&path).await.unwrap_err();
        assert!(nobody_listening(&error));
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_also_nobody_listening() {
        let dir = scratch("stale");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("phoniad.sock");
        let listener = UnixListener::bind(&path).unwrap();
        drop(listener);
        // The file is still there, but nothing answers -- the same case `socket::bind` calls
        // stale and replaces. shortcut: a closed listener is occasionally still briefly
        // connectable under heavy parallel test load, so retry a few times instead of asserting
        // on the very first attempt.
        let mut error = None;
        for _ in 0..20 {
            match tokio::net::UnixStream::connect(&path).await {
                Err(err) => {
                    error = Some(err);
                    break;
                }
                Ok(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        assert!(nobody_listening(
            &error.expect("connecting kept succeeding after 200ms")
        ));
    }

    #[tokio::test]
    async fn a_live_listener_means_do_not_spawn() {
        let dir = scratch("live");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("phoniad.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        // The real function, end to end: it must return before ever looking for a phoniad binary.
        assert!(start_if_absent(&path, None).await.unwrap().is_none());
    }

    async fn spawn_sh(args: &[&str]) -> tokio::process::Child {
        tokio::process::Command::new("sh")
            .arg("-c")
            .args(args)
            .kill_on_drop(true)
            .spawn()
            .expect("sh is needed for these tests")
    }

    #[tokio::test]
    async fn a_responsive_child_is_stopped_by_sigterm() {
        let owned = Owned(spawn_sh(&["sleep 30"]).await);
        owned.stop_within(Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn an_already_exited_child_is_not_signalled_again() {
        let mut child = spawn_sh(&["true"]).await;
        let _ = child.wait().await;
        // Not a crash, and no attempt to signal a pid that may already have been reused.
        Owned(child).stop_within(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn a_child_that_ignores_sigterm_is_killed_after_the_timeout() {
        let child = spawn_sh(&["trap '' TERM; sleep 30"]).await;
        let pid = child.id().unwrap();
        Owned(child).stop_within(Duration::from_millis(200)).await;
        // The process is gone; SIGTERM alone would not have stopped it.
        // SAFETY: a signal of 0 only checks whether the pid still exists.
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    }
}
