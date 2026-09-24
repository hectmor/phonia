//! `phonia ctl`: drives a running `phoniad` over its socket.

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use phonia_ipc::{
    AddAt, Client, ClientError, ClientInfo, Event, ItemId, NewTrack, Payload, Queue, Repeat, Request, SeekTarget, State,
    Status,
};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct CtlArgs {
    /// The daemon's socket. Default: [daemon] socket in the config file, else
    /// `$XDG_RUNTIME_DIR/phonia/phoniad.sock`.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// Print the daemon's answers as the raw protocol JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: CtlCommand,
}

#[derive(Subcommand)]
pub enum CtlCommand {
    /// Shows what is playing.
    Status,
    /// Plays entry N of the queue (as `queue list` numbers them), or starts the queue.
    Play { entry: Option<usize> },
    Pause,
    Resume,
    /// Pauses if playing, resumes if paused.
    Toggle,
    Next,
    Prev,
    /// Stops playback and releases the audio device; the queue is kept.
    Stop,
    /// Seeks: `90` goes to 1:30, `+10` skips ahead 10 s, `-10` goes back 10 s.
    Seek {
        #[arg(allow_hyphen_values = true)]
        position: String,
    },
    /// Shows and edits the queue.
    Queue {
        #[command(subcommand)]
        action: QueueAction,
    },
    Shuffle { mode: Switch },
    Repeat { mode: RepeatMode },
    /// Prints events as they happen, until interrupted.
    Watch,
    /// Stops the daemon.
    Shutdown,
}

#[derive(Subcommand)]
pub enum QueueAction {
    List,
    /// Adds tracks: `tidal:<id>` (or just the id), `file:/abs/path`, or a path to a file.
    Add {
        #[arg(required = true)]
        sources: Vec<String>,
        /// Put them right after the track that is playing.
        #[arg(long, conflicts_with = "at")]
        next: bool,
        /// Put them at this position (1 is first).
        #[arg(long)]
        at: Option<usize>,
    },
    /// Removes entries by position.
    Rm {
        #[arg(required = true)]
        entries: Vec<usize>,
    },
    Clear,
    /// Moves an entry to another position.
    Move { entry: usize, to: usize },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum RepeatMode {
    Off,
    One,
    All,
}

pub async fn run(args: CtlArgs, config_flag: Option<&Path>) -> Result<()> {
    let info = ClientInfo { name: "phonia-ctl".to_string(), version: env!("CARGO_PKG_VERSION").to_string() };
    let loaded = phonia_core::config::load(phonia_core::config::discover_from_env(config_flag).as_ref())?;
    let settings = phonia_core::config::resolve(
        phonia_core::config::Overrides { socket: args.socket.clone(), ..Default::default() },
        &loaded.file,
    );
    let socket = settings.socket_path(phonia_ipc::socket::default_socket_path);
    let client = Client::connect(Some(&socket), info).await.map_err(explain_connection_error)?;
    let json = args.json;

    match args.command {
        CtlCommand::Status => {
            let status = client.status().await?;
            print_payload(json, &Payload::Status(status.clone()), || format_status(&status))
        }
        CtlCommand::Play { entry } => {
            let item = match entry {
                Some(number) => Some(entry_id(&client.queue().await?, number)?),
                None => None,
            };
            ack(&client, json, Request::Play { item }).await
        }
        CtlCommand::Pause => ack(&client, json, Request::Pause).await,
        CtlCommand::Resume => ack(&client, json, Request::Resume).await,
        CtlCommand::Toggle => ack(&client, json, Request::TogglePause).await,
        CtlCommand::Next => ack(&client, json, Request::Next).await,
        CtlCommand::Prev => ack(&client, json, Request::Previous).await,
        CtlCommand::Stop => ack(&client, json, Request::Stop).await,
        CtlCommand::Seek { position } => ack(&client, json, Request::Seek { target: parse_seek(&position)? }).await,
        CtlCommand::Queue { action } => queue(&client, json, action).await,
        CtlCommand::Shuffle { mode } => {
            ack(&client, json, Request::SetShuffle { shuffle: matches!(mode, Switch::On) }).await
        }
        CtlCommand::Repeat { mode } => {
            let repeat = match mode {
                RepeatMode::Off => Repeat::Off,
                RepeatMode::One => Repeat::One,
                RepeatMode::All => Repeat::All,
            };
            ack(&client, json, Request::SetRepeat { repeat }).await
        }
        CtlCommand::Watch => watch(&client, json).await,
        CtlCommand::Shutdown => ack(&client, json, Request::Shutdown).await,
    }
}

fn explain_connection_error(error: ClientError) -> anyhow::Error {
    match error {
        ClientError::Io(error) => anyhow!("{error}. Is phoniad running? Start it with `phoniad --device hw:N,0`."),
        other => anyhow!(other),
    }
}

async fn ack(client: &Client, json: bool, request: Request) -> Result<()> {
    let payload = client.request(request).await?;
    print_payload(json, &payload, || "ok".to_string())
}

fn print_payload(json: bool, payload: &Payload, text: impl FnOnce() -> String) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(payload)?);
    } else {
        println!("{}", text());
    }
    Ok(())
}

