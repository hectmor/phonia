//! The contents of `config.toml`: what the file may say, and how it is read.
//!
//! Every setting is optional so that "not written" can be told apart from "written, and equal to
//! the default": that is what lets `phonia config show` say where each value came from. Unknown
//! keys are an error, not ignored: a typo like `devise = "hw:DS2,0"` would otherwise quietly play
//! on the wrong card.

use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

/// The highest quality to ask TIDAL for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    #[default]
    Hires,
    Lossless,
}

impl fmt::Display for Quality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Quality::Hires => "hires",
            Quality::Lossless => "lossless",
        })
    }
}

impl FromStr for Quality {
    type Err = String;

    /// The same words the config file and the command line both accept.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "hires" => Ok(Quality::Hires),
            "lossless" => Ok(Quality::Lossless),
            other => Err(format!(
                "unknown quality {other:?}: expected hires or lossless"
            )),
        }
    }
}

/// How phonia gets at the sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    /// phonia opens the card itself, with nothing mixing or resampling in between: bit-perfect.
    #[default]
    Exclusive,
    /// phonia plays through the desktop's sound server (PipeWire, PulseAudio), which mixes and
    /// resamples: not bit-perfect, but any output works, Bluetooth included.
    Shared,
}

impl fmt::Display for OutputMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OutputMode::Exclusive => "exclusive",
            OutputMode::Shared => "shared",
        })
    }
}

/// How long a pause lasts before the sound card is handed back to the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseAfterPause {
    /// Keep the card for as long as there is a track.
    Never,
    /// Hand it back after this long; zero hands it back on every pause.
    After(Duration),
}

impl Default for ReleaseAfterPause {
    fn default() -> Self {
        ReleaseAfterPause::After(Duration::from_secs(10))
    }
}

impl ReleaseAfterPause {
    /// The wait, or `None` for never.
    pub fn duration(self) -> Option<Duration> {
        match self {
            ReleaseAfterPause::Never => None,
            ReleaseAfterPause::After(wait) => Some(wait),
        }
    }
}

impl fmt::Display for ReleaseAfterPause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReleaseAfterPause::Never => f.write_str("never"),
            ReleaseAfterPause::After(wait) => write!(f, "{} s", wait.as_secs()),
        }
    }
}

