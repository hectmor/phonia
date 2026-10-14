//! A client for the daemon: connects, does the handshake, matches responses to requests and hands
//! out the event stream.
//!
//! There is deliberately no automatic reconnection: what to do when the daemon goes away (retry,
//! tell the user, start it) is the application's call. [`Client::closed`] says when it happens.

use crate::dto::{Queue, Status};
use crate::framing::{self, FrameError};
use crate::proto::{
    ClientInfo, ClientMessage, Event, PROTOCOL, Payload, ProtocolError, Reply, Request, RequestId,
    ServerHello, ServerMessage,
};
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{broadcast, oneshot, watch};

/// Events held for a reader that is slow to pick them up.
const EVENT_BACKLOG: usize = 1024;

#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    /// The daemon answered with an error.
    Protocol(ProtocolError),
    /// The connection is gone.
    Closed,
    /// The daemon did not behave like a phonia daemon of a compatible version.
    Handshake(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Io(error) => write!(f, "{error}"),
            ClientError::Protocol(error) => write!(f, "{}", error.message),
            ClientError::Closed => write!(f, "the connection to the daemon was closed"),
            ClientError::Handshake(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(error: io::Error) -> Self {
        ClientError::Io(error)
    }
}

impl From<FrameError> for ClientError {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::Io(error) => ClientError::Io(error),
            FrameError::TooLarge => ClientError::Handshake(error.to_string()),
        }
    }
}

/// The state at the moment of subscribing; events with a higher `seq` follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub seq: u64,
    pub status: Status,
    pub queue: Queue,
}

type Writer = Box<dyn AsyncWrite + Unpin + Send>;

struct Inner {
    writer: tokio::sync::Mutex<Writer>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    next_id: AtomicU64,
    events: broadcast::Sender<(u64, Event)>,
    server: ServerHello,
    closed: watch::Receiver<bool>,
}

/// A connection to the daemon. Cheap to clone; all clones share the connection.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl Client {
    /// Connects to the daemon's socket (the default path if none is given).
    pub async fn connect(path: Option<&Path>, info: ClientInfo) -> Result<Client, ClientError> {
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::socket::default_socket_path);
        let stream = tokio::net::UnixStream::connect(&path)
            .await
            .map_err(|error| {
                ClientError::Io(io::Error::new(
                    error.kind(),
                    format!(
                        "could not connect to the daemon at {}: {error}",
                        path.display()
                    ),
                ))
            })?;
        Client::from_stream(stream, info).await
    }

    /// Runs the protocol over any stream (a socket, or a pipe in tests).
    pub async fn from_stream<S>(stream: S, info: ClientInfo) -> Result<Client, ClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (read_half, write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);

        let mut buffer = Vec::new();
        let banner = framing::read_frame(&mut reader, &mut buffer)
            .await?
            .ok_or(ClientError::Closed)?;
        let server = match serde_json::from_slice::<ServerMessage>(banner) {
            Ok(ServerMessage::Hello(hello)) => hello,
            _ => {
                return Err(ClientError::Handshake(
                    "the peer did not greet like a phonia daemon".into(),
                ));
            }
        };
        if !PROTOCOL.compatible_with(server.protocol) {
            return Err(ClientError::Handshake(format!(
                "the daemon speaks protocol {}.{}, this client {}.{}",
                server.protocol.major, server.protocol.minor, PROTOCOL.major, PROTOCOL.minor
            )));
        }

        let (events, _) = broadcast::channel(EVENT_BACKLOG);
        let (closed_tx, closed) = watch::channel(false);
        let inner = Arc::new(Inner {
            writer: tokio::sync::Mutex::new(Box::new(write_half)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            events,
            server,
            closed,
        });
        tokio::spawn(read_loop(reader, inner.clone(), closed_tx));

        let client = Client { inner };
        client
            .request(Request::Hello {
                protocol: PROTOCOL,
                client: info,
            })
            .await?;
        Ok(client)
    }

    /// What the daemon said about itself when the connection opened.
    pub fn server(&self) -> &ServerHello {
        &self.inner.server
    }

    /// Sends a request and waits for its answer.
    pub async fn request(&self, request: Request) -> Result<Payload, ClientError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, answer);

        let bytes = serde_json::to_vec(&ClientMessage {
            id: RequestId(id),
            request,
        })
        .map_err(|error| ClientError::Handshake(error.to_string()))?;
        let written = {
            let mut writer = self.inner.writer.lock().await;
            framing::write_frame(&mut *writer, &bytes).await
        };
        if let Err(error) = written {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(ClientError::Io(error));
        }

        match answered.await {
            Ok(Reply::Ok(payload)) => Ok(payload),
            Ok(Reply::Err(error)) => Err(ClientError::Protocol(error)),
            Err(_) => Err(ClientError::Closed),
        }
    }

    pub async fn status(&self) -> Result<Status, ClientError> {
        match self.request(Request::Status).await? {
            Payload::Status(status) => Ok(status),
            other => Err(unexpected("status", &other)),
        }
    }

    pub async fn queue(&self) -> Result<Queue, ClientError> {
        match self.request(Request::Queue).await? {
            Payload::Queue(queue) => Ok(queue),
            other => Err(unexpected("queue", &other)),
        }
    }

    /// Starts receiving events. The snapshot is what to render first; the stream carries everything
    /// after it, in order and without gaps.
    pub async fn subscribe(&self) -> Result<(Snapshot, EventStream), ClientError> {
        // Listen before asking, so no event can fall between the snapshot and the first read.
        let events = self.inner.events.subscribe();
        let snapshot = self.resubscribe().await?;
        let stream = EventStream {
            events,
            seen: snapshot.seq,
            client: self.clone(),
        };
        Ok((snapshot, stream))
    }

    async fn resubscribe(&self) -> Result<Snapshot, ClientError> {
        match self.request(Request::Subscribe).await? {
            Payload::Snapshot { seq, status, queue } => Ok(Snapshot { seq, status, queue }),
            other => Err(unexpected("snapshot", &other)),
        }
    }

    /// Completes when the connection is gone.
    pub async fn closed(&self) {
        let mut closed = self.inner.closed.clone();
        let _ = closed.wait_for(|gone| *gone).await;
    }
}

