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
            other => Err(format!("unknown quality {other:?}: expected hires or lossless")),
        }
    }
}

/// How phonia gets at the sound card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    /// phonia opens the card itself, with nothing mixing or resampling in between: bit-perfect.
    #[default]
    Exclusive,
}

impl fmt::Display for OutputMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OutputMode::Exclusive => "exclusive",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Output {
    /// The ALSA device: `hw:N,D`, a card id such as `hw:DS2,0`, or `auto`.
    pub device: Option<String>,
    pub mode: Option<OutputMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tidal {
    pub max_quality: Option<Quality>,
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

[tidal]
max_quality = "lossless"

[daemon]
socket = "/run/user/1000/phonia/phoniad.sock"
verbose = true
"#;

    #[test]
    fn a_full_file() {
        assert_eq!(
            parse(FULL).unwrap(),
            ConfigFile {
                output: Output { device: Some("hw:DS2,0".into()), mode: Some(OutputMode::Exclusive) },
                tidal: Tidal { max_quality: Some(Quality::Lossless) },
                daemon: Daemon { socket: Some("/run/user/1000/phonia/phoniad.sock".into()), verbose: Some(true) },
            }
        );
    }

    #[test]
    fn a_minimal_file_leaves_everything_else_unset() {
        let file = parse("[output]\ndevice = \"auto\"\n").unwrap();
        assert_eq!(file.output.device.as_deref(), Some("auto"));
        assert_eq!((file.output.mode, file.tidal.max_quality, file.daemon.socket, file.daemon.verbose), (None, None, None, None));
    }

    #[test]
    fn an_empty_file_and_a_file_with_only_comments_are_the_defaults() {
        assert_eq!(parse("").unwrap(), ConfigFile::default());
        assert_eq!(parse("# nothing to see\n\n").unwrap(), ConfigFile::default());
        assert_eq!(parse("[output]\n").unwrap(), ConfigFile::default(), "an empty section says nothing either");
    }

    #[test]
    fn a_typo_in_a_key_is_an_error_that_names_it_and_where() {
        let error = parse("[output]\ndevise = \"hw:DS2,0\"\n").unwrap_err().to_string();
        assert!(error.contains("devise"), "{error}");
        assert!(error.contains("line 2"), "{error}");
    }

    #[test]
    fn a_typo_in_a_section_is_an_error_too() {
        let error = parse("[outputs]\ndevice = \"hw:1,0\"\n").unwrap_err().to_string();
        assert!(error.contains("outputs"), "{error}");
        assert!(parse("[tidal]\nquality = \"hires\"\n").is_err(), "the key is max_quality");
    }

    #[test]
    fn a_value_of_the_wrong_type_is_an_error() {
        let error = parse("[daemon]\nverbose = \"yes\"\n").unwrap_err().to_string();
        assert!(error.contains("invalid type") && error.contains("verbose"), "{error}");
        assert!(parse("[output]\ndevice = 3\n").is_err());
    }

    #[test]
    fn a_word_that_is_not_an_option_lists_the_ones_that_are() {
        let quality = parse("[tidal]\nmax_quality = \"mqa\"\n").unwrap_err().to_string();
        assert!(quality.contains("unknown variant") && quality.contains("hires") && quality.contains("lossless"), "{quality}");

        let mode = parse("[output]\nmode = \"shared\"\n").unwrap_err().to_string();
        assert!(mode.contains("unknown variant") && mode.contains("exclusive"), "{mode}");
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
