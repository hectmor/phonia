//! The thin zbus adapter for #34: registers `org.mpris.MediaPlayer2.phonia` on the session bus
//! and keeps a [`Model`] current from the daemon's own event stream, the same one IPC clients
//! subscribe to (`Daemon::subscribe`/`snapshot`) -- `Daemon::fan_in` itself is untouched, this is
//! purely an additive consumer, like `server.rs` is.
//!
//! Every transport method, including `Seek`/`SetPosition`, maps to `self.model.*_request()`
//! (written in [`super::model`]) and routes it through `Daemon::handle`, exactly like `phonia
//! ctl` already does -- no new control-plane code, just a new caller of the same one. `Volume`,
//! `LoopStatus` and `Shuffle` are writable the same way. `OpenUri`/`Raise`/`Quit` are permanently
//! out of scope (#34 decision 2). The `Seeked` signal needed no new trigger: the engine already
//! publishes `ipc::Event::Seeked` for any seek, including one `Request::Seek` started right here,
//! and part 2/3 already turn that into the signal.

use super::model::{self, Changed, Model};
use crate::daemon::Daemon;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::names::InterfaceName;
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{Connection, connection, interface};

const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const BUS_NAME: &str = "org.mpris.MediaPlayer2.phonia";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

/// A session bus lookup or connection attempt that hangs (no bus running, a stuck activation...)
/// must not hold up the rest of startup.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Keeps the MPRIS service alive. Dropping it closes the connection, which frees the bus name.
pub struct Handle {
    _connection: Connection,
}

/// Starts the MPRIS service on the user's session bus. Never fails outright: a missing session
/// bus, a name already taken past the instance-pid fallback, or anything else wrong logs one
/// line and returns `None` -- the same "fails soft, never blocks or crashes" rule #31's own
/// hardware-mixer check already established, since the daemon must stay usable headless or on a
/// minimal install with no session bus at all.
pub async fn start(daemon: Arc<Daemon>) -> Option<Handle> {
    match try_start(daemon, None).await {
        Ok(handle) => Some(handle),
        Err(error) => {
            eprintln!("phoniad: mpris disabled ({error:#})");
            None
        }
    }
}

/// Like [`start`], but on the bus at `address` instead of the session bus, and reporting its
/// error rather than swallowing it -- what a test against a private bus wants, mirroring
/// `DbusReserver::on_bus` in `phonia-core`.
pub async fn start_on_bus(daemon: Arc<Daemon>, address: &str) -> Result<Handle> {
    try_start(daemon, Some(address)).await
}

async fn try_start(daemon: Arc<Daemon>, bus_address: Option<&str>) -> Result<Handle> {
    let (_, status, queue) = daemon.snapshot();
    let model = Model::from_snapshot(&status, &queue);

    let builder = match bus_address {
        Some(address) => connection::Builder::address(address),
        None => connection::Builder::session(),
    }
    .context("no session bus")?
    .serve_at(OBJECT_PATH, Root)
    .context("registering the MPRIS root object")?
    .serve_at(
        OBJECT_PATH,
        Player {
            model,
            daemon: daemon.clone(),
        },
    )
    .context("registering the MPRIS player object")?;
    let connection = tokio::time::timeout(CONNECT_TIMEOUT, builder.build())
        .await
        .context("timed out connecting to the bus")?
        .context("connecting to the bus")?;

    if acquire_name(&connection).await.is_none() {
        bail!("could not acquire an mpris bus name");
    }

    let player_ref = connection
        .object_server()
        .interface::<_, Player>(OBJECT_PATH)
        .await
        .context("looking up the registered player object")?;

    let mut events = daemon.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok((_, event)) => apply(&player_ref, &event).await,
                Err(RecvError::Lagged(_)) => resync(&daemon, &player_ref).await,
                Err(RecvError::Closed) => break,
            }
        }
    });

    Ok(Handle {
        _connection: connection,
    })
}

/// `org.mpris.MediaPlayer2.phonia`, falling back to the spec's own `...phonia.instance<pid>`
/// form if a second daemon instance can't acquire the plain name -- the same spirit as the
/// socket path already allowing several instances via distinct paths. `None` means both failed.
async fn acquire_name(connection: &Connection) -> Option<String> {
    for name in [
        BUS_NAME.to_string(),
        format!("{BUS_NAME}.instance{}", std::process::id()),
    ] {
        match connection
            .request_name_with_flags(name.as_str(), RequestNameFlags::DoNotQueue.into())
            .await
        {
            Ok(RequestNameReply::PrimaryOwner) => return Some(name),
            _ => continue,
        }
    }
    None
}

