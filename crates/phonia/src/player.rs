//! The terminal player: runs the playback engine, shows progress and, optionally, reads
//! commands from the keyboard.

use anyhow::{Result, anyhow};
use phonia_core::engine::{Command, Engine, EndReason, Event, SeekTarget, State, TrackSupplier};
use phonia_core::output::alsa::AlsaSinkFactory;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

/// How far `f` and `r` move.
const SEEK_STEP: Duration = Duration::from_secs(10);

const HELP: &str = "\
Commands (type one and press Enter):
  p or Enter  pause / resume     n  next track      b  previous track
  f  forward 10 s                r  back 10 s       s <seconds>  seek to a position
  q  quit                        ?  this help";

/// What the user asked for on the keyboard.
#[derive(Debug, PartialEq)]
enum Key {
    TogglePause,
    Next,
    Previous,
    Forward,
    Rewind,
    SeekTo(Duration),
    Help,
    Quit,
    Unknown(String),
}

fn parse_key(line: &str) -> Key {
    let line = line.trim();
    match line {
        "" | "p" => Key::TogglePause,
        "n" => Key::Next,
        "b" => Key::Previous,
        "f" => Key::Forward,
        "r" => Key::Rewind,
        "q" => Key::Quit,
        "?" | "h" => Key::Help,
        _ => line
            .strip_prefix('s')
            .and_then(|seconds| seconds.trim().parse::<f64>().ok())
            .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
            .map_or_else(|| Key::Unknown(line.to_string()), Key::SeekTo),
    }
}

/// The engine command a key stands for, if it stands for one.
fn command_for(key: &Key) -> Option<Command> {
    Some(match key {
        Key::TogglePause => Command::TogglePause,
        Key::Next => Command::Next,
        Key::Previous => Command::Previous,
        Key::Forward => Command::Seek(SeekTarget::Forward(SEEK_STEP)),
        Key::Rewind => Command::Seek(SeekTarget::Backward(SEEK_STEP)),
        Key::SeekTo(at) => Command::Seek(SeekTarget::Absolute(*at)),
        Key::Help | Key::Quit | Key::Unknown(_) => return None,
    })
}

fn format_progress(elapsed_secs: f64, total_secs: Option<f64>) -> String {
    match total_secs {
        Some(total) => format!("{:>6.1}s / {:>6.1}s", elapsed_secs, total),
        None => format!("{:>6.1}s", elapsed_secs),
    }
}

/// Stdin lines, read on their own thread because reading blocks.
fn read_keys() -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

async fn next_line(keys: &mut Option<mpsc::UnboundedReceiver<String>>) -> Option<String> {
    match keys {
        Some(keys) => keys.recv().await,
        None => std::future::pending().await,
    }
}

/// What has been printed, so that a line of text never lands in the middle of the `\r` progress
/// line.
#[derive(Default)]
struct Console {
    progress_shown: bool,
}

impl Console {
    fn progress(&mut self, position: Duration, duration: Option<Duration>) {
        print!("\r{}", format_progress(position.as_secs_f64(), duration.map(|d| d.as_secs_f64())));
        let _ = std::io::stdout().flush();
        self.progress_shown = true;
    }

    fn line(&mut self, text: impl std::fmt::Display) {
        if std::mem::take(&mut self.progress_shown) {
            println!();
        }
        println!("{text}");
    }

    /// Ends the progress line, if there is one.
    fn finish(&mut self) {
        if std::mem::take(&mut self.progress_shown) {
            println!();
        }
    }
}

