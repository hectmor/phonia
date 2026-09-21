//! The messages: what a client may ask, and what the daemon answers and announces.

use crate::dto::{ItemId, Queue, Repeat, SinkReport, Spec, State, Status, EndReason};
use serde::{Deserialize, Serialize};

/// The protocol version this crate speaks.
pub const PROTOCOL: Version = Version { major: 1, minor: 0 };

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
    /// Optional features this daemon has, so clients can adapt (none yet).
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
    Hello { protocol: Version, client: ClientInfo },
    Status,
    Queue,
    /// Start receiving events. The answer is a [`Payload::Snapshot`] to render immediately;
    /// events with a higher `seq` follow.
    Subscribe,
    Unsubscribe,
    /// Play an entry now, or (without one) start from the queue.
    Play { item: Option<ItemId> },
    Stop,
    Pause,
    Resume,
    TogglePause,
    Next,
    Previous,
    Seek { target: SeekTarget },
    /// Adds tracks, resolving their titles and lengths first.
    QueueAdd {
        tracks: Vec<NewTrack>,
        #[serde(default)]
        at: AddAt,
    },
    QueueRemove { ids: Vec<ItemId> },
    QueueMove { id: ItemId, to: usize },
    QueueClear,
    SetShuffle { shuffle: bool },
    SetRepeat { repeat: Repeat },
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
    Added { ids: Vec<ItemId>, rejected: Vec<Rejected>, unresolved: Vec<Unresolved> },
    Removed { count: usize },
    /// The state right now, and the sequence number of the last event it includes.
    Snapshot { seq: u64, status: Status, queue: Queue },
    #[serde(other)]
    Unknown,
}

/// The answer to a request: it worked, or here is why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Ok(Payload),
    Err(ProtocolError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    StateChanged { state: State },
    TrackStarted { item_id: Option<ItemId>, source: Option<String>, title: Option<String>, duration_ms: Option<u64>, spec: Spec },
    TrackEnded { item_id: Option<ItemId>, reason: EndReason },
    Position { position_ms: u64, duration_ms: Option<u64> },
    Seeked { position_ms: u64 },
    SeekRejected { reason: String },
    QueueChanged { queue: Queue },
    QueueExhausted,
    SinkReport(SinkReport),
    Error { message: String },
    /// The daemon is stopping.
    ShuttingDown,
    /// This client fell behind and missed `skipped` events; here is the state now.
    Resync { skipped: u64, seq: u64, status: Status, queue: Queue },
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