async fn apply(player_ref: &InterfaceRef<Player>, event: &phonia_ipc::Event) {
    let (changed, values) = {
        let mut player = player_ref.get_mut().await;
        let changed = player.model.apply(event);
        (changed.clone(), changed_values(&player.model, &changed))
    };
    emit(player_ref, &values, changed.seeked_us).await;
}

/// A subscriber too slow to keep up lost events it can't individually diff against; resyncing
/// from a fresh snapshot and reporting every tracked property is the only sound recovery, the
/// same as a lagged IPC client gets a full resync rather than a partial one.
async fn resync(daemon: &Arc<Daemon>, player_ref: &InterfaceRef<Player>) {
    let (_, status, queue) = daemon.snapshot();
    let values = {
        let mut player = player_ref.get_mut().await;
        player.model = Model::from_snapshot(&status, &queue);
        all_values(&player.model)
    };
    emit(player_ref, &values, None).await;
}

async fn emit(
    player_ref: &InterfaceRef<Player>,
    values: &HashMap<&'static str, Value<'static>>,
    seeked_us: Option<i64>,
) {
    if !values.is_empty() {
        let interface_name = InterfaceName::try_from(PLAYER_INTERFACE)
            .expect("a literal, always-valid interface name");
        let result = zbus::fdo::Properties::properties_changed(
            player_ref.signal_emitter(),
            interface_name,
            values.clone(),
            std::borrow::Cow::Borrowed(&[]),
        )
        .await;
        if let Err(error) = result {
            eprintln!("phoniad: could not announce an mpris property change: {error:#}");
        }
    }
    if let Some(position) = seeked_us
        && let Err(error) = player_ref.signal_emitter().seeked(position).await
    {
        eprintln!("phoniad: could not announce an mpris seek: {error:#}");
    }
}

/// The MPRIS properties this service tracks and batches into one `PropertiesChanged` signal.
const TRACKED_PROPERTIES: [&str; 10] = [
    "PlaybackStatus",
    "Metadata",
    "Volume",
    "LoopStatus",
    "Shuffle",
    "CanGoNext",
    "CanGoPrevious",
    "CanPlay",
    "CanPause",
    "CanSeek",
];

fn property_value(model: &Model, name: &str) -> Value<'static> {
    match name {
        "PlaybackStatus" => Value::from(model.status().as_str().to_string()),
        "LoopStatus" => Value::from(model.loop_status().as_str().to_string()),
        "Shuffle" => Value::from(model.shuffle()),
        "Volume" => Value::from(model.volume()),
        "Metadata" => Value::from(metadata_dict(model.metadata())),
        "CanGoNext" => Value::from(model.can_go_next()),
        "CanGoPrevious" => Value::from(model.can_go_previous()),
        "CanPlay" => Value::from(model.can_play()),
        "CanPause" => Value::from(model.can_pause()),
        "CanSeek" => Value::from(model.can_seek()),
        other => unreachable!("{other} is not one of mpris::service's TRACKED_PROPERTIES"),
    }
}

fn changed_values(model: &Model, changed: &Changed) -> HashMap<&'static str, Value<'static>> {
    let flags = [
        changed.status,
        changed.metadata,
        changed.volume,
        changed.loop_status,
        changed.shuffle,
        changed.can_go_next,
        changed.can_go_previous,
        changed.can_play,
        changed.can_pause,
        changed.can_seek,
    ];
    TRACKED_PROPERTIES
        .iter()
        .zip(flags)
        .filter(|(_, changed)| *changed)
        .map(|(name, _)| (*name, property_value(model, name)))
        .collect()
}

fn all_values(model: &Model) -> HashMap<&'static str, Value<'static>> {
    TRACKED_PROPERTIES
        .iter()
        .map(|name| (*name, property_value(model, name)))
        .collect()
}

fn owned<'v, T: Into<Value<'v>>>(value: T) -> OwnedValue {
    OwnedValue::try_from(value.into()).expect("mpris property values never hold a file descriptor")
}

fn parse_loop_status(value: &str) -> zbus::fdo::Result<model::LoopStatus> {
    match value {
        "None" => Ok(model::LoopStatus::None),
        "Track" => Ok(model::LoopStatus::Track),
        "Playlist" => Ok(model::LoopStatus::Playlist),
        other => Err(zbus::fdo::Error::InvalidArgs(format!(
            "{other} is not a valid LoopStatus"
        ))),
    }
}

