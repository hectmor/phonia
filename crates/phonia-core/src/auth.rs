//! Session load/save/login for the TIDAL client.
//!
//! PKCE login (the browser-based authorization-code flow, `TidalAuth::with_pkce()`) is used
//! here instead of the OAuth device-code flow (`TidalAuth::with_oauth()`) because tokens minted
//! through the device-code flow never get granted the `HI_RES_LOSSLESS` entitlement by TIDAL's
//! backend, even for subscribers whose account has it. PKCE is the only flow that returns tokens
//! usable for HiRes streaming, which is the whole point of this player.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tidlers::TidalClient;
use tidlers::auth::TidalAuth;

/// TIDAL's authorization codes expire within minutes, so a login started longer ago than this is
/// not worth finishing: the user has to start over.
const PENDING_LOGIN_MAX_AGE_SECS: u64 = 5 * 60;

fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("could not determine the user's config directory"))?
        .join("phonia");
    fs::create_dir_all(&dir).with_context(|| format!("creating config directory {dir:?}"))?;
    Ok(dir)
}

fn session_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("session.json"))
}

fn pending_login_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("pending-login.json"))
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Writes `contents` to `path`, readable only by the owner (the files we store hold tokens or
/// the PKCE `code_verifier`). The mode is also enforced on an already-existing file, since the
/// mode passed to `open` only applies when the file is created.
fn write_secret_file(path: &Path, contents: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("opening {path:?}"))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting permissions on {path:?}"))?;
    file.write_all(contents.as_bytes()).with_context(|| format!("writing {path:?}"))?;
    Ok(())
}

/// Writes the session (tokens included) to `session.json`, readable only by the owner.
pub fn save_session(client: &TidalClient) -> Result<()> {
    let path = session_path()?;
    write_secret_file(&path, &client.get_json()).context("saving the session")
}

/// A PKCE login that has been started but not finished. The PKCE `code_verifier` only exists in
/// the client that generated the login URL, so to finish the login from another process we keep
/// that client's serialized state (tidlers keeps the PKCE config inside the session JSON).
#[derive(Serialize, Deserialize)]
struct PendingLogin {
    created_at: u64,
    client_json: String,
}

fn save_pending_login(path: &Path, client_json: &str, now: u64) -> Result<()> {
    let pending = PendingLogin { created_at: now, client_json: client_json.to_string() };
    let json = serde_json::to_string(&pending).context("serializing the pending login")?;
    write_secret_file(path, &json).context("saving the pending login")
}

/// Loads the client of a pending login. A missing, corrupt or stale (older than
/// [`PENDING_LOGIN_MAX_AGE_SECS`]) pending login is an error telling the user to start over;
/// a stale one is also deleted.
fn load_pending_login(path: &Path, now: u64) -> Result<TidalClient> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("no login in progress. Run `phonia login --no-wait` first.")
        }
        Err(e) => return Err(e).with_context(|| format!("reading the pending login at {path:?}")),
    };
    let pending: PendingLogin = serde_json::from_str(&raw).with_context(|| {
        format!("the pending login at {path:?} is corrupt; run `phonia login --no-wait` again")
    })?;

    if now.saturating_sub(pending.created_at) > PENDING_LOGIN_MAX_AGE_SECS {
        let _ = fs::remove_file(path);
        bail!(
            "the pending login expired (TIDAL's authorization codes only last a few minutes). \
             Run `phonia login --no-wait` again."
        );
    }

    TidalClient::from_json(&pending.client_json).with_context(|| {
        format!("the pending login at {path:?} is corrupt; run `phonia login --no-wait` again")
    })
}

