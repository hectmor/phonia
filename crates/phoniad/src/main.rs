//! `phoniad`: the phonia daemon.
//!
//! Plays a queue through the bit-perfect ALSA output and lets clients (`phonia ctl`, a TUI, ...)
//! drive it over a Unix socket.

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use phoniad::daemon::{Daemon, DaemonParts, wait_for_shutdown};
use phoniad::{server, socket};
use phonia_core::diag::{self, Level};
use phonia_core::openers::{DispatchOpener, TidalOpener};
use phonia_core::output::alsa::AlsaSinkFactory;
use phonia_core::{auth, tidal};
use std::path::PathBuf;
use std::sync::Arc;
use tidlers::client::models::playback::AudioQuality;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "phoniad", about = "The phonia daemon: plays a queue and serves clients over a Unix socket")]
struct Args {
    /// The ALSA device to play on. Use a raw hardware device (`hw:N,D`) for bit-perfect output.
    #[arg(long, default_value = "hw:1,0")]
    device: String,
    /// Where to listen (default: `$XDG_RUNTIME_DIR/phonia/phoniad.sock`).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// The quality to ask TIDAL for.
    #[arg(long, value_enum, default_value_t = Quality::Hires)]
    quality: Quality,
    /// Print the library's notes and warnings to the terminal.
    #[arg(long)]
    verbose: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Quality {
    Hires,
    Lossless,
}

impl From<Quality> for AudioQuality {
    fn from(quality: Quality) -> Self {
        match quality {
            Quality::Hires => AudioQuality::HiRes,
            Quality::Lossless => AudioQuality::Lossless,
        }
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run(Args::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("phoniad: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<()> {
    // A daemon has no terminal to talk to: what the library would print goes nowhere unless asked.
    diag::set_level(if args.verbose { Level::All } else { Level::Silent });

    let tidal_opener = match auth::load_client().await {
        Ok(client) => Some(TidalOpener::new(tidal::build_http_client()?, client, args.quality.into())),
        Err(error) => {
            eprintln!("phoniad: TIDAL is not available ({error:#}); only local files will play");
            None
        }
    };
    let opener = Arc::new(DispatchOpener::new(tidal_opener));

    let (report_tx, reports) = mpsc::unbounded_channel();
    let sinks = Arc::new(AlsaSinkFactory::new(args.device.clone()).on_report(Arc::new(move |report| {
        // Called on the audio thread: an unbounded send never blocks.
        let _ = report_tx.send(report);
    })));
    let daemon = Daemon::start(DaemonParts { sinks, opener, reports })?;

    let path = args.socket.unwrap_or_else(phonia_ipc::socket::default_socket_path);
    let (listener, _guard) = socket::bind(&path).await?;
    eprintln!("phoniad: listening on {} (device {})", path.display(), args.device);

    let mut interrupt = signal(SignalKind::interrupt()).context("installing the SIGINT handler")?;
    let mut terminate = signal(SignalKind::terminate()).context("installing the SIGTERM handler")?;
    let mut stopping = daemon.shutdown_signal();
    let serving = tokio::spawn(server::serve(listener, daemon.clone()));

    tokio::select! {
        _ = interrupt.recv() => eprintln!("phoniad: interrupted, shutting down"),
        _ = terminate.recv() => eprintln!("phoniad: terminated, shutting down"),
        _ = wait_for_shutdown(&mut stopping) => eprintln!("phoniad: asked to shut down"),
    }

    daemon.request_shutdown();
    let _ = serving.await;
    // Releases the DAC. `_guard` then removes the socket file.
    daemon.stop_engine().await;
    Ok(())
}
