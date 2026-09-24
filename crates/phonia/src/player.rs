//! The terminal player: plays a queue through the playback engine, shows progress and,
//! optionally, reads commands from the keyboard.

use anyhow::{Result, anyhow};
use phonia_core::control::Controller;
use phonia_core::engine::{self, Command, Engine, EndReason, Event, SeekTarget, State, TrackSupplier};
use phonia_core::output::alsa::AlsaSinkFactory;
use phonia_core::queue::{ItemId, Queue, QueueSnapshot, Repeat};
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
  l  list the queue              z  shuffle on/off  x  repeat off/all/one
  d <n>  remove entry n          j <n>  jump to entry n
  o  pause and give the DAC back (p takes it again)
  q  quit                        ?  this help";

/// What the user asked for on the keyboard.
#[derive(Debug, PartialEq)]
enum Key {
    TogglePause,
    Next,
    Previous,
    Forward,
    Rewind,
    /// Pause and hand the sound card back to the desktop.
    Release,
    SeekTo(Duration),
    List,
    ToggleShuffle,
    CycleRepeat,
    /// An entry, numbered from 1 as `l` lists them.
    Remove(usize),
    Jump(usize),
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
        "o" => Key::Release,
        "q" => Key::Quit,
        "l" => Key::List,
        "z" => Key::ToggleShuffle,
        "x" => Key::CycleRepeat,
        "?" | "h" => Key::Help,
        _ => parse_argument_key(line).unwrap_or_else(|| Key::Unknown(line.to_string())),
    }
}

/// The keys that take an argument: `s <seconds>`, `d <n>` and `j <n>`.
fn parse_argument_key(line: &str) -> Option<Key> {
    let entry = |rest: &str| rest.trim().parse::<usize>().ok().filter(|n| *n >= 1);
    if let Some(rest) = line.strip_prefix('s') {
        let seconds = rest.trim().parse::<f64>().ok()?;
        return Duration::try_from_secs_f64(seconds).ok().map(Key::SeekTo);
    }
    if let Some(rest) = line.strip_prefix('d') {
        return entry(rest).map(Key::Remove);
    }
    line.strip_prefix('j').and_then(entry).map(Key::Jump)
}

/// The engine command a key stands for, if it stands for one.
fn command_for(key: &Key) -> Option<Command> {
    Some(match key {
        Key::TogglePause => Command::TogglePause,
        Key::Next => Command::Next,
        Key::Previous => Command::Previous,
        Key::Forward => Command::Seek(SeekTarget::Forward(SEEK_STEP)),
        Key::Rewind => Command::Seek(SeekTarget::Backward(SEEK_STEP)),
        Key::Release => Command::Release,
        Key::SeekTo(at) => Command::Seek(SeekTarget::Absolute(*at)),
        Key::List
        | Key::ToggleShuffle
        | Key::CycleRepeat
        | Key::Remove(_)
        | Key::Jump(_)
        | Key::Help
        | Key::Quit
        | Key::Unknown(_) => return None,
    })
}

fn repeat_name(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Off => "off",
        Repeat::All => "all",
        Repeat::One => "one",
    }
}

/// `off` -> `all` -> `one` -> `off`, the order most players cycle in.
fn next_repeat(repeat: Repeat) -> Repeat {
    match repeat {
        Repeat::Off => Repeat::All,
        Repeat::All => Repeat::One,
        Repeat::One => Repeat::Off,
    }
}

/// The queue as `l` shows it, entries numbered from 1 and the current one marked.
fn format_queue(queue: &QueueSnapshot) -> String {
    let mut text = format!(
        "Queue ({} entries, shuffle {}, repeat {}):",
        queue.items.len(),
        if queue.shuffle { "on" } else { "off" },
        repeat_name(queue.repeat)
    );
    for (index, item) in queue.items.iter().enumerate() {
        let marker = if queue.current == Some(item.id) { '>' } else { ' ' };
        let name = item.track.title.as_deref().unwrap_or(&item.track.source.0);
        text.push_str(&format!("\n {marker} {:>3}. {name}", index + 1));
    }
    text
}

