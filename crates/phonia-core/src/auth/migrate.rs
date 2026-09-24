//! Moving the session from `session.json` into a store that is not that file.
//!
//! A person who logged in before the keyring existed has a file and no keyring item. Their
//! session must never be lost, so it is copied into the keyring, read back to make sure it is
//! there, and only then is the file destroyed; if any step fails the file stays and is used for
//! this run, with a warning.

use super::store::{FileStore, SessionStore, StoreError, StoredSession};
use futures_util::future::BoxFuture;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

/// A store in front of the old `session.json`.
pub struct MigratingStore {
    primary: Arc<dyn SessionStore>,
    legacy: FileStore,
}

impl MigratingStore {
    pub fn new(primary: Arc<dyn SessionStore>, legacy: FileStore) -> Self {
        Self { primary, legacy }
    }

    /// The old file, if it is still there.
    pub fn leftover(&self) -> Option<PathBuf> {
        self.legacy.path().exists().then(|| self.legacy.path().to_path_buf())
    }

    fn forget_legacy(&self) {
        if self.legacy.path().exists()
            && let Err(error) = destroy(self.legacy.path())
        {
            eprintln!("phonia: could not remove {:?}: {error}", self.legacy.path());
        }
    }

    async fn load_or_migrate(&self) -> Result<Option<StoredSession>, StoreError> {
        match self.primary.load().await {
            Ok(Some(stored)) => {
                if self.legacy.path().exists() {
                    eprintln!(
                        "phonia: the session is in {}; removing the old {:?}",
                        self.primary.describe(),
                        self.legacy.path()
                    );
                    self.forget_legacy();
                }
                Ok(Some(stored))
            }
            Ok(None) => match self.legacy.load().await? {
                Some(stored) => {
                    self.migrate(&stored).await;
                    Ok(Some(stored))
                }
                None => Ok(None),
            },
            Err(error) => {
                // The keyring can't be reached, but there is a session in the file: use it, and
                // say why, rather than lock the user out.
                match self.legacy.load().await {
                    Ok(Some(stored)) => {
                        eprintln!(
                            "phonia: {error}; using the session in {:?} for now",
                            self.legacy.path()
                        );
                        Ok(Some(stored))
                    }
                    _ => Err(hint(error)),
                }
            }
        }
    }

    /// Copies the session to the primary store and, once it reads back the same, destroys the file.
    async fn migrate(&self, stored: &StoredSession) {
        let saved = self.primary.save(stored).await;
        let verified = match saved {
            Ok(()) => matches!(self.primary.load().await, Ok(Some(back)) if back == *stored),
            Err(ref error) => {
                eprintln!("phonia: could not move the session to {}: {error}", self.primary.describe());
                return;
            }
        };
        if !verified {
            eprintln!(
                "phonia: the session written to {} could not be read back; keeping {:?}",
                self.primary.describe(),
                self.legacy.path()
            );
            return;
        }
        self.forget_legacy();
        eprintln!(
            "phonia: moved the TIDAL session from {:?} to {}; the file was overwritten and removed",
            self.legacy.path(),
            self.primary.describe()
        );
    }
}

/// What to do about a store that isn't there, for someone who never chose one.
fn hint(error: StoreError) -> StoreError {
    match error {
        StoreError::Unavailable(why) => StoreError::Unavailable(format!(
            "{why}. Set `session_store = \"file\"` under [tidal] in the config file to keep the session in a file instead"
        )),
        other => other,
    }
}

/// Overwrites the file with zeros, then removes it. Only best effort: a journaling filesystem or an
/// SSD may keep older copies of the blocks.
pub(super) fn destroy(path: &std::path::Path) -> std::io::Result<()> {
    let length = fs::metadata(path)?.len() as usize;
    let mut file = fs::OpenOptions::new().write(true).open(path)?;
    file.write_all(&vec![0; length])?;
    file.sync_all()?;
    drop(file);
    fs::remove_file(path)
}

impl SessionStore for MigratingStore {
    fn describe(&self) -> String {
        self.primary.describe()
    }

