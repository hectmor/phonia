//! The TIDAL login of the process, shared by what needs it: opening tracks and browsing the
//! catalog.

use crate::auth;
use anyhow::{Result, anyhow};
use std::sync::Arc;
use tidlers::TidalClient;
use tokio::sync::{RwLock, RwLockReadGuard};

/// The TIDAL login of a process: read from its store the first time it is needed (so a daemon
/// that started before the network, or before `phonia login`, still works once they are there),
/// and kept fresh from then on.
pub(crate) struct TidalSession {
    /// Behind a lock because the access token expires after a while and has to be refreshed in
    /// place, which a long-running process (a daemon) will always eventually need.
    pub(crate) client: RwLock<Option<TidalClient>>,
    /// Where to read the login from when there is no client yet.
    pub(crate) store: Option<Arc<dyn auth::SessionStore>>,
}

impl TidalSession {
    /// The client with a valid access token, refreshed if the old one has expired. TIDAL does not
    /// rotate refresh tokens, so there is nothing to save afterwards.
    pub(crate) async fn fresh(&self) -> Result<RwLockReadGuard<'_, TidalClient>> {
        {
            let mut guard = self.client.write().await;
            if guard.is_none() {
                let store = self
                    .store
                    .as_deref()
                    .ok_or_else(|| anyhow!("there is no TIDAL session"))?;
                *guard = Some(auth::load_client(store).await?);
            }
            let client = guard.as_mut().expect("loaded just above");
            client
                .refresh_access_token(false)
                .await
                .map_err(|error| anyhow!("refreshing the TIDAL access token: {error}"))?;
        }
        Ok(RwLockReadGuard::map(self.client.read().await, |client| {
            client
                .as_ref()
                .expect("a loaded client is never taken away")
        }))
    }
}
