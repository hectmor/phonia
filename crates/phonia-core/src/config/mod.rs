//! Configuration: the settings file, and how a setting is decided from the command line, the
//! file and the built-in defaults.
//!
//! Precedence is `flag > file > default`. Nothing here reads the environment or parses a command
//! line: the binaries hand in what they were given, so every rule is a plain function that a test
//! can call.

mod file;

pub use file::{ConfigFile, Daemon, Output, OutputMode, Quality, ReleaseAfterPause, SessionStoreKind, Tidal, parse};

use anyhow::{Context, Result, anyhow, bail};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "config.toml";
/// The environment variable that names a config file to use instead of the default one.
pub const CONFIG_ENV: &str = "PHONIA_CONFIG";

/// The directory phonia keeps its files in (the session, the config): `<config dir>/phonia`.
/// Not created here.
pub fn dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("phonia"))
}

/// Where the config file to read comes from, which decides what a missing file means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Named by `--config` or `PHONIA_CONFIG`: it has to exist.
    Explicit(PathBuf),
    /// The usual place: absent is fine, it just means "all defaults".
    Default(PathBuf),
}

impl ConfigSource {
    pub fn path(&self) -> &Path {
        match self {
            ConfigSource::Explicit(path) | ConfigSource::Default(path) => path,
        }
    }
}

/// Picks the config file: the `--config` flag, else the environment variable, else the default
/// place. Pure; [`discover_from_env`] supplies the real environment.
pub fn discover(flag: Option<&Path>, env: Option<&OsStr>, dir: Option<&Path>) -> Option<ConfigSource> {
    if let Some(path) = flag {
        return Some(ConfigSource::Explicit(path.to_path_buf()));
    }
    if let Some(path) = env.filter(|value| !value.is_empty()) {
        return Some(ConfigSource::Explicit(PathBuf::from(path)));
    }
    dir.map(|dir| ConfigSource::Default(dir.join(CONFIG_FILE)))
}

pub fn discover_from_env(flag: Option<&Path>) -> Option<ConfigSource> {
    let env = std::env::var_os(CONFIG_ENV);
    discover(flag, env.as_deref(), dir().as_deref())
}

/// A config file that was read, and where from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Loaded {
    pub file: ConfigFile,
    /// The file that was read; `None` when there was none (all defaults).
    pub path: Option<PathBuf>,
}

/// Reads the config file. A default file that isn't there is not an error; one that was asked for
/// by name and isn't there is, and so is a file that can't be read or understood (the message
/// names the file, and the key and line).
pub fn load(source: Option<&ConfigSource>) -> Result<Loaded> {
    let Some(source) = source else { return Ok(Loaded::default()) };
    let path = source.path();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && matches!(source, ConfigSource::Default(_)) => {
            return Ok(Loaded::default());
        }
        Err(error) => return Err(anyhow!(error)).with_context(|| format!("reading the config file {}", path.display())),
    };
    let file = parse(&text).with_context(|| format!("in the config file {}", path.display()))?;
    Ok(Loaded { file, path: Some(path.to_path_buf()) })
}

/// Where a setting's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Flag,
    File,
    Default,
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Origin::Flag => "command line",
            Origin::File => "config file",
            Origin::Default => "default",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sourced<T> {
    pub value: T,
    pub origin: Origin,
}

impl<T> Sourced<T> {
    /// The flag if given, else the file's value, else the default.
    pub fn pick(flag: Option<T>, file: Option<T>, default: T) -> Self {
        match (flag, file) {
            (Some(value), _) => Sourced { value, origin: Origin::Flag },
            (None, Some(value)) => Sourced { value, origin: Origin::File },
            (None, None) => Sourced { value: default, origin: Origin::Default },
        }
    }
}

/// What the command line said. `None` means the flag was not given.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub device: Option<String>,
    pub max_quality: Option<Quality>,
    pub socket: Option<PathBuf>,
    pub verbose: Option<bool>,
}

/// The settings in force, each with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `None` when neither the command line nor the file names a device.
    pub device: Sourced<Option<String>>,
    pub mode: Sourced<OutputMode>,
    pub reserve: Sourced<bool>,
    pub release_after_pause: Sourced<ReleaseAfterPause>,
    pub max_quality: Sourced<Quality>,
    pub session_store: Sourced<SessionStoreKind>,
    /// `None` means "the default socket path", which only the binaries know.
    pub socket: Sourced<Option<PathBuf>>,
    pub verbose: Sourced<bool>,
}

