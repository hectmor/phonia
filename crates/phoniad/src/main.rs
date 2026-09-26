//! `phoniad`: the phonia daemon.
//!
//! Plays a queue through the bit-perfect ALSA output and lets clients (`phonia ctl`, a TUI, ...)
//! drive it over a Unix socket.

use anyhow::{Context, Result};
use clap::Parser;
use phonia_core::config::{self, Overrides, Quality};
use phonia_core::diag::{self, Level};
use phonia_core::engine;
use phonia_core::openers::{DispatchOpener, TidalOpener};
use phonia_core::{auth, tidal};
use phoniad::daemon::{Daemon, DaemonParts, wait_for_shutdown};
use phoniad::outputs::{Build, Outputs};
use phoniad::{server, socket};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(
    name = "phoniad",
    about = "The phonia daemon: plays a queue and serves clients over a Unix socket"
)]
struct Args {
    /// The config file to read instead of `~/.config/phonia/config.toml` (also `PHONIA_CONFIG`).
    /// It is read once, at startup: restart the daemon to apply a change.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// The ALSA device: hw:N,D, a card id such as hw:DS2,0, or `auto`. Default: [output] device in
    /// the config file (`phonia devices` lists the cards).
    #[arg(long)]
    device: Option<String>,
    /// The output to start on: exclusive:<card>, shared:default or shared:<name> (see `phonia
    /// devices`). Default: [output] in the config file. Clients can switch it later.
    #[arg(long, value_name = "ID", conflicts_with = "device")]
    output: Option<String>,
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
    let mut overrides = Overrides {
        device: args.device,
        max_quality: args.quality,
        socket: args.socket,
        verbose: args.verbose.then_some(true),
        ..Overrides::default()
    };
    if let Some(id) = &args.output {
        overrides = overrides.with_output_id(id)?;
    }
    let settings = config::resolve(overrides, &loaded.file);
    let output = settings.output()?;

    // A daemon has no terminal to talk to: what the library would print goes nowhere unless asked.
    diag::set_level(if settings.verbose.value {
        Level::All
    } else {
        Level::Silent
    });

    // The login is read when TIDAL is first used, not now: the daemon may start before the network
    // is up, or before `phonia login`, and should then work without a restart.
    // The startup check must not open a keyring prompt with nobody there; the store that TIDAL
    // uses later may, since by then someone asked for a TIDAL track.
    let quiet = auth::open_store(settings.session_store.value, auth::Interaction::Never)?;
    match auth::load_client(&*quiet).await {
        Ok(_) => {}
        Err(error) => eprintln!(
            "phoniad: no TIDAL session yet ({error:#}); local files play, TIDAL will once you log in"
        ),
    }
    let store = auth::open_store(settings.session_store.value, auth::Interaction::Allow)?;
    let tidal_opener = TidalOpener::from_store(
        tidal::build_http_client()?,
        store,
        settings.max_quality.value.into(),
    );
    let opener = Arc::new(DispatchOpener::new(Some(tidal_opener)));

    let (report_tx, reports) = mpsc::unbounded_channel();
    // In exclusive mode this asks WirePlumber or PulseAudio for the card before opening it, and
    // answers them when they ask for it back; in shared mode it plays through the sound server.
    // The same recipe builds the sink for any output a client later switches to.
    let reserve = settings.reserve.value;
    let build: Build = Arc::new(move |spec| {
        let reports = report_tx.clone();
        phonia_core::output::factory_for(
            spec,
            reserve,
            tokio::runtime::Handle::current(),
            // Called on the audio thread: an unbounded send never blocks.
            Arc::new(move |report| {
                let _ = reports.send(report);
            }),
        )
    });
    let sinks = build(&output);
    let daemon = Daemon::start(DaemonParts {
        sinks,
        outputs: Outputs::new(output.clone(), build),
        opener,
        reports,
        engine: engine::Options {
            release_after_pause: settings.release_after_pause.value.duration(),
            ..engine::Options::default()
        },
    })?;

    daemon.refresh_route().await;

    // Tells clients when outputs come and go, so a list of them can be kept up to date. Without a
    // sound server there is nothing to watch.
    let _outputs_watch = phonia_core::output::shared::pulse::watch_outputs({
        let daemon = daemon.clone();
        move || daemon.outputs_changed()
    })
    .ok();

    let path = settings.socket_path(phonia_ipc::socket::default_socket_path);
    let (listener, _guard) = socket::bind(&path).await?;
    match &loaded.path {
        Some(config) => eprintln!("phoniad: config {}", config.display()),
        None => eprintln!("phoniad: no config file, using the defaults"),
    }
    eprintln!(
        "phoniad: listening on {} ({})",
        path.display(),
        output.describe()
    );

    let mut interrupt = signal(SignalKind::interrupt()).context("installing the SIGINT handler")?;
    let mut terminate =
        signal(SignalKind::terminate()).context("installing the SIGTERM handler")?;
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