/// Builds the `a{sv}` dict MPRIS expects for `Metadata`, straight from [`model::Metadata`]: the
/// object path falls back to `NO_TRACK` if it was somehow not a valid path, and every optional
/// field is only present in the dict when known, per spec.
fn metadata_dict(metadata: &model::Metadata) -> HashMap<String, OwnedValue> {
    let mut dict = HashMap::new();
    let track_id = ObjectPath::try_from(metadata.track_id.clone())
        .unwrap_or_else(|_| ObjectPath::try_from(model::NO_TRACK).expect("a valid literal path"));
    dict.insert("mpris:trackid".to_string(), owned(track_id));
    if let Some(length) = metadata.length_us {
        dict.insert("mpris:length".to_string(), owned(length));
    }
    if let Some(art_url) = &metadata.art_url {
        dict.insert("mpris:artUrl".to_string(), owned(art_url.clone()));
    }
    if let Some(title) = &metadata.title {
        dict.insert("xesam:title".to_string(), owned(title.clone()));
    }
    if !metadata.artist.is_empty() {
        dict.insert("xesam:artist".to_string(), owned(metadata.artist.clone()));
    }
    if let Some(url) = &metadata.url {
        dict.insert("xesam:url".to_string(), owned(url.clone()));
    }
    dict
}

/// `org.mpris.MediaPlayer2`: identity only, nothing a desktop widget writes.
struct Root;

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    /// Out of scope (#34 decision 2): phonia has no window to raise, so this always does
    /// nothing, regardless of `CanRaise`.
    async fn raise(&self) {}

    /// Out of scope (#34 decision 2): a widget's own "close" must never be able to kill the
    /// daemon the TUI depends on, so this always does nothing, regardless of `CanQuit`.
    async fn quit(&self) {}

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn identity(&self) -> &str {
        "phonia"
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

/// `org.mpris.MediaPlayer2.Player`. Holds the one [`Model`] the whole service shares; the
/// object server's own lock around every interface instance is what makes mutating it from the
/// background event task (via `InterfaceRef::get_mut`) safe against a concurrent property read.
/// `daemon` is only needed now that transport methods actually call `Daemon::handle`.
struct Player {
    model: Model,
    daemon: Arc<Daemon>,
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    async fn next(&self) {
        let _ = self.daemon.handle(self.model.next_request()).await;
    }

    async fn previous(&self) {
        let _ = self.daemon.handle(self.model.previous_request()).await;
    }

    async fn pause(&self) {
        if let Some(request) = self.model.pause_request() {
            let _ = self.daemon.handle(request).await;
        }
    }

    async fn play_pause(&self) {
        let _ = self.daemon.handle(self.model.play_pause_request()).await;
    }

    async fn stop(&self) {
        let _ = self.daemon.handle(self.model.stop_request()).await;
    }

    async fn play(&self) {
        if let Some(request) = self.model.play_request() {
            let _ = self.daemon.handle(request).await;
        }
    }

    async fn seek(&self, offset: i64) {
        let _ = self.daemon.handle(Model::seek_request(offset)).await;
    }

    async fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        if let Some(request) = self.model.set_position_request(track_id.as_str(), position) {
            let _ = self.daemon.handle(request).await;
        }
    }
    /// Permanently out of scope (#34 decision 2): phonia's queue has no notion of opening an
    /// arbitrary URI.
    async fn open_uri(&self, _uri: String) {}

    #[zbus(property)]
    fn playback_status(&self) -> &str {
        self.model.status().as_str()
    }

    #[zbus(property)]
    fn loop_status(&self) -> &str {
        self.model.loop_status().as_str()
    }

    #[zbus(property)]
    async fn set_loop_status(&self, value: &str) -> zbus::fdo::Result<()> {
        let loop_status = parse_loop_status(value)?;
        let _ = self
            .daemon
            .handle(Model::set_loop_status_request(loop_status))
            .await;
        Ok(())
    }

    /// No playback-rate feature exists; always normal speed.
    #[zbus(property(emits_changed_signal = "const"))]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.model.shuffle()
    }

    #[zbus(property)]
    async fn set_shuffle(&self, value: bool) {
        let _ = self.daemon.handle(Model::set_shuffle_request(value)).await;
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        metadata_dict(self.model.metadata())
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.model.volume()
    }

    #[zbus(property)]
    async fn set_volume(&self, value: f64) {
        for request in Model::set_volume_requests(value) {
            let _ = self.daemon.handle(request).await;
        }
    }

    /// Per spec, `Position` gets no change notification; polled, or inferred from `Seeked`.
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.model.position_us()
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.model.can_go_next()
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.model.can_go_previous()
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.model.can_play()
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.model.can_pause()
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.model.can_seek()
    }

    /// The spec's own invariant: this player does implement MPRIS, even while every individual
    /// transport capability above is still off.
    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }

    #[zbus(signal)]
    pub async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;
}
