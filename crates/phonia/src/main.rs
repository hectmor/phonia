use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use phonia_core::output::alsa::{self, AlsaSink};
use phonia_core::{auth, decode, stream, tidal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    Login,
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
    },
    /// Decodes and plays a local file (FLAC or fMP4) through the same ALSA output path,
    /// to test the output without depending on TIDAL.
    PlayFile {
        path: PathBuf,
        #[arg(long, default_value = "hw:1,0")]
        device: String,
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
        Command::Login => auth::login().await,
        Command::Play { track_id, device, quality, save_mp4 } => {
            run_play(&track_id, &device, quality.into(), save_mp4.as_deref()).await
        }
        Command::PlayFile { path, device } => run_play_file(&path, &device).await,
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
) -> Result<()> {
    let client = auth::load_client().await?;
    let http = tidal::build_http_client()?;

    println!("Fetching playbackinfo for track {track_id}...");
    let info = tidal::fetch_playback_info(&http, &client, track_id, quality).await?;
    tidal::print_playback_info(&info);

    let tee = save_mp4
        .map(|path| std::fs::File::create(path).with_context(|| format!("creating {path:?}")))
        .transpose()?;
    if let Some(path) = save_mp4 {
        println!("Saving audio to {path:?} as it streams...");
    }

    let (source, extension) = match &info.manifest {
        tidal::ManifestKind::Json { url, .. } => (stream::open_url(&http, url, tee), None),
        tidal::ManifestKind::Dash(dash) => (stream::open_dash(&http, dash, tee), Some("mp4")),
    };

    play_source(source, extension, device.to_string()).await
}

async fn run_play_file(path: &Path, device: &str) -> Result<()> {
    let extension = path.extension().and_then(|e| e.to_str()).map(str::to_string);
    let file = std::fs::File::open(path).with_context(|| format!("opening {path:?}"))?;
    play_source(file, extension.as_deref(), device.to_string()).await
}

/// Shared decode+play path for both `play` and `play-file`: decodes `source` and streams the
/// result to the bit-perfect ALSA sink, watching for Ctrl+C between writes.
async fn play_source(
    source: impl symphonia::core::io::MediaSource + 'static,
    extension: Option<&str>,
    device: String,
) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                println!("\nInterrupt signal received, stopping playback...");
                stop.store(true, Ordering::SeqCst);
            }
        });
    }

    let extension = extension.map(str::to_string);

    tokio::task::spawn_blocking(move || -> Result<()> {
        let decoder = decode::Decoder::open(source, extension.as_deref())
            .context("opening the decoder")?;
        let spec = decoder.spec();
        println!(
            "Source: {} bits / {} Hz / {} channel(s)",
            spec.bits_per_sample, spec.sample_rate, spec.channels
        );

        let mut sink =
            AlsaSink::open(&device, spec, None, stop.clone()).context("opening the ALSA output")?;

        decoder.run(|samples| sink.write_chunk(samples))?;

        sink.finish()
    })
    .await
    .context("the decode/playback task panicked")??;

    Ok(())
}
