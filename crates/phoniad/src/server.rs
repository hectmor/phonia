//! Serving clients: one task per connection, speaking the newline-delimited JSON protocol.
//!
//! A connection has one reader (this task) and one writer task fed by a bounded queue. Responses
//! and events both go through that queue, so a client that stops reading can only ever fill its
//! own queue: the event forwarder then waits, falls behind the daemon's broadcast, and is told to
//! resync, instead of the daemon buffering without limit.

use crate::daemon::{Daemon, wait_for_shutdown};
use phonia_ipc::framing::{FrameError, read_frame, write_frame};
use phonia_ipc::{
    ClientMessage, ErrorCode, Event, Payload, ProtocolError, Reply, Request, RequestId, ServerMessage,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// Messages queued for one client before its event forwarder has to wait.
const OUTBOX: usize = 256;
/// A client that takes longer than this to accept a message is considered gone.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a closing connection is given to flush what it already queued.
const FLUSH_GRACE: Duration = Duration::from_secs(2);

/// Accepts connections until the daemon is asked to stop. Every peer must be the same user as the
/// daemon: the socket's own permissions already say so, this is a second lock on the same door.
pub async fn serve(listener: UnixListener, daemon: Arc<Daemon>) {
    let mut shutdown = daemon.shutdown_signal();
    let uid = phonia_ipc::socket::current_uid();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    if stream.peer_cred().map(|cred| cred.uid()).ok() != Some(uid) {
                        continue; // dropped: not our user
                    }
                    tokio::spawn(serve_connection(stream, daemon.clone()));
                }
                Err(error) => {
                    eprintln!("phoniad: accepting a connection failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            _ = wait_for_shutdown(&mut shutdown) => break,
        }
    }
}

/// Talks to one client until it goes away, misbehaves, or the daemon stops.
pub async fn serve_connection<S>(stream: S, daemon: Arc<Daemon>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let (outbox, queued) = mpsc::channel::<ServerMessage>(OUTBOX);
    let writer = tokio::spawn(write_loop(write_half, queued));

    let mut connection = Connection { daemon: daemon.clone(), outbox: outbox.clone(), handshaken: false, forwarder: None };
    if outbox.send(ServerMessage::Hello(daemon.hello())).await.is_ok() {
        connection.read_loop(BufReader::new(read_half)).await;
    }

    if let Some(forwarder) = connection.forwarder.take() {
        forwarder.abort();
    }
    drop(connection);
    drop(outbox);
    // Let what was already queued (the last response, say) reach the client.
    let _ = tokio::time::timeout(FLUSH_GRACE, writer).await;
}

async fn write_loop<W: AsyncWrite + Unpin>(mut writer: W, mut queued: mpsc::Receiver<ServerMessage>) {
    while let Some(message) = queued.recv().await {
        let Ok(bytes) = serde_json::to_vec(&message) else { continue };
        match tokio::time::timeout(WRITE_TIMEOUT, write_frame(&mut writer, &bytes)).await {
            Ok(Ok(())) => {}
            _ => break, // too slow, or gone
        }
    }
    let _ = writer.shutdown().await;
}

struct Connection {
    daemon: Arc<Daemon>,
    outbox: mpsc::Sender<ServerMessage>,
    /// Whether the client has said `hello` with a compatible version.
    handshaken: bool,
    /// Forwards events to this client, once it has subscribed.
    forwarder: Option<JoinHandle<()>>,
}

impl Connection {
    async fn read_loop<R: tokio::io::AsyncBufRead + Unpin>(&mut self, mut reader: R) {
        let mut shutdown = self.daemon.shutdown_signal();
        let mut buffer = Vec::new();
        loop {
            let frame = tokio::select! {
                frame = read_frame(&mut reader, &mut buffer) => frame,
                _ = wait_for_shutdown(&mut shutdown) => {
                    // The daemon announced it is stopping just before this: let it reach the client.
                    self.let_events_drain().await;
                    return;
                }
            };
            let bytes = match frame {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return, // the client closed the connection
                Err(FrameError::TooLarge) => {
                    self.respond(RequestId(0), error(ErrorCode::BadRequest, "message too large")).await;
                    return;
                }
                Err(FrameError::Io(_)) => return,
            };

            let message: ClientMessage = match serde_json::from_slice(bytes) {
                Ok(message) => message,
                Err(parse_error) => {
                    let id = request_id_of(bytes);
                    let reply = error(ErrorCode::BadRequest, &format!("malformed request: {parse_error}"));
                    self.respond(id, reply).await;
                    continue;
                }
            };
            if !self.handle(message).await {
                return;
            }
        }
    }