async fn queue(client: &Client, json: bool, action: QueueAction) -> Result<()> {
    match action {
        QueueAction::List => {
            let queue = client.queue().await?;
            print_payload(json, &Payload::Queue(queue.clone()), || format_queue(&queue))
        }
        QueueAction::Add { sources, next, at } => {
            let tracks = sources
                .iter()
                .map(|source| resolve_source(source).map(|source| NewTrack { source }))
                .collect::<Result<Vec<_>>>()?;
            let at = match (next, at) {
                (true, _) => AddAt::Next,
                (false, Some(position)) => AddAt::Index { index: position.saturating_sub(1) },
                (false, None) => AddAt::End,
            };
            let payload = client.request(Request::QueueAdd { tracks, at }).await?;
            print_payload(json, &payload, || format_added(&payload))
        }
        QueueAction::Rm { entries } => {
            let queue = client.queue().await?;
            let ids = entries.iter().map(|number| entry_id(&queue, *number)).collect::<Result<Vec<_>>>()?;
            let payload = client.request(Request::QueueRemove { ids }).await?;
            print_payload(json, &payload, || match &payload {
                Payload::Removed { count } => format!("removed {count} entries"),
                _ => "ok".to_string(),
            })
        }
        QueueAction::Clear => ack(client, json, Request::QueueClear).await,
        QueueAction::Move { entry, to } => {
            let id = entry_id(&client.queue().await?, entry)?;
            ack(client, json, Request::QueueMove { id, to: to.saturating_sub(1) }).await
        }
    }
}

