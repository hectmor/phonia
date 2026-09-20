mod player;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use phonia_core::engine::TrackRef;
use phonia_core::openers::{FileOpener, TidalOpener};
use phonia_core::output::alsa;
use phonia_core::queue::{Queue, QueueTrack, Repeat};
use phonia_core::{auth, tidal};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use tidlers::client::models::playback::AudioQuality;

/// phonia -- phase 0: CLI spike that validates PKCE login -> HiRes playbackinfo -> DASH segments
/// -> FLAC decoding -> bit-perfect ALSA output to a USB DAC.
#[derive(Parser)]
#[command(name = "phonia", about = "Bit-perfect TIDAL hi-fi player (phase 0)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

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
        #[arg(long, default_value = "hw:1,0")]
        device: String,
        #[arg(long, value_enum, default_value_t = Quality::Hires)]
        quality: Quality,
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
        #[arg(long, default_value = "hw:1,0")]
        device: String,
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
    /// Lists which formats and rates the given ALSA device accepts, without playing anything.
    ProbeDevice {
        #[arg(long, default_value = "hw:1,0")]
        device: String,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Quality {
    Hires,
    Lossless,
}

impl From<Quality> for AudioQuality {
    fn from(q: Quality) -> Self {
        match q {
            Quality::Hires => AudioQuality::HiRes,
            Quality::Lossless => AudioQuality::Lossless,
        }
    }
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
            run_play(&track_ids, &device, quality.into(), save_mp4.as_deref(), interactive, shuffle, repeat.into()).await
        }
        Command::PlayFile { paths, device, interactive, shuffle, repeat } => {
            let queue = Queue::new(Arc::new(FileOpener));
            queue.add(paths.iter().map(|path| QueueTrack {
                source: TrackRef(path.to_string_lossy().into_owned()),
                title: path.file_name().map(|name| name.to_string_lossy().into_owned()),
                duration: None,
            }));
            queue.set_shuffle(shuffle);
            queue.set_repeat(repeat.into());
            player::run(queue, &device, interactive).await
        }
        Command::ProbeDevice { device } => alsa::probe_device(&device),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_play(
    track_ids: &[String],
    device: &str,
    quality: AudioQuality,
    save_mp4: Option<&Path>,
    interactive: bool,
    shuffle: bool,
    repeat: Repeat,
) -> Result<()> {
    let client = auth::load_client().await?;
    let http = tidal::build_http_client()?;

    let mut opener = TidalOpener::new(http, Arc::new(client), quality).print_info();
    if let Some(path) = save_mp4 {
        let file = std::fs::File::create(path).with_context(|| format!("creating {path:?}"))?;
        println!("Saving audio to {path:?} as it streams (the first track opened)...");
        opener = opener.saving_to(file);
    }

    let queue = Queue::new(Arc::new(opener));
    queue.add(track_ids.iter().map(|id| QueueTrack {
        source: TrackRef(id.clone()),
        title: Some(id.clone()),
        duration: None,
    }));
    queue.set_shuffle(shuffle);
    queue.set_repeat(repeat);
    player::run(queue, device, interactive).await
}
