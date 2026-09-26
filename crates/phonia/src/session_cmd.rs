//! `phonia logout` and `phonia whoami`: what to do with the TIDAL login once it exists.

use anyhow::{Context, Result};
use phonia_core::auth::{self, Interaction, client_from};
use phonia_core::config::SessionStoreKind;
use std::path::PathBuf;

/// Who TIDAL says the login belongs to (only known after asking it).
#[derive(Debug, PartialEq, Eq)]
pub struct Account {
    pub username: String,
    pub user_id: u64,
}

/// What `whoami` found out. Never holds a token.
#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub store: String,
    pub provider: Option<String>,
    /// `Ok(true)` if there is a session, `Ok(false)` if there is none, `Err` why it can't be told.
    pub session: Result<bool, String>,
    /// A `session.json` from before the keyring that is still there.
    pub leftover: Option<PathBuf>,
    /// Filled by `--check`, which asks TIDAL.
    pub account: Option<Result<Account, String>>,
}

pub fn format_report(report: &Report) -> String {
    let store = match &report.provider {
        Some(provider) => format!("{} (provided by {provider})", report.store),
        None => report.store.clone(),
    };
    let mut text = format!("Session store: {store}");
    match &report.session {
        Ok(true) => text.push_str("\nSession:       yes (a refresh token is stored)"),
        Ok(false) => text.push_str("\nSession:       none: run `phonia login`"),
        Err(why) => text.push_str(&format!("\nSession:       could not be read: {why}")),
    }
    if let Some(leftover) = &report.leftover {
        text.push_str(&format!(
            "\nLeftover file: {} (from before the keyring)",
            leftover.display()
        ));
    }
    match &report.account {
        Some(Ok(account)) => text.push_str(&format!(
            "\nAccount:       {} (user id {})",
            account.username, account.user_id
        )),
        Some(Err(why)) => text.push_str(&format!(
            "\nAccount:       TIDAL did not accept the login: {why}"
        )),
        None => {}
    }
    text
}

pub async fn whoami(kind: SessionStoreKind, check: bool) -> Result<()> {
    let store = auth::open_store(kind, Interaction::Allow)?;
    // Reading is what moves an old file into the keyring, so it comes before looking for leftovers.
    let session = store.load().await;
    let leftover = match kind {
        SessionStoreKind::Keyring => auth::legacy_session_file()
            .ok()
            .filter(|path| path.exists()),
        SessionStoreKind::File => None,
    };

    let account = match (&session, check) {
        (Ok(Some(stored)), true) => Some(ask_tidal(stored).await),
        _ => None,
    };
    let report = Report {
        store: store.describe(),
        provider: store.provider().await,
        session: session
            .map(|stored| stored.is_some())
            .map_err(|error| error.to_string()),
        leftover,
        account,
    };
    println!("{}", format_report(&report));
    Ok(())
}

async fn ask_tidal(stored: &auth::StoredSession) -> Result<Account, String> {
    let mut client = client_from(stored);
    client
        .refresh_access_token(false)
        .await
        .map_err(|error| error.to_string())?;
    let user = client
        .user_info
        .as_ref()
        .ok_or_else(|| "TIDAL sent no account details".to_string())?;
    Ok(Account {
        username: user.username.clone(),
        user_id: user.user_id,
    })
}

pub async fn logout(kind: SessionStoreKind) -> Result<()> {
    let store = auth::open_store(kind, Interaction::Allow)?;
    let had_session = auth::logout(&*store).await.context("logging out")?;
    if had_session {
        println!(
            "Logged out: the session was removed from {}.",
            store.describe()
        );
        println!(
            "The token itself is not revoked on TIDAL's side (phonia cannot do that); to invalidate it, \
             remove phonia from your authorized apps in your TIDAL account settings."
        );
    } else {
        println!("There was no session to remove.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            store: "the desktop keyring".into(),
            provider: Some("gnome-keyring-d".into()),
            session: Ok(true),
            leftover: None,
            account: None,
        }
    }

    #[test]
    fn a_logged_in_report_names_the_store_and_its_provider_but_no_secret() {
        assert_eq!(
            format_report(&report()),
            "Session store: the desktop keyring (provided by gnome-keyring-d)\nSession:       yes (a refresh token is stored)"
        );
    }

    #[test]
    fn no_session_says_how_to_log_in() {
        let text = format_report(&Report {
            session: Ok(false),
            provider: None,
            ..report()
        });
        assert_eq!(
            text,
            "Session store: the desktop keyring\nSession:       none: run `phonia login`"
        );
    }

    #[test]
    fn a_store_that_cannot_be_read_says_why() {
        let text = format_report(&Report {
            session: Err("the keyring is locked".into()),
            ..report()
        });
        assert!(
            text.contains("could not be read: the keyring is locked"),
            "{text}"
        );
    }

    #[test]
    fn a_leftover_file_and_the_checked_account_are_shown() {
        let text = format_report(&Report {
            leftover: Some("/home/u/.config/phonia/session.json".into()),
            account: Some(Ok(Account {
                username: "someone".into(),
                user_id: 42,
            })),
            ..report()
        });
        assert!(
            text.contains(
                "Leftover file: /home/u/.config/phonia/session.json (from before the keyring)"
            ),
            "{text}"
        );
        assert!(
            text.ends_with("Account:       someone (user id 42)"),
            "{text}"
        );

        let text = format_report(&Report {
            account: Some(Err("401".into())),
            ..report()
        });
        assert!(
            text.contains("TIDAL did not accept the login: 401"),
            "{text}"
        );
    }
}
