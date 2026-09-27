//! The connection to the daemon: connecting, subscribing, and connecting again when it drops.
//!
//! [`spawn`] starts a task that turns everything about the connection into [`Msg`]s for the
//! interface. It never gives up on a daemon that is merely not there yet or went away: it tries
//! again, waiting a little longer each time (up to [`MAX_DELAY`]). What stops it is a peer that is
//! not a compatible phonia daemon, since asking again would not change that; the user can still
//! ask (`R`).
//!
//! How to connect is a parameter ([`Connector`]), so the tests run it against a scripted daemon
//! over an in-memory pipe.

use crate::app::Msg;
use futures_util::future::BoxFuture;
use phonia_ipc::{Client, ClientError};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};

/// How long to wait before the first new attempt.
const FIRST_DELAY: Duration = Duration::from_millis(250);
/// The longest wait between attempts.
pub const MAX_DELAY: Duration = Duration::from_secs(5);

/// Opens a connection to the daemon.
pub type Connector = Arc<dyn Fn() -> BoxFuture<'static, Result<Client, ClientError>> + Send + Sync>;

/// The waits between attempts: 250 ms, doubling up to five seconds, starting over once a
/// connection has worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    next: Duration,
}

impl Backoff {
    pub fn new() -> Self {
        Self { next: FIRST_DELAY }
    }

    /// How long to wait now; the wait after this one is longer.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = (self.next * 2).min(MAX_DELAY);
        delay
    }

    pub fn reset(&mut self) {
        self.next = FIRST_DELAY;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// How one attempt ended.
enum Attempt {
    /// It was connected at some point, and is not any more.
    Lost { was_connected: bool, reason: String },
    /// The peer is not a phonia daemon this client can talk to.
    Refused(String),
    /// Nobody is listening for messages any more: the interface has gone.
    InterfaceGone,
}

/// Starts the connection task. `retry` cuts the wait short (or ends a refusal); the task stops
/// when the interface drops the receiving end of `messages`.
pub fn spawn(
    connector: Connector,
    messages: mpsc::UnboundedSender<Msg>,
    retry: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(connector, messages, retry))
}

async fn run(connector: Connector, messages: mpsc::UnboundedSender<Msg>, retry: Arc<Notify>) {
    let mut backoff = Backoff::new();
    loop {
        match attempt(&connector, &messages).await {
            Attempt::InterfaceGone => return,
            Attempt::Refused(reason) => {
                if messages.send(Msg::Refused { reason }).is_err() {
                    return;
                }
                // Asking again would get the same answer: wait until the user says so.
                retry.notified().await;
                backoff.reset();
            }
            Attempt::Lost {
                was_connected,
                reason,
            } => {
                // It worked before, so this is a new outage, not a longer one.
                if was_connected {
                    backoff.reset();
                }
                let delay = backoff.next_delay();
                if messages
                    .send(Msg::Disconnected {
                        reason,
                        retry_in: delay,
                    })
                    .is_err()
                {
                    return;
                }
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = retry.notified() => backoff.reset(),
                }
            }
        }
    }
}

