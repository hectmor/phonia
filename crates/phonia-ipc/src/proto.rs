//! The messages: what a client may ask, and what the daemon answers and announces.

use crate::dto::{
    AlbumListRef, AlbumSummary, ArtistSummary, CatalogKind, CatalogRef, EndReason, FolderEntry,
    ItemId, Lyrics, OutputInfo, Page, PlaylistListRef, PlaylistSummary, Quality, Queue,
    ReleaseReason, Repeat, ReplayGain, Route, SinkReport, Spec, State, Status, StreamQuality,
    TrackSummary,
};
use serde::{Deserialize, Serialize};

/// Capability: the daemon understands `release` and reports `output` (protocol 1.1).
pub const CAP_OUTPUT_RELEASE: &str = "output_release";

/// Capability: the daemon lists its outputs and can switch between them (protocol 1.2).
pub const CAP_OUTPUT_SELECT: &str = "output_select";

/// Capability: the daemon has a volume it can set on outputs that allow it (protocol 1.3).
pub const CAP_VOLUME: &str = "volume";

/// Capability: the daemon joins tracks of the same format with no gap, and says so in
/// `track_started` (protocol 1.4).
pub const CAP_GAPLESS: &str = "gapless";

/// Capability: the daemon says which quality TIDAL delivered, and the best tier to ask for can be
/// changed while it runs (protocol 1.5).
pub const CAP_QUALITY: &str = "quality";

/// Capability: the daemon can search TIDAL's catalog (protocol 1.6). Only advertised by a daemon
/// that has a TIDAL login to do it with.
pub const CAP_CATALOG: &str = "catalog";

/// Capability: the daemon can fetch a track's lyrics from TIDAL, synced or plain (protocol 1.10).
/// Only advertised alongside [`CAP_CATALOG`].
pub const CAP_LYRICS: &str = "lyrics";

