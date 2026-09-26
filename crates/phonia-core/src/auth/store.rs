//! Where the TIDAL session is kept between runs.
//!
//! What has to survive is small: the refresh token, which TIDAL never rotates and which is the
//! only long-lived credential, and the client id and secret it was issued to (it only works with
//! them). The access token lives four hours and is fetched again with the refresh token, and
//! everything else `tidlers` keeps in its session (email, birthday, user id...) comes back with
//! that refresh, so none of it is stored.

use anyhow::{Context, Result, anyhow};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tidlers::TidalClient;
use tidlers::auth::TidalAuth;

/// The version written into a stored session, to tell it from the older format (which is the
/// whole `tidlers` client) and from later ones.
const VERSION: u32 = 1;

/// What is kept of a login.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSession {
    pub v: u32,
    pub refresh_token: String,
    /// TIDAL's client credentials the token was issued to. Not secret (they are the Android app's
    /// and ship inside `tidlers`), but a refresh token only works together with them.
    pub client_id: String,
    pub client_secret: String,
}

impl fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredSession")
            .field("v", &self.v)
            .field("refresh_token", &"<hidden>")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<hidden>")
            .finish()
    }
}

/// What the client of a login says to keep.
pub fn stored_from(client: &TidalClient) -> Result<StoredSession> {
    let auth = &client.session.auth;
    let refresh_token = auth
        .refresh_token
        .clone()
        .ok_or_else(|| anyhow!("the login did not return a refresh token"))?;
    Ok(StoredSession {
        v: VERSION,
        refresh_token,
        client_id: auth.pkce_config.client_id.clone(),
        client_secret: auth.pkce_config.client_secret.clone(),
    })
}

/// A client that can refresh its access token. It has none yet, which `tidlers` treats as
/// expired, so the first use refreshes it.
pub fn client_from(stored: &StoredSession) -> TidalClient {
    let mut client = TidalClient::new(&TidalAuth::with_pkce());
    let auth = &mut client.session.auth;
    auth.refresh_token = Some(stored.refresh_token.clone());
    auth.client_id = stored.client_id.clone();
    auth.client_secret = stored.client_secret.clone();
    auth.pkce_config.client_id = stored.client_id.clone();
    auth.pkce_config.client_secret = stored.client_secret.clone();
    client
}

#[derive(Debug)]
pub enum StoreError {
    /// The place that keeps the session can't be reached.
    Unavailable(String),
    /// It is there but locked, and nothing could unlock it.
    Locked(String),
    /// What is stored can't be read.
    Corrupt(String),
    Other(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Unavailable(why) => write!(f, "the session store is not available: {why}"),
            StoreError::Locked(why) => write!(f, "the session store is locked: {why}"),
            StoreError::Corrupt(why) => write!(f, "the stored session is unreadable: {why}"),
            StoreError::Other(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// A place to keep the session. Asynchronous because a desktop keyring is reached over D-Bus, and
/// every caller is already on the runtime.
pub trait SessionStore: Send + Sync {
    /// Where the session is kept, for messages.
    fn describe(&self) -> String;

    /// The stored session, or `None` if there is none.
    fn load(&self) -> BoxFuture<'_, Result<Option<StoredSession>, StoreError>>;

    fn save<'a>(&'a self, session: &'a StoredSession) -> BoxFuture<'a, Result<(), StoreError>>;

    /// Forgets the session. Whether there was one.
    fn delete(&self) -> BoxFuture<'_, Result<bool, StoreError>>;

    /// The program that keeps the session for us, when there is one (the keyring's daemon).
    fn provider(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }
}

/// The session in a file only its owner can read.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<Option<StoredSession>, StoreError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StoreError::Other(format!(
                    "reading {:?}: {error}",
                    self.path
                )));
            }
        };
        let corrupt = |why: String| StoreError::Corrupt(format!("{:?}: {why}", self.path));
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|error| corrupt(error.to_string()))?;

        if value.get("v").is_some() {
            return serde_json::from_value(value)
                .map(Some)
                .map_err(|error| corrupt(error.to_string()));
        }
        if value.get("session").is_some() {
            // The first versions saved the whole `tidlers` client, personal data and access token
            // included. Keep what matters and rewrite the file without the rest.
            let client =
                TidalClient::from_json(&text).map_err(|error| corrupt(error.to_string()))?;
            let stored = stored_from(&client).map_err(|error| corrupt(format!("{error:#}")))?;
            self.write(&stored)?;
            eprintln!(
                "phonia: {:?} was in an older format; rewritten keeping only the refresh token",
                self.path
            );
            return Ok(Some(stored));
        }
        Err(corrupt("it is not a phonia session".to_string()))
    }

    /// Replaces the file in one step, so a crash never leaves half a session.
    fn write(&self, session: &StoredSession) -> Result<(), StoreError> {
        let json =
            serde_json::to_string(session).map_err(|error| StoreError::Other(error.to_string()))?;
        let write = || -> Result<()> {
            let temporary = self.path.with_extension("json.tmp");
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&temporary)
                .with_context(|| format!("opening {temporary:?}"))?;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .with_context(|| format!("restricting permissions on {temporary:?}"))?;
            file.write_all(json.as_bytes())
                .with_context(|| format!("writing {temporary:?}"))?;
            file.sync_all()
                .with_context(|| format!("syncing {temporary:?}"))?;
            fs::rename(&temporary, &self.path).with_context(|| format!("replacing {:?}", self.path))
        };
        write().map_err(|error| StoreError::Other(format!("{error:#}")))
    }
}