/// Decides every setting: command line over file over default.
pub fn resolve(overrides: Overrides, file: &ConfigFile) -> Settings {
    Settings {
        device: Sourced::pick(overrides.device.map(Some), file.output.device.clone().map(Some), None),
        mode: Sourced::pick(None, file.output.mode, OutputMode::default()),
        reserve: Sourced::pick(None, file.output.reserve, true),
        release_after_pause: Sourced::pick(None, file.output.release_after_pause, ReleaseAfterPause::default()),
        max_quality: Sourced::pick(overrides.max_quality, file.tidal.max_quality, Quality::default()),
        session_store: Sourced::pick(None, file.tidal.session_store, SessionStoreKind::default()),
        socket: Sourced::pick(overrides.socket.map(Some), file.daemon.socket.clone().map(Some), None),
        verbose: Sourced::pick(overrides.verbose, file.daemon.verbose, false),
    }
}

impl Settings {
    /// The device to play on, or an explanation of how to name one.
    pub fn require_device(&self) -> Result<&str> {
        match self.device.value.as_deref() {
            Some(device) => Ok(device),
            None => bail!(
                "no audio device configured. Run `phonia devices` to see the sound cards, then put \
                 `device = \"hw:...\"` under [output] in the config file, or pass --device"
            ),
        }
    }

    /// The socket to use: the configured one, or `default()` (the daemon's usual path).
    pub fn socket_path(&self, default: impl FnOnce() -> PathBuf) -> PathBuf {
        self.socket.value.clone().unwrap_or_else(default)
    }
}

