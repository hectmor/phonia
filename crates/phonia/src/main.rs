mod player;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use phonia_core::engine::{Command as EngineCommand, TrackRef};
use phonia_core::output::alsa;
use phonia_core::suppliers::{FileSupplier, TidalSupplier};
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
    /// Downloads and plays a TIDAL track by its ID.
    Play {
        track_id: String,
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
    },
    /// Lists which formats and rates the given ALSA device accepts, without playing anything.
    ProbeDevice {
        #[arg(long, default_value = "hw:1,0")]
        device: String,
    },
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
        Command::Play { track_id, device, quality, save_mp4, interactive } => {
            run_play(&track_id, &device, quality.into(), save_mp4.as_deref(), interactive).await
        }
        Command::PlayFile { paths, device, interactive } => {
            player::run(FileSupplier::new(paths), &device, EngineCommand::Play(None), interactive).await
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
    track_id: &str,
    device: &str,
    quality: AudioQuality,
    save_mp4: Option<&Path>,
    interactive: bool,
) -> Result<()> {
    let client = auth::load_client().await?;
    let http = tidal::build_http_client()?;

    let mut supplier = TidalSupplier::new(http, Arc::new(client), quality).print_info();
    if let Some(path) = save_mp4 {
        let file = std::fs::File::create(path).with_context(|| format!("creating {path:?}"))?;
        println!("Saving audio to {path:?} as it streams...");
        supplier = supplier.saving_to(file);
    }

    let start = EngineCommand::Play(Some(TrackRef(track_id.to_string())));
    player::run(Arc::new(supplier), device, start, interactive).await
}
