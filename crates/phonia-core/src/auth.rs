//! Session load/save/login for the TIDAL client.
//!
//! PKCE login (the browser-based authorization-code flow, `TidalAuth::with_pkce()`) is used
//! here instead of the OAuth device-code flow (`TidalAuth::with_oauth()`) because tokens minted
//! through the device-code flow never get granted the `HI_RES_LOSSLESS` entitlement by TIDAL's
//! backend, even for subscribers whose account has it. PKCE is the only flow that returns tokens
//! usable for HiRes streaming, which is the whole point of this player.

use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use tidlers::TidalClient;
use tidlers::auth::TidalAuth;

fn session_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("could not determine the user's config directory"))?
        .join("phonia");
    fs::create_dir_all(&dir).with_context(|| format!("creating config directory {dir:?}"))?;
    Ok(dir.join("session.json"))
}

fn save_session(client: &TidalClient) -> Result<()> {
    let path = session_path()?;
    let json = client.get_json();

    // Session file contains an access + refresh token, so keep it readable only by the owner.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("opening {path:?} to save the session"))?;
    file.write_all(json.as_bytes())
        .with_context(|| format!("writing the session to {path:?}"))?;

    Ok(())
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

/// Runs the interactive PKCE login flow: prints a URL for the user to open in their browser,
/// tries to open it automatically, then reads the redirect URL they land on (TIDAL sends PKCE
/// redirects to an "oops"/error page by design; the authorization code is in that URL's query
/// string regardless) from stdin.
pub async fn login() -> Result<()> {
    let auth = TidalAuth::with_pkce();
    let mut client = TidalClient::new(&auth);

    let url = client
        .initiate_pkce_login()
        .map_err(|e| anyhow!("could not start PKCE login: {e}"))?;

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
    let redirect_url = input.trim();
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
