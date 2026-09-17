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
        .ok_or_else(|| anyhow!("no se pudo determinar el directorio de configuración del usuario"))?
        .join("phonia");
    fs::create_dir_all(&dir).with_context(|| format!("creando el directorio de configuración {dir:?}"))?;
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
        .with_context(|| format!("abriendo {path:?} para guardar la sesión"))?;
    file.write_all(json.as_bytes())
        .with_context(|| format!("escribiendo la sesión en {path:?}"))?;

    Ok(())
}

/// Loads the saved session (if any), refreshing the access token if it has expired, and
/// persists the (possibly refreshed) session back to disk.
///
/// Fails with a clear message if the user has never logged in.
pub async fn load_client() -> Result<TidalClient> {
    let path = session_path()?;
    if !path.exists() {
        bail!("no hay ninguna sesión guardada todavía. Ejecuta `phonia login` primero.");
    }

    let json = fs::read_to_string(&path).with_context(|| format!("leyendo la sesión guardada en {path:?}"))?;
    let mut client = TidalClient::from_json(&json)
        .with_context(|| format!("la sesión guardada en {path:?} está corrupta; vuelve a hacer `phonia login`"))?;

    client
        .refresh_access_token(false)
        .await
        .context("refrescando el token de acceso")?;

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
        .map_err(|e| anyhow!("no se pudo iniciar el login PKCE: {e}"))?;

    println!("Abre esta URL en tu navegador para iniciar sesión en TIDAL:\n\n  {url}\n");
    // Best effort only; on a headless box this will just fail silently and the user copy-pastes
    // the URL themselves.
    let _ = std::process::Command::new("xdg-open").arg(&url).status();

    println!("Tras iniciar sesión, TIDAL te redirigirá a una página de error (\"oops\"), eso es");
    println!("normal: copia la URL COMPLETA de la barra de direcciones de esa página y pégala aquí.");
    print!("URL de redirección: ");
    std::io::stdout().flush().ok();

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("leyendo la URL de redirección de stdin")?;
    let redirect_url = input.trim();
    if redirect_url.is_empty() {
        bail!("no se recibió ninguna URL de redirección");
    }

    client
        .finish_pkce_login(redirect_url)
        .await
        .map_err(|e| anyhow!("no se pudo completar el login PKCE: {e}"))?;

    if let Err(e) = client.refresh_user_info().await {
        eprintln!("Aviso: no se pudo refrescar la información de usuario: {e}");
    }

    save_session(&client)?;

    if let Some(user) = &client.user_info {
        println!("\nSesión iniciada correctamente como: {}", user.username);
    } else {
        println!("\nSesión iniciada correctamente.");
    }

    match client.subscription().await {
        Ok(sub) => println!("Tipo de suscripción: {}", sub.subscription.subscription_type),
        Err(e) => eprintln!("Aviso: no se pudo obtener el tipo de suscripción: {e}"),
    }

    Ok(())
}