async fn watch(client: &Client, json: bool) -> Result<()> {
    let (snapshot, mut events) = client.subscribe().await?;
    if json {
        println!("{}", serde_json::to_string(&Payload::Snapshot { seq: snapshot.seq, status: snapshot.status, queue: snapshot.queue })?);
    } else {
        println!("{}\n{}", format_status(&snapshot.status), format_queue(&snapshot.queue));
    }
    loop {
        tokio::select! {
            event = events.next_seq() => {
                let Some((seq, event)) = event else { return Ok(()) };
                let last = event == Event::ShuttingDown;
                if json {
                    println!("{}", serde_json::to_string(&phonia_ipc::ServerMessage::Event { seq, event })?);
                } else {
                    println!("[{seq}] {}", format_event(&event));
                }
                if last {
                    return Ok(());
                }
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

// ---- pure pieces, tested below --------------------------------------------------------------

/// `90` is an absolute position, `+10` and `-10` are relative to where playback is. Seconds may
/// have decimals.
fn parse_seek(text: &str) -> Result<SeekTarget> {
    let text = text.trim();
    let (sign, digits) = match text.chars().next() {
        Some('+') => ('+', &text[1..]),
        Some('-') => ('-', &text[1..]),
        _ => (' ', text),
    };
    let seconds: f64 = digits.trim().parse().map_err(|_| anyhow!("{text:?} is not a position: use 90, +10 or -10 (seconds)"))?;
    if !seconds.is_finite() || seconds < 0.0 {
        bail!("{text:?} is not a position: use 90, +10 or -10 (seconds)");
    }
    let ms = (seconds * 1000.0).round() as u64;
    Ok(match sign {
        '+' => SeekTarget::Forward { ms },
        '-' => SeekTarget::Backward { ms },
        _ => SeekTarget::Absolute { ms },
    })
}

/// A track as the user wrote it, as a source the daemon understands. A path is made absolute here
/// (the daemon's working directory is not the user's) but not checked: the daemon knows whether
/// the file can be played, and says which tracks it refused without dropping the rest.
fn resolve_source(text: &str) -> Result<String> {
    if text.starts_with("file:") || text.starts_with("tidal:") {
        return Ok(text.to_string());
    }
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(format!("tidal:{text}"));
    }
    let path = std::path::absolute(Path::new(text)).with_context(|| format!("{text:?} is not a usable path"))?;
    phonia_ipc::source::file(&path).map_err(|reason| anyhow!(reason))
}

/// The entry at position `number` (from 1) of the queue.
fn entry_id(queue: &Queue, number: usize) -> Result<ItemId> {
    number
        .checked_sub(1)
        .and_then(|index| queue.items.get(index))
        .map(|item| item.id)
        .ok_or_else(|| anyhow!("there is no queue entry {number} (the queue has {})", queue.items.len()))
}

fn format_ms(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, seconds % 3600 / 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

fn state_name(state: State) -> &'static str {
    match state {
        State::Stopped => "stopped",
        State::Loading => "loading",
        State::Playing => "playing",
        State::Paused => "paused",
        State::Seeking => "seeking",
    }
}

fn format_status(status: &Status) -> String {
    let mut text = format!("State:    {}", state_name(status.state));
    if let Some(track) = &status.track {
        let name = track.title.as_deref().or(track.source.as_deref()).unwrap_or("?");
        text.push_str(&format!("\nTrack:    {name}"));
        let total = status.duration_ms.map(|ms| format!(" / {}", format_ms(ms))).unwrap_or_default();
        text.push_str(&format!("\nPosition: {}{total}", format_ms(status.position_ms)));
    }
    if let Some(spec) = status.spec {
        text.push_str(&format!("\nFormat:   {}-bit / {} Hz / {} ch", spec.bits_per_sample, spec.sample_rate, spec.channels));
    }
    text
}

fn format_queue(queue: &Queue) -> String {
    let repeat = match queue.repeat {
        Repeat::Off => "off",
        Repeat::One => "one",
        Repeat::All => "all",
    };
    let mut text = format!(
        "Queue ({} entries, shuffle {}, repeat {repeat}):",
        queue.items.len(),
        if queue.shuffle { "on" } else { "off" }
    );
    for (index, item) in queue.items.iter().enumerate() {
        let marker = if queue.current == Some(item.id) { '>' } else { ' ' };
        let name = item.title.as_deref().unwrap_or(&item.source);
        let length = item.duration_ms.map(|ms| format!("  [{}]", format_ms(ms))).unwrap_or_default();
        text.push_str(&format!("\n {marker} {:>3}. {name}{length}", index + 1));
    }
    text
}

fn format_added(payload: &Payload) -> String {
    let Payload::Added { ids, rejected, unresolved } = payload else { return "ok".to_string() };
    let mut text = format!("added {} tracks", ids.len());
    for refused in rejected {
        text.push_str(&format!("\n  not added: {} ({})", refused.source, refused.reason));
    }
    for item in unresolved {
        text.push_str(&format!("\n  added, but its details could not be fetched: {}", item.reason));
    }
    text
}

fn format_event(event: &Event) -> String {
    match event {
        Event::StateChanged { state } => format!("state {}", state_name(*state)),
        Event::TrackStarted { title, source, spec, .. } => format!(
            "started {} ({}-bit / {} Hz)",
            title.as_deref().or(source.as_deref()).unwrap_or("?"),
            spec.bits_per_sample,
            spec.sample_rate
        ),
        Event::TrackEnded { reason, .. } => format!("ended ({reason:?})").to_lowercase(),
        Event::Position { position_ms, duration_ms } => match duration_ms {
            Some(total) => format!("position {} / {}", format_ms(*position_ms), format_ms(*total)),
            None => format!("position {}", format_ms(*position_ms)),
        },
        Event::Seeked { position_ms } => format!("seeked to {}", format_ms(*position_ms)),
        Event::SeekRejected { reason } => format!("seek rejected: {reason}"),
        Event::QueueChanged { queue } => format!("queue changed ({} entries)", queue.items.len()),
        Event::QueueExhausted => "end of the queue".to_string(),
        Event::SinkReport(report) => format!(
            "output {}: {} {}",
            report.device,
            report.negotiated_format,
            if report.bit_perfect { "BIT-PERFECT".to_string() } else { format!("CONVERTED ({})", report.problem.as_deref().unwrap_or("?")) }
        ),
        Event::Error { message } => format!("error: {message}"),
        Event::ShuttingDown => "the daemon is shutting down".to_string(),
        Event::Resync { skipped, .. } => format!("resynced (missed {skipped} events)"),
        Event::Unknown => "(an event this client does not know)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phonia_ipc::{QueueItem, Spec, Track};

    #[test]
    fn seek_positions() {
        assert_eq!(parse_seek("90").unwrap(), SeekTarget::Absolute { ms: 90_000 });
        assert_eq!(parse_seek("0").unwrap(), SeekTarget::Absolute { ms: 0 });
        assert_eq!(parse_seek("+10").unwrap(), SeekTarget::Forward { ms: 10_000 });
        assert_eq!(parse_seek("-2.5").unwrap(), SeekTarget::Backward { ms: 2_500 });
        assert_eq!(parse_seek(" 12.345 ").unwrap(), SeekTarget::Absolute { ms: 12_345 });
    }

    #[test]
    fn nonsense_seek_positions_are_refused() {
        for bad in ["", "abc", "+", "--5", "1:30", "NaN", "inf", "+-3"] {
            assert!(parse_seek(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn sources_as_the_user_writes_them() {
        assert_eq!(resolve_source("tidal:233059491").unwrap(), "tidal:233059491");
        assert_eq!(resolve_source("233059491").unwrap(), "tidal:233059491", "a bare number is a TIDAL id");
        assert_eq!(resolve_source("file:/music/a.flac").unwrap(), "file:/music/a.flac");
    }

    #[test]
    fn a_path_becomes_an_absolute_file_source() {
        assert_eq!(resolve_source("/music/a track.flac").unwrap(), "file:/music/a track.flac");

        // A relative path is resolved against where the user is, whether or not the file exists:
        // it is the daemon's job to say a file can't be played.
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(resolve_source("missing.flac").unwrap(), format!("file:{}", cwd.join("missing.flac").display()));
    }

    fn queue() -> Queue {
        let item = |id: u64, title: Option<&str>, ms: Option<u64>| QueueItem {
            id: ItemId(id),
            source: format!("file:/m/{id}.flac"),
            title: title.map(str::to_string),
            duration_ms: ms,
        };
        Queue {
            version: 1,
            items: vec![item(10, Some("One"), Some(65_000)), item(11, None, None), item(12, Some("Three"), Some(3_725_000))],
            order: vec![ItemId(10), ItemId(11), ItemId(12)],
            current: Some(ItemId(11)),
            shuffle: true,
            repeat: Repeat::All,
        }
    }

    #[test]
    fn entries_are_numbered_from_one() {
        let queue = queue();
        assert_eq!(entry_id(&queue, 1).unwrap(), ItemId(10));
        assert_eq!(entry_id(&queue, 3).unwrap(), ItemId(12));
        for bad in [0, 4, 99] {
            let error = entry_id(&queue, bad).unwrap_err();
            assert!(error.to_string().contains("no queue entry"), "{bad}: {error}");
        }
    }

    #[test]
    fn times_read_as_minutes_and_seconds() {
        assert_eq!(format_ms(0), "0:00");
        assert_eq!(format_ms(65_000), "1:05");
        assert_eq!(format_ms(59_999), "0:59");
        assert_eq!(format_ms(3_725_000), "1:02:05");
    }

    #[test]
    fn the_queue_listing_marks_the_current_entry_and_shows_lengths() {
        assert_eq!(
            format_queue(&queue()),
            "Queue (3 entries, shuffle on, repeat all):\n     1. One  [1:05]\n >   2. file:/m/11.flac\n     3. Three  [1:02:05]"
        );
    }

    #[test]
    fn the_status_shows_what_is_known() {
        let status = Status {
            state: State::Playing,
            track: Some(Track { item_id: Some(ItemId(10)), source: Some("tidal:1".into()), title: Some("Song".into()), duration_ms: Some(348_680) }),
            spec: Some(Spec { sample_rate: 192_000, channels: 2, bits_per_sample: 24 }),
            position_ms: 83_000,
            duration_ms: Some(348_680),
        };
        assert_eq!(format_status(&status), "State:    playing\nTrack:    Song\nPosition: 1:23 / 5:48\nFormat:   24-bit / 192000 Hz / 2 ch");
        let idle = Status { state: State::Stopped, track: None, spec: None, position_ms: 0, duration_ms: None };
        assert_eq!(format_status(&idle), "State:    stopped");
    }

    #[test]
    fn the_result_of_adding_lists_what_went_wrong() {
        let payload = Payload::Added {
            ids: vec![ItemId(1)],
            rejected: vec![phonia_ipc::Rejected { source: "file:/x.flac".into(), reason: "no such file".into() }],
            unresolved: vec![phonia_ipc::Unresolved { id: ItemId(1), reason: "TIDAL unreachable".into() }],
        };
        assert_eq!(
            format_added(&payload),
            "added 1 tracks\n  not added: file:/x.flac (no such file)\n  added, but its details could not be fetched: TIDAL unreachable"
        );
    }

    #[test]
    fn events_read_as_one_line_each() {
        assert_eq!(format_event(&Event::StateChanged { state: State::Paused }), "state paused");
        assert_eq!(format_event(&Event::Position { position_ms: 61_000, duration_ms: Some(120_000) }), "position 1:01 / 2:00");
        assert_eq!(format_event(&Event::QueueExhausted), "end of the queue");
        assert_eq!(format_event(&Event::Unknown), "(an event this client does not know)");
    }
}