async fn attempt(connector: &Connector, messages: &mpsc::UnboundedSender<Msg>) -> Attempt {
    let client = match connector().await {
        Ok(client) => client,
        Err(ClientError::Handshake(reason)) => return Attempt::Refused(reason),
        Err(error) => {
            return Attempt::Lost {
                was_connected: false,
                reason: error.to_string(),
            };
        }
    };
    let (snapshot, mut events) = match client.subscribe().await {
        Ok(subscribed) => subscribed,
        Err(ClientError::Handshake(reason)) => return Attempt::Refused(reason),
        Err(error) => {
            return Attempt::Lost {
                was_connected: false,
                reason: error.to_string(),
            };
        }
    };
    let server = client.server();
    let connected = Msg::Connected {
        server: server.server.clone(),
        protocol: server.protocol,
        capabilities: server.capabilities.clone(),
        status: snapshot.status,
        queue: snapshot.queue,
    };
    if messages.send(connected).is_err() {
        return Attempt::InterfaceGone;
    }

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(event) => {
                    if messages.send(Msg::Daemon(event)).is_err() {
                        return Attempt::InterfaceGone;
                    }
                }
                None => break,
            },
            _ = client.closed() => break,
        }
    }
    Attempt::Lost {
        was_connected: true,
        reason: "the connection to the daemon was closed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phonia_ipc::framing::{read_frame, write_frame};
    use phonia_ipc::{
        ClientInfo, ClientMessage, Event, Payload, Queue, Repeat, Reply, Request, ServerHello,
        ServerInfo, ServerMessage, State, Status, Version,
    };
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncWriteExt, BufReader, DuplexStream};

    fn status() -> Status {
        Status {
            state: State::Stopped,
            track: None,
            spec: None,
            position_ms: 0,
            duration_ms: None,
            output: phonia_ipc::Output::Closed,
            route: None,
            volume: None,
            quality_range: None,
        }
    }

    fn queue() -> Queue {
        Queue {
            version: 1,
            items: Vec::new(),
            order: Vec::new(),
            current: None,
            shuffle: false,
            repeat: Repeat::Off,
        }
    }

    fn hello(major: u32) -> ServerHello {
        ServerHello {
            protocol: Version { major, minor: 5 },
            server: ServerInfo {
                name: "phoniad".into(),
                version: "9.9".into(),
                pid: 1,
            },
            capabilities: vec!["volume".into()],
        }
    }

    /// A daemon on the other end of a pipe: greets, answers `hello` and `subscribe`, pushes
    /// `events` right after the subscription, and hangs up when `hang_up` fires (or never).
    fn fake_daemon(
        major: u32,
        events: Vec<Event>,
        hang_up: tokio::sync::oneshot::Receiver<()>,
    ) -> DuplexStream {
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let (read_half, mut writer) = tokio::io::split(server_side);
            let mut reader = BufReader::new(read_half);
            let banner = serde_json::to_vec(&ServerMessage::Hello(hello(major))).unwrap();
            write_frame(&mut writer, &banner).await.unwrap();
            let mut buffer = Vec::new();
            let mut hang_up = hang_up;
            loop {
                let bytes = tokio::select! {
                    frame = read_frame(&mut reader, &mut buffer) => match frame {
                        Ok(Some(bytes)) => bytes.to_vec(),
                        _ => return,
                    },
                    _ = &mut hang_up => {
                        let _ = writer.shutdown().await;
                        return;
                    }
                };
                let Ok(message) = serde_json::from_slice::<ClientMessage>(&bytes) else {
                    continue;
                };
                let payload = match message.request {
                    Request::Subscribe => Payload::Snapshot {
                        seq: 10,
                        status: status(),
                        queue: queue(),
                    },
                    _ => Payload::Ack,
                };
                let reply = ServerMessage::Response {
                    id: message.id,
                    reply: Reply::Ok(payload),
                };
                write_frame(&mut writer, &serde_json::to_vec(&reply).unwrap())
                    .await
                    .unwrap();
                if matches!(message.request, Request::Subscribe) {
                    for (i, event) in events.iter().enumerate() {
                        let event = ServerMessage::Event {
                            seq: 11 + i as u64,
                            event: event.clone(),
                        };
                        write_frame(&mut writer, &serde_json::to_vec(&event).unwrap())
                            .await
                            .unwrap();
                    }
                }
            }
        });
        client_side
    }

    fn info() -> ClientInfo {
        ClientInfo {
            name: "test".into(),
            version: "0".into(),
        }
    }

    /// A connector that hands out the given attempts, one per call, then fails as if nobody
    /// listened.
    fn connector(
        attempts: Vec<Result<DuplexStream, ClientError>>,
    ) -> (Connector, Arc<AtomicUsize>) {
        let attempts = Arc::new(Mutex::new(attempts.into_iter().collect::<Vec<_>>()));
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let connector: Connector = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            let next = {
                let mut attempts = attempts.lock().unwrap();
                if attempts.is_empty() {
                    None
                } else {
                    Some(attempts.remove(0))
                }
            };
            Box::pin(async move {
                match next {
                    Some(Ok(stream)) => Client::from_stream(stream, info()).await,
                    Some(Err(error)) => Err(error),
                    None => Err(ClientError::Io(std::io::Error::other("nobody listens"))),
                }
            })
        });
        (connector, calls)
    }

    fn refused() -> ClientError {
        ClientError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        ))
    }

    async fn next(messages: &mut mpsc::UnboundedReceiver<Msg>) -> Msg {
        tokio::time::timeout(Duration::from_secs(60), messages.recv())
            .await
            .expect("a message in time")
            .expect("the task is alive")
    }

    #[test]
    fn the_waits_double_up_to_five_seconds_and_start_over() {
        let mut backoff = Backoff::new();
        let waits: Vec<u128> = (0..7).map(|_| backoff.next_delay().as_millis()).collect();
        assert_eq!(waits, [250, 500, 1000, 2000, 4000, 5000, 5000]);
        backoff.reset();
        assert_eq!(backoff.next_delay(), FIRST_DELAY);
    }

    #[tokio::test(start_paused = true)]
    async fn it_connects_reports_the_state_and_forwards_events() {
        let (_keep, hang_up) = tokio::sync::oneshot::channel();
        let daemon = fake_daemon(1, vec![Event::QueueExhausted], hang_up);
        let (connector, _) = connector(vec![Ok(daemon)]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn(connector, tx, Arc::new(Notify::new()));

        let Msg::Connected {
            server,
            protocol,
            capabilities,
            status: got,
            queue: got_queue,
        } = next(&mut rx).await
        else {
            panic!("not connected");
        };
        assert_eq!(
            (server.name.as_str(), server.version.as_str()),
            ("phoniad", "9.9")
        );
        assert_eq!(protocol, Version { major: 1, minor: 5 });
        assert_eq!(capabilities, ["volume"]);
        assert_eq!(got, status());
        assert_eq!(got_queue, queue());
        assert_eq!(next(&mut rx).await, Msg::Daemon(Event::QueueExhausted));
    }

    #[tokio::test(start_paused = true)]
    async fn when_the_daemon_hangs_up_it_says_so_and_connects_again_from_the_start() {
        let (hang_up_tx, hang_up) = tokio::sync::oneshot::channel();
        let (_keep, second_hang_up) = tokio::sync::oneshot::channel();
        let first = fake_daemon(1, vec![], hang_up);
        let second = fake_daemon(1, vec![], second_hang_up);
        let (connector, calls) = connector(vec![Ok(first), Ok(second)]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn(connector, tx, Arc::new(Notify::new()));

        assert!(matches!(next(&mut rx).await, Msg::Connected { .. }));
        hang_up_tx.send(()).unwrap();
        let Msg::Disconnected { reason, retry_in } = next(&mut rx).await else {
            panic!("not disconnected");
        };
        assert!(reason.contains("closed"), "{reason}");
        assert_eq!(
            retry_in, FIRST_DELAY,
            "a connection that worked starts the waits over"
        );
        assert!(matches!(next(&mut rx).await, Msg::Connected { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_daemon_that_is_not_there_is_tried_again_after_longer_and_longer_waits() {
        let (connector, calls) = connector(vec![Err(refused()), Err(refused()), Err(refused())]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn(connector, tx, Arc::new(Notify::new()));

        let mut waits = Vec::new();
        for _ in 0..3 {
            let Msg::Disconnected { reason, retry_in } = next(&mut rx).await else {
                panic!("not disconnected");
            };
            assert!(reason.contains("refused"), "{reason}");
            waits.push(retry_in);
        }
        assert_eq!(
            waits,
            [
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_millis(1000)
            ]
        );
        assert!(calls.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_daemon_of_another_major_version_is_refused_and_not_retried() {
        let (_keep, hang_up) = tokio::sync::oneshot::channel();
        let (connector, calls) = connector(vec![Ok(fake_daemon(2, vec![], hang_up))]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn(connector, tx, Arc::new(Notify::new()));

        let Msg::Refused { reason } = next(&mut rx).await else {
            panic!("not refused");
        };
        assert!(reason.contains("protocol 2.5"), "{reason}");
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(rx.try_recv().is_err(), "it waits for the user");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "and does not ask again");
    }

    #[tokio::test(start_paused = true)]
    async fn asking_again_by_hand_ends_the_wait_and_the_refusal() {
        let (_keep1, hang_up1) = tokio::sync::oneshot::channel();
        let (_keep2, hang_up2) = tokio::sync::oneshot::channel();
        let (connector, calls) = connector(vec![
            Ok(fake_daemon(2, vec![], hang_up1)),
            Ok(fake_daemon(1, vec![], hang_up2)),
        ]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let retry = Arc::new(Notify::new());
        spawn(connector, tx, retry.clone());

        assert!(matches!(next(&mut rx).await, Msg::Refused { .. }));
        retry.notify_one();
        assert!(matches!(next(&mut rx).await, Msg::Connected { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn asking_again_cuts_a_long_wait_short() {
        // Ends up waiting 5 s, then is told to try now.
        let (connector, calls) = connector(Vec::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let retry = Arc::new(Notify::new());
        spawn(connector, tx, retry.clone());

        for _ in 0..7 {
            assert!(matches!(next(&mut rx).await, Msg::Disconnected { .. }));
        }
        let before = calls.load(Ordering::SeqCst);
        let started = tokio::time::Instant::now();
        retry.notify_one();
        assert!(
            matches!(next(&mut rx).await, Msg::Disconnected { retry_in, .. } if retry_in == FIRST_DELAY)
        );
        assert!(
            started.elapsed() < MAX_DELAY,
            "did not wait the five seconds"
        );
        assert!(calls.load(Ordering::SeqCst) > before);
    }

    #[tokio::test(start_paused = true)]
    async fn the_task_ends_when_the_interface_goes_away() {
        let (connector, _) = connector(Vec::new());
        let (tx, rx) = mpsc::unbounded_channel();
        let task = spawn(connector, tx, Arc::new(Notify::new()));
        drop(rx);
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .expect("it stops")
            .unwrap();
    }
}