fn delete_pending_login(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Loads the saved session (if any), refreshing the access token if it has expired, and
/// persists the (possibly refreshed) session back to disk.
///
/// Fails with a clear message if the user has never logged in.
pub async fn load_client() -> Result<TidalClient> {
    let path = session_path()?;
    if !path.exists() {
        bail!("no session saved yet. Run `phonia login` first.");
    }

    let json = fs::read_to_string(&path).with_context(|| format!("reading the saved session from {path:?}"))?;
    let mut client = TidalClient::from_json(&json)
        .with_context(|| format!("the session saved at {path:?} is corrupt; run `phonia login` again"))?;

    client
        .refresh_access_token(false)
        .await
        .context("refreshing the access token")?;

    save_session(&client)?;

    Ok(client)
}

/// Finishes a PKCE login: exchanges the redirect URL's authorization code for tokens and saves
/// the session.
async fn complete_login(mut client: TidalClient, redirect_url: &str) -> Result<()> {
    let redirect_url = redirect_url.trim();
    if redirect_url.is_empty() {
        bail!("no redirect URL was received");
    }

    client
        .finish_pkce_login(redirect_url)
        .await
        .map_err(|e| anyhow!("could not complete PKCE login: {e}"))?;

    if let Err(e) = client.refresh_user_info().await {
        eprintln!("Warning: could not refresh user info: {e}");
    }

    save_session(&client)?;

    if let Some(user) = &client.user_info {
        println!("\nLogged in successfully as: {}", user.username);
    } else {
        println!("\nLogged in successfully.");
    }

    match client.subscription().await {
        Ok(sub) => println!("Subscription type: {}", sub.subscription.subscription_type),
        Err(e) => eprintln!("Warning: could not get the subscription type: {e}"),
    }

    Ok(())
}

fn start_pkce_login() -> Result<(TidalClient, String)> {
    let auth = TidalAuth::with_pkce();
    let mut client = TidalClient::new(&auth);
    let url = client
        .initiate_pkce_login()
        .map_err(|e| anyhow!("could not start PKCE login: {e}"))?;
    Ok((client, url))
}

/// Runs the interactive PKCE login flow: prints a URL for the user to open in their browser,
/// tries to open it automatically, then reads the redirect URL they land on (TIDAL sends PKCE
/// redirects to an "oops"/error page by design; the authorization code is in that URL's query
/// string regardless) from stdin.
pub async fn login() -> Result<()> {
    let (client, url) = start_pkce_login()?;

    println!("Open this URL in your browser to log in to TIDAL:\n\n  {url}\n");
    // Best effort only; on a headless box this will just fail silently and the user copy-pastes
    // the URL themselves.
    let _ = std::process::Command::new("xdg-open").arg(&url).status();

    println!("After logging in, TIDAL will redirect you to an error page (\"oops\"), that's");
    println!("expected: copy the FULL URL from that page's address bar and paste it here.");
    print!("Redirect URL: ");
    std::io::stdout().flush().ok();

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("reading the redirect URL from stdin")?;

    complete_login(client, &input).await
}

/// First half of the two-step login, for when stdin is not an interactive terminal: prints the
/// login URL and saves the pending PKCE state so [`login_finish`] can complete it from another
/// process.
pub fn login_begin() -> Result<()> {
    let (client, url) = start_pkce_login()?;
    save_pending_login(&pending_login_path()?, &client.get_json(), now_secs())?;

    println!("Open this URL in your browser to log in to TIDAL:\n\n  {url}\n");
    println!("After logging in, TIDAL will redirect you to an error page (\"oops\"), that's");
    println!("expected: copy the FULL URL from that page's address bar, then run (quickly, the");
    println!("code expires within minutes):\n");
    println!("  phonia login --finish '<redirect-url>'");
    Ok(())
}

/// Second half of the two-step login: completes the login started by [`login_begin`] using the
/// redirect URL the browser ended up on.
pub async fn login_finish(redirect_url: &str) -> Result<()> {
    let path = pending_login_path()?;
    let client = load_pending_login(&path, now_secs())?;
    complete_login(client, redirect_url).await?;
    delete_pending_login(&path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("phonia-auth-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("pending-login.json")
    }

    fn pkce_client() -> (TidalClient, String) {
        start_pkce_login().expect("PKCE login should start offline")
    }

    /// The URL's query parameters as a sorted set: tidlers builds the URL from a `HashMap`, so
    /// the raw string's parameter order differs between calls even for identical PKCE state.
    fn query_params(url: &str) -> Vec<&str> {
        let mut params: Vec<&str> = url.split_once('?').unwrap().1.split('&').collect();
        params.sort_unstable();
        params
    }

    #[test]
    fn pkce_state_survives_a_json_round_trip() {
        // The two-step design depends on this: the login URL is derived from the PKCE config
        // inside the client session, so a client rebuilt from JSON must reproduce the same URL
        // (and therefore still hold the matching `code_verifier`).
        let (client, url) = pkce_client();
        let mut restored = TidalClient::from_json(&client.get_json()).unwrap();
        assert_eq!(query_params(&restored.initiate_pkce_login().unwrap()), query_params(&url));
    }

    #[test]
    fn saved_pending_login_loads_back_and_is_owner_only() {
        let path = temp_path("roundtrip");
        let (client, url) = pkce_client();
        save_pending_login(&path, &client.get_json(), 1_000).unwrap();

        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

        let mut loaded = load_pending_login(&path, 1_000 + 10).unwrap();
        assert_eq!(query_params(&loaded.initiate_pkce_login().unwrap()), query_params(&url));
    }

    #[test]
    fn stale_pending_login_is_rejected_and_deleted() {
        let path = temp_path("stale");
        let (client, _) = pkce_client();
        save_pending_login(&path, &client.get_json(), 1_000).unwrap();

        let err = load_pending_login(&path, 1_000 + PENDING_LOGIN_MAX_AGE_SECS + 1).err().unwrap();
        assert!(err.to_string().contains("expired"), "{err}");
        assert!(!path.exists());
    }

    #[test]
    fn pending_login_at_the_age_limit_is_still_valid() {
        let path = temp_path("limit");
        let (client, _) = pkce_client();
        save_pending_login(&path, &client.get_json(), 1_000).unwrap();
        assert!(load_pending_login(&path, 1_000 + PENDING_LOGIN_MAX_AGE_SECS).is_ok());
    }

    #[test]
    fn missing_pending_login_tells_the_user_what_to_run() {
        let path = temp_path("missing");
        let err = load_pending_login(&path, 1_000).err().unwrap();
        assert!(err.to_string().contains("phonia login --no-wait"), "{err}");
    }

    #[test]
    fn corrupt_pending_login_is_reported() {
        let path = temp_path("corrupt");
        fs::write(&path, "not json").unwrap();
        let err = load_pending_login(&path, 1_000).err().unwrap();
        assert!(err.to_string().contains("corrupt"), "{err}");
    }

    #[test]
    fn delete_pending_login_removes_the_file() {
        let path = temp_path("delete");
        fs::write(&path, "x").unwrap();
        delete_pending_login(&path);
        assert!(!path.exists());
    }
}