impl From<Quality> for tidlers::client::models::playback::AudioQuality {
    fn from(quality: Quality) -> Self {
        match quality {
            Quality::Hires => Self::HiRes,
            Quality::Lossless => Self::Lossless,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("phonia-config-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- which file ------------------------------------------------------------------------

    #[test]
    fn the_flag_beats_the_environment_beats_the_default_place() {
        let dir = Path::new("/home/u/.config/phonia");
        let flag = Path::new("/tmp/flag.toml");
        let env = OsStr::new("/tmp/env.toml");

        assert_eq!(discover(Some(flag), Some(env), Some(dir)), Some(ConfigSource::Explicit("/tmp/flag.toml".into())));
        assert_eq!(discover(None, Some(env), Some(dir)), Some(ConfigSource::Explicit("/tmp/env.toml".into())));
        assert_eq!(discover(None, None, Some(dir)), Some(ConfigSource::Default("/home/u/.config/phonia/config.toml".into())));
        assert_eq!(discover(None, Some(OsStr::new("")), Some(dir)), Some(ConfigSource::Default("/home/u/.config/phonia/config.toml".into())), "an empty variable is unset");
        assert_eq!(discover(None, None, None), None, "no config directory and nothing named: no file");
    }

    // ---- reading it ------------------------------------------------------------------------

    #[test]
    fn no_file_at_all_means_the_defaults() {
        assert_eq!(load(None).unwrap(), Loaded::default());
    }

    #[test]
    fn a_missing_default_file_is_fine() {
        let source = ConfigSource::Default(temp_dir("missing-default").join("config.toml"));
        assert_eq!(load(Some(&source)).unwrap(), Loaded::default());
    }

    #[test]
    fn a_missing_file_that_was_asked_for_is_an_error_naming_it() {
        let path = temp_dir("missing-explicit").join("nope.toml");
        let error = format!("{:#}", load(Some(&ConfigSource::Explicit(path.clone()))).unwrap_err());
        assert!(error.contains("nope.toml"), "{error}");
    }

    #[test]
    fn a_file_is_read_and_remembered() {
        let path = temp_dir("read").join("config.toml");
        std::fs::write(&path, "[output]\ndevice = \"hw:DS2,0\"\n").unwrap();
        let loaded = load(Some(&ConfigSource::Default(path.clone()))).unwrap();
        assert_eq!(loaded.file.output.device.as_deref(), Some("hw:DS2,0"));
        assert_eq!(loaded.path, Some(path));
    }

    #[test]
    fn a_broken_file_is_an_error_with_the_file_the_key_and_the_line() {
        let path = temp_dir("broken").join("config.toml");
        std::fs::write(&path, "[output]\n\ndevise = \"hw:1,0\"\n").unwrap();
        let error = format!("{:#}", load(Some(&ConfigSource::Default(path))).unwrap_err());
        assert!(error.contains("config.toml") && error.contains("devise") && error.contains("line 3"), "{error}");
    }

    // ---- deciding the settings -------------------------------------------------------------

    fn file(device: Option<&str>, quality: Option<Quality>, socket: Option<&str>, verbose: Option<bool>) -> ConfigFile {
        ConfigFile {
            output: Output { device: device.map(str::to_string), ..Output::default() },
            tidal: Tidal { max_quality: quality, ..Tidal::default() },
            daemon: Daemon { socket: socket.map(PathBuf::from), verbose },
        }
    }

    #[test]
    fn with_nothing_said_everything_is_the_default() {
        let settings = resolve(Overrides::default(), &ConfigFile::default());
        assert_eq!(settings.device, Sourced { value: None, origin: Origin::Default });
        assert_eq!(settings.mode, Sourced { value: OutputMode::Exclusive, origin: Origin::Default });
        assert_eq!(settings.reserve, Sourced { value: true, origin: Origin::Default });
        assert_eq!(
            settings.release_after_pause,
            Sourced { value: ReleaseAfterPause::After(std::time::Duration::from_secs(10)), origin: Origin::Default }
        );
        assert_eq!(settings.max_quality, Sourced { value: Quality::Hires, origin: Origin::Default });
        assert_eq!(settings.session_store, Sourced { value: SessionStoreKind::File, origin: Origin::Default });
        assert_eq!(settings.socket, Sourced { value: None, origin: Origin::Default });
        assert_eq!(settings.verbose, Sourced { value: false, origin: Origin::Default });
    }

    #[test]
    fn the_file_beats_the_default() {
        let settings = resolve(Overrides::default(), &file(Some("hw:DS2,0"), Some(Quality::Lossless), Some("/s"), Some(true)));
        assert_eq!(settings.device, Sourced { value: Some("hw:DS2,0".into()), origin: Origin::File });
        assert_eq!(settings.max_quality, Sourced { value: Quality::Lossless, origin: Origin::File });
        assert_eq!(settings.socket, Sourced { value: Some("/s".into()), origin: Origin::File });
        assert_eq!(settings.verbose, Sourced { value: true, origin: Origin::File });
    }

    #[test]
    fn the_command_line_beats_the_file() {
        let overrides = Overrides {
            device: Some("hw:9,0".into()),
            max_quality: Some(Quality::Hires),
            socket: Some("/flag".into()),
            verbose: Some(false),
        };
        let settings = resolve(overrides, &file(Some("hw:DS2,0"), Some(Quality::Lossless), Some("/file"), Some(true)));
        assert_eq!(settings.device, Sourced { value: Some("hw:9,0".into()), origin: Origin::Flag });
        assert_eq!(settings.max_quality, Sourced { value: Quality::Hires, origin: Origin::Flag });
        assert_eq!(settings.socket, Sourced { value: Some("/flag".into()), origin: Origin::Flag });
        assert_eq!(settings.verbose, Sourced { value: false, origin: Origin::Flag }, "an explicit false beats a true in the file");
    }

    #[test]
    fn a_setting_equal_to_its_default_still_reports_where_it_was_written() {
        let settings = resolve(Overrides::default(), &file(None, Some(Quality::Hires), None, Some(false)));
        assert_eq!(settings.max_quality.origin, Origin::File, "written in the file, though it is the default value");
        assert_eq!(settings.verbose.origin, Origin::File);
    }

    #[test]
    fn a_device_is_required_and_the_message_says_how_to_set_one() {
        let error = resolve(Overrides::default(), &ConfigFile::default()).require_device().unwrap_err().to_string();
        assert!(error.contains("phonia devices") && error.contains("[output]") && error.contains("--device"), "{error}");

        let named = resolve(Overrides { device: Some("hw:DS2,0".into()), ..Overrides::default() }, &ConfigFile::default());
        assert_eq!(named.require_device().unwrap(), "hw:DS2,0");
    }

    #[test]
    fn the_socket_falls_back_to_whatever_the_caller_calls_the_default() {
        let unset = resolve(Overrides::default(), &ConfigFile::default());
        assert_eq!(unset.socket_path(|| PathBuf::from("/usual.sock")), PathBuf::from("/usual.sock"));

        let set = resolve(Overrides::default(), &file(None, None, Some("/mine.sock"), None));
        assert_eq!(set.socket_path(|| unreachable!("the default must not be asked for")), PathBuf::from("/mine.sock"));
    }

    #[test]
    fn origins_read_naturally() {
        assert_eq!(Origin::Flag.to_string(), "command line");
        assert_eq!(Origin::File.to_string(), "config file");
        assert_eq!(Origin::Default.to_string(), "default");
    }

    #[test]
    fn qualities_map_to_tidals() {
        use tidlers::client::models::playback::AudioQuality;
        assert!(matches!(AudioQuality::from(Quality::Hires), AudioQuality::HiRes));
        assert!(matches!(AudioQuality::from(Quality::Lossless), AudioQuality::Lossless));
    }
}
