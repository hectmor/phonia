use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use phonia_core::output::alsa::{self, AlsaSink};
use phonia_core::{auth, decode, tidal};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tidlers::client::models::playback::AudioQuality;

/// phonia -- fase 0: spike de CLI que valida login PKCE -> playbackinfo HiRes -> segmentos DASH
/// -> decodificación FLAC -> salida ALSA bit-perfect a un DAC USB.
#[derive(Parser)]
#[command(name = "phonia", about = "Reproductor TIDAL hi-fi bit-perfect (fase 0)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inicia sesión en TIDAL mediante PKCE (necesario para poder recibir HI_RES_LOSSLESS).
    Login,
    /// Descarga y reproduce una pista de TIDAL por su ID.
    Play {
        track_id: String,
        #[arg(long, default_value = "hw:1,0")]
        device: String,
        #[arg(long, value_enum, default_value_t = Quality::Hires)]
        quality: Quality,
        /// Si se indica, guarda los bytes descargados (fMP4/DASH o el manifiesto JSON) en esta ruta.
        #[arg(long)]
        save_mp4: Option<PathBuf>,
    },
    /// Decodifica y reproduce un archivo local (FLAC o fMP4) por la misma ruta de salida ALSA,
    /// para poder probar la salida sin depender de TIDAL.
    PlayFile {
        path: PathBuf,
        #[arg(long, default_value = "hw:1,0")]
        device: String,
    },
    /// Lista qué formatos y frecuencias acepta el dispositivo ALSA indicado, sin reproducir nada.
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
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Login => auth::login().await,
        Command::Play { track_id, device, quality, save_mp4 } => {
            run_play(&track_id, &device, quality.into(), save_mp4.as_deref()).await
        }
        Command::PlayFile { path, device } => run_play_file(&path, &device).await,
        Command::ProbeDevice { device } => alsa::probe_device(&device),
    };

    if let Err(e) = &result {
        eprintln!("Error: {e:#}");
    }

    result
}

async fn run_play(
    track_id: &str,
    device: &str,
    quality: AudioQuality,
    save_mp4: Option<&Path>,
) -> Result<()> {
    let client = auth::load_client().await?;
    let http = tidal::build_http_client()?;

    println!("Consultando playbackinfo para la pista {track_id}...");
    let info = tidal::fetch_playback_info(&http, &client, track_id, quality).await?;
    tidal::print_playback_info(&info);

    let (bytes, extension) = match &info.manifest {
        tidal::ManifestKind::Json { url, .. } => {
            println!("Descargando audio...");
            (tidal::download_json_manifest(&http, url).await?, None)
        }
        tidal::ManifestKind::Dash(dash) => (tidal::download_dash(&http, dash).await?, Some("mp4")),
    };

    if let Some(path) = save_mp4 {
        std::fs::write(path, &bytes).with_context(|| format!("guardando en {path:?}"))?;
        println!("Guardado en {path:?} ({} bytes)", bytes.len());
    }

    play_source(std::io::Cursor::new(bytes), extension, device.to_string()).await
}

async fn run_play_file(path: &Path, device: &str) -> Result<()> {
    let extension = path.extension().and_then(|e| e.to_str()).map(str::to_string);
    let file = std::fs::File::open(path).with_context(|| format!("abriendo {path:?}"))?;
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
                println!("\nSeñal de interrupción recibida, deteniendo la reproducción...");
                stop.store(true, Ordering::SeqCst);
            }
        });
    }

    let extension = extension.map(str::to_string);

    tokio::task::spawn_blocking(move || -> Result<()> {
        let decoder = decode::Decoder::open(source, extension.as_deref())
            .context("abriendo el decodificador")?;
        let spec = decoder.spec();
        println!(
            "Fuente: {} bits / {} Hz / {} canal(es)",
            spec.bits_per_sample, spec.sample_rate, spec.channels
        );

        let mut sink =
            AlsaSink::open(&device, spec, None, stop.clone()).context("abriendo la salida ALSA")?;

        decoder.run(|samples| sink.write_chunk(samples))?;

        sink.finish()
    })
    .await
    .context("la tarea de decodificación/reproducción entró en pánico")??;

    Ok(())
}
