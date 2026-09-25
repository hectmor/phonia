//! `phoniad`: the phonia daemon.
//!
//! Plays a queue through the bit-perfect ALSA output and lets clients (`phonia ctl`, a TUI, ...)
//! drive it over a Unix socket.

use anyhow::{Context, Result};
use clap::Parser;
use phoniad::daemon::{Daemon, DaemonParts, wait_for_shutdown};
use phoniad::{server, socket};
use phonia_core::config::{self, Overrides, Quality};
use phonia_core::diag::{self, Level};
use phonia_core::engine;
use phonia_core::openers::{DispatchOpener, TidalOpener};
use phonia_core::{auth, tidal};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "phoniad", about = "The phonia daemon: plays a queue and serves clients over a Unix socket")]
struct Args {
    /// The config file to read instead of `~/.config/phonia/config.toml` (also `PHONIA_CONFIG`).
    /// It is read once, at startup: restart the daemon to apply a change.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// The ALSA device: hw:N,D, a card id such as hw:DS2,0, or `auto`. Default: [output] device in
    /// the config file (`phonia devices` lists the cards).
    #[arg(long)]
    device: Option<String>,
    /// Where to listen. Default: [daemon] socket in the config file, else
    /// `$XDG_RUNTIME_DIR/phonia/phoniad.sock`.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// The highest quality to ask TIDAL for: hires or lossless. Default: [tidal] max_quality in the
    /// config file, else hires.
    #[arg(long, value_name = "hires|lossless")]
    quality: Option<Quality>,
    /// Print the library's notes and warnings to the terminal.
    #[arg(long)]
    verbose: bool,
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
    let source = config::discover_from_env(args.config.as_deref());
    let loaded = config::load(source.as_ref())?;
    let settings = config::resolve(
        Overrides {
            device: args.device,
            max_quality: args.quality,
            socket: args.socket,
            verbose: args.verbose.then_some(true),
        },
        &loaded.file,
    );
    let output = settings.output()?;

    // A daemon has no terminal to talk to: what the library would print goes nowhere unless asked.
    diag::set_level(if settings.verbose.value { Level::All } else { Level::Silent });

    // The login is read when TIDAL is first used, not now: the daemon may start before the network
    // is up, or before `phonia login`, and should then work without a restart.
    // The startup check must not open a keyring prompt with nobody there; the store that TIDAL
    // uses later may, since by then someone asked for a TIDAL track.
    let quiet = auth::open_store(settings.session_store.value, auth::Interaction::Never)?;
    match auth::load_client(&*quiet).await {
        Ok(_) => {}
        Err(error) => eprintln!("phoniad: no TIDAL session yet ({error:#}); local files play, TIDAL will once you log in"),
    }
    let store = auth::open_store(settings.session_store.value, auth::Interaction::Allow)?;
    let tidal_opener =
        TidalOpener::from_store(tidal::build_http_client()?, store, settings.max_quality.value.into());
    let opener = Arc::new(DispatchOpener::new(Some(tidal_opener)));

    let (report_tx, reports) = mpsc::unbounded_channel();
    // In exclusive mode this asks WirePlumber or PulseAudio for the card before opening it, and
    // answers them when they ask for it back; in shared mode it plays through the sound server.
    let sinks = phonia_core::output::factory_for(
        &output,
        settings.reserve.value,
        tokio::runtime::Handle::current(),
        Arc::new(move |report| {
            // Called on the audio thread: an unbounded send never blocks.
            let _ = report_tx.send(report);
        }),
    );
    let daemon = Daemon::start(DaemonParts {
        sinks,
        opener,
        reports,
        engine: engine::Options {
            release_after_pause: settings.release_after_pause.value.duration(),
            ..engine::Options::default()
        },
    })?;

    let path = settings.socket_path(phonia_ipc::socket::default_socket_path);
    let (listener, _guard) = socket::bind(&path).await?;
    match &loaded.path {
        Some(config) => eprintln!("phoniad: config {}", config.display()),
        None => eprintln!("phoniad: no config file, using the defaults"),
    }
    eprintln!("phoniad: listening on {} ({})", path.display(), output.describe());

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