    fn provider(&self) -> BoxFuture<'_, Option<String>> {
        self.primary.provider()
    }

    fn load(&self) -> BoxFuture<'_, Result<Option<StoredSession>, StoreError>> {
        Box::pin(self.load_or_migrate())
    }

    fn save<'a>(&'a self, session: &'a StoredSession) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.primary.save(session).await.map_err(hint)?;
            self.forget_legacy();
            Ok(())
        })
    }

    fn delete(&self) -> BoxFuture<'_, Result<bool, StoreError>> {
        Box::pin(async move {
            let had_file = self.legacy.path().exists();
            self.forget_legacy();
            let had_item = self.primary.delete().await.map_err(hint)?;
            Ok(had_item || had_file)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::MemoryStore;

    fn session() -> StoredSession {
        StoredSession { v: 1, refresh_token: "refresh".into(), client_id: "id".into(), client_secret: "secret".into() }
    }

    fn legacy_file(name: &str, with: Option<&StoredSession>) -> FileStore {
        let dir = std::env::temp_dir().join(format!("phonia-migrate-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = FileStore::new(dir.join("session.json"));
        if let Some(session) = with {
            fs::write(store.path(), serde_json::to_string(session).unwrap()).unwrap();
        }
        store
    }

    #[tokio::test]
    async fn an_old_file_moves_into_the_store_and_is_destroyed() {
        let keyring = Arc::new(MemoryStore::new());
        let file = legacy_file("moves", Some(&session()));
        let path = file.path().to_path_buf();
        let store = MigratingStore::new(keyring.clone(), file);

        assert_eq!(store.load().await.unwrap(), Some(session()));
        assert_eq!(keyring.load().await.unwrap(), Some(session()), "it is in the keyring now");
        assert!(!path.exists(), "and the file is gone");
    }

    #[tokio::test]
    async fn a_store_that_cannot_be_written_keeps_the_file_and_the_session() {
        let keyring = Arc::new(MemoryStore::new());
        keyring.fail_with("locked");
        let file = legacy_file("failing", Some(&session()));
        let path = file.path().to_path_buf();
        let store = MigratingStore::new(keyring, file);

        assert_eq!(store.load().await.unwrap(), Some(session()), "the session is used from the file");
        assert!(path.exists(), "and nothing was lost");
    }

    /// A store that says it saved and then can't return it.
    struct Forgetful;

    impl SessionStore for Forgetful {
        fn describe(&self) -> String {
            "a forgetful store".into()
        }
        fn load(&self) -> BoxFuture<'_, Result<Option<StoredSession>, StoreError>> {
            Box::pin(async { Ok(None) })
        }
        fn save<'a>(&'a self, _: &'a StoredSession) -> BoxFuture<'a, Result<(), StoreError>> {
            Box::pin(async { Ok(()) })
        }
        fn delete(&self) -> BoxFuture<'_, Result<bool, StoreError>> {
            Box::pin(async { Ok(false) })
        }
    }

    #[tokio::test]
    async fn the_file_is_only_destroyed_once_the_store_reads_back_the_same() {
        let file = legacy_file("readback", Some(&session()));
        let path = file.path().to_path_buf();
        let store = MigratingStore::new(Arc::new(Forgetful), file);
        assert_eq!(store.load().await.unwrap(), Some(session()));
        assert!(path.exists(), "an unverified copy is not a reason to lose the file");
    }

    #[tokio::test]
    async fn a_session_already_in_the_store_wins_and_a_leftover_file_is_removed() {
        let keyring = Arc::new(MemoryStore::with(session()));
        let other = StoredSession { refresh_token: "older".into(), ..session() };
        let file = legacy_file("leftover", Some(&other));
        let path = file.path().to_path_buf();
        let store = MigratingStore::new(keyring, file);

        assert_eq!(store.load().await.unwrap(), Some(session()));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn with_no_session_anywhere_there_is_none() {
        let store = MigratingStore::new(Arc::new(MemoryStore::new()), legacy_file("none", None));
        assert_eq!(store.load().await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_unreachable_store_and_no_file_is_refused_with_the_way_out() {
        let keyring = Arc::new(MemoryStore::new());
        keyring.fail_with("no bus");
        let store = MigratingStore::new(keyring, legacy_file("refuse", None));
        let error = store.load().await.unwrap_err().to_string();
        assert!(error.contains("no bus") && error.contains("session_store = \"file\""), "{error}");
    }

    #[tokio::test]
    async fn saving_a_login_removes_the_old_file() {
        let keyring = Arc::new(MemoryStore::new());
        let file = legacy_file("save", Some(&session()));
        let path = file.path().to_path_buf();
        let store = MigratingStore::new(keyring.clone(), file);

        store.save(&session()).await.unwrap();
        assert_eq!(keyring.load().await.unwrap(), Some(session()));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn logging_out_forgets_both() {
        let keyring = Arc::new(MemoryStore::with(session()));
        let file = legacy_file("logout", Some(&session()));
        let path = file.path().to_path_buf();
        let store = MigratingStore::new(keyring.clone(), file);

        assert!(store.delete().await.unwrap());
        assert_eq!(keyring.load().await.unwrap(), None);
        assert!(!path.exists());
        assert!(!store.delete().await.unwrap(), "nothing left the second time");
    }

    #[test]
    fn destroying_a_file_overwrites_it_before_removing_it() {
        let file = legacy_file("destroy", Some(&session()));
        let path = file.path().to_path_buf();
        // Keep a second name for the same inode to see what was left in it.
        let alias = path.with_file_name("alias");
        fs::hard_link(&path, &alias).unwrap();
        let length = fs::metadata(&path).unwrap().len() as usize;

        destroy(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read(&alias).unwrap(), vec![0; length], "the contents were zeroed, not just unlinked");
    }
}
