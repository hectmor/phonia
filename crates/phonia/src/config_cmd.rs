//! `phonia config`: where the configuration comes from and what it says.

use anyhow::Result;
use clap::Subcommand;
use phonia_core::config::{self, ConfigSource, Overrides, Settings};
use std::path::Path;

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Prints the config file that is used, and whether it exists.
    Path,
    /// Prints each setting with its value and where the value comes from (the config file or the
    /// built-in default; command-line flags apply per command and are not included).
    Show,
}

pub fn run(action: ConfigAction, config_flag: Option<&Path>) -> Result<()> {
    let source = config::discover_from_env(config_flag);
    match action {
        ConfigAction::Path => println!("{}", describe_source(source.as_ref())),
        ConfigAction::Show => {
            let loaded = config::load(source.as_ref())?;
            let settings = config::resolve(Overrides::default(), &loaded.file);
            println!(
                "{}",
                format_settings(
                    &settings,
                    source.as_ref(),
                    loaded.path.is_some(),
                    &phonia_ipc::socket::default_socket_path()
                )
            );
        }
    }
    Ok(())
}

fn describe_source(source: Option<&ConfigSource>) -> String {
    match source {
        None => "no config directory could be found, so no config file is read".to_string(),
        Some(source) => {
            let path = source.path();
            let how = match (source, path.exists()) {
                (ConfigSource::Explicit(_), true) => "named explicitly",
                (ConfigSource::Explicit(_), false) => "named explicitly, but it does not exist",
                (ConfigSource::Default(_), true) => "the default place",
                (ConfigSource::Default(_), false) => {
                    "the default place; it does not exist, so the defaults are used"
                }
            };
            format!("{} ({how})", path.display())
        }
    }
}

fn format_settings(
    settings: &Settings,
    source: Option<&ConfigSource>,
    read: bool,
    default_socket: &Path,
) -> String {
    let file = match (source, read) {
        (Some(source), true) => source.path().display().to_string(),
        (Some(source), false) => format!("{} (not found: all defaults)", source.path().display()),
        (None, _) => "none (all defaults)".to_string(),
    };
    let rows = [
        (
            "output.device",
            settings
                .device
                .value
                .clone()
                .unwrap_or_else(|| "(not set)".to_string()),
            settings.device.origin,
        ),
        (
            "output.mode",
            settings.mode.value.to_string(),
            settings.mode.origin,
        ),
        (
            "output.sink",
            settings.sink.value.clone(),
            settings.sink.origin,
        ),
        (
            "output.reserve",
            settings.reserve.value.to_string(),
            settings.reserve.origin,
        ),
        (
            "output.release_after_pause",
            settings.release_after_pause.value.to_string(),
            settings.release_after_pause.origin,
        ),
        (
            "tidal.max_quality",
            settings.max_quality.value.to_string(),
            settings.max_quality.origin,
        ),
        (
            "tidal.session_store",
            settings.session_store.value.to_string(),
            settings.session_store.origin,
        ),
        (
            "daemon.socket",
            settings
                .socket
                .value
                .clone()
                .unwrap_or_else(|| default_socket.to_path_buf())
                .display()
                .to_string(),
            settings.socket.origin,
        ),
        (
            "daemon.verbose",
            settings.verbose.value.to_string(),
            settings.verbose.origin,
        ),
    ];
    let key_width = rows.iter().map(|row| row.0.len()).max().unwrap_or(0);
    let value_width = rows.iter().map(|row| row.1.len()).max().unwrap_or(0);

    let mut text = format!("Config file: {file}");
    for (key, value, origin) in rows {
        text.push_str(&format!(
            "\n  {key:<key_width$}  {value:<value_width$}  ({origin})"
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use phonia_core::config::{ConfigFile, Output, Quality, Tidal};

    fn settings(file: &ConfigFile) -> Settings {
        config::resolve(Overrides::default(), file)
    }

    #[test]
    fn with_no_file_everything_is_a_default_and_the_device_is_not_set() {
        let text = format_settings(
            &settings(&ConfigFile::default()),
            Some(&ConfigSource::Default(
                "/home/u/.config/phonia/config.toml".into(),
            )),
            false,
            Path::new("/run/user/1000/phonia/phoniad.sock"),
        );
        assert_eq!(
            text,
            "Config file: /home/u/.config/phonia/config.toml (not found: all defaults)\n\
             \x20 output.device               (not set)                           (default)\n\
             \x20 output.mode                 exclusive                           (default)\n\
             \x20 output.sink                 default                             (default)\n\
             \x20 output.reserve              true                                (default)\n\
             \x20 output.release_after_pause  10 s                                (default)\n\
             \x20 tidal.max_quality           hires                               (default)\n\
             \x20 tidal.session_store         keyring                             (default)\n\
             \x20 daemon.socket               /run/user/1000/phonia/phoniad.sock  (default)\n\
             \x20 daemon.verbose              false                               (default)"
        );
    }

    #[test]
    fn values_from_the_file_say_so() {
        let file = ConfigFile {
            output: Output {
                device: Some("hw:DS2,0".into()),
                ..Output::default()
            },
            tidal: Tidal {
                max_quality: Some(Quality::Lossless),
                ..Tidal::default()
            },
            ..ConfigFile::default()
        };
        let text = format_settings(
            &settings(&file),
            Some(&ConfigSource::Explicit("/tmp/c.toml".into())),
            true,
            Path::new("/s"),
        );
        assert!(text.starts_with("Config file: /tmp/c.toml\n"), "{text}");
        assert!(
            text.contains("hw:DS2,0") && text.contains("(config file)"),
            "{text}"
        );
        assert!(text.contains("lossless"), "{text}");
        assert_eq!(
            text.matches("(default)").count(),
            7,
            "mode, sink, reserve, release_after_pause, session_store, socket and verbose were not written"
        );
    }

    #[test]
    fn the_path_says_whether_the_file_exists() {
        assert!(describe_source(None).contains("no config file is read"));

        let missing = ConfigSource::Explicit("/definitely/not/here.toml".into());
        assert_eq!(
            describe_source(Some(&missing)),
            "/definitely/not/here.toml (named explicitly, but it does not exist)"
        );

        let missing_default = ConfigSource::Default("/definitely/not/config.toml".into());
        assert!(describe_source(Some(&missing_default)).contains("defaults are used"));

        let here = std::env::current_exe().unwrap();
        assert!(
            describe_source(Some(&ConfigSource::Explicit(here))).ends_with("(named explicitly)")
        );
    }
}
