mod config_cmd;
mod ctl;
mod player;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use phonia_core::engine::TrackRef;
use phonia_core::config::{self, Overrides, Quality, Settings};
use phonia_core::openers::{DispatchOpener, Source, TidalOpener};
use phonia_core::output::{alsa, device};
use phonia_core::queue::{Queue, QueueTrack, Repeat};
use phonia_core::{auth, tidal};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

/// phonia -- phase 0: CLI spike that validates PKCE login -> HiRes playbackinfo -> DASH segments
/// -> FLAC decoding -> bit-perfect ALSA output to a USB DAC.
#[derive(Parser)]
#[command(name = "phonia", about = "Bit-perfect TIDAL hi-fi player (phase 0)")]
struct Cli {
    /// The config file to read instead of `~/.config/phonia/config.toml` (also `PHONIA_CONFIG`).
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

/// The `--device` help shared by the commands that play.
const DEVICE_HELP: &str = "The ALSA device: hw:N,D, a card id such as hw:DS2,0, or `auto`. \
    Default: [output] device in the config file (`phonia devices` lists the cards)";

#[derive(Subcommand)]
enum Command {
    /// Logs in to TIDAL via PKCE (required to be granted HI_RES_LOSSLESS).
    ///
    /// In a terminal this prompts for the redirect URL. Without one (or with `--no-wait`) it
    /// only prints the login URL; finish afterwards with `--finish <redirect-url>`.
    Login {
        /// Print the login URL and exit instead of waiting for the redirect URL.
        #[arg(long, conflicts_with = "finish")]
        no_wait: bool,
        /// Finish a login started with `--no-wait`, using the URL the browser ended up on.
        #[arg(long, value_name = "REDIRECT_URL")]
        finish: Option<String>,
    },
    /// Streams and plays TIDAL tracks by their IDs, one after another.
    Play {
        #[arg(required = true)]
        track_ids: Vec<String>,
        #[arg(long, help = DEVICE_HELP)]
        device: Option<String>,
        /// The highest quality to ask TIDAL for: hires or lossless. Default: [tidal] max_quality
        /// in the config file, else hires.
        #[arg(long, value_name = "hires|lossless")]
        quality: Option<Quality>,
        /// If given, saves the downloaded bytes (fMP4/DASH or the JSON manifest) to this path.
        #[arg(long)]
        save_mp4: Option<PathBuf>,
        /// Read playback commands from the keyboard (pause, seek, ...); `?` lists them.
        #[arg(long)]
        interactive: bool,
        /// Play the tracks in a random order.
        #[arg(long)]
        shuffle: bool,
        /// What to do at the end of a track or of the queue.
        #[arg(long, value_enum, default_value_t = RepeatMode::Off)]
        repeat: RepeatMode,
    },
    /// Decodes and plays local files (FLAC or fMP4), one after another, through the same
    /// playback engine and ALSA output, to test them without depending on TIDAL.
    PlayFile {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        #[arg(long, help = DEVICE_HELP)]
        device: Option<String>,
        /// Read playback commands from the keyboard (pause, seek, next, ...); `?` lists them.
        #[arg(long)]
        interactive: bool,
        /// Play the files in a random order.
        #[arg(long)]
        shuffle: bool,
        /// What to do at the end of a track or of the queue.
        #[arg(long, value_enum, default_value_t = RepeatMode::Off)]
        repeat: RepeatMode,
    },
    /// Controls a running `phoniad` (the daemon): playback, queue and events.
    Ctl(ctl::CtlArgs),
    /// Lists the sound cards that can play, with the device name to put in the config file.
    Devices,
    /// Shows where the configuration comes from and what it says.
    Config {
        #[command(subcommand)]
        action: config_cmd::ConfigAction,
    },
    /// Lists which formats and rates the given ALSA device accepts, without playing anything.
    ProbeDevice {
        #[arg(long, help = DEVICE_HELP)]
        device: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RepeatMode {
    /// Stop after the last track.
    Off,
    /// Repeat the track that is playing, until you skip.
    One,
    /// Start over after the last track.
    All,
}

impl From<RepeatMode> for Repeat {
    fn from(mode: RepeatMode) -> Self {
        match mode {
            RepeatMode::Off => Repeat::Off,
            RepeatMode::One => Repeat::One,
            RepeatMode::All => Repeat::All,
        }
    }
}

/// Reads the config file and decides the settings: the command line over the file over the
/// defaults. Only the commands that need settings call this, so a broken config file doesn't stop
/// `phonia devices` or `phonia config path`, which are what you use to fix it.
fn settings(config_flag: Option<&Path>, overrides: Overrides) -> Result<Settings> {
    let source = config::discover_from_env(config_flag);
    let loaded = config::load(source.as_ref())?;
    Ok(config::resolve(overrides, &loaded.file))
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Login { no_wait, finish } => match finish {
            Some(redirect_url) => auth::login_finish(&redirect_url).await,
            None if no_wait || !std::io::stdin().is_terminal() => auth::login_begin(),
            None => auth::login().await,
        },
        Command::Play { track_ids, device, quality, save_mp4, interactive, shuffle, repeat } => {
            match settings(cli.config.as_deref(), Overrides { device, max_quality: quality, ..Overrides::default() }) {
                Ok(settings) => {
                    run_play(&track_ids, &settings, save_mp4.as_deref(), interactive, shuffle, repeat.into()).await
                }
                Err(error) => Err(error),
            }
        }
        Command::PlayFile { paths, device, interactive, shuffle, repeat } => {
            match settings(cli.config.as_deref(), Overrides { device, ..Overrides::default() }) {
                Ok(settings) => run_play_file(&paths, &settings, interactive, shuffle, repeat.into()).await,
                Err(error) => Err(error),
            }
        }
        Command::Ctl(args) => ctl::run(args, cli.config.as_deref()).await,
        Command::Devices => device::list(std::path::Path::new(device::ASOUND)).map(|text| println!("{text}")),
        Command::Config { action } => config_cmd::run(action, cli.config.as_deref()),
        Command::ProbeDevice { device } => {
            match settings(cli.config.as_deref(), Overrides { device, ..Overrides::default() })
                .and_then(|settings| settings.require_device().map(str::to_string))
            {
                Ok(device) => alsa::probe_device(&device),
                Err(error) => Err(error),
            }
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_play_file(
    paths: &[PathBuf],
    settings: &Settings,
    interactive: bool,
    shuffle: bool,
    repeat: Repeat,
) -> Result<()> {
    let device = settings.require_device()?;
    let queue = Queue::new(Arc::new(DispatchOpener::new(None)));
    for path in paths {
        // Sources are absolute, so a file means the same thing wherever it is opened.
        let path = std::fs::canonicalize(path).with_context(|| format!("opening {path:?}"))?;
        queue.add([QueueTrack {
            source: TrackRef(Source::file(&path)?.to_wire()),
            title: path.file_name().map(|name| name.to_string_lossy().into_owned()),
            duration: None,
        }]);
    }
    queue.set_shuffle(shuffle);
    queue.set_repeat(repeat);
    player::run(queue, device, interactive).await
}

async fn run_play(
    track_ids: &[String],
    settings: &Settings,
    save_mp4: Option<&Path>,
    interactive: bool,
    shuffle: bool,
    repeat: Repeat,
) -> Result<()> {
    let device = settings.require_device()?;
    let client = auth::load_client().await?;
    let http = tidal::build_http_client()?;

    let mut opener = TidalOpener::new(http, client, settings.max_quality.value.into()).print_info();
    if let Some(path) = save_mp4 {
        let file = std::fs::File::create(path).with_context(|| format!("creating {path:?}"))?;
        println!("Saving audio to {path:?} as it streams (the first track opened)...");
        opener = opener.saving_to(file);
    }

    let queue = Queue::new(Arc::new(DispatchOpener::new(Some(opener))));
    for id in track_ids {
        queue.add([QueueTrack {
            source: TrackRef(Source::parse(&format!("tidal:{id}"))?.to_wire()),
            title: Some(id.clone()),
            duration: None,
        }]);
    }
    queue.set_shuffle(shuffle);
    queue.set_repeat(repeat);
    player::run(queue, device, interactive).await
}