impl<'de> Deserialize<'de> for ReleaseAfterPause {
    /// A number of seconds, or the word `never`.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = ReleaseAfterPause;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number of seconds (0 or more) or \"never\"")
            }

            fn visit_i64<E: serde::de::Error>(self, seconds: i64) -> Result<Self::Value, E> {
                u64::try_from(seconds)
                    .map(|seconds| ReleaseAfterPause::After(Duration::from_secs(seconds)))
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Signed(seconds), &self))
            }

            fn visit_u64<E: serde::de::Error>(self, seconds: u64) -> Result<Self::Value, E> {
                Ok(ReleaseAfterPause::After(Duration::from_secs(seconds)))
            }

            fn visit_str<E: serde::de::Error>(self, word: &str) -> Result<Self::Value, E> {
                match word {
                    "never" => Ok(ReleaseAfterPause::Never),
                    other => Err(E::invalid_value(serde::de::Unexpected::Str(other), &self)),
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Output {
    /// The ALSA device: `hw:N,D`, a card id such as `hw:DS2,0`, or `auto`.
    pub device: Option<String>,
    pub mode: Option<OutputMode>,
    /// In shared mode, the output to play on: `"default"` (the desktop's, following it) or the name
    /// of one, as `phonia devices` lists it.
    pub sink: Option<String>,
    /// Ask the desktop (WirePlumber, PulseAudio) to release the card before opening it.
    pub reserve: Option<bool>,
    pub release_after_pause: Option<ReleaseAfterPause>,
}

/// Where the TIDAL login is kept between runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStoreKind {
    /// The desktop keyring (Secret Service: GNOME Keyring, KWallet, KeePassXC).
    #[default]
    Keyring,
    /// A file only you can read (`session.json` in the config directory).
    File,
}

impl fmt::Display for SessionStoreKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SessionStoreKind::Keyring => "keyring",
            SessionStoreKind::File => "file",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tidal {
    pub max_quality: Option<Quality>,
    pub session_store: Option<SessionStoreKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Daemon {
    pub socket: Option<PathBuf>,
    pub verbose: Option<bool>,
}

/// The whole file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    pub output: Output,
    pub tidal: Tidal,
    pub daemon: Daemon,
}

/// Reads the text of a config file. The error names the offending key and line.
pub fn parse(text: &str) -> Result<ConfigFile, toml::de::Error> {
    toml::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[output]
device = "hw:DS2,0"
mode = "exclusive"
sink = "default"
reserve = false
release_after_pause = 30

[tidal]
max_quality = "lossless"
session_store = "file"

[daemon]
socket = "/run/user/1000/phonia/phoniad.sock"
verbose = true
"#;

    #[test]
    fn a_full_file() {
        assert_eq!(
            parse(FULL).unwrap(),
            ConfigFile {
                output: Output {
                    device: Some("hw:DS2,0".into()),
                    mode: Some(OutputMode::Exclusive),
                    sink: Some("default".into()),
                    reserve: Some(false),
                    release_after_pause: Some(ReleaseAfterPause::After(Duration::from_secs(30))),
                },
                tidal: Tidal {
                    max_quality: Some(Quality::Lossless),
                    session_store: Some(SessionStoreKind::File)
                },
                daemon: Daemon {
                    socket: Some("/run/user/1000/phonia/phoniad.sock".into()),
                    verbose: Some(true)
                },
            }
        );
    }

    #[test]
    fn the_time_before_a_pause_gives_the_card_back() {
        let read = |text: &str| parse(&format!("[output]\nrelease_after_pause = {text}\n"));
        assert_eq!(
            read("0").unwrap().output.release_after_pause,
            Some(ReleaseAfterPause::After(Duration::ZERO))
        );
        assert_eq!(
            read("\"never\"").unwrap().output.release_after_pause,
            Some(ReleaseAfterPause::Never)
        );
        for bad in ["-1", "\"soon\"", "true", "1.5"] {
            let error = read(bad).unwrap_err().to_string();
            assert!(
                error.contains("release_after_pause") || error.contains("never"),
                "{bad}: {error}"
            );
        }
    }

    #[test]
    fn an_unknown_session_store_is_an_error() {
        let error = parse("[tidal]\nsession_store = \"cloud\"\n")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("session_store") || error.contains("cloud"),
            "{error}"
        );
    }

    #[test]
    fn a_minimal_file_leaves_everything_else_unset() {
        let file = parse("[output]\ndevice = \"auto\"\n").unwrap();
        assert_eq!(file.output.device.as_deref(), Some("auto"));
        assert_eq!(
            (
                file.output.mode,
                file.tidal.max_quality,
                file.daemon.socket,
                file.daemon.verbose
            ),
            (None, None, None, None)
        );
    }

    #[test]
    fn an_empty_file_and_a_file_with_only_comments_are_the_defaults() {
        assert_eq!(parse("").unwrap(), ConfigFile::default());
        assert_eq!(
            parse("# nothing to see\n\n").unwrap(),
            ConfigFile::default()
        );
        assert_eq!(
            parse("[output]\n").unwrap(),
            ConfigFile::default(),
            "an empty section says nothing either"
        );
    }

    #[test]
    fn a_typo_in_a_key_is_an_error_that_names_it_and_where() {
        let error = parse("[output]\ndevise = \"hw:DS2,0\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("devise"), "{error}");
        assert!(error.contains("line 2"), "{error}");
    }

    #[test]
    fn a_typo_in_a_section_is_an_error_too() {
        let error = parse("[outputs]\ndevice = \"hw:1,0\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("outputs"), "{error}");
        assert!(
            parse("[tidal]\nquality = \"hires\"\n").is_err(),
            "the key is max_quality"
        );
    }

    #[test]
    fn a_value_of_the_wrong_type_is_an_error() {
        let error = parse("[daemon]\nverbose = \"yes\"\n")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("invalid type") && error.contains("verbose"),
            "{error}"
        );
        assert!(parse("[output]\ndevice = 3\n").is_err());
    }

    #[test]
    fn a_word_that_is_not_an_option_lists_the_ones_that_are() {
        let quality = parse("[tidal]\nmax_quality = \"mqa\"\n")
            .unwrap_err()
            .to_string();
        assert!(
            quality.contains("unknown variant")
                && quality.contains("hires")
                && quality.contains("lossless"),
            "{quality}"
        );

        let mode = parse("[output]\nmode = \"cloud\"\n")
            .unwrap_err()
            .to_string();
        assert!(
            mode.contains("unknown variant")
                && mode.contains("exclusive")
                && mode.contains("shared"),
            "{mode}"
        );
    }

    #[test]
    fn shared_mode_and_its_output_are_read() {
        let file = parse("[output]\nmode = \"shared\"\nsink = \"bluez_output.AA\"\n").unwrap();
        assert_eq!(file.output.mode, Some(OutputMode::Shared));
        assert_eq!(file.output.sink.as_deref(), Some("bluez_output.AA"));
    }

    #[test]
    fn broken_toml_is_an_error_with_a_position() {
        let error = parse("[output\ndevice = 1").unwrap_err().to_string();
        assert!(error.contains("line 1"), "{error}");
    }

    #[test]
    fn the_command_line_and_the_file_accept_the_same_words() {
        for quality in [Quality::Hires, Quality::Lossless] {
            assert_eq!(quality.to_string().parse::<Quality>().unwrap(), quality);
        }
        assert!("MQA".parse::<Quality>().is_err());
        assert_eq!(OutputMode::Exclusive.to_string(), "exclusive");
    }
}
