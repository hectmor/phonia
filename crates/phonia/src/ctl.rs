//! `phonia ctl`: drives a running `phoniad` over its socket.

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use phonia_ipc::{
    AddAt, CAP_OUTPUT_RELEASE, CAP_OUTPUT_SELECT, Client, ClientError, ClientInfo, Event, ItemId, NewTrack, Output,
    OutputInfo, OutputMode, Payload, Queue, ReleaseReason, Repeat, Request, SeekTarget, State, Status,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long `resume` waits to hear that playback is back, which after a release includes taking
/// the DAC from whoever has it.
const RESUME_WAIT: Duration = Duration::from_secs(5);

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
    /// Continues after a pause. If the DAC was handed back, takes it again first, and says so if
    /// someone else has it and won't let go.
    Resume,
    /// Pauses and hands the DAC back to the desktop, so another program can use it. `resume`
    /// takes it again and carries on from the same place.
    Release,
    /// Lists the outputs and shows which one is playing; `output set <n>` plays through another
    /// one from now on, keeping the track and the position. An exclusive card is bit-perfect;
    /// a shared output goes through the desktop's sound server and is not.
    Output {
        #[command(subcommand)]
        action: Option<OutputAction>,
    },
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
pub enum OutputAction {
    /// Plays through this output from now on: its number in the list, its id, or part of its name.
    Set { output: String },
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
        CtlCommand::Resume => resume(&client, json).await,
        CtlCommand::Release => {
            if !client.server().capabilities.iter().any(|capability| capability == CAP_OUTPUT_RELEASE) {
                bail!("this phoniad is too old to hand the DAC back (protocol 1.0): restart it after updating");
            }
            ack(&client, json, Request::Release).await
        }
        CtlCommand::Toggle => ack(&client, json, Request::TogglePause).await,
        CtlCommand::Next => ack(&client, json, Request::Next).await,
        CtlCommand::Prev => ack(&client, json, Request::Previous).await,
        CtlCommand::Stop => ack(&client, json, Request::Stop).await,
        CtlCommand::Seek { position } => ack(&client, json, Request::Seek { target: parse_seek(&position)? }).await,
        CtlCommand::Output { action } => output(&client, json, action).await,
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

/// `resume`, waiting to hear that playback is back: taking the DAC again can be refused, and that
/// is worth saying instead of a bare "ok".
async fn resume(client: &Client, json: bool) -> Result<()> {
    if json {
        return ack(client, json, Request::Resume).await;
    }
    let (snapshot, mut events) = client.subscribe().await?;
    if snapshot.status.state != State::Paused {
        return ack(client, json, Request::Resume).await;
    }
    client.request(Request::Resume).await?;
    let outcome = tokio::time::timeout(RESUME_WAIT, async {
        loop {
            match events.next().await {
                Some(Event::StateChanged { state: State::Playing }) | None => return Ok(()),
                Some(Event::Error { message }) => return Err(anyhow!(message)),
                Some(_) => {}
            }
        }
    })
    .await;
    match outcome {
        Ok(result) => result?,
        Err(_) => bail!("resume was sent, but playback has not started after {} s", RESUME_WAIT.as_secs()),
    }
    println!("ok");
    Ok(())
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

async fn output(client: &Client, json: bool, action: Option<OutputAction>) -> Result<()> {
    if !client.server().capabilities.iter().any(|capability| capability == CAP_OUTPUT_SELECT) {
        bail!("this phoniad is too old to switch outputs (protocol 1.1): restart it after updating");
    }
    let listing = client.request(Request::Outputs).await?;
    let Payload::Outputs { outputs, current } = &listing else { bail!("the daemon answered something unexpected") };
    match action {
        None => print_payload(json, &listing, || format_outputs(outputs, current.as_deref())),
        Some(OutputAction::Set { output }) => {
            let id = resolve_output(&output, outputs)?;
            ack(client, json, Request::SetOutput { output: id }).await
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

/// The output a person meant: its number in the list, its id, or a part of its id or name that only
/// one output has.
fn resolve_output(wanted: &str, outputs: &[OutputInfo]) -> Result<String> {
    if let Ok(number) = wanted.parse::<usize>() {
        return number
            .checked_sub(1)
            .and_then(|index| outputs.get(index))
            .map(|output| output.id.clone())
            .ok_or_else(|| anyhow!("there is no output {number} (`phonia ctl output` lists {})", outputs.len()));
    }
    if let Some(exact) = outputs.iter().find(|output| output.id == wanted) {
        return Ok(exact.id.clone());
    }
    let lower = wanted.to_lowercase();
    let matching: Vec<&OutputInfo> = outputs
        .iter()
        .filter(|output| output.id.to_lowercase().contains(&lower) || output.name.to_lowercase().contains(&lower))
        .collect();
    match matching.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => bail!("no output matches {wanted:?} (`phonia ctl output` lists them)"),
        many => bail!(
            "{wanted:?} matches {} outputs; be more specific: {}",
            many.len(),
            many.iter().map(|output| output.id.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn how(output: &OutputInfo) -> String {
    match (output.mode, output.bit_perfect) {
        (OutputMode::Exclusive, true) => "bit-perfect".to_string(),
        _ => match (&output.codec, output.lossy) {
            (Some(codec), true) => format!("shared, lossy ({codec})"),
            (None, true) => "shared, lossy".to_string(),
            _ => "shared, not bit-perfect".to_string(),
        },
    }
}

fn format_outputs(outputs: &[OutputInfo], current: Option<&str>) -> String {
    if outputs.is_empty() {
        return "No outputs found.".to_string();
    }
    let width = outputs.iter().map(|output| output.id.len()).max().unwrap_or(0);
    let mut text = "Outputs (`phonia ctl output set <n>` moves playback, keeping the position):".to_string();
    for (index, output) in outputs.iter().enumerate() {
        let marker = if current == Some(output.id.as_str()) { '*' } else { ' ' };
        let detail = output.detail.as_deref().map(|detail| format!(" ({detail})")).unwrap_or_default();
        text.push_str(&format!(
            "\n {marker} {:>2}. {:<width$}  {}{detail}  [{}]",
            index + 1,
            output.id,
            output.name,
            how(output)
        ));
    }
    text
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
    if let Output::Released { by } = &status.output {
        match by {
            Some(by) => text.push_str(&format!(" (DAC released to {by})")),
            None => text.push_str(" (DAC released)"),
        }
    }
    if let Some(track) = &status.track {
        let name = track.title.as_deref().or(track.source.as_deref()).unwrap_or("?");
        text.push_str(&format!("\nTrack:    {name}"));
        let total = status.duration_ms.map(|ms| format!(" / {}", format_ms(ms))).unwrap_or_default();
        text.push_str(&format!("\nPosition: {}{total}", format_ms(status.position_ms)));
    }
    if let Some(route) = &status.route {
        let how = match route.mode {
            OutputMode::Exclusive => "exclusive",
            OutputMode::Shared => "shared, not bit-perfect",
            OutputMode::Unknown => "?",
        };
        text.push_str(&format!("\nOutput:   {} [{how}]", route.description));
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
            match (report.mode, report.bit_perfect) {
                (_, true) => "BIT-PERFECT".to_string(),
                (Some(OutputMode::Shared), false) => match (&report.codec, report.lossy) {
                    (Some(codec), true) => format!("SHARED, LOSSY CODEC ({codec})"),
                    (None, true) => "SHARED, LOSSY".to_string(),
                    _ => match report.resampled_to {
                        Some(rate) => format!("SHARED (not bit-perfect, resampled to {rate} Hz)"),
                        None => "SHARED (not bit-perfect)".to_string(),
                    },
                },
                _ => format!("CONVERTED ({})", report.problem.as_deref().unwrap_or("?")),
            }
        ),
        Event::OutputReleased { by, reason } => {
            let why = match reason {
                ReleaseReason::Idle => "paused for a while",
                ReleaseReason::Command => "asked to",
                ReleaseReason::Requested => "another program asked for it",
                ReleaseReason::Lost => "the output went away",
                ReleaseReason::Unknown => "?",
            };
            match by {
                Some(by) => format!("DAC released to {by} ({why})"),
                None => format!("DAC released ({why})"),
            }
        }
        Event::OutputAcquired => "DAC taken again".to_string(),
        Event::OutputChanged { route } => format!("output now {} ({})", route.description, route.id),
        Event::OutputsChanged => "the outputs changed (`phonia ctl output` lists them)".to_string(),
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
            output: Output::Open,
            route: None,
        };
        assert_eq!(format_status(&status), "State:    playing\nTrack:    Song\nPosition: 1:23 / 5:48\nFormat:   24-bit / 192000 Hz / 2 ch");
        let idle =
            Status { state: State::Stopped, track: None, spec: None, position_ms: 0, duration_ms: None, output: Output::Closed, route: None };
        assert_eq!(format_status(&idle), "State:    stopped");

        let released = Status { state: State::Paused, output: Output::Released { by: Some("jackd".into()) }, ..idle.clone() };
        assert_eq!(format_status(&released), "State:    paused (DAC released to jackd)");
        let released = Status { output: Output::Released { by: None }, ..released };
        assert_eq!(format_status(&released), "State:    paused (DAC released)");
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
        assert_eq!(
            format_event(&Event::OutputReleased { by: Some("jackd".into()), reason: ReleaseReason::Requested }),
            "DAC released to jackd (another program asked for it)"
        );
        assert_eq!(
            format_event(&Event::OutputReleased { by: None, reason: ReleaseReason::Idle }),
            "DAC released (paused for a while)"
        );
        assert_eq!(format_event(&Event::OutputAcquired), "DAC taken again");
        assert_eq!(format_event(&Event::Unknown), "(an event this client does not know)");
    }

    fn out(id: &str, mode: OutputMode, name: &str, bit_perfect: bool, lossy: bool, codec: Option<&str>) -> OutputInfo {
        OutputInfo {
            id: id.into(),
            mode,
            name: name.into(),
            detail: Some("USB".into()),
            bit_perfect,
            lossy,
            codec: codec.map(str::to_string),
            is_default: false,
        }
    }

    fn outputs() -> Vec<OutputInfo> {
        vec![
            out("exclusive:hw:DS2,0", OutputMode::Exclusive, "Fosi Audio DS2", true, false, None),
            out("shared:alsa_output.usb-DS2", OutputMode::Shared, "Fosi Audio DS2 Analog Stereo", false, false, None),
            out("shared:bluez_output.AA", OutputMode::Shared, "Soundcore Life P2", false, true, Some("SBC")),
        ]
    }

    #[test]
    fn the_output_list_marks_the_current_one_and_says_how_each_plays() {
        assert_eq!(
            format_outputs(&outputs(), Some("shared:bluez_output.AA")),
            "Outputs (`phonia ctl output set <n>` moves playback, keeping the position):\n\
             \x20   1. exclusive:hw:DS2,0          Fosi Audio DS2 (USB)  [bit-perfect]\n\
             \x20   2. shared:alsa_output.usb-DS2  Fosi Audio DS2 Analog Stereo (USB)  [shared, not bit-perfect]\n\
             \x20*  3. shared:bluez_output.AA      Soundcore Life P2 (USB)  [shared, lossy (SBC)]"
        );
        assert_eq!(format_outputs(&[], None), "No outputs found.");
    }

    #[test]
    fn an_output_is_picked_by_number_id_or_a_part_of_its_name() {
        let all = outputs();
        assert_eq!(resolve_output("2", &all).unwrap(), "shared:alsa_output.usb-DS2");
        assert_eq!(resolve_output("exclusive:hw:DS2,0", &all).unwrap(), "exclusive:hw:DS2,0");
        assert_eq!(resolve_output("soundcore", &all).unwrap(), "shared:bluez_output.AA");
        assert_eq!(resolve_output("bluez", &all).unwrap(), "shared:bluez_output.AA");
    }

    #[test]
    fn a_wrong_or_ambiguous_output_is_explained() {
        let all = outputs();
        for (wanted, expect) in [("0", "no output 0"), ("9", "no output 9"), ("nothing", "no output matches"), ("ds2", "matches 2 outputs")] {
            let error = resolve_output(wanted, &all).unwrap_err().to_string();
            assert!(error.contains(expect), "{wanted}: {error}");
        }
    }

    #[test]
    fn the_status_says_where_the_sound_goes() {
        let status = Status {
            state: State::Playing,
            track: None,
            spec: None,
            position_ms: 0,
            duration_ms: None,
            output: Output::Open,
            route: Some(phonia_ipc::Route {
                id: "shared:bluez_output.AA".into(),
                mode: OutputMode::Shared,
                description: "Soundcore Life P2".into(),
            }),
        };
        assert_eq!(format_status(&status), "State:    playing\nOutput:   Soundcore Life P2 [shared, not bit-perfect]");
    }

    #[test]
    fn output_events_and_shared_reports_read_naturally() {
        let route = phonia_ipc::Route { id: "shared:x".into(), mode: OutputMode::Shared, description: "Speaker".into() };
        assert_eq!(format_event(&Event::OutputChanged { route }), "output now Speaker (shared:x)");
        assert!(format_event(&Event::OutputsChanged).contains("outputs changed"));
        assert_eq!(
            format_event(&Event::OutputReleased { by: None, reason: ReleaseReason::Lost }),
            "DAC released (the output went away)"
        );

        let report = |mode, bit_perfect, codec: Option<&str>, lossy, resampled| {
            Event::SinkReport(phonia_ipc::SinkReport {
                device: "Soundcore Life P2".into(),
                source: Spec { sample_rate: 96_000, channels: 2, bits_per_sample: 24 },
                negotiated_format: "S32LE".into(),
                bit_perfect,
                problem: Some("why".into()),
                hw_params: None,
                mode: Some(mode),
                resampled_to: resampled,
                codec: codec.map(str::to_string),
                lossy,
            })
        };
        assert_eq!(
            format_event(&report(OutputMode::Shared, false, Some("SBC"), true, Some(48_000))),
            "output Soundcore Life P2: S32LE SHARED, LOSSY CODEC (SBC)"
        );
        assert_eq!(
            format_event(&report(OutputMode::Shared, false, None, false, Some(48_000))),
            "output Soundcore Life P2: S32LE SHARED (not bit-perfect, resampled to 48000 Hz)"
        );
        assert_eq!(
            format_event(&report(OutputMode::Exclusive, true, None, false, None)),
            "output Soundcore Life P2: S32LE BIT-PERFECT"
        );
    }
}
