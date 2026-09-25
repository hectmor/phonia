mod config_cmd;
mod ctl;
mod player;
mod session_cmd;

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
    /// Forgets the TIDAL login: removes the stored session (and any old `session.json`).
    Logout,
    /// Shows where the TIDAL login is kept and whether there is one. Never prints a token.
    Whoami {
        /// Also ask TIDAL which account the login belongs to.
        #[arg(long)]
        check: bool,
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

/// Where the TIDAL login is kept, according to the settings.
fn session_store(config_flag: Option<&Path>) -> Result<Arc<dyn auth::SessionStore>> {
    auth::open_store(settings(config_flag, Overrides::default())?.session_store.value, auth::Interaction::Allow)
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Login { no_wait, finish } => match session_store(cli.config.as_deref()) {
            Ok(store) => match finish {
                Some(redirect_url) => auth::login_finish(&*store, &redirect_url).await,
                None if no_wait || !std::io::stdin().is_terminal() => auth::login_begin(),
                None => auth::login(&*store).await,
            },
            Err(error) => Err(error),
        },
        Command::Logout => match settings(cli.config.as_deref(), Overrides::default()) {
            Ok(settings) => session_cmd::logout(settings.session_store.value).await,
            Err(error) => Err(error),
        },
        Command::Whoami { check } => match settings(cli.config.as_deref(), Overrides::default()) {
            Ok(settings) => session_cmd::whoami(settings.session_store.value, check).await,
            Err(error) => Err(error),
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
        Command::Devices => run_devices().await,
        Command::Config { action } => config_cmd::run(action, cli.config.as_deref()),
        Command::ProbeDevice { device } => {
            match settings(cli.config.as_deref(), Overrides { device, ..Overrides::default() })
                .and_then(|settings| settings.require_device().map(str::to_string))
            {
                Ok(device) => {
                    let reserver = probe_reserver(cli.config.as_deref());
                    tokio::task::spawn_blocking(move || alsa::probe_device(&device, reserver))
                        .await
                        .map_err(|error| anyhow::anyhow!("the probe stopped unexpectedly: {error}"))
                        .and_then(|result| result)
                }
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
    let sinks = sinks_for(settings)?;
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
    player::run(queue, sinks, interactive, player_options(settings)).await
}

async fn run_play(
    track_ids: &[String],
    settings: &Settings,
    save_mp4: Option<&Path>,
    interactive: bool,
    shuffle: bool,
    repeat: Repeat,
) -> Result<()> {
    let sinks = sinks_for(settings)?;
    let store = auth::open_store(settings.session_store.value, auth::Interaction::Allow)?;
    let mut client = auth::load_client(&*store).await?;
    // Only to fail now, with a clear message, if the login no longer works.
    client.refresh_access_token(false).await.context("refreshing the access token")?;
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
    player::run(queue, sinks, interactive, player_options(settings)).await
}

/// What the settings say about how the engine treats the sound card.
fn player_options(settings: &phonia_core::config::Settings) -> phonia_core::engine::Options {
    phonia_core::engine::Options {
        release_after_pause: settings.release_after_pause.value.duration(),
        ..phonia_core::engine::Options::default()
    }
}

/// The reservation to probe under, unless the settings turn reservation off.
fn probe_reserver(config_flag: Option<&std::path::Path>) -> Option<Arc<dyn phonia_core::output::reserve::DeviceReserver>> {
    let settings = settings(config_flag, Overrides::default()).ok()?;
    reserver(&settings)
}

/// `phonia devices`: the sound cards (exclusive) and the sound server's outputs (shared).
async fn run_devices() -> Result<()> {
    println!("{}\n", device::list(std::path::Path::new(device::ASOUND))?);
    match phonia_core::output::shared::pulse::outputs().await {
        Ok(outputs) => println!("{}", phonia_core::output::shared::pulse::format_outputs(&outputs)),
        Err(error) => println!("PipeWire outputs (shared mode): not available ({error:#})."),
    }
    Ok(())
}

/// The output to play on, as the settings choose it, printing what each sink reports when it starts.
fn sinks_for(settings: &Settings) -> Result<Arc<dyn phonia_core::output::SinkFactory>> {
    Ok(phonia_core::output::factory_for(
        &settings.output()?,
        settings.reserve.value,
        tokio::runtime::Handle::current(),
        Arc::new(|report| {
            println!();
            println!("{}", report.to_text());
        }),
    ))
}

/// Asks the desktop for the sound card through D-Bus, unless `[output] reserve` is off.
fn reserver(settings: &phonia_core::config::Settings) -> Option<Arc<dyn phonia_core::output::reserve::DeviceReserver>> {
    settings.reserve.value.then(|| {
        Arc::new(phonia_core::output::dbus::DbusReserver::new(
            tokio::runtime::Handle::current(),
            phonia_core::output::reserve::PRIORITY,
        )) as Arc<dyn phonia_core::output::reserve::DeviceReserver>
    })
}