fn unexpected(wanted: &str, got: &Payload) -> ClientError {
    ClientError::Handshake(format!("expected a {wanted} answer, got {got:?}"))
}

/// The daemon's events, in order.
pub struct EventStream {
    events: broadcast::Receiver<(u64, Event)>,
    /// The sequence number of the last event handed out.
    seen: u64,
    client: Client,
}

impl EventStream {
    /// The next event, or `None` once the connection is gone. If this reader fell too far behind
    /// to have every event, the answer is a `Resync` with the state as it is now.
    pub async fn next(&mut self) -> Option<Event> {
        self.next_seq().await.map(|(_, event)| event)
    }

    /// Like [`EventStream::next`], with the event's sequence number: consecutive numbers mean no
    /// event was missed.
    pub async fn next_seq(&mut self) -> Option<(u64, Event)> {
        loop {
            match self.events.recv().await {
                Ok((seq, _)) if seq <= self.seen => continue,
                Ok((seq, event)) => {
                    self.seen = seq;
                    return Some((seq, event));
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    let snapshot = self.client.resubscribe().await.ok()?;
                    self.seen = snapshot.seq;
                    let event = Event::Resync {
                        skipped,
                        seq: snapshot.seq,
                        status: snapshot.status,
                        queue: snapshot.queue,
                    };
                    return Some((snapshot.seq, event));
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

async fn read_loop<R: AsyncBufRead + Unpin>(
    mut reader: R,
    inner: Arc<Inner>,
    closed: watch::Sender<bool>,
) {
    let mut buffer = Vec::new();
    while let Ok(Some(bytes)) = framing::read_frame(&mut reader, &mut buffer).await {
        match serde_json::from_slice::<ServerMessage>(bytes) {
            Ok(ServerMessage::Response { id, reply }) => {
                if let Some(answer) = inner.pending.lock().unwrap().remove(&id.0) {
                    let _ = answer.send(reply);
                }
            }
            Ok(ServerMessage::Event { seq, event }) => {
                // Nobody listening is not an error.
                let _ = inner.events.send((seq, event));
            }
            Ok(ServerMessage::Hello(_)) | Err(_) => {} // a message this client can't use: ignore it
        }
    }
    // Dropping the waiting senders makes every pending request fail with `Closed`.
    inner.pending.lock().unwrap().clear();
    let _ = closed.send(true);
}