/// The entry a key's number refers to.
fn entry_at(queue: &QueueSnapshot, number: usize) -> Option<ItemId> {
    queue.items.get(number.checked_sub(1)?).map(|item| item.id)
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

/// Plays the queue from its start until it is exhausted or the user quits. Ctrl+C stops
/// playback and releases the device; a second one exits at once.
pub async fn run(queue: Arc<Queue>, device: &str, interactive: bool, options: engine::Options) -> Result<()> {
    let sinks = Arc::new(AlsaSinkFactory::new(device).on_report(Arc::new(|report| {
        println!();
        println!("{}", report.to_text());
    })));
    let supplier: Arc<dyn TrackSupplier> = queue.clone();
    let engine = Engine::spawn_with_options(tokio::runtime::Handle::current(), sinks, supplier, options)?;
    let controller = Controller::new(engine, queue.clone());
    let mut events = controller.subscribe_events();
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut keys = interactive.then(read_keys);

    if interactive {
        println!("{HELP}");
    }
    controller.send(Command::Play(None))?;

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
                        let _ = controller.send(Command::Stop);
                    }
                    Key::Help => console.line(HELP),
                    Key::Unknown(text) => console.line(format!("Unknown command '{text}' (? for help)")),
                    Key::List => console.line(format_queue(&queue.snapshot())),
                    Key::ToggleShuffle => {
                        let shuffle = !queue.snapshot().shuffle;
                        queue.set_shuffle(shuffle);
                        console.line(format!("Shuffle {}", if shuffle { "on" } else { "off" }));
                    }
                    Key::CycleRepeat => {
                        let repeat = next_repeat(queue.snapshot().repeat);
                        queue.set_repeat(repeat);
                        console.line(format!("Repeat {}", repeat_name(repeat)));
                    }
                    Key::Remove(number) => remove_entry(&controller, number, &mut console),
                    Key::Jump(number) => match entry_at(&queue.snapshot(), number) {
                        Some(id) => {
                            let _ = controller.play_item(id);
                        }
                        None => console.line(format!("There is no entry {number}")),
                    },
                    key => {
                        if let Some(command) = command_for(&key) {
                            let _ = controller.send(command);
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
                let _ = controller.send(Command::Stop);
            }
        }
    }

    console.finish();
    controller.shutdown();
    error.map_or(Ok(()), |message| Err(anyhow!(message)))
}

/// Takes entry `number` out of the queue (the controller skips ahead if it was the one playing).
fn remove_entry(controller: &Controller, number: usize, console: &mut Console) {
    match entry_at(&controller.snapshot(), number) {
        Some(id) => {
            controller.remove(&[id]);
            console.line(format!("Removed entry {number}"));
        }
        None => console.line(format!("There is no entry {number}")),
    }
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
        Event::OutputReleased { by: Some(by), .. } => console.line(format!("DAC released to {by}")),
        Event::OutputReleased { by: None, .. } => console.line("DAC released"),
        Event::OutputAcquired => console.line("DAC taken again"),
        Event::QueueExhausted => console.line("End of the queue."),
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
        assert_eq!(parse_key("o"), Key::Release);
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
    fn queue_keys() {
        assert_eq!(parse_key("l"), Key::List);
        assert_eq!(parse_key("z"), Key::ToggleShuffle);
        assert_eq!(parse_key("x"), Key::CycleRepeat);
        assert_eq!(parse_key("d 3"), Key::Remove(3));
        assert_eq!(parse_key("d3"), Key::Remove(3));
        assert_eq!(parse_key("j 12"), Key::Jump(12));
    }

    #[test]
    fn nonsense_is_reported_not_guessed() {
        for bad in ["s", "s abc", "s -5", "y", "pp", "seek 10", "d", "d 0", "d -1", "d x", "j", "j 0", "j 1.5"] {
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

    fn snapshot(titles: &[&str], current: Option<usize>, shuffle: bool, repeat: Repeat) -> QueueSnapshot {
        use phonia_core::engine::TrackRef;
        use phonia_core::queue::{QueueItem, QueueTrack};
        let items: Vec<QueueItem> = titles
            .iter()
            .enumerate()
            .map(|(index, title)| QueueItem {
                id: ItemId(index as u64 + 1),
                track: QueueTrack { source: TrackRef(format!("/music/{title}.flac")), title: Some(title.to_string()), duration: None },
            })
            .collect();
        QueueSnapshot {
            version: 1,
            order: items.iter().map(|item| item.id).collect(),
            current: current.map(|index| items[index].id),
            items,
            shuffle,
            repeat,
        }
    }

    #[test]
    fn the_queue_listing_numbers_entries_and_marks_the_current_one() {
        let text = format_queue(&snapshot(&["one", "two", "three"], Some(1), true, Repeat::All));
        assert_eq!(
            text,
            "Queue (3 entries, shuffle on, repeat all):\n     1. one\n >   2. two\n     3. three"
        );
    }

    #[test]
    fn an_empty_queue_lists_only_its_header() {
        assert_eq!(format_queue(&snapshot(&[], None, false, Repeat::Off)), "Queue (0 entries, shuffle off, repeat off):");
    }

    #[test]
    fn an_entry_without_a_title_is_listed_by_its_source() {
        let mut queue = snapshot(&["x"], None, false, Repeat::Off);
        queue.items[0].track.title = None;
        assert!(format_queue(&queue).ends_with("1. /music/x.flac"));
    }

    #[test]
    fn entries_are_numbered_from_one() {
        let queue = snapshot(&["a", "b"], None, false, Repeat::Off);
        assert_eq!(entry_at(&queue, 1), Some(ItemId(1)));
        assert_eq!(entry_at(&queue, 2), Some(ItemId(2)));
        assert_eq!(entry_at(&queue, 3), None);
        assert_eq!(entry_at(&queue, 0), None);
    }

    #[test]
    fn repeat_cycles_off_all_one() {
        assert_eq!(next_repeat(Repeat::Off), Repeat::All);
        assert_eq!(next_repeat(Repeat::All), Repeat::One);
        assert_eq!(next_repeat(Repeat::One), Repeat::Off);
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