/// The protocol version this crate speaks.
pub const PROTOCOL: Version = Version {
    major: 1,
    minor: 12,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl Version {
    /// Two versions can talk when their major numbers agree.
    pub fn compatible_with(self, other: Version) -> bool {
        self.major == other.major
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub pid: u32,
}

/// Identifies a request; its response carries the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

/// What the server says when a connection opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    pub protocol: Version,
    pub server: ServerInfo,
    /// Optional features this daemon has, so clients can adapt (see [`CAP_OUTPUT_RELEASE`]).
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientMessage {
    pub id: RequestId,
    pub request: Request,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SeekTarget {
    Absolute { ms: u64 },
    Forward { ms: u64 },
    Backward { ms: u64 },
}

/// A track to add to the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTrack {
    /// `file:/abs/path` or `tidal:<id>` (see [`crate::source`]).
    pub source: String,
}

/// Where new entries go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AddAt {
    /// After the last entry.
    #[default]
    End,
    /// Right after the one playing.
    Next,
    /// At this position (clamped).
    Index { index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Must come first. Announces the protocol version the client speaks.
    Hello {
        protocol: Version,
        client: ClientInfo,
    },
    Status,
    Queue,
    /// Start receiving events. The answer is a [`Payload::Snapshot`] to render immediately;
    /// events with a higher `seq` follow.
    Subscribe,
    Unsubscribe,
    /// Play an entry now, or (without one) start from the queue.
    Play {
        item: Option<ItemId>,
    },
    Stop,
    Pause,
    Resume,
    TogglePause,
    Next,
    Previous,
    Seek {
        target: SeekTarget,
    },
    /// Pauses and hands the audio device back so another program can use it (since 1.1).
    /// `resume` takes it again.
    Release,
    /// The outputs the daemon can play on, and the one it is playing on (since 1.2).
    Outputs,
    /// Plays through another output from now on, keeping the track and the position (since 1.2).
    /// `output` is an id from `outputs`.
    SetOutput {
        output: String,
    },
    /// Sets the volume, 0 to 100 (since 1.3). Refused for an output with no volume of its own to
    /// set: an exclusive card with no hardware mixer control.
    SetVolume {
        percent: u8,
    },
    /// Mutes or unmutes (since 1.3). Refused like `set_volume`.
    SetMute {
        mute: bool,
    },
    /// Sets the best tier to ask TIDAL for, from the next track opened on (since 1.5). The track
    /// playing, and one already opened ahead, keep theirs. Refused below the daemon's minimum.
    SetMaxQuality {
        quality: Quality,
    },
    /// Searches TIDAL's catalog (since 1.6). `kinds` empty means all of them; `offset` and `limit`
    /// page each kind (`limit` defaults to 50 and is capped at 300). Answered with
    /// [`Payload::SearchResults`], where a kind that was not asked for is absent.
    Search {
        query: String,
        #[serde(default)]
        kinds: Vec<CatalogKind>,
        #[serde(default)]
        offset: u32,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// An album: its details and its tracks (since 1.6). Answered with [`Payload::Album`].
    /// `limit` (default and most 100) is how many tracks come in the first page.
    Album {
        id: String,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// An artist, all that a view of it shows, in one answer (since 1.6): its bio, its most
    /// listened to tracks, its albums, and its EPs and singles. Answered with
    /// [`Payload::Artist`]; `limit` (default 50, most 100) is the size of each list's first page.
    Artist {
        id: String,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// One page of a list of tracks (since 1.6): to go on after the first page of an album, or of
    /// an artist's top tracks. Answered with [`Payload::Tracks`].
    Tracks {
        from: CatalogRef,
        #[serde(default)]
        offset: u32,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// One page of a list of albums (since 1.6). Answered with [`Payload::Albums`].
    Albums {
        from: AlbumListRef,
        #[serde(default)]
        offset: u32,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// One page of a list of playlists (since 1.6). Answered with [`Payload::Playlists`].
    Playlists {
        from: PlaylistListRef,
        #[serde(default)]
        offset: u32,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// The library, all that a view of it shows, in one answer (since 1.6): the first page of the
    /// user's favorite tracks, of their favorite albums, and of their own playlists. Answered with
    /// [`Payload::Library`]; `limit` (default 50, most 100) is the size of each list's first page.
    Library {
        #[serde(default)]
        limit: Option<u32>,
    },
    /// A track's lyrics, synced or plain (since 1.10). Answered with [`Payload::Lyrics`], which
    /// repeats `id` so a client can discard a stale answer after the track has moved on.
    Lyrics {
        id: String,
    },
    /// One page of a playlist folder's own contents, sub-folders and playlists alike (since
    /// 1.11): `folder` is `None` for the root of "My Collection", or `Some(id)` for a sub-folder,
    /// using the id a [`crate::dto::FolderEntry::Folder`] from an earlier page already handed
    /// back. Answered with [`Payload::PlaylistFolder`].
    PlaylistFolder {
        #[serde(default)]
        folder: Option<String>,
        #[serde(default)]
        offset: u32,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// Adds the tracks of an album or a playlist (since 1.6): the daemon lists them from TIDAL
    /// itself, so their titles and lengths come with them, and answers like `queue_add`, with
    /// [`Payload::Added`]. A track TIDAL lists but does not stream where the daemon is comes back
    /// among the `rejected` ones. At most 1000 at once.
    QueueAddFrom {
        from: CatalogRef,
        #[serde(default)]
        at: AddAt,
    },
    /// Adds tracks, resolving their titles and lengths first.
    QueueAdd {
        tracks: Vec<NewTrack>,
        #[serde(default)]
        at: AddAt,
    },
    QueueRemove {
        ids: Vec<ItemId>,
    },
    QueueMove {
        id: ItemId,
        to: usize,
    },
    QueueClear,
    SetShuffle {
        shuffle: bool,
    },
    SetRepeat {
        repeat: Repeat,
    },
    /// Stops the daemon.
    Shutdown,
    /// A request this version does not know; answered with [`ErrorCode::UnknownRequest`].
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The client's major protocol version differs from the server's.
    UnsupportedVersion,
    /// A request other than `hello` came first.
    HandshakeRequired,
    UnknownRequest,
    /// The request was malformed or its arguments make no sense.
    BadRequest,
    BadSource,
    /// The entry the request names does not exist.
    NotFound,
    /// The request is fine but this output can't do it (a volume on a card with no hardware mixer
    /// control). Since 1.3.
    Unsupported,
    /// There is no TIDAL login, or TIDAL no longer accepts it: run `phonia login`. Since 1.6.
    NotLoggedIn,
    /// TIDAL, or the network to it, is not answering. Since 1.6.
    Unavailable,
    /// TIDAL is being asked too often, or the daemon has too many searches under way. Since 1.6.
    RateLimited,
    /// The playback engine is gone.
    EngineGone,
    Internal,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
}

/// A track that was not added, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    pub source: String,
    pub reason: String,
}

/// A track that was added although its title and length could not be found out just now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unresolved {
    pub id: ItemId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Payload {
    Ack,
    Status(Status),
    Queue(Queue),
    /// The outcome of [`Request::QueueAdd`]: tracks that are wrong are refused, the rest are added
    /// (those whose metadata could not be fetched right now are listed in `unresolved`).
    Added {
        ids: Vec<ItemId>,
        rejected: Vec<Rejected>,
        unresolved: Vec<Unresolved>,
    },
    Removed {
        count: usize,
    },
    /// The outputs the daemon can play on, and the id of the current one.
    Outputs {
        outputs: Vec<OutputInfo>,
        current: Option<String>,
    },
    /// What a search found (since 1.6): one page of each kind that was asked for.
    SearchResults {
        query: String,
        #[serde(default)]
        tracks: Option<Page<TrackSummary>>,
        #[serde(default)]
        albums: Option<Page<AlbumSummary>>,
        #[serde(default)]
        artists: Option<Page<ArtistSummary>>,
        #[serde(default)]
        playlists: Option<Page<PlaylistSummary>>,
    },
    /// An album and the first page of its tracks (since 1.6).
    Album {
        album: AlbumSummary,
        tracks: Page<TrackSummary>,
    },
    /// An artist with the first page of each of its lists (since 1.6). `bio` is plain text, and
    /// absent for an artist TIDAL has nothing to say about (or when it could not be fetched: the
    /// rest is worth showing without it).
    Artist {
        artist: ArtistSummary,
        #[serde(default)]
        bio: Option<String>,
        top_tracks: Page<TrackSummary>,
        albums: Page<AlbumSummary>,
        singles: Page<AlbumSummary>,
    },
    /// A page of tracks, and the list it is of (since 1.6).
    Tracks {
        from: CatalogRef,
        page: Page<TrackSummary>,
    },
    /// A page of albums, and the list it is of (since 1.6).
    Albums {
        from: AlbumListRef,
        page: Page<AlbumSummary>,
    },
    /// A page of playlists, and the list it is of (since 1.6).
    Playlists {
        from: PlaylistListRef,
        page: Page<PlaylistSummary>,
    },
    /// The library, with the first page of each of its lists (since 1.6).
    Library {
        favorite_tracks: Page<TrackSummary>,
        favorite_albums: Page<AlbumSummary>,
        my_playlists: Page<PlaylistSummary>,
    },
    /// A track's lyrics, or `None` when TIDAL has none for it at all (since 1.10). `id` is the
    /// track the request asked about, so a client can discard this if it no longer matches the
    /// track playing by the time the answer arrives.
    Lyrics {
        id: String,
        lyrics: Option<Lyrics>,
    },
    /// A page of a playlist folder's contents, and which folder it was (since 1.11). `folder`
    /// repeats what was asked, so a client can tell which folder (root, or which sub-folder) the
    /// page belongs to.
    PlaylistFolder {
        folder: Option<String>,
        page: Page<FolderEntry>,
    },
    /// The state right now, and the sequence number of the last event it includes.
    Snapshot {
        seq: u64,
        status: Status,
        queue: Queue,
    },
    #[serde(other)]
    Unknown,
}

/// The answer to a request: it worked, or here is why not.
// A reply is built, serialized and dropped: boxing the payload would only complicate every caller.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Ok(Payload),
    Err(ProtocolError),
}

/// Built, serialized and dropped, the same as [`Reply`]: boxing `Resync`'s own `Status`/`Queue`
/// would only complicate every caller.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    StateChanged {
        state: State,
    },
    TrackStarted {
        item_id: Option<ItemId>,
        source: Option<String>,
        /// The track's own title; no longer includes the artist (since 1.9).
        title: Option<String>,
        /// The performing artist(s), joined `", "`; see [`crate::dto::Track::artist`] (since 1.9).
        #[serde(default)]
        artist: Option<String>,
        duration_ms: Option<u64>,
        spec: Spec,
        /// The track was joined to the one before it with no gap: playback never stopped, so no
        /// `state_changed` came between them (since 1.4).
        #[serde(default)]
        gapless: bool,
        /// What TIDAL delivered, for a track streamed from TIDAL (since 1.5).
        #[serde(default)]
        quality: Option<StreamQuality>,
        /// The track's album's cover id, to turn into a URL with [`crate::image::url`] (since
        /// 1.6).
        #[serde(default)]
        cover: Option<String>,
        /// The gain ReplayGain decided for this track; see [`crate::dto::Track::replay_gain`]
        /// (since 1.8).
        #[serde(default)]
        replay_gain: Option<ReplayGain>,
    },
    TrackEnded {
        item_id: Option<ItemId>,
        reason: EndReason,
    },
    Position {
        position_ms: u64,
        duration_ms: Option<u64>,
    },
    Seeked {
        position_ms: u64,
    },
    SeekRejected {
        reason: String,
    },
    QueueChanged {
        queue: Queue,
    },
    QueueExhausted,
    /// The daemon paused and gave the audio device back; the track and position are kept.
    OutputReleased {
        by: Option<String>,
        reason: ReleaseReason,
    },
    /// It took the device again.
    OutputAcquired,
    /// The daemon now plays through another output (since 1.2).
    OutputChanged {
        route: Route,
    },
    /// Outputs appeared or disappeared: ask `outputs` again (since 1.2).
    OutputsChanged,
    /// The volume or the mute changed, from a request or from the desktop's mixer (since 1.3).
    VolumeChanged {
        percent: u8,
        muted: bool,
    },
    /// The best tier to ask TIDAL for changed (since 1.5).
    MaxQualityChanged {
        quality: Quality,
    },
    SinkReport(SinkReport),
    Error {
        message: String,
    },
    /// The daemon is stopping.
    ShuttingDown,
    /// This client fell behind and missed `skipped` events; here is the state now.
    Resync {
        skipped: u64,
        seq: u64,
        status: Status,
        queue: Queue,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Sent once, as soon as a client connects.
    Hello(ServerHello),
    Response {
        id: RequestId,
        #[serde(flatten)]
        reply: Reply,
    },
    /// `seq` grows by one with every event, in the same order for every client.
    Event { seq: u64, event: Event },
}