    /// Gives the event forwarder a moment to send what it has left, ending with `shutting_down`
    /// (which makes it stop by itself). It is cut off if that takes too long.
    async fn let_events_drain(&mut self) {
        if let Some(mut forwarder) = self.forwarder.take()
            && tokio::time::timeout(FLUSH_GRACE, &mut forwarder).await.is_err()
        {
            forwarder.abort();
        }
    }

    /// Returns `false` when the connection should close.
    async fn handle(&mut self, message: ClientMessage) -> bool {
        let id = message.id;
        match message.request {
            Request::Hello { protocol, .. } => {
                if !phonia_ipc::PROTOCOL.compatible_with(protocol) {
                    let reason = format!(
                        "this daemon speaks protocol {}.{}, the client {}.{}",
                        phonia_ipc::PROTOCOL.major, phonia_ipc::PROTOCOL.minor, protocol.major, protocol.minor
                    );
                    self.respond(id, error(ErrorCode::UnsupportedVersion, &reason)).await;
                    return false;
                }
                self.handshaken = true;
                self.respond(id, Reply::Ok(Payload::Ack)).await;
            }
            _ if !self.handshaken => {
                self.respond(id, error(ErrorCode::HandshakeRequired, "send a hello request first")).await;
            }
            Request::Subscribe => self.subscribe(id).await,
            Request::Unsubscribe => {
                if let Some(forwarder) = self.forwarder.take() {
                    forwarder.abort();
                }
                self.respond(id, Reply::Ok(Payload::Ack)).await;
            }
            request => {
                let reply = self.daemon.handle(request).await;
                self.respond(id, reply).await;
            }
        }
        true
    }

    /// Answers with a snapshot and then forwards every later event. The receiver is made before the
    /// snapshot is taken, so nothing falls in between.
    async fn subscribe(&mut self, id: RequestId) {
        if let Some(previous) = self.forwarder.take() {
            previous.abort();
        }
        let events = self.daemon.subscribe();
        let (seq, status, queue) = self.daemon.snapshot();
        self.respond(id, Reply::Ok(Payload::Snapshot { seq, status, queue })).await;
        self.forwarder = Some(tokio::spawn(forward_events(events, seq, self.outbox.clone(), self.daemon.clone())));
    }

    async fn respond(&self, id: RequestId, reply: Reply) {
        let _ = self.outbox.send(ServerMessage::Response { id, reply }).await;
    }
}

/// Sends a subscriber every event after `seen`. If it is too slow to keep up with the daemon, it
/// gets the current state in one piece (`resync`) instead of the events it missed.
async fn forward_events(
    mut events: broadcast::Receiver<(u64, Event)>,
    mut seen: u64,
    outbox: mpsc::Sender<ServerMessage>,
    daemon: Arc<Daemon>,
) {
    loop {
        let message = match events.recv().await {
            Ok((seq, _)) if seq <= seen => continue,
            Ok((seq, event)) => {
                seen = seq;
                let last = event == Event::ShuttingDown;
                if outbox.send(ServerMessage::Event { seq, event }).await.is_err() || last {
                    return;
                }
                continue;
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                let (seq, status, queue) = daemon.snapshot();
                seen = seq;
                ServerMessage::Event { seq, event: Event::Resync { skipped, seq, status, queue } }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        // Waiting here when the client is slow is what makes `events` lag, and lag becomes a resync.
        if outbox.send(message).await.is_err() {
            return;
        }
    }
}

fn error(code: ErrorCode, message: &str) -> Reply {
    Reply::Err(ProtocolError { code, message: message.to_string() })
}

/// The id of a request that could not be fully parsed, so the error can still be matched to it.
fn request_id_of(bytes: &[u8]) -> RequestId {
    let id = serde_json::from_slice::<serde_json::Value>(bytes).ok().and_then(|value| value.get("id")?.as_u64());
    RequestId(id.unwrap_or(0))
}