impl SessionStore for FileStore {
    fn describe(&self) -> String {
        format!("the file {}", self.path.display())
    }

    fn load(&self) -> BoxFuture<'_, Result<Option<StoredSession>, StoreError>> {
        Box::pin(async move { self.read() })
    }

    fn save<'a>(&'a self, session: &'a StoredSession) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move { self.write(session) })
    }

    fn delete(&self) -> BoxFuture<'_, Result<bool, StoreError>> {
        Box::pin(async move {
            match fs::remove_file(&self.path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(StoreError::Other(format!(
                    "deleting {:?}: {error}",
                    self.path
                ))),
            }
        })
    }
}

/// A store in memory, for tests. It can be told to fail, and counts how often it was read.
#[derive(Default)]
pub struct MemoryStore {
    session: Mutex<Option<StoredSession>>,
    fail_with: Mutex<Option<String>>,
    loads: Mutex<u32>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(session: StoredSession) -> Self {
        Self {
            session: Mutex::new(Some(session)),
            ..Self::default()
        }
    }

    /// Every later operation fails as if the store could not be reached.
    pub fn fail_with(&self, why: &str) {
        *self.fail_with.lock().unwrap() = Some(why.to_string());
    }

    pub fn loads(&self) -> u32 {
        *self.loads.lock().unwrap()
    }

    fn check(&self) -> Result<(), StoreError> {
        match self.fail_with.lock().unwrap().clone() {
            Some(why) => Err(StoreError::Unavailable(why)),
            None => Ok(()),
        }
    }
}

impl SessionStore for MemoryStore {
    fn describe(&self) -> String {
        "memory".to_string()
    }