/// Plays whatever `supplier` offers, starting with `start`, until it is over or interrupted.
/// Ctrl+C stops playback and releases the device; a second one exits at once.
pub async fn run(supplier: Arc<dyn TrackSupplier>, device: &str, start: Command, interactive: bool) -> Result<()> {
    let sinks = Arc::new(AlsaSinkFactory::new(device).print_diagnostics());
    let engine = Engine::spawn(tokio::runtime::Handle::current(), sinks, supplier)?;
    let mut events = engine.subscribe();
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut keys = interactive.then(read_keys);

    if interactive {
        println!("{HELP}");
    }
    engine.send(start)?;

    let mut console = Console::default();
    let mut error = None;
    let mut stopping = false;
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => {
                    if handle_event(event, interactive, &mut console, &mut error) {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            line = next_line(&mut keys) => match line {
                None => keys = None, // stdin closed: keep playing
                Some(line) => match parse_key(&line) {
                    Key::Quit => {
                        stopping = true;
                        let _ = engine.send(Command::Stop);
                    }
                    Key::Help => console.line(HELP),
                    Key::Unknown(text) => console.line(format!("Unknown command '{text}' (? for help)")),
                    key => {
                        if let Some(command) = command_for(&key) {
                            let _ = engine.send(command);
                        }
                    }
                },
            },
            _ = sigint.recv() => {
                if stopping {
                    std::process::exit(130);
                }
                console.line("Interrupt received, stopping playback...");
                stopping = true;
                let _ = engine.send(Command::Stop);
            }
        }
    }

    console.finish();
    engine.shutdown();
    error.map_or(Ok(()), |message| Err(anyhow!(message)))
}

/// Reacts to one engine event. Returns `true` once the engine has stopped, which is how every
/// run ends: the queue ran out, the user quit, or something failed.
fn handle_event(event: Event, interactive: bool, console: &mut Console, error: &mut Option<String>) -> bool {
    match event {
        Event::Position { position, duration } => console.progress(position, duration),
        Event::TrackStarted { meta, .. } => {
            console.line(format!("Playing: {}", meta.title.as_deref().unwrap_or(&meta.track.0)));
        }
        Event::TrackEnded { reason: EndReason::Completed, .. } => console.finish(),
        Event::StateChanged(State::Stopped) => return true,
        Event::StateChanged(state) if interactive => console.line(format!("[{state:?}]")),
        Event::Seeked { position } if interactive => console.line(format!("Seeked to {:.1}s", position.as_secs_f64())),
        Event::SeekRejected { reason } => console.line(format!("Seek rejected: {reason}")),
        Event::Error { message } => {
            error.get_or_insert(message);
        }
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_keys() {
        assert_eq!(parse_key("p"), Key::TogglePause);
        assert_eq!(parse_key(""), Key::TogglePause, "Enter alone toggles pause");
        assert_eq!(parse_key("  n \n"), Key::Next);
        assert_eq!(parse_key("b"), Key::Previous);
        assert_eq!(parse_key("f"), Key::Forward);
        assert_eq!(parse_key("r"), Key::Rewind);
        assert_eq!(parse_key("q"), Key::Quit);
        assert_eq!(parse_key("?"), Key::Help);
    }

    #[test]
    fn seek_to_takes_seconds_with_or_without_a_space() {
        assert_eq!(parse_key("s 90"), Key::SeekTo(Duration::from_secs(90)));
        assert_eq!(parse_key("s90.5"), Key::SeekTo(Duration::from_millis(90_500)));
        assert_eq!(parse_key("s 0"), Key::SeekTo(Duration::ZERO));
    }

    #[test]
    fn nonsense_is_reported_not_guessed() {
        for bad in ["s", "s abc", "s -5", "x", "pp", "seek 10"] {
            assert_eq!(parse_key(bad), Key::Unknown(bad.to_string()), "{bad:?}");
        }
    }

    #[test]
    fn keys_map_to_engine_commands() {
        assert_eq!(command_for(&Key::TogglePause), Some(Command::TogglePause));
        assert_eq!(command_for(&Key::Forward), Some(Command::Seek(SeekTarget::Forward(SEEK_STEP))));
        assert_eq!(command_for(&Key::Rewind), Some(Command::Seek(SeekTarget::Backward(SEEK_STEP))));
        assert_eq!(
            command_for(&Key::SeekTo(Duration::from_secs(3))),
            Some(Command::Seek(SeekTarget::Absolute(Duration::from_secs(3))))
        );
        assert_eq!(command_for(&Key::Quit), None);
        assert_eq!(command_for(&Key::Help), None);
    }

    #[test]
    fn format_progress_without_total() {
        assert_eq!(format_progress(12.3, None), "  12.3s");
    }

    #[test]
    fn format_progress_with_total() {
        assert_eq!(format_progress(12.3, Some(205.1)), "  12.3s /  205.1s");
    }
}
