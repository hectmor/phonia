//! `phonia ctl`: drives a running `phoniad` over its socket.

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use phonia_ipc::{
    AddAt, CAP_CATALOG, CAP_OUTPUT_RELEASE, CAP_OUTPUT_SELECT, CAP_QUALITY, CAP_VOLUME,
    CatalogKind, CatalogRef, Client, ClientError, ClientInfo, Event, ItemId, NewTrack, Output,
    OutputInfo, OutputMode, Payload, Quality, QualityRange, Queue, ReleaseReason, Repeat, Request,
    SeekTarget, State, Status, Volume,
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
    Play {
        entry: Option<usize>,
    },
    Pause,
    /// Continues after a pause. If the DAC was handed back, takes it again first, and says so if
    /// someone else has it and won't let go.
    Resume,
    /// Pauses and hands the DAC back to the desktop, so another program can use it. `resume`
    /// takes it again and carries on from the same place.
    Release,
    /// Shows or sets the volume of a shared output: `volume 60` sets 60%, `volume +5` and
    /// `volume -5` change it. The scale is the desktop mixer's (100% is unity gain, 50% about
    /// -18 dB), never above 100%. An exclusive card has no volume: the audio is not scaled.
    Volume {
        #[arg(allow_hyphen_values = true)]
        change: Option<String>,
    },
    /// Shows the tiers phonia asks TIDAL for, or sets the best one (`quality lossless`): from the
    /// next track opened on, so the one playing and the one already opened ahead keep theirs.
    /// The tiers are hires, lossless, high and low; below the minimum in the config file
    /// (`[tidal] min_quality`) is refused.
    Quality {
        tier: Option<Quality>,
    },
    /// Searches TIDAL: `search korn`, or `search nu metal --kind albums --kind tracks --limit 10`.
    /// Tracks are printed with the `tidal:<id>` that `queue add` takes.
    Search {
        /// What to look for (several words are one search).
        #[arg(required = true)]
        query: Vec<String>,
        /// Only this kind of result; may be given more than once. Default: all four.
        #[arg(long = "kind", value_enum)]
        kinds: Vec<SearchKind>,
        /// How many of each kind, 1 to 300. Default: 50.
        #[arg(long)]
        limit: Option<u32>,
        /// Where to start, to see the next page.
        #[arg(long, default_value_t = 0)]
        offset: u32,
    },
    /// Shows an album: its details and its tracks (the first 50, or `--limit`, at most 100).
    /// Tracks print the `tidal:<id>` that `queue add` takes; `queue add album:<id>` adds it whole.
    Album {
        /// The album's id, as `search` prints it.
        id: String,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Shows an artist: its bio, its most listened to tracks, its albums, and its EPs and
    /// singles (the first 50 of each, or `--limit`, at most 100).
    Artist {
        /// The artist's id, as `search` prints it.
        id: String,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Shows the library: favorite tracks, favorite albums, and the playlists you created
    /// yourself (the first 50 of each, or `--limit`, at most 100).
    Library {
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Mutes (`on`), unmutes (`off`) or flips (`toggle`, the default) a shared output; the level is kept.
    Mute {
        mode: Option<MuteMode>,
    },
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
    Shuffle {
        mode: Switch,
    },
    Repeat {
        mode: RepeatMode,
    },
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
    /// Adds tracks: `tidal:<id>` (or just the id), `file:/abs/path`, or a path to a file. Or a
    /// whole album or playlist, on its own: `album:<id>`, `playlist:<uuid>` (`ctl search` prints
    /// them).
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
    Move {
        entry: usize,
        to: usize,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

/// What `search --kind` can name.
#[derive(Clone, Copy, ValueEnum)]
pub enum SearchKind {
    Tracks,
    Albums,
    Artists,
    Playlists,
}

impl From<SearchKind> for CatalogKind {
    fn from(kind: SearchKind) -> Self {
        match kind {
            SearchKind::Tracks => CatalogKind::Tracks,
            SearchKind::Albums => CatalogKind::Albums,
            SearchKind::Artists => CatalogKind::Artists,
            SearchKind::Playlists => CatalogKind::Playlists,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum MuteMode {
    On,
    Off,
    Toggle,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum RepeatMode {
    Off,
    One,
    All,
}

/// The daemon's socket: the flag, else the config file, else the default path.
pub fn socket_path(config_flag: Option<&Path>, flag: Option<PathBuf>) -> Result<PathBuf> {
    let loaded =
        phonia_core::config::load(phonia_core::config::discover_from_env(config_flag).as_ref())?;
    let settings = phonia_core::config::resolve(
        phonia_core::config::Overrides {
            socket: flag,
            ..Default::default()
        },
        &loaded.file,
    );
    Ok(settings.socket_path(phonia_ipc::socket::default_socket_path))
}

pub async fn run(args: CtlArgs, config_flag: Option<&Path>) -> Result<()> {
    let info = ClientInfo {
        name: "phonia-ctl".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let socket = socket_path(config_flag, args.socket.clone())?;
    let client = Client::connect(Some(&socket), info)
        .await
        .map_err(explain_connection_error)?;
    let json = args.json;

    match args.command {
        CtlCommand::Status => {
            let status = client.status().await?;
            print_payload(json, &Payload::Status(status.clone()), || {
                format_status(&status)
            })
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
            if !client
                .server()
                .capabilities
                .iter()
                .any(|capability| capability == CAP_OUTPUT_RELEASE)
            {
                bail!(
                    "this phoniad is too old to hand the DAC back (protocol 1.0): restart it after updating"
                );
            }
            ack(&client, json, Request::Release).await
        }
        CtlCommand::Toggle => ack(&client, json, Request::TogglePause).await,
        CtlCommand::Next => ack(&client, json, Request::Next).await,
        CtlCommand::Prev => ack(&client, json, Request::Previous).await,
        CtlCommand::Stop => ack(&client, json, Request::Stop).await,
        CtlCommand::Seek { position } => {
            ack(
                &client,
                json,
                Request::Seek {
                    target: parse_seek(&position)?,
                },
            )
            .await
        }
        CtlCommand::Volume { change } => volume(&client, json, change).await,
        CtlCommand::Quality { tier } => quality(&client, json, tier).await,
        CtlCommand::Search {
            query,
            kinds,
            limit,
            offset,
        } => search(&client, json, query.join(" "), kinds, limit, offset).await,
        CtlCommand::Album { id, limit } => album(&client, json, id, limit).await,
        CtlCommand::Artist { id, limit } => artist(&client, json, id, limit).await,
        CtlCommand::Library { limit } => library(&client, json, limit).await,
        CtlCommand::Mute { mode } => mute(&client, json, mode.unwrap_or(MuteMode::Toggle)).await,
        CtlCommand::Output { action } => output(&client, json, action).await,
        CtlCommand::Queue { action } => queue(&client, json, action).await,
        CtlCommand::Shuffle { mode } => {
            ack(
                &client,
                json,
                Request::SetShuffle {
                    shuffle: matches!(mode, Switch::On),
                },
            )
            .await
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
        ClientError::Io(error) => {
            anyhow!("{error}. Is phoniad running? Start it with `phoniad --device hw:N,0`.")
        }
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
                Some(Event::StateChanged {
                    state: State::Playing,
                })
                | None => return Ok(()),
                Some(Event::Error { message }) => return Err(anyhow!(message)),
                Some(_) => {}
            }
        }
    })
    .await;
    match outcome {
        Ok(result) => result?,
        Err(_) => bail!(
            "resume was sent, but playback has not started after {} s",
            RESUME_WAIT.as_secs()
        ),
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
            print_payload(json, &Payload::Queue(queue.clone()), || {
                format_queue(&queue)
            })
        }
        QueueAction::Add { sources, next, at } => {
            let at = match (next, at) {
                (true, _) => AddAt::Next,
                (false, Some(position)) => AddAt::Index {
                    index: position.saturating_sub(1),
                },
                (false, None) => AddAt::End,
            };
            let request = match catalog_source(&sources)? {
                Some(from) => {
                    if !client
                        .server()
                        .capabilities
                        .iter()
                        .any(|capability| capability == CAP_CATALOG)
                    {
                        bail!(
                            "this phoniad cannot add albums or playlists: it needs protocol 1.6 \
                             and a TIDAL login (run `phonia login`, then restart it)"
                        );
                    }
                    Request::QueueAddFrom { from, at }
                }
                None => {
                    let tracks = sources
                        .iter()
                        .map(|source| resolve_source(source).map(|source| NewTrack { source }))
                        .collect::<Result<Vec<_>>>()?;
                    Request::QueueAdd { tracks, at }
                }
            };
            let payload = client.request(request).await?;
            print_payload(json, &payload, || format_added(&payload))
        }
        QueueAction::Rm { entries } => {
            let queue = client.queue().await?;
            let ids = entries
                .iter()
                .map(|number| entry_id(&queue, *number))
                .collect::<Result<Vec<_>>>()?;
            let payload = client.request(Request::QueueRemove { ids }).await?;
            print_payload(json, &payload, || match &payload {
                Payload::Removed { count } => format!("removed {count} entries"),
                _ => "ok".to_string(),
            })
        }
        QueueAction::Clear => ack(client, json, Request::QueueClear).await,
        QueueAction::Move { entry, to } => {
            let id = entry_id(&client.queue().await?, entry)?;
            ack(
                client,
                json,
                Request::QueueMove {
                    id,
                    to: to.saturating_sub(1),
                },
            )
            .await
        }
    }
}

/// The volume the output has now, or why it has none.
async fn current_volume(client: &Client) -> Result<Volume> {
    if !client
        .server()
        .capabilities
        .iter()
        .any(|capability| capability == CAP_VOLUME)
    {
        bail!(
            "this phoniad is too old to set the volume (protocol 1.2): restart it after updating"
        );
    }
    client.status().await?.volume.ok_or_else(|| {
        anyhow!(
            "this output has no volume to set: an exclusive card plays the audio unscaled. Use the DAC's own \
             volume, or `phonia ctl output set` a shared output"
        )
    })
}

async fn volume(client: &Client, json: bool, change: Option<String>) -> Result<()> {
    let current = current_volume(client).await?;
    match change {
        None => print_payload(json, &Payload::Status(client.status().await?), || {
            format_volume(&current)
        }),
        Some(change) => {
            let percent = parse_volume(&change, current.percent)?;
            ack(client, json, Request::SetVolume { percent }).await
        }
    }
}

async fn quality(client: &Client, json: bool, tier: Option<Quality>) -> Result<()> {
    if !client
        .server()
        .capabilities
        .iter()
        .any(|capability| capability == CAP_QUALITY)
    {
        bail!(
            "this phoniad is too old to report or set the quality (protocol 1.5): restart it after updating"
        );
    }
    match tier {
        None => {
            let status = client.status().await?;
            print_payload(json, &Payload::Status(status.clone()), || {
                format_quality_status(&status)
            })
        }
        Some(quality) => ack(client, json, Request::SetMaxQuality { quality }).await,
    }
}

/// The tiers asked for, and what the track playing got.
fn format_quality_status(status: &Status) -> String {
    let mut text = match &status.quality_range {
        Some(range) => format_range(range),
        None => "Quality: this daemon does not play from TIDAL".to_string(),
    };
    if let Some(quality) = status.track.as_ref().and_then(|track| track.quality) {
        text.push_str(&format!(
            "\nPlaying: {}",
            phonia_ipc::fmt::stream_quality(&quality)
        ));
    }
    text
}

fn format_range(range: &QualityRange) -> String {
    format!(
        "Quality: asking for up to {}, playing nothing below {}",
        range.max, range.min
    )
}

async fn search(
    client: &Client,
    json: bool,
    query: String,
    kinds: Vec<SearchKind>,
    limit: Option<u32>,
    offset: u32,
) -> Result<()> {
    if !client
        .server()
        .capabilities
        .iter()
        .any(|capability| capability == CAP_CATALOG)
    {
        bail!(
            "this phoniad cannot search TIDAL: it needs protocol 1.6 and a TIDAL login \
             (run `phonia login`, then restart it)"
        );
    }
    let payload = client
        .request(Request::Search {
            query,
            kinds: kinds.into_iter().map(CatalogKind::from).collect(),
            offset,
            limit,
        })
        .await?;
    print_payload(json, &payload, || format_search(&payload))
}

/// Fails, saying what to do, if the daemon cannot browse TIDAL.
fn require_catalog(client: &Client, what: &str) -> Result<()> {
    if client
        .server()
        .capabilities
        .iter()
        .any(|capability| capability == CAP_CATALOG)
    {
        return Ok(());
    }
    bail!(
        "this phoniad cannot show {what}: it needs protocol 1.6 and a TIDAL login \
         (run `phonia login`, then restart it)"
    )
}

async fn album(client: &Client, json: bool, id: String, limit: Option<u32>) -> Result<()> {
    require_catalog(client, "albums")?;
    // An album fits in one page far more often than not: ask for the most there is.
    let payload = client
        .request(Request::Album {
            id,
            limit: Some(limit.unwrap_or(100)),
        })
        .await?;
    print_payload(json, &payload, || format_album(&payload))
}

async fn artist(client: &Client, json: bool, id: String, limit: Option<u32>) -> Result<()> {
    require_catalog(client, "artists")?;
    let payload = client.request(Request::Artist { id, limit }).await?;
    print_payload(json, &payload, || format_artist(&payload))
}

async fn library(client: &Client, json: bool, limit: Option<u32>) -> Result<()> {
    require_catalog(client, "a library")?;
    let payload = client.request(Request::Library { limit }).await?;
    print_payload(json, &payload, || format_library(&payload))
}

/// The URL of a cover or a picture, at a size fit for opening in a browser rather than for a
/// terminal cell; `None` when there is no id to build one from.
fn cover_url(kind: phonia_ipc::image::Kind, id: Option<&str>) -> Option<String> {
    phonia_ipc::image::url(kind, id?, 640)
}

/// The album's line, then who it is by, its copyright, and its tracks, numbered as on the album,
/// with a heading for each disc when there is more than one.
fn format_album(payload: &Payload) -> String {
    let Payload::Album { album, tracks } = payload else {
        return "unexpected answer".to_string();
    };
    let mut text = format!("{}   album {}", phonia_ipc::fmt::album(album), album.id);
    if let Some(ms) = album.duration_ms {
        text.push_str(&format!("\nLength: {}", phonia_ipc::fmt::ms(ms)));
    }
    if let Some(copyright) = &album.copyright {
        text.push_str(&format!("\n{copyright}"));
    }
    if let Some(cover) = cover_url(phonia_ipc::image::Kind::AlbumCover, album.cover.as_deref()) {
        text.push_str(&format!("\nCover: {cover}"));
    }
    let discs: std::collections::BTreeSet<u32> = tracks
        .items
        .iter()
        .filter_map(|t| t.volume_number)
        .collect();
    let mut disc = None;
    for (index, track) in tracks.items.iter().enumerate() {
        if discs.len() > 1 && track.volume_number != disc {
            disc = track.volume_number;
            if let Some(number) = disc {
                text.push_str(&format!("\n\nDisc {number}:"));
            }
        } else if index == 0 {
            text.push('\n');
        }
        let number = track
            .track_number
            .map_or_else(|| tracks.offset as usize + index + 1, |n| n as usize);
        text.push_str(&format!(
            "\n {number:>3}. {}   tidal:{}",
            phonia_ipc::fmt::track_short(track),
            track.id
        ));
    }
    if tracks.total > tracks.items.len() as u64 {
        text.push_str(&format!(
            "\n\nShowing {} of {} tracks (`queue add album:{}` adds them all).",
            tracks.items.len(),
            tracks.total,
            album.id
        ));
    }
    text
}

/// The artist, a few lines of its bio, and a section for each list.
fn format_artist(payload: &Payload) -> String {
    let Payload::Artist {
        artist,
        bio,
        top_tracks,
        albums,
        singles,
    } = payload
    else {
        return "unexpected answer".to_string();
    };
    let mut text = format!("{}   artist {}", artist.name, artist.id);
    if let Some(picture) = cover_url(
        phonia_ipc::image::Kind::ArtistPicture,
        artist.picture.as_deref(),
    ) {
        text.push_str(&format!("\nPicture: {picture}"));
    }
    if let Some(bio) = bio {
        // A bio is long: the first paragraph, cut where a sentence ends if it is still long.
        let first = bio.split("\n\n").next().unwrap_or(bio).trim();
        let cut: String = first.chars().take(400).collect();
        let cut = if cut.len() < first.len() {
            format!("{}...", cut.trim_end())
        } else {
            cut
        };
        text.push_str(&format!("\n\n{cut}"));
    }
    fn section<T>(
        text: &mut String,
        title: &str,
        page: &phonia_ipc::Page<T>,
        row: impl Fn(&T) -> String,
    ) {
        text.push_str(&format!(
            "\n\n{title} ({} of {}):",
            page.items.len(),
            page.total
        ));
        if page.items.is_empty() {
            text.push_str("\n  none");
        }
        for (index, item) in page.items.iter().enumerate() {
            text.push_str(&format!("\n {:>3}. {}", index + 1, row(item)));
        }
    }
    section(&mut text, "Top tracks", top_tracks, |track| {
        format!("{}   tidal:{}", phonia_ipc::fmt::track(track), track.id)
    });
    section(&mut text, "Albums", albums, |album| {
        format!("{}   album {}", phonia_ipc::fmt::album(album), album.id)
    });
    section(&mut text, "EPs and singles", singles, |album| {
        format!("{}   album {}", phonia_ipc::fmt::album(album), album.id)
    });
    text
}

/// The three lists a library has, each numbered from the start.
fn format_library(payload: &Payload) -> String {
    let Payload::Library {
        favorite_tracks,
        favorite_albums,
        my_playlists,
    } = payload
    else {
        return "unexpected answer".to_string();
    };
    fn section<T>(
        text: &mut String,
        title: &str,
        page: &phonia_ipc::Page<T>,
        row: impl Fn(&T) -> String,
    ) {
        text.push_str(&format!(
            "\n\n{title} ({} of {}):",
            page.items.len(),
            page.total
        ));
        if page.items.is_empty() {
            text.push_str("\n  none");
        }
        for (index, item) in page.items.iter().enumerate() {
            text.push_str(&format!("\n {:>3}. {}", index + 1, row(item)));
        }
    }
    let mut text = "Library:".to_string();
    section(&mut text, "Favorite tracks", favorite_tracks, |track| {
        format!("{}   tidal:{}", phonia_ipc::fmt::track(track), track.id)
    });
    section(&mut text, "Favorite albums", favorite_albums, |album| {
        format!("{}   album {}", phonia_ipc::fmt::album(album), album.id)
    });
    section(&mut text, "Your playlists", my_playlists, |playlist| {
        format!(
            "{}   playlist {}",
            phonia_ipc::fmt::playlist(playlist),
            playlist.id
        )
    });
    text
}

/// One section per kind that was asked for, each row numbered from the start of the list and
/// ending with what to give the other commands.
fn format_search(payload: &Payload) -> String {
    let Payload::SearchResults {
        query,
        tracks,
        albums,
        artists,
        playlists,
    } = payload
    else {
        return "unexpected answer".to_string();
    };
    fn section<T>(
        text: &mut String,
        title: &str,
        page: &Option<phonia_ipc::Page<T>>,
        row: impl Fn(&T) -> String,
    ) {
        let Some(page) = page else { return };
        text.push_str(&format!(
            "\n\n{title} ({} of {}):",
            page.items.len(),
            page.total
        ));
        if page.items.is_empty() {
            text.push_str("\n  none");
        }
        for (index, item) in page.items.iter().enumerate() {
            let number = page.offset as usize + index + 1;
            text.push_str(&format!("\n {number:>3}. {}", row(item)));
        }
    }
    let mut text = format!("Search for {query:?}:");
    section(&mut text, "Tracks", tracks, |track| {
        format!("{}   tidal:{}", phonia_ipc::fmt::track(track), track.id)
    });
    section(&mut text, "Albums", albums, |album| {
        format!("{}   album {}", phonia_ipc::fmt::album(album), album.id)
    });
    section(&mut text, "Artists", artists, |artist| {
        format!("{}   artist {}", phonia_ipc::fmt::artist(artist), artist.id)
    });
    section(&mut text, "Playlists", playlists, |playlist| {
        format!(
            "{}   playlist {}",
            phonia_ipc::fmt::playlist(playlist),
            playlist.id
        )
    });
    text
}

async fn mute(client: &Client, json: bool, mode: MuteMode) -> Result<()> {
    let current = current_volume(client).await?;
    let mute = match mode {
        MuteMode::On => true,
        MuteMode::Off => false,
        MuteMode::Toggle => !current.muted,
    };
    ack(client, json, Request::SetMute { mute }).await
}

async fn output(client: &Client, json: bool, action: Option<OutputAction>) -> Result<()> {
    if !client
        .server()
        .capabilities
        .iter()
        .any(|capability| capability == CAP_OUTPUT_SELECT)
    {
        bail!(
            "this phoniad is too old to switch outputs (protocol 1.1): restart it after updating"
        );
    }
    let listing = client.request(Request::Outputs).await?;
    let Payload::Outputs { outputs, current } = &listing else {
        bail!("the daemon answered something unexpected")
    };
    match action {
        None => print_payload(json, &listing, || {
            format_outputs(outputs, current.as_deref())
        }),
        Some(OutputAction::Set { output }) => {
            let id = resolve_output(&output, outputs)?;
            ack(client, json, Request::SetOutput { output: id }).await
        }
    }
}

async fn watch(client: &Client, json: bool) -> Result<()> {
    let (snapshot, mut events) = client.subscribe().await?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&Payload::Snapshot {
                seq: snapshot.seq,
                status: snapshot.status,
                queue: snapshot.queue
            })?
        );
    } else {
        println!(
            "{}\n{}",
            format_status(&snapshot.status),
            format_queue(&snapshot.queue)
        );
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
    let seconds: f64 = digits
        .trim()
        .parse()
        .map_err(|_| anyhow!("{text:?} is not a position: use 90, +10 or -10 (seconds)"))?;
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
    let path = std::path::absolute(Path::new(text))
        .with_context(|| format!("{text:?} is not a usable path"))?;
    phonia_ipc::source::file(&path).map_err(|reason| anyhow!(reason))
}

/// An album or a playlist named as `album:<id>` or `playlist:<uuid>`, if that is what `sources`
/// is. It must be alone: each such addition is one request of its own, and mixing it with tracks
/// would leave open where each goes.
fn catalog_source(sources: &[String]) -> Result<Option<CatalogRef>> {
    let named = |text: &str| -> Option<CatalogRef> {
        if let Some(id) = text.strip_prefix("album:") {
            Some(CatalogRef::Album { id: id.to_string() })
        } else {
            text.strip_prefix("playlist:")
                .map(|id| CatalogRef::Playlist { id: id.to_string() })
        }
    };
    let found: Vec<Option<CatalogRef>> = sources.iter().map(|text| named(text)).collect();
    if found.iter().all(Option::is_none) {
        return Ok(None);
    }
    match found.as_slice() {
        [Some(from)] => Ok(Some(from.clone())),
        _ => bail!("an album or a playlist has to be added on its own, without other sources"),
    }
}

/// The entry at position `number` (from 1) of the queue.
fn entry_id(queue: &Queue, number: usize) -> Result<ItemId> {
    number
        .checked_sub(1)
        .and_then(|index| queue.items.get(index))
        .map(|item| item.id)
        .ok_or_else(|| {
            anyhow!(
                "there is no queue entry {number} (the queue has {})",
                queue.items.len()
            )
        })
}

/// `60` sets 60%, `+5` and `-5` change it, all within 0 to 100 (a `%` is fine).
fn parse_volume(text: &str, current: u8) -> Result<u8> {
    let text = text.trim().trim_end_matches('%').trim();
    let bad = || anyhow!("{text:?} is not a volume: use 60, +5 or -5 (percent, 0 to 100)");
    let (sign, digits) = match text.chars().next() {
        Some('+') => (1i32, &text[1..]),
        Some('-') => (-1, &text[1..]),
        _ => (0, text),
    };
    let amount: u32 = digits.trim().parse().map_err(|_| bad())?;
    let amount = i32::try_from(amount).map_err(|_| bad())?;
    let target = match sign {
        0 => amount,
        _ => i32::from(current) + sign * amount,
    };
    Ok(target.clamp(0, 100) as u8)
}

fn format_volume(volume: &Volume) -> String {
    if volume.muted {
        format!("Volume: {}% (muted)", volume.percent)
    } else {
        format!("Volume: {}%", volume.percent)
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
            .ok_or_else(|| {
                anyhow!(
                    "there is no output {number} (`phonia ctl output` lists {})",
                    outputs.len()
                )
            });
    }
    if let Some(exact) = outputs.iter().find(|output| output.id == wanted) {
        return Ok(exact.id.clone());
    }
    let lower = wanted.to_lowercase();
    let matching: Vec<&OutputInfo> = outputs
        .iter()
        .filter(|output| {
            output.id.to_lowercase().contains(&lower) || output.name.to_lowercase().contains(&lower)
        })
        .collect();
    match matching.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => bail!("no output matches {wanted:?} (`phonia ctl output` lists them)"),
        many => bail!(
            "{wanted:?} matches {} outputs; be more specific: {}",
            many.len(),
            many.iter()
                .map(|output| output.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
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
    let width = outputs
        .iter()
        .map(|output| output.id.len())
        .max()
        .unwrap_or(0);
    let mut text =
        "Outputs (`phonia ctl output set <n>` moves playback, keeping the position):".to_string();
    for (index, output) in outputs.iter().enumerate() {
        let marker = if current == Some(output.id.as_str()) {
            '*'
        } else {
            ' '
        };
        let detail = output
            .detail
            .as_deref()
            .map(|detail| format!(" ({detail})"))
            .unwrap_or_default();
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
        let name = track
            .title
            .as_deref()
            .or(track.source.as_deref())
            .unwrap_or("?");
        text.push_str(&format!("\nTrack:    {name}"));
        let total = status
            .duration_ms
            .map(|ms| format!(" / {}", phonia_ipc::fmt::ms(ms)))
            .unwrap_or_default();
        text.push_str(&format!(
            "\nPosition: {}{total}",
            phonia_ipc::fmt::ms(status.position_ms)
        ));
        if let Some(quality) = &track.quality {
            text.push_str(&format!(
                "\nQuality:  {}",
                phonia_ipc::fmt::stream_quality(quality)
            ));
        }
    }
    if let Some(route) = &status.route {
        let how = match route.mode {
            OutputMode::Exclusive => "exclusive",
            OutputMode::Shared => "shared, not bit-perfect",
            OutputMode::Unknown => "?",
        };
        text.push_str(&format!("\nOutput:   {} [{how}]", route.description));
    }
    if let Some(volume) = &status.volume {
        text.push_str(&format!(
            "\n{}",
            format_volume(volume).replacen("Volume: ", "Volume:   ", 1)
        ));
    }
    if let Some(spec) = status.spec {
        text.push_str(&format!(
            "\nFormat:   {}-bit / {} Hz / {} ch",
            spec.bits_per_sample, spec.sample_rate, spec.channels
        ));
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
        let marker = if queue.current == Some(item.id) {
            '>'
        } else {
            ' '
        };
        let name = item.title.as_deref().unwrap_or(&item.source);
        let length = item
            .duration_ms
            .map(|ms| format!("  [{}]", phonia_ipc::fmt::ms(ms)))
            .unwrap_or_default();
        text.push_str(&format!("\n {marker} {:>3}. {name}{length}", index + 1));
    }
    text
}

fn format_added(payload: &Payload) -> String {
    let Payload::Added {
        ids,
        rejected,
        unresolved,
    } = payload
    else {
        return "ok".to_string();
    };
    let mut text = format!("added {} tracks", ids.len());
    for refused in rejected {
        text.push_str(&format!(
            "\n  not added: {} ({})",
            refused.source, refused.reason
        ));
    }
    for item in unresolved {
        text.push_str(&format!(
            "\n  added, but its details could not be fetched: {}",
            item.reason
        ));
    }
    text
}

fn format_event(event: &Event) -> String {
    match event {
        Event::StateChanged { state } => format!("state {}", state_name(*state)),
        Event::TrackStarted {
            title,
            source,
            spec,
            gapless,
            quality,
            ..
        } => format!(
            "started {} ({}-bit / {} Hz){}{}",
            title.as_deref().or(source.as_deref()).unwrap_or("?"),
            spec.bits_per_sample,
            spec.sample_rate,
            quality
                .map(|quality| format!(" [{}]", phonia_ipc::fmt::stream_quality(&quality)))
                .unwrap_or_default(),
            if *gapless { " [gapless]" } else { "" }
        ),
        Event::MaxQualityChanged { quality } => format!("best quality asked for is now {quality}"),
        Event::TrackEnded { reason, .. } => format!("ended ({reason:?})").to_lowercase(),
        Event::Position {
            position_ms,
            duration_ms,
        } => match duration_ms {
            Some(total) => format!(
                "position {} / {}",
                phonia_ipc::fmt::ms(*position_ms),
                phonia_ipc::fmt::ms(*total)
            ),
            None => format!("position {}", phonia_ipc::fmt::ms(*position_ms)),
        },
        Event::Seeked { position_ms } => format!("seeked to {}", phonia_ipc::fmt::ms(*position_ms)),
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
        Event::OutputChanged { route } => {
            format!("output now {} ({})", route.description, route.id)
        }
        Event::OutputsChanged => "the outputs changed (`phonia ctl output` lists them)".to_string(),
        Event::VolumeChanged { percent, muted } => {
            format!("volume {percent}%{}", if *muted { " (muted)" } else { "" })
        }
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
        assert_eq!(
            parse_seek("90").unwrap(),
            SeekTarget::Absolute { ms: 90_000 }
        );
        assert_eq!(parse_seek("0").unwrap(), SeekTarget::Absolute { ms: 0 });
        assert_eq!(
            parse_seek("+10").unwrap(),
            SeekTarget::Forward { ms: 10_000 }
        );
        assert_eq!(
            parse_seek("-2.5").unwrap(),
            SeekTarget::Backward { ms: 2_500 }
        );
        assert_eq!(
            parse_seek(" 12.345 ").unwrap(),
            SeekTarget::Absolute { ms: 12_345 }
        );
    }

    #[test]
    fn nonsense_seek_positions_are_refused() {
        for bad in ["", "abc", "+", "--5", "1:30", "NaN", "inf", "+-3"] {
            assert!(parse_seek(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn sources_as_the_user_writes_them() {
        assert_eq!(
            resolve_source("tidal:233059491").unwrap(),
            "tidal:233059491"
        );
        assert_eq!(
            resolve_source("233059491").unwrap(),
            "tidal:233059491",
            "a bare number is a TIDAL id"
        );
        assert_eq!(
            resolve_source("file:/music/a.flac").unwrap(),
            "file:/music/a.flac"
        );
    }

    #[test]
    fn a_path_becomes_an_absolute_file_source() {
        assert_eq!(
            resolve_source("/music/a track.flac").unwrap(),
            "file:/music/a track.flac"
        );

        // A relative path is resolved against where the user is, whether or not the file exists:
        // it is the daemon's job to say a file can't be played.
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            resolve_source("missing.flac").unwrap(),
            format!("file:{}", cwd.join("missing.flac").display())
        );
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
            items: vec![
                item(10, Some("One"), Some(65_000)),
                item(11, None, None),
                item(12, Some("Three"), Some(3_725_000)),
            ],
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
            assert!(
                error.to_string().contains("no queue entry"),
                "{bad}: {error}"
            );
        }
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
            track: Some(Track {
                item_id: Some(ItemId(10)),
                source: Some("tidal:1".into()),
                title: Some("Song".into()),
                duration_ms: Some(348_680),
                quality: None,
            }),
            spec: Some(Spec {
                sample_rate: 192_000,
                channels: 2,
                bits_per_sample: 24,
            }),
            position_ms: 83_000,
            duration_ms: Some(348_680),
            output: Output::Open,
            route: None,
            volume: None,
            quality_range: None,
        };
        assert_eq!(
            format_status(&status),
            "State:    playing\nTrack:    Song\nPosition: 1:23 / 5:48\nFormat:   24-bit / 192000 Hz / 2 ch"
        );
        let idle = Status {
            state: State::Stopped,
            track: None,
            spec: None,
            position_ms: 0,
            duration_ms: None,
            output: Output::Closed,
            route: None,
            volume: None,
            quality_range: None,
        };
        assert_eq!(format_status(&idle), "State:    stopped");

        let released = Status {
            state: State::Paused,
            output: Output::Released {
                by: Some("jackd".into()),
            },
            ..idle.clone()
        };
        assert_eq!(
            format_status(&released),
            "State:    paused (DAC released to jackd)"
        );
        let released = Status {
            output: Output::Released { by: None },
            ..released
        };
        assert_eq!(format_status(&released), "State:    paused (DAC released)");
    }

    #[test]
    fn the_result_of_adding_lists_what_went_wrong() {
        let payload = Payload::Added {
            ids: vec![ItemId(1)],
            rejected: vec![phonia_ipc::Rejected {
                source: "file:/x.flac".into(),
                reason: "no such file".into(),
            }],
            unresolved: vec![phonia_ipc::Unresolved {
                id: ItemId(1),
                reason: "TIDAL unreachable".into(),
            }],
        };
        assert_eq!(
            format_added(&payload),
            "added 1 tracks\n  not added: file:/x.flac (no such file)\n  added, but its details could not be fetched: TIDAL unreachable"
        );
    }

    #[test]
    fn events_read_as_one_line_each() {
        assert_eq!(
            format_event(&Event::StateChanged {
                state: State::Paused
            }),
            "state paused"
        );
        assert_eq!(
            format_event(&Event::Position {
                position_ms: 61_000,
                duration_ms: Some(120_000)
            }),
            "position 1:01 / 2:00"
        );
        assert_eq!(format_event(&Event::QueueExhausted), "end of the queue");
        assert_eq!(
            format_event(&Event::OutputReleased {
                by: Some("jackd".into()),
                reason: ReleaseReason::Requested
            }),
            "DAC released to jackd (another program asked for it)"
        );
        assert_eq!(
            format_event(&Event::OutputReleased {
                by: None,
                reason: ReleaseReason::Idle
            }),
            "DAC released (paused for a while)"
        );
        assert_eq!(format_event(&Event::OutputAcquired), "DAC taken again");
        assert_eq!(
            format_event(&Event::Unknown),
            "(an event this client does not know)"
        );
    }

    fn out(
        id: &str,
        mode: OutputMode,
        name: &str,
        bit_perfect: bool,
        lossy: bool,
        codec: Option<&str>,
    ) -> OutputInfo {
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
            out(
                "exclusive:hw:DS2,0",
                OutputMode::Exclusive,
                "Fosi Audio DS2",
                true,
                false,
                None,
            ),
            out(
                "shared:alsa_output.usb-DS2",
                OutputMode::Shared,
                "Fosi Audio DS2 Analog Stereo",
                false,
                false,
                None,
            ),
            out(
                "shared:bluez_output.AA",
                OutputMode::Shared,
                "Soundcore Life P2",
                false,
                true,
                Some("SBC"),
            ),
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
        assert_eq!(
            resolve_output("2", &all).unwrap(),
            "shared:alsa_output.usb-DS2"
        );
        assert_eq!(
            resolve_output("exclusive:hw:DS2,0", &all).unwrap(),
            "exclusive:hw:DS2,0"
        );
        assert_eq!(
            resolve_output("soundcore", &all).unwrap(),
            "shared:bluez_output.AA"
        );
        assert_eq!(
            resolve_output("bluez", &all).unwrap(),
            "shared:bluez_output.AA"
        );
    }

    #[test]
    fn a_wrong_or_ambiguous_output_is_explained() {
        let all = outputs();
        for (wanted, expect) in [
            ("0", "no output 0"),
            ("9", "no output 9"),
            ("nothing", "no output matches"),
            ("ds2", "matches 2 outputs"),
        ] {
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
            volume: Some(Volume {
                percent: 72,
                muted: false,
            }),
            quality_range: None,
        };
        assert_eq!(
            format_status(&status),
            "State:    playing\nOutput:   Soundcore Life P2 [shared, not bit-perfect]\nVolume:   72%"
        );
    }

    #[test]
    fn output_events_and_shared_reports_read_naturally() {
        let route = phonia_ipc::Route {
            id: "shared:x".into(),
            mode: OutputMode::Shared,
            description: "Speaker".into(),
        };
        assert_eq!(
            format_event(&Event::OutputChanged { route }),
            "output now Speaker (shared:x)"
        );
        assert!(format_event(&Event::OutputsChanged).contains("outputs changed"));
        assert_eq!(
            format_event(&Event::OutputReleased {
                by: None,
                reason: ReleaseReason::Lost
            }),
            "DAC released (the output went away)"
        );

        let report = |mode, bit_perfect, codec: Option<&str>, lossy, resampled| {
            Event::SinkReport(phonia_ipc::SinkReport {
                device: "Soundcore Life P2".into(),
                source: Spec {
                    sample_rate: 96_000,
                    channels: 2,
                    bits_per_sample: 24,
                },
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
            format_event(&report(
                OutputMode::Shared,
                false,
                Some("SBC"),
                true,
                Some(48_000)
            )),
            "output Soundcore Life P2: S32LE SHARED, LOSSY CODEC (SBC)"
        );
        assert_eq!(
            format_event(&report(
                OutputMode::Shared,
                false,
                None,
                false,
                Some(48_000)
            )),
            "output Soundcore Life P2: S32LE SHARED (not bit-perfect, resampled to 48000 Hz)"
        );
        assert_eq!(
            format_event(&report(OutputMode::Exclusive, true, None, false, None)),
            "output Soundcore Life P2: S32LE BIT-PERFECT"
        );
    }

    #[test]
    fn volumes_are_set_or_changed_within_zero_to_a_hundred() {
        assert_eq!(parse_volume("60", 20).unwrap(), 60);
        assert_eq!(parse_volume("60%", 20).unwrap(), 60);
        assert_eq!(
            parse_volume("+5", 98).unwrap(),
            100,
            "never above unity gain"
        );
        assert_eq!(parse_volume("-5", 3).unwrap(), 0);
        assert_eq!(parse_volume("-10", 50).unwrap(), 40);
        assert_eq!(parse_volume("250", 50).unwrap(), 100);
        for bad in ["", "loud", "1.5", "+", "--3"] {
            assert!(parse_volume(bad, 50).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_volume_and_volume_events_read_simply() {
        assert_eq!(
            format_volume(&Volume {
                percent: 72,
                muted: false
            }),
            "Volume: 72%"
        );
        assert_eq!(
            format_volume(&Volume {
                percent: 72,
                muted: true
            }),
            "Volume: 72% (muted)"
        );
        assert_eq!(
            format_event(&Event::VolumeChanged {
                percent: 40,
                muted: true
            }),
            "volume 40% (muted)"
        );
    }

    #[test]
    fn a_track_that_started_with_no_gap_is_marked() {
        let started = |gapless| Event::TrackStarted {
            item_id: None,
            source: Some("tidal:1".into()),
            title: Some("Song".into()),
            duration_ms: None,
            spec: Spec {
                sample_rate: 96_000,
                channels: 2,
                bits_per_sample: 24,
            },
            gapless,
            quality: None,
        };
        assert_eq!(
            format_event(&started(false)),
            "started Song (24-bit / 96000 Hz)"
        );
        assert_eq!(
            format_event(&started(true)),
            "started Song (24-bit / 96000 Hz) [gapless]"
        );
    }

    #[test]
    fn a_track_that_fell_back_says_what_was_asked_in_events() {
        let fell = phonia_ipc::StreamQuality {
            requested: Quality::Hires,
            delivered: Quality::Lossless,
        };

        let started = |quality| Event::TrackStarted {
            item_id: None,
            source: None,
            title: Some("Song".into()),
            duration_ms: None,
            spec: Spec {
                sample_rate: 44_100,
                channels: 2,
                bits_per_sample: 16,
            },
            gapless: true,
            quality: Some(quality),
        };
        assert_eq!(
            format_event(&started(fell)),
            "started Song (16-bit / 44100 Hz) [lossless (asked for hires)] [gapless]"
        );
        assert_eq!(
            format_event(&Event::MaxQualityChanged {
                quality: Quality::Lossless
            }),
            "best quality asked for is now lossless"
        );
    }

    #[test]
    fn the_quality_command_shows_the_range_and_the_track() {
        let mut status = Status {
            state: State::Playing,
            track: Some(Track {
                item_id: None,
                source: Some("tidal:1".into()),
                title: None,
                duration_ms: None,
                quality: Some(phonia_ipc::StreamQuality {
                    requested: Quality::Hires,
                    delivered: Quality::Lossless,
                }),
            }),
            spec: None,
            position_ms: 0,
            duration_ms: None,
            output: Output::Open,
            route: None,
            volume: None,
            quality_range: Some(QualityRange {
                max: Quality::Hires,
                min: Quality::Lossless,
            }),
        };
        assert_eq!(
            format_quality_status(&status),
            "Quality: asking for up to hires, playing nothing below lossless\nPlaying: lossless (asked for hires)"
        );
        status.quality_range = None;
        status.track = None;
        assert!(format_quality_status(&status).contains("does not play from TIDAL"));
    }

    #[test]
    fn a_search_prints_a_section_per_kind_with_what_to_give_the_other_commands() {
        use phonia_ipc::{AlbumRef, ArtistRef, ArtistSummary, Page, TrackSummary};
        let payload = Payload::SearchResults {
            query: "korn".into(),
            tracks: Some(Page {
                items: vec![TrackSummary {
                    id: "33723914".into(),
                    title: "Here to Stay".into(),
                    version: None,
                    artists: vec![ArtistRef {
                        id: "780".into(),
                        name: "Korn".into(),
                    }],
                    album: Some(AlbumRef {
                        id: "9".into(),
                        title: "Untouchables".into(),
                        cover: None,
                    }),
                    duration_ms: Some(271_000),
                    explicit: false,
                    track_number: Some(2),
                    volume_number: None,
                    quality: Some(Quality::Hires),
                    streamable: true,
                }],
                total: 123,
                offset: 50,
            }),
            albums: Some(Page {
                items: vec![],
                total: 0,
                offset: 0,
            }),
            artists: Some(Page {
                items: vec![ArtistSummary {
                    id: "780".into(),
                    name: "Korn".into(),
                    picture: None,
                }],
                total: 1,
                offset: 0,
            }),
            playlists: None,
        };
        assert_eq!(
            format_search(&payload),
            "Search for \"korn\":\n\
             \nTracks (1 of 123):\n  51. Korn - Here to Stay - Untouchables - 4:31 - hires   tidal:33723914\n\
             \nAlbums (0 of 0):\n  none\n\
             \nArtists (1 of 1):\n   1. Korn   artist 780"
        );
    }

    #[test]
    fn an_album_or_a_playlist_is_told_from_tracks_and_must_be_alone() {
        let sources = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            catalog_source(&sources(&["album:33723912"])).unwrap(),
            Some(CatalogRef::Album {
                id: "33723912".into()
            })
        );
        assert_eq!(
            catalog_source(&sources(&["playlist:5545fb2d-fd50"])).unwrap(),
            Some(CatalogRef::Playlist {
                id: "5545fb2d-fd50".into()
            })
        );
        assert_eq!(
            catalog_source(&sources(&["tidal:1", "12345", "/music/a.flac"])).unwrap(),
            None,
            "plain tracks are not a catalog addition"
        );
        for mixed in [
            sources(&["album:1", "tidal:2"]),
            sources(&["tidal:2", "album:1"]),
            sources(&["album:1", "album:2"]),
        ] {
            assert!(catalog_source(&mixed).is_err(), "{mixed:?}");
        }
    }

    fn album_track(id: &str, title: &str, number: u32, disc: u32) -> phonia_ipc::TrackSummary {
        phonia_ipc::TrackSummary {
            id: id.into(),
            title: title.into(),
            version: None,
            artists: vec![],
            album: None,
            duration_ms: Some(75_000),
            explicit: false,
            track_number: Some(number),
            volume_number: Some(disc),
            quality: Some(Quality::Hires),
            streamable: true,
        }
    }

    fn an_album(tracks: Vec<phonia_ipc::TrackSummary>, total: u64) -> Payload {
        Payload::Album {
            album: phonia_ipc::AlbumSummary {
                id: "9".into(),
                title: "Issues".into(),
                version: None,
                artists: vec![phonia_ipc::ArtistRef {
                    id: "780".into(),
                    name: "Korn".into(),
                }],
                release_date: Some("1999-11-16".into()),
                track_count: Some(total as u32),
                duration_ms: Some(3_200_000),
                explicit: false,
                quality: Some(Quality::Hires),
                kind: Some(phonia_ipc::AlbumKind::Album),
                copyright: Some("(P) 1999 Sony".into()),
                cover: None,
            },
            tracks: phonia_ipc::Page {
                items: tracks,
                total,
                offset: 0,
            },
        }
    }

    #[test]
    fn an_album_prints_its_details_and_its_tracks_as_numbered_on_the_album() {
        let text = format_album(&an_album(
            vec![
                album_track("1", "Dead", 1, 1),
                album_track("2", "Trash", 2, 1),
            ],
            2,
        ));
        assert_eq!(
            text,
            "Korn - Issues - 1999 - 2 tracks - hires   album 9\n\
             Length: 53:20\n\
             (P) 1999 Sony\n\
             \n   1. Dead - 1:15 - hires   tidal:1\n   2. Trash - 1:15 - hires   tidal:2"
        );
        assert!(!text.contains("Disc"), "one disc needs no heading");
        assert!(!text.contains("Cover:"), "no cover id, no line");
    }

    #[test]
    fn an_album_and_an_artist_print_a_cover_url_when_they_have_one() {
        use phonia_ipc::ArtistSummary;
        let Payload::Album { mut album, tracks } = an_album(vec![], 0) else {
            panic!("not an album")
        };
        album.cover = Some("3c6247c7-d0d7-4978-91b1-0bddc13f45b5".into());
        let text = format_album(&Payload::Album { album, tracks });
        assert!(
            text.contains(
                "Cover: https://resources.tidal.com/images/3c6247c7/d0d7/4978/91b1/0bddc13f45b5/640x640.jpg"
            ),
            "{text}"
        );

        let payload = Payload::Artist {
            artist: ArtistSummary {
                id: "780".into(),
                name: "Korn".into(),
                picture: Some("ca8a29d3-efcd-4cd2-8dea-a376e1c64b1e".into()),
            },
            bio: None,
            top_tracks: phonia_ipc::Page {
                items: vec![],
                total: 0,
                offset: 0,
            },
            albums: phonia_ipc::Page {
                items: vec![],
                total: 0,
                offset: 0,
            },
            singles: phonia_ipc::Page {
                items: vec![],
                total: 0,
                offset: 0,
            },
        };
        let text = format_artist(&payload);
        assert!(
            text.contains(
                "Picture: https://resources.tidal.com/images/ca8a29d3/efcd/4cd2/8dea/a376e1c64b1e/750x750.jpg"
            ),
            "{text}"
        );
    }

    #[test]
    fn an_album_of_several_discs_has_a_heading_for_each() {
        let text = format_album(&an_album(
            vec![
                album_track("1", "One", 1, 1),
                album_track("2", "Two", 2, 1),
                album_track("3", "Three", 1, 2),
            ],
            3,
        ));
        assert!(text.contains("\n\nDisc 1:\n   1. One"), "{text}");
        assert!(text.contains("\n\nDisc 2:\n   1. Three"), "{text}");
    }

    #[test]
    fn a_long_album_says_how_many_tracks_it_has_beyond_the_page_and_how_to_add_them_all() {
        let text = format_album(&an_album(vec![album_track("1", "One", 1, 1)], 120));
        assert!(
            text.ends_with("Showing 1 of 120 tracks (`queue add album:9` adds them all)."),
            "{text}"
        );
    }

    #[test]
    fn an_artist_prints_a_short_bio_and_a_section_for_each_list() {
        use phonia_ipc::{AlbumKind, AlbumSummary, ArtistSummary, Page};
        let single = AlbumSummary {
            id: "11".into(),
            title: "Freak".into(),
            version: None,
            artists: vec![],
            release_date: Some("1999-01-01".into()),
            track_count: Some(1),
            duration_ms: None,
            explicit: false,
            quality: None,
            kind: Some(AlbumKind::Single),
            copyright: None,
            cover: None,
        };
        let payload = Payload::Artist {
            artist: ArtistSummary {
                id: "780".into(),
                name: "Korn".into(),
                picture: None,
            },
            bio: Some(format!("{}\n\nA second paragraph.", "x".repeat(500))),
            top_tracks: Page {
                items: vec![album_track("1", "Blind", 1, 1)],
                total: 300,
                offset: 0,
            },
            albums: Page {
                items: vec![],
                total: 0,
                offset: 0,
            },
            singles: Page {
                items: vec![single],
                total: 30,
                offset: 0,
            },
        };
        let text = format_artist(&payload);
        assert!(text.starts_with("Korn   artist 780\n\nxxx"), "{text}");
        assert!(
            text.contains("...\n\nTop tracks (1 of 300):"),
            "the bio is cut: {text}"
        );
        assert!(
            !text.contains("second paragraph"),
            "only the first paragraph: {text}"
        );
        assert!(text.contains("Albums (0 of 0):\n  none"), "{text}");
        assert!(
            text.contains(
                "EPs and singles (1 of 30):\n   1. Freak - 1999 - single - 1 track   album 11"
            ),
            "{text}"
        );
    }

    #[test]
    fn an_artist_without_a_bio_goes_straight_to_its_lists() {
        use phonia_ipc::{ArtistSummary, Page};
        fn empty<T>() -> Page<T> {
            Page {
                items: vec![],
                total: 0,
                offset: 0,
            }
        }
        let text = format_artist(&Payload::Artist {
            artist: ArtistSummary {
                id: "1".into(),
                name: "Nobody".into(),
                picture: None,
            },
            bio: None,
            top_tracks: empty(),
            albums: empty(),
            singles: empty(),
        });
        assert!(
            text.starts_with("Nobody   artist 1\n\nTop tracks (0 of 0):"),
            "{text}"
        );
    }

    #[test]
    fn a_library_prints_a_section_for_each_of_its_three_lists() {
        use phonia_ipc::{AlbumKind, AlbumSummary, Page};
        let payload = Payload::Library {
            favorite_tracks: Page {
                items: vec![album_track("1", "Blind", 1, 1)],
                total: 1,
                offset: 0,
            },
            favorite_albums: Page {
                items: vec![AlbumSummary {
                    id: "9".into(),
                    title: "Issues".into(),
                    version: None,
                    artists: vec![],
                    release_date: Some("1999-11-16".into()),
                    track_count: Some(16),
                    duration_ms: None,
                    explicit: false,
                    quality: None,
                    kind: Some(AlbumKind::Album),
                    copyright: None,
                    cover: None,
                }],
                total: 1,
                offset: 0,
            },
            my_playlists: Page {
                items: vec![],
                total: 0,
                offset: 0,
            },
        };
        let text = format_library(&payload);
        assert!(
            text.starts_with("Library:\n\nFavorite tracks (1 of 1):"),
            "{text}"
        );
        assert!(
            text.contains("Favorite albums (1 of 1):\n   1. Issues - 1999 - 16 tracks   album 9"),
            "{text}"
        );
        assert!(text.contains("Your playlists (0 of 0):\n  none"), "{text}");
    }

    #[test]
    fn a_playlist_of_a_library_prints_like_one_from_a_search() {
        use phonia_ipc::{Page, PlaylistSummary};
        fn empty<T>() -> Page<T> {
            Page {
                items: vec![],
                total: 0,
                offset: 0,
            }
        }
        let text = format_library(&Payload::Library {
            favorite_tracks: empty(),
            favorite_albums: empty(),
            my_playlists: Page {
                items: vec![PlaylistSummary {
                    id: "p-1".into(),
                    title: "Road trip".into(),
                    creator: Some("hectmor".into()),
                    description: None,
                    track_count: Some(10),
                    duration_ms: None,
                    cover: None,
                }],
                total: 1,
                offset: 0,
            },
        });
        assert!(
            text.contains(
                "Your playlists (1 of 1):\n   1. Road trip - hectmor - 10 tracks   playlist p-1"
            ),
            "{text}"
        );
    }
}