    fn load(&self) -> BoxFuture<'_, Result<Option<StoredSession>, StoreError>> {
        Box::pin(async move {
            self.check()?;
            *self.loads.lock().unwrap() += 1;
            Ok(self.session.lock().unwrap().clone())
        })
    }

    fn save<'a>(&'a self, session: &'a StoredSession) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.check()?;
            *self.session.lock().unwrap() = Some(session.clone());
            Ok(())
        })
    }

    fn delete(&self) -> BoxFuture<'_, Result<bool, StoreError>> {
        Box::pin(async move {
            self.check()?;
            Ok(self.session.lock().unwrap().take().is_some())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> StoredSession {
        StoredSession {
            v: VERSION,
            refresh_token: "refresh-secret".into(),
            client_id: "the-client".into(),
            client_secret: "the-client-secret".into(),
        }
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phonia-store-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("session.json")
    }

    /// The session as the first versions wrote it: the whole `tidlers` client, with everything
    /// that entails.
    fn legacy_json() -> String {
        let mut client = client_from(&session());
        client.session.auth.access_token = Some("access-secret".into());
        client.session.auth.refresh_expiry = Some(14_400);
        client.session.auth.last_refresh_time = Some(1_700_000_000);
        client.session.auth.user_id = Some(42);
        client.get_json()
    }

    #[test]
    fn what_is_stored_rebuilds_a_client_that_can_refresh() {
        let client = client_from(&session());
        let auth = &client.session.auth;
        assert_eq!(auth.refresh_token.as_deref(), Some("refresh-secret"));
        assert_eq!(
            (
                auth.pkce_config.client_id.as_str(),
                auth.pkce_config.client_secret.as_str()
            ),
            ("the-client", "the-client-secret")
        );
        assert!(
            auth.access_token.is_none() && client.user_info.is_none(),
            "nothing but the credentials is restored"
        );
        assert!(
            auth.pkce_login,
            "it is a PKCE client, whose refresh uses the PKCE credentials"
        );
    }

    #[test]
    fn what_a_login_leaves_is_what_gets_stored() {
        let client = client_from(&session());
        assert_eq!(stored_from(&client).unwrap(), session());
    }

    #[test]
    fn a_login_without_a_refresh_token_cannot_be_stored() {
        let client = TidalClient::new(&TidalAuth::with_pkce());
        assert!(
            stored_from(&client)
                .unwrap_err()
                .to_string()
                .contains("refresh token")
        );
    }

    #[test]
    fn the_debug_output_hides_the_secrets() {
        let text = format!("{:?}", session());
        assert!(
            !text.contains("refresh-secret") && !text.contains("the-client-secret"),
            "{text}"
        );
        assert!(
            text.contains("the-client"),
            "the client id is not secret: {text}"
        );
    }

    #[tokio::test]
    async fn a_file_store_round_trips_and_is_owner_only() {
        let path = temp_file("roundtrip");
        let store = FileStore::new(&path);
        assert_eq!(store.load().await.unwrap(), None);

        store.save(&session()).await.unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(store.load().await.unwrap(), Some(session()));
        assert!(
            !path.with_extension("json.tmp").exists(),
            "no temporary file is left behind"
        );

        assert!(store.delete().await.unwrap());
        assert!(!store.delete().await.unwrap(), "nothing left to delete");
        assert_eq!(store.load().await.unwrap(), None);
    }

    #[tokio::test]
    async fn saving_over_a_file_with_loose_permissions_tightens_them() {
        let path = temp_file("loose");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        FileStore::new(&path).save(&session()).await.unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_session_is_reported_not_overwritten() {
        let path = temp_file("corrupt");
        for text in ["not json", "{\"hello\":1}"] {
            fs::write(&path, text).unwrap();
            let error = FileStore::new(&path).load().await.unwrap_err();
            assert!(matches!(error, StoreError::Corrupt(_)), "{text}: {error}");
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                text,
                "the file is left as it was"
            );
        }
    }

    #[tokio::test]
    async fn the_older_format_is_read_and_rewritten_without_the_personal_data() {
        let path = temp_file("legacy");
        fs::write(&path, legacy_json()).unwrap();
        let store = FileStore::new(&path);

        assert_eq!(store.load().await.unwrap(), Some(session()));
        let rewritten = fs::read_to_string(&path).unwrap();
        assert!(
            !rewritten.contains("access-secret") && !rewritten.contains("user_id"),
            "{rewritten}"
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            store.load().await.unwrap(),
            Some(session()),
            "and it reads the same the second time"
        );
    }

    #[tokio::test]
    async fn the_memory_store_behaves_like_a_store_and_can_fail() {
        let store = MemoryStore::new();
        assert_eq!(store.load().await.unwrap(), None);
        store.save(&session()).await.unwrap();
        assert_eq!(store.load().await.unwrap(), Some(session()));
        assert_eq!(store.loads(), 2);
        assert!(store.delete().await.unwrap());

        store.fail_with("no bus");
        assert!(matches!(
            store.load().await,
            Err(StoreError::Unavailable(_))
        ));
    }
}
