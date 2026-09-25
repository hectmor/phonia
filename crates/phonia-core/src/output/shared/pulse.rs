//! The real sound-server side of shared mode, over the PulseAudio protocol that PipeWire serves
//! (`pipewire-pulse`) and PulseAudio speaks natively.
//!
//! The `pulseaudio` crate is a pure-Rust client with its own reactor thread, and its futures need
//! no particular runtime, so the audio thread drives them with a plain `block_on`.

use super::ring::Ring;
use super::{SharedSink, Timing, Transport};
use crate::output::alsa::{ReportHandler, SharedRoute, SinkReport};
use crate::output::{AudioSink, SinkFactory, Volume, VolumeControl, VolumeHandler};
use crate::decode::SourceSpec;
use anyhow::{Context, Result, anyhow, bail};
use futures_executor::block_on;
use pulseaudio::protocol;
use pulseaudio::{Client, PlaybackStream};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a named output gets to appear (a card that phonia has just given back takes a moment to
/// become an output again).
const OUTPUT_WAIT: Duration = Duration::from_secs(3);
const OUTPUT_POLL: Duration = Duration::from_millis(200);

/// Fraction of a second of silence in front of a new stream. The server drops the first ~1024
/// frames it is given, so this is what it drops.
const PREFIX_DIVISOR: u32 = 10;
/// How long the ring lets the audio thread run ahead of the server, in fractions of a second.
const RING_DIVISOR: u32 = 4;
/// What the server is asked to hold, and how often it asks for more, in fractions of a second.
const TARGET_NUM: u32 = 2; // 0.4 s: 2/5
const TARGET_DEN: u32 = 5;
const REQUEST_DIVISOR: u32 = 50;
/// The resampler quality PipeWire is asked for (0 to 14; its default is 4).
const RESAMPLE_QUALITY: &CStr = c"10";

use std::ffi::CStr;

/// Which output to play on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Wherever the desktop's default output is, following it when it changes.
    Default,
    /// One output by name, and never moved to another.
    Named(String),
}

impl Target {
    /// `None` or `"default"` mean the default output.
    pub fn parse(sink: Option<&str>) -> Self {
        match sink {
            None | Some("default") => Target::Default,
            Some(name) => Target::Named(name.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Usb,
    Bluetooth,
    Hdmi,
    Internal,
    Other,
}

impl OutputKind {
    pub fn label(self) -> &'static str {
        match self {
            OutputKind::Usb => "USB",
            OutputKind::Bluetooth => "Bluetooth",
            OutputKind::Hdmi => "HDMI",
            OutputKind::Internal => "built-in",
            OutputKind::Other => "output",
        }
    }
}

/// An output of the sound server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// What goes in the config file.
    pub name: String,
    pub description: String,
    pub kind: OutputKind,
    /// The Bluetooth codec in use, when there is one.
    pub codec: Option<String>,
    /// Whether what reaches the speaker has lost information on the way (a Bluetooth link does).
    pub lossy: bool,
    pub rate: u32,
    pub index: u32,
    pub is_default: bool,
}

/// What kind of output a sink is, from the properties the server keeps on it.
pub fn classify(name: &str, props: &BTreeMap<String, String>) -> (OutputKind, Option<String>, bool) {
    let get = |key: &str| props.get(key).map(String::as_str);
    if get("device.api") == Some("bluez5") || name.starts_with("bluez_") {
        let codec = get("api.bluez5.codec").map(|codec| codec.to_uppercase());
        return (OutputKind::Bluetooth, codec, true);
    }
    let lower = name.to_lowercase();
    let kind = if get("device.bus") == Some("usb") || lower.contains(".usb-") || lower.contains("_usb-") {
        OutputKind::Usb
    } else if lower.contains("hdmi") || get("device.profile.description").is_some_and(|d| d.contains("HDMI")) {
        OutputKind::Hdmi
    } else if get("device.bus") == Some("pci") || lower.contains("pci-") {
        OutputKind::Internal
    } else {
        OutputKind::Other
    };
    (kind, None, false)
}

fn props_of(props: &protocol::Props) -> BTreeMap<String, String> {
    props
        .iter()
        .filter_map(|(key, value)| {
            let value = value.strip_suffix(&[0]).unwrap_or(value);
            Some((key.to_str().ok()?.to_string(), std::str::from_utf8(value).ok()?.to_string()))
        })
        .collect()
}

fn output_from(info: &protocol::SinkInfo, default_name: Option<&str>) -> Output {
    let name = info.name.to_string_lossy().into_owned();
    let (kind, codec, lossy) = classify(&name, &props_of(&info.props));
    Output {
        description: info.description.as_ref().map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone()),
        is_default: default_name == Some(name.as_str()),
        rate: info.sample_spec.sample_rate,
        index: info.index,
        name,
        kind,
        codec,
        lossy,
    }
}

/// Connects to the sound server.
pub fn connect() -> Result<Client> {
    Client::from_env(c"phonia").map_err(|error| anyhow!("no sound server (PipeWire or PulseAudio) to connect to: {error}"))
}

/// Every output the sound server has.
pub async fn list_outputs(client: &Client) -> Result<Vec<Output>> {
    let default_name = client
        .server_info()
        .await
        .context("asking the sound server about itself")?
        .default_sink_name
        .map(|name| name.to_string_lossy().into_owned());
    let sinks = client.list_sinks().await.context("listing the sound server's outputs")?;
    Ok(sinks.iter().map(|sink| output_from(sink, default_name.as_deref())).collect())
}

/// The list `phonia devices` shows: the outputs, with the text to put in the config file.
pub fn format_outputs(outputs: &[Output]) -> String {
    let mut lines = vec![
        "PipeWire outputs, shared and NOT bit-perfect (set mode = \"shared\" and sink = \"<name>\" under [output]):"
            .to_string(),
    ];
    if outputs.is_empty() {
        lines.push("  (the sound server has no outputs)".to_string());
        return lines.join("\n");
    }
    let width = outputs.iter().map(|output| output.name.len()).max().unwrap_or(0).max("default".len());
    let default = outputs.iter().find(|output| output.is_default);
    lines.push(format!(
        "  {} {:<width$}  {}",
        if default.is_some() { "*" } else { " " },
        "default",
        match default {
            Some(default) => format!("the desktop's default output, now {}", default.description),
            None => "the desktop's default output".to_string(),
        }
    ));
    for output in outputs {
        let mut details = vec![output.kind.label().to_string()];
        details.extend(output.codec.clone());
        if output.lossy {
            details.push("lossy".to_string());
        }
        lines.push(format!("    {:<width$}  {}  ({})", output.name, output.description, details.join(", ")));
    }
    lines.push("A named output is never swapped for another if it goes away; \"default\" follows the desktop.".to_string());
    lines.join("\n")
}

/// Connects and lists, for `phonia devices`.
pub async fn outputs() -> Result<Vec<Output>> {
    list_outputs(&connect()?).await
}

/// How long after phonia sets the volume that changes the server reports are taken as its own echo.
const OWN_CHANGE_ECHO: Duration = Duration::from_millis(600);

/// What the server calls unity gain: 100%.
const VOLUME_NORM: u32 = 65_536;

/// The server's raw volume for a percentage: what the desktop's mixers show as that percentage.
fn raw_from_percent(percent: u8) -> u32 {
    (u32::from(percent.min(100)) * VOLUME_NORM + 50) / 100
}

/// The percentage a raw volume shows as.
fn percent_from_raw(raw: u32) -> u8 {
    ((u64::from(raw) * 100 + u64::from(VOLUME_NORM) / 2) / u64::from(VOLUME_NORM)).min(255) as u8
}

/// The volume of the stream on the server, kept across streams.
///
/// A new track in another format, or another output, is a new stream that must start at the volume
/// that was set, so the state lives here and is handed to each stream as it is created. Changes
/// while a stream plays go to the server as commands on a connection of their own (the client
/// library does not expose them), and changes made by someone else (the desktop's mixer) come back
/// through [`Watcher`].
pub struct SharedVolume {
    inner: Mutex<VolumeInner>,
    handler: Mutex<Option<VolumeHandler>>,
    next_generation: AtomicU64,
}

#[derive(Default)]
struct VolumeInner {
    volume: Volume,
    /// When phonia last set the volume itself. The server tells us about our own changes too, and
    /// reading them back while several are in flight would report an old value as if it were new.
    last_set: Option<std::time::Instant>,
    stream: Option<AttachedStream>,
    /// The connection the commands go over; made when first needed and again after a failure.
    link: Option<(BufReader<UnixStream>, u16)>,
}

struct AttachedStream {
    /// The stream's index on the server (its sink input).
    index: u32,
    channels: u8,
    generation: u64,
}

impl SharedVolume {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::default(),
            handler: Mutex::new(None),
            next_generation: AtomicU64::new(1),
        })
    }

    /// The volume a stream should be created with.
    fn channel_volume(&self, channels: u8) -> (protocol::ChannelVolume, bool) {
        let volume = self.inner.lock().unwrap().volume;
        let mut channel_volume = protocol::ChannelVolume::empty();
        for _ in 0..channels {
            channel_volume.push(protocol::Volume::from_u32_clamped(raw_from_percent(volume.percent)));
        }
        (channel_volume, volume.muted)
    }

    /// A stream now plays: later changes go to it. Returns what to give back to [`detach`].
    fn attach(&self, index: u32, channels: u8) -> u64 {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        self.inner.lock().unwrap().stream = Some(AttachedStream { index, channels, generation });
        generation
    }

    /// The stream is gone, unless a newer one already took its place.
    fn detach(&self, generation: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.stream.as_ref().is_some_and(|stream| stream.generation == generation) {
            inner.stream = None;
        }
    }

    /// Sends the volume to the stream that plays, if there is one.
    fn apply(inner: &mut VolumeInner) -> Result<()> {
        let Some(stream) = &inner.stream else { return Ok(()) };
        let (index, channels) = (stream.index, stream.channels);
        let volume = inner.volume;
        // A connection that has died is made again once.
        for attempt in 0..2 {
            if inner.link.is_none() {
                inner.link = Some(raw_connect()?);
            }
            let (reader, version) = inner.link.as_mut().expect("just made");
            let sent = (|| -> Result<()> {
                let mut channel_volume = protocol::ChannelVolume::empty();
                for _ in 0..channels {
                    channel_volume.push(protocol::Volume::from_u32_clamped(raw_from_percent(volume.percent)));
                }
                protocol::write_command_message(
                    reader.get_mut(),
                    10,
                    &protocol::Command::SetSinkInputVolume(protocol::SetStreamVolumeParams { index, volume: channel_volume }),
                    *version,
                )?;
                protocol::read_ack_message(reader)?;
                protocol::write_command_message(
                    reader.get_mut(),
                    11,
                    &protocol::Command::SetSinkInputMute(protocol::SetStreamMuteParams { index, mute: volume.muted }),
                    *version,
                )?;
                protocol::read_ack_message(reader)?;
                Ok(())
            })();
            match sent {
                Ok(()) => return Ok(()),
                Err(_) if attempt == 0 => inner.link = None,
                Err(error) => return Err(error).context("setting the stream's volume"),
            }
        }
        unreachable!("the second attempt returns")
    }

    /// The desktop changed the volume of stream `index`.
    fn changed_outside(&self, index: u32, volume: Volume) {
        {
            let mut inner = self.inner.lock().unwrap();
            let ours = inner.last_set.is_some_and(|at| at.elapsed() < OWN_CHANGE_ECHO);
            if ours || inner.stream.as_ref().map(|stream| stream.index) != Some(index) || inner.volume == volume {
                return;
            }
            inner.volume = volume;
        }
        let handler = self.handler.lock().unwrap().clone();
        if let Some(handler) = handler {
            handler(volume);
        }
    }
}

impl VolumeControl for SharedVolume {
    fn get(&self) -> Volume {
        self.inner.lock().unwrap().volume
    }

    fn set(&self, volume: Volume) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.volume = Volume { percent: volume.percent.min(100), muted: volume.muted };
        inner.last_set = Some(std::time::Instant::now());
        Self::apply(&mut inner)
    }

    fn on_change(&self, handler: VolumeHandler) {
        *self.handler.lock().unwrap() = Some(handler);
    }
}

/// Finds our stream among the server's sink inputs by the id we gave it, and how many channels it has.
fn find_stream(stream_id: &str) -> Result<(u32, u8)> {
    let (mut reader, version) = raw_connect()?;
    protocol::write_command_message(reader.get_mut(), 3, &protocol::Command::GetSinkInputInfoList, version)?;
    let (_, inputs) = protocol::read_reply_message::<protocol::SinkInputInfoList>(&mut reader, version)
        .context("listing the sound server's streams")?;
    inputs
        .iter()
        .find(|input| input.props.get_bytes(c"phonia.stream-id").map(|id| id.strip_suffix(&[0]).unwrap_or(id)) == Some(stream_id.as_bytes()))
        .map(|input| (input.index, input.sample_spec.channels))
        .ok_or_else(|| anyhow!("the new stream is not among the sound server's streams"))
}

/// The volume of stream `index` as the server has it now.
fn read_stream_volume(index: u32) -> Result<Volume> {
    let (mut reader, version) = raw_connect()?;
    protocol::write_command_message(reader.get_mut(), 4, &protocol::Command::GetSinkInputInfo(index), version)?;
    let (_, info) = protocol::read_reply_message::<protocol::SinkInputInfo>(&mut reader, version)?;
    let channels = info.cvolume.channels();
    let raw = channels.iter().map(|volume| u64::from(volume.as_u32())).sum::<u64>() / channels.len().max(1) as u64;
    Ok(Volume { percent: percent_from_raw(raw as u32), muted: info.muted })
}

static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(1);

/// The real [`Transport`]: one playback stream on the server.
struct PulseTransport {
    stream: PlaybackStream,
    /// Shuts the subscription connection when the stream goes.
    _watcher: Option<Watcher>,
    volume: Arc<SharedVolume>,
    generation: u64,
}

impl Transport for PulseTransport {
    fn cork(&mut self, corked: bool) -> Result<()> {
        let result = if corked { block_on(self.stream.cork()) } else { block_on(self.stream.uncork()) };
        result.context("pausing or resuming the stream")
    }

    fn flush(&mut self) -> Result<()> {
        block_on(self.stream.flush()).context("flushing the stream")
    }

    fn timing(&mut self) -> Result<Timing> {
        let timing = block_on(self.stream.timing_info()).context("asking the server for the stream's timing")?;
        Ok(Timing {
            queued_bytes: (timing.write_offset - timing.read_offset).max(0) as u64,
            write_bytes: timing.write_offset.max(0) as u64,
            read_bytes: timing.read_offset.max(0) as u64,
            sink_latency: Duration::from_micros(timing.sink_usec + timing.source_usec),
        })
    }
}

impl Drop for PulseTransport {
    fn drop(&mut self) {
        self.volume.detach(self.generation);
        // Ends the stream on the server; a failure only means it was already gone.
        let _ = block_on(self.stream.clone().delete());
    }
}

/// A second connection that listens for outputs coming and going, and tells the ring when the one
/// the stream plays on is gone. A stream that is not allowed to move is not closed when its output
/// disappears: it stays, silent, so nothing else would notice.
struct Watcher {
    socket: UnixStream,
}

impl Watcher {
    /// `sink` is the output the stream may not leave, when there is one; `stream_index` is the
    /// stream, whose volume the desktop's mixer may change.
    fn start(sink: Option<(u32, String)>, stream_index: u32, ring: Ring, volume: Arc<SharedVolume>) -> Result<Watcher> {
        let (mut reader, version) = raw_connect()?;
        protocol::write_command_message(
            reader.get_mut(),
            2,
            &protocol::Command::Subscribe(protocol::SubscriptionMask::SINK | protocol::SubscriptionMask::SINK_INPUT),
            version,
        )?;
        protocol::read_ack_message(&mut reader).context("subscribing to the sound server's outputs")?;
        let socket = reader.get_ref().try_clone().context("keeping a handle on the subscription")?;

        std::thread::Builder::new().name("phonia-output-watch".into()).spawn(move || {
            loop {
                match protocol::read_command_message(&mut reader, version) {
                    Ok((_, protocol::Command::SubscribeEvent(event))) => {
                        use protocol::{SubscriptionEventFacility as Facility, SubscriptionEventType as Kind};
                        match (event.event_facility, event.event_type) {
                            (Facility::Sink, Kind::Removed)
                                if sink.as_ref().is_some_and(|(index, _)| event.index == Some(*index)) =>
                            {
                                let name = sink.as_ref().map(|(_, name)| name.as_str()).unwrap_or_default();
                                ring.mark_gone(&format!("the output '{name}' went away"));
                                return;
                            }
                            (Facility::SinkInput, Kind::Changed) if event.index == Some(stream_index) => {
                                if let Ok(current) = read_stream_volume(stream_index) {
                                    volume.changed_outside(stream_index, current);
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if !ring.is_closed() {
                            ring.mark_gone(&format!("lost the connection to the sound server ({error})"));
                        }
                        return;
                    }
                }
            }
        })?;
        Ok(Watcher { socket })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // Unblocks the thread's read so that it ends.
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

/// Calls a function whenever the sound server's outputs change: one appears, one goes away, or the
/// desktop's default moves. Ends when dropped.
pub struct OutputWatch {
    socket: UnixStream,
}

impl Drop for OutputWatch {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

/// Starts telling `on_change` about changes to the outputs. The connection is not remade if the
/// server goes away: the watch just ends.
pub fn watch_outputs(on_change: impl Fn() + Send + 'static) -> Result<OutputWatch> {
    let (mut reader, version) = raw_connect()?;
    let mask = protocol::SubscriptionMask::SINK | protocol::SubscriptionMask::SERVER;
    protocol::write_command_message(reader.get_mut(), 2, &protocol::Command::Subscribe(mask), version)?;
    protocol::read_ack_message(&mut reader).context("subscribing to the sound server's outputs")?;
    let socket = reader.get_ref().try_clone().context("keeping a handle on the subscription")?;

    std::thread::Builder::new().name("phonia-outputs-watch".into()).spawn(move || {
        while let Ok((_, command)) = protocol::read_command_message(&mut reader, version) {
            if let protocol::Command::SubscribeEvent(event) = command {
                use protocol::{SubscriptionEventFacility as Facility, SubscriptionEventType as Kind};
                let changed = match event.event_facility {
                    Facility::Sink => matches!(event.event_type, Kind::New | Kind::Removed),
                    Facility::Server => true,
                    _ => false,
                };
                if changed {
                    on_change();
                }
            }
        }
    })?;
    Ok(OutputWatch { socket })
}

fn raw_connect() -> Result<(BufReader<UnixStream>, u16)> {
    let path = pulseaudio::socket_path_from_env().ok_or_else(|| anyhow!("no sound server socket"))?;
    let mut sock = BufReader::new(UnixStream::connect(path).context("connecting to the sound server")?);
    let cookie = pulseaudio::cookie_path_from_env().and_then(|path| std::fs::read(path).ok()).unwrap_or_default();
    let auth = protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie };
    protocol::write_command_message(sock.get_mut(), 0, &protocol::Command::Auth(auth), protocol::MAX_VERSION)?;
    let (_, reply) = protocol::read_reply_message::<protocol::AuthReply>(&mut sock, protocol::MAX_VERSION)?;
    let version = protocol::MAX_VERSION.min(reply.version);
    let mut props = protocol::Props::new();
    props.set(protocol::Prop::ApplicationName, c"phonia");
    protocol::write_command_message(sock.get_mut(), 1, &protocol::Command::SetClientName(props), version)?;
    protocol::read_reply_message::<protocol::SetClientNameReply>(&mut sock, version)?;
    Ok((sock, version))
}

/// Opens streams on the sound server, for the engine.
pub struct SharedSinkFactory {
    target: Target,
    client: Mutex<Option<Client>>,
    on_report: Option<ReportHandler>,
    volume: Arc<SharedVolume>,
}

impl SharedSinkFactory {
    pub fn new(target: Target) -> Self {
        Self { target, client: Mutex::new(None), on_report: None, volume: SharedVolume::new() }
    }

    /// Have every sink hand its report (which says it is not bit-perfect, and why) to `handler`
    /// when it starts playing.
    pub fn on_report(mut self, handler: ReportHandler) -> Self {
        self.on_report = Some(handler);
        self
    }

    /// The connection, made when first needed and again if the server went away in between.
    fn client(&self) -> Result<Client> {
        let mut client = self.client.lock().unwrap();
        if let Some(client) = client.as_ref() {
            return Ok(client.clone());
        }
        let connected = connect()?;
        *client = Some(connected.clone());
        Ok(connected)
    }

    fn forget_client(&self) {
        self.client.lock().unwrap().take();
    }

    /// The output to play on. A named one that is not there yet is waited for, since giving a card
    /// back to the desktop is what makes it an output again.
    fn resolve(&self, client: &Client) -> Result<protocol::SinkInfo> {
        match &self.target {
            Target::Default => {
                let info = block_on(client.server_info()).context("asking the sound server about itself")?;
                let name = info.default_sink_name.ok_or_else(|| anyhow!("the sound server has no default output"))?;
                block_on(client.sink_info_by_name(name)).context("looking up the default output")
            }
            Target::Named(name) => {
                let c_name = CString::new(name.as_str()).context("an output name can't contain a NUL")?;
                let deadline = std::time::Instant::now() + OUTPUT_WAIT;
                loop {
                    match block_on(client.sink_info_by_name(c_name.clone())) {
                        Ok(info) => return Ok(info),
                        Err(_) if std::time::Instant::now() < deadline => std::thread::sleep(OUTPUT_POLL),
                        Err(_) => bail!(
                            "the sound server has no output named '{name}'. `phonia devices` lists them, and \
                             a Bluetooth speaker has to be connected first"
                        ),
                    }
                }
            }
        }
    }

    fn open_stream(&self, spec: SourceSpec) -> Result<Box<dyn AudioSink>> {
        let client = self.client()?;
        let info = self.resolve(&client)?;
        let output = output_from(&info, None);

        let channel_map = match spec.channels {
            1 => protocol::ChannelMap::mono(),
            2 => protocol::ChannelMap::stereo(),
            other => bail!("shared mode plays mono and stereo, not {other} channels"),
        };
        let rate = spec.sample_rate;
        let frame_bytes = spec.channels * 4;
        let named = matches!(self.target, Target::Named(_));

        // A named output is what the user chose: if it goes away, the stream must not jump to the
        // speakers.
        // The stream starts at the volume that was set, so a new track never jumps in loudness.
        let (cvolume, muted) = self.volume.channel_volume(spec.channels as u8);
        let flags = protocol::stream::StreamFlags { no_move: named, start_muted: Some(muted), ..Default::default() };
        // How the stream is found on the server afterwards, to set its volume.
        let stream_id = NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed).to_string();
        let mut props = protocol::Props::new();
        props.set(protocol::Prop::ApplicationName, c"phonia");
        props.set(protocol::Prop::MediaRole, c"Music");
        props.set_bytes(c"phonia.stream-id", CString::new(stream_id.as_str())?.to_bytes_with_nul());
        props.set_bytes(c"resample.quality", RESAMPLE_QUALITY.to_bytes_with_nul());
        let params = protocol::PlaybackStreamParams {
            sample_spec: protocol::SampleSpec {
                format: protocol::SampleFormat::S32Le,
                channels: spec.channels as u8,
                sample_rate: rate,
            },
            channel_map,
            cvolume: Some(cvolume),
            sink_name: Some(if named { info.name.clone() } else { protocol::DEFAULT_SINK.to_owned() }),
            flags,
            props,
            buffer_attr: protocol::stream::BufferAttr {
                max_length: u32::MAX,
                target_length: rate * frame_bytes * TARGET_NUM / TARGET_DEN,
                // The default waits for seconds of audio before it starts.
                pre_buffering: 0,
                minimum_request_length: rate * frame_bytes / REQUEST_DIVISOR,
                fragment_size: u32::MAX,
            },
            ..Default::default()
        };

        let ring = Ring::new((rate / RING_DIVISOR) as usize, (rate / REQUEST_DIVISOR) as usize, spec.channels as usize);
        let prefix_frames = u64::from(rate / PREFIX_DIVISOR);
        ring.push_silence(prefix_frames as usize);
        let stream = match block_on(client.create_playback_stream(params, ring.source())) {
            Ok(stream) => stream,
            Err(error) => {
                // A dead connection is made again next time.
                self.forget_client();
                return Err(anyhow!(error)).context("creating the playback stream on the sound server");
            }
        };

        // Where the stream is on the server, so that its volume can be set and followed.
        let attached = match find_stream(&stream_id) {
            Ok((index, channels)) => Some((index, self.volume.attach(index, channels))),
            Err(error) => {
                crate::warn!("phonia: the volume can't be controlled for this stream: {error:#}");
                None
            }
        };
        let generation = attached.map_or(0, |(_, generation)| generation);
        let sink = named.then(|| (stream.sink(), output.description.clone()));
        let watcher = match Watcher::start(sink, attached.map_or(u32::MAX, |(index, _)| index), ring.clone(), self.volume.clone()) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                crate::warn!("phonia: not watching the output: {error:#}");
                None
            }
        };

        let route = SharedRoute {
            sink: output.description.clone(),
            sink_rate: output.rate,
            kind: output.kind.label().to_string(),
            codec: output.codec.clone(),
            lossy: output.lossy,
        };
        let report = self
            .on_report
            .clone()
            .map(|handler| (handler, SinkReport::shared(spec, "S32LE".to_string(), route)));
        let server_target_frames = u64::from(rate * TARGET_NUM / TARGET_DEN);
        let transport = PulseTransport { stream, _watcher: watcher, volume: self.volume.clone(), generation };
        Ok(Box::new(SharedSink::new(spec, ring, transport, prefix_frames, server_target_frames, report)))
    }
}

impl SinkFactory for SharedSinkFactory {
    fn open(&self, spec: SourceSpec) -> Result<Box<dyn AudioSink>> {
        self.open_stream(spec)
    }

    fn volume(&self) -> Option<Arc<dyn VolumeControl>> {
        Some(self.volume.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn props(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_bluetooth_output_is_lossy_and_names_its_codec() {
        let (kind, codec, lossy) = classify(
            "bluez_output.AA_BB_CC.1",
            &props(&[("device.api", "bluez5"), ("api.bluez5.codec", "ldac")]),
        );
        assert_eq!((kind, codec.as_deref(), lossy), (OutputKind::Bluetooth, Some("LDAC"), true));

        let (kind, codec, lossy) = classify("bluez_output.AA_BB_CC.1", &props(&[]));
        assert_eq!((kind, codec, lossy), (OutputKind::Bluetooth, None, true), "recognised by name too");
    }

    #[test]
    fn cards_are_classified_by_bus_and_name() {
        let usb = "alsa_output.usb-Speed_Dragon_Fosi_Audio_DS2_5000000001-01.analog-stereo";
        assert_eq!(classify(usb, &props(&[])).0, OutputKind::Usb);
        assert_eq!(classify("x", &props(&[("device.bus", "usb")])).0, OutputKind::Usb);
        assert_eq!(classify("alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__HDMI1__sink", &props(&[])).0, OutputKind::Hdmi);
        assert_eq!(
            classify("alsa_output.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Speaker__sink", &props(&[])).0,
            OutputKind::Internal
        );
        assert_eq!(classify("phonia_spike", &props(&[])), (OutputKind::Other, None, false));
    }

    #[test]
    fn a_wired_output_is_not_lossy() {
        assert!(!classify("alsa_output.usb-x", &props(&[])).2);
    }

    #[test]
    fn the_output_list_shows_the_default_and_marks_bluetooth_as_lossy() {
        let output = |name: &str, description: &str, kind, codec: Option<&str>, lossy, is_default| Output {
            name: name.into(),
            description: description.into(),
            kind,
            codec: codec.map(str::to_string),
            lossy,
            rate: 48_000,
            index: 1,
            is_default,
        };
        let text = format_outputs(&[
            output("alsa_output.usb-DS2", "Fosi Audio DS2 Analog Stereo", OutputKind::Usb, None, false, true),
            output("bluez_output.AA", "Soundcore Life P2", OutputKind::Bluetooth, Some("SBC"), true, false),
        ]);
        assert_eq!(
            text,
            "PipeWire outputs, shared and NOT bit-perfect (set mode = \"shared\" and sink = \"<name>\" under [output]):\n\
             \x20 * default              the desktop's default output, now Fosi Audio DS2 Analog Stereo\n\
             \x20   alsa_output.usb-DS2  Fosi Audio DS2 Analog Stereo  (USB)\n\
             \x20   bluez_output.AA      Soundcore Life P2  (Bluetooth, SBC, lossy)\n\
             A named output is never swapped for another if it goes away; \"default\" follows the desktop."
        );
    }

    #[test]
    fn an_empty_server_says_so() {
        assert!(format_outputs(&[]).contains("no outputs"));
    }

    #[test]
    fn the_target_names_the_default_or_one_output() {
        assert_eq!(Target::parse(None), Target::Default);
        assert_eq!(Target::parse(Some("default")), Target::Default);
        assert_eq!(Target::parse(Some("bluez_output.AA")), Target::Named("bluez_output.AA".into()));
    }

    #[test]
    fn server_properties_are_read_as_text_without_their_terminator() {
        let mut p = protocol::Props::new();
        p.set(protocol::Prop::MediaRole, c"music");
        assert_eq!(props_of(&p).get("media.role").map(String::as_str), Some("music"));
    }

    #[test]
    fn percentages_are_the_ones_the_desktop_mixers_show() {
        assert_eq!(raw_from_percent(100), 65_536);
        assert_eq!(raw_from_percent(0), 0);
        assert_eq!(raw_from_percent(50), 32_768);
        assert_eq!(raw_from_percent(200), 65_536, "phonia never boosts above unity gain");
        for percent in 0..=100u8 {
            assert_eq!(percent_from_raw(raw_from_percent(percent)), percent, "{percent}");
        }
        assert_eq!(percent_from_raw(98_304), 150, "a mixer may boost; it is reported as it is");
    }

    #[test]
    fn a_new_stream_starts_at_the_volume_that_was_set() {
        let volume = SharedVolume::new();
        volume.set(Volume { percent: 30, muted: true }).unwrap();
        let (channel_volume, muted) = volume.channel_volume(2);
        assert_eq!(channel_volume.channels().iter().map(|v| v.as_u32()).collect::<Vec<_>>(), [raw_from_percent(30); 2]);
        assert!(muted);
        assert_eq!(volume.get(), Volume { percent: 30, muted: true });
    }

    #[test]
    fn setting_more_than_unity_is_capped() {
        let volume = SharedVolume::new();
        volume.set(Volume { percent: 250, muted: false }).unwrap();
        assert_eq!(volume.get().percent, 100);
    }

    #[test]
    fn a_change_from_the_mixer_is_reported_once_and_only_for_the_stream_that_plays() {
        let volume = SharedVolume::new();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let seen = heard.clone();
        volume.on_change(Arc::new(move |v| seen.lock().unwrap().push(v)));
        let generation = volume.attach(7, 2);

        volume.changed_outside(9, Volume { percent: 10, muted: false }); // some other stream
        volume.changed_outside(7, Volume { percent: 40, muted: false });
        volume.changed_outside(7, Volume { percent: 40, muted: false }); // nothing changed
        assert_eq!(*heard.lock().unwrap(), [Volume { percent: 40, muted: false }]);
        assert_eq!(volume.get().percent, 40);

        volume.detach(generation);
        volume.changed_outside(7, Volume { percent: 20, muted: false });
        assert_eq!(heard.lock().unwrap().len(), 1, "the stream is gone");
    }

    #[test]
    fn what_the_server_reports_right_after_phonia_set_the_volume_is_its_own_echo() {
        let volume = SharedVolume::new();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let seen = heard.clone();
        volume.on_change(Arc::new(move |v| seen.lock().unwrap().push(v)));
        // (Set before a stream is attached, so that nothing is sent to a real server.)
        volume.set(Volume { percent: 45, muted: false }).unwrap();
        volume.attach(7, 2);
        volume.changed_outside(7, Volume { percent: 40, muted: false }); // an older value read back late
        assert!(heard.lock().unwrap().is_empty());
        assert_eq!(volume.get().percent, 45);

        std::thread::sleep(OWN_CHANGE_ECHO + Duration::from_millis(50));
        volume.changed_outside(7, Volume { percent: 20, muted: false }); // the mixer, later
        assert_eq!(volume.get().percent, 20);
    }

    #[test]
    fn an_older_stream_going_away_does_not_detach_a_newer_one() {
        let volume = SharedVolume::new();
        let old = volume.attach(1, 2);
        let new = volume.attach(2, 2);
        volume.detach(old);
        volume.changed_outside(2, Volume { percent: 55, muted: false });
        assert_eq!(volume.get().percent, 55, "the newer stream is still attached");
        volume.detach(new);
    }

    // ---- against the real sound server -------------------------------------------------------
    //
    // These use a null sink they create and remove themselves and record what reaches it with
    // `parec`, so nothing is ever played on a real output. They are ignored by default:
    // `cargo test -p phonia-core -- --ignored shared::pulse` (needs PipeWire or PulseAudio, `pactl`
    // and `parec`).

    struct NullSink(String);

    impl NullSink {
        /// Every test has a sink of its own: they run side by side.
        fn load(name: &str) -> Self {
            let out = std::process::Command::new("pactl")
                .args(["load-module", "module-null-sink", &format!("sink_name={name}"), "channels=2", "format=s32le", "rate=48000"])
                .output()
                .expect("pactl is needed for these tests");
            assert!(out.status.success(), "could not load a null sink: {}", String::from_utf8_lossy(&out.stderr));
            NullSink(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
    }

    impl Drop for NullSink {
        fn drop(&mut self) {
            let _ = std::process::Command::new("pactl").args(["unload-module", &self.0]).status();
        }
    }

    /// A 24-bit ramp, left-justified in `i32`, nonzero and unique per frame.
    fn ramp(frames: usize) -> Vec<i32> {
        (0..frames).flat_map(|i| [((i as i32 + 1) & 0x7f_ffff) << 8, -(((i as i32 + 1) & 0x7f_ffff) << 8)]).collect()
    }

    /// Records what reaches the null sink until dropped.
    struct Recorder {
        child: std::process::Child,
        path: std::path::PathBuf,
    }

    impl Recorder {
        fn start(name: &str, sink: &str) -> Self {
            let path = std::env::temp_dir().join(format!("phonia-shared-test-{}-{name}.raw", std::process::id()));
            let file = std::fs::File::create(&path).unwrap();
            let child = std::process::Command::new("parec")
                .args([&format!("--device={sink}.monitor"), "--format=s32le", "--rate=48000", "--channels=2", "--latency-msec=20", "--raw"])
                .stdout(file)
                .spawn()
                .expect("parec is needed for these tests");
            std::thread::sleep(Duration::from_millis(500));
            Recorder { child, path }
        }

        /// The runs `(first frame, length)` of the ramp in what was recorded, ignoring silence.
        fn runs(mut self) -> Vec<(i64, usize)> {
            std::thread::sleep(Duration::from_millis(500));
            // SIGINT makes parec flush what it has buffered before it exits; a kill would lose it.
            // SAFETY: signalling a child process we started and still hold.
            unsafe { libc::kill(self.child.id() as i32, libc::SIGINT) };
            let _ = self.child.wait();
            let bytes = std::fs::read(&self.path).unwrap();
            let _ = std::fs::remove_file(&self.path);
            let samples: Vec<i32> = bytes.as_chunks::<4>().0.iter().map(|c| i32::from_le_bytes(*c)).collect();
            let mut runs: Vec<(i64, usize)> = Vec::new();
            for frame in samples.as_chunks::<2>().0.iter().filter(|f| f[0] != 0 || f[1] != 0) {
                let index = i64::from(frame[0] >> 8) - 1;
                match runs.last_mut() {
                    Some((start, len)) if *start + *len as i64 == index => *len += 1,
                    _ => runs.push((index, 1)),
                }
            }
            runs
        }
    }

    fn write_all(sink: &mut dyn AudioSink, samples: &[i32], channels: usize) {
        let mut done = 0;
        while done < samples.len() {
            done += sink.write(&samples[done..]).unwrap() * channels;
        }
    }

    #[test]
    #[ignore = "needs a sound server, pactl and parec"]
    fn audio_reaches_a_named_output_complete_and_in_order_across_a_pause() {
        let sink_name = "phonia_test_pause";
        let _null = NullSink::load(sink_name);
        let recorder = Recorder::start("pause", sink_name);
        let spec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };
        let factory = SharedSinkFactory::new(Target::Named(sink_name.into()));
        let mut sink = factory.open(spec).unwrap();

        let audio = ramp(24_000);
        write_all(&mut *sink, &audio[..audio.len() / 2], 2);
        sink.pause().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(sink.delay_frames().unwrap() > 0, "audio is waiting while paused");
        sink.resume().unwrap();
        write_all(&mut *sink, &audio[audio.len() / 2..], 2);
        sink.drain().unwrap();
        assert!((sink.delay_frames().unwrap() as f64 / 48_000.0) < 0.2, "nothing is left after a drain");
        drop(sink);

        assert_eq!(recorder.runs(), [(0, 24_000)], "every frame, in order, none lost at the start (the silence prefix)");
    }

    #[test]
    #[ignore = "needs a sound server, pactl and parec"]
    fn the_stream_says_what_it_is_and_the_sink_is_reusable_after_a_drain() {
        let sink_name = "phonia_test_reuse";
        let _null = NullSink::load(sink_name);
        let recorder = Recorder::start("reuse", sink_name);
        let spec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };
        let reports = Arc::new(Mutex::new(Vec::new()));
        let seen = reports.clone();
        let factory = SharedSinkFactory::new(Target::Named(sink_name.into()))
            .on_report(Arc::new(move |report| seen.lock().unwrap().push(report)));
        let mut sink = factory.open(spec).unwrap();

        let audio = ramp(12_000);
        write_all(&mut *sink, &audio, 2);
        sink.drain().unwrap();
        let text = std::process::Command::new("pactl").args(["list", "sink-inputs"]).output().unwrap();
        let text = String::from_utf8_lossy(&text.stdout).to_string();
        assert!(text.contains("media.role = \"music\"") && text.contains("resample.quality = \"10\""), "{text}");

        // The same sink takes the next track: the server would have ended a stream that was drained.
        let more: Vec<i32> = ramp(24_000)[24_000..].to_vec();
        write_all(&mut *sink, &more, 2);
        sink.drain().unwrap();
        drop(sink);
        assert_eq!(recorder.runs(), [(0, 24_000)], "the two writes are one continuous ramp");

        let reports = reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert!(!reports[0].bit_perfect());
        assert!(reports[0].to_text().contains("SHARED (not bit-perfect)"), "{}", reports[0].to_text());
    }

    #[test]
    #[ignore = "needs a sound server, pactl and parec"]
    fn a_named_output_that_goes_away_is_reported_as_gone_and_never_replaced() {
        let sink_name = "phonia_test_gone";
        let null = NullSink::load(sink_name);
        let spec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };
        let factory = SharedSinkFactory::new(Target::Named(sink_name.into()));
        let mut sink = factory.open(spec).unwrap();
        write_all(&mut *sink, &ramp(4_800), 2);

        drop(null); // the output disappears under the stream
        let audio = ramp(48_000);
        let started = std::time::Instant::now();
        let error = loop {
            match sink.write(&audio) {
                Ok(_) => assert!(started.elapsed() < Duration::from_secs(10), "the loss was never noticed"),
                Err(error) => break error,
            }
        };
        assert!(error.downcast_ref::<crate::output::OutputGone>().is_some(), "{error:#}");
    }

    #[test]
    #[ignore = "needs a sound server"]
    fn a_missing_named_output_is_an_error_that_says_how_to_find_the_names() {
        let spec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };
        let error = SharedSinkFactory::new(Target::Named("no_such_output".into())).open(spec).err().unwrap();
        assert!(format!("{error:#}").contains("phonia devices"), "{error:#}");
    }

    fn pactl_output(args: &[&str]) -> String {
        String::from_utf8_lossy(&std::process::Command::new("pactl").args(args).output().unwrap().stdout).to_string()
    }

    /// The server's index for the sink called `name`.
    fn sink_index(name: &str) -> String {
        let sinks = pactl_output(&["list", "short", "sinks"]);
        sinks.lines().find(|line| line.split_whitespace().nth(1) == Some(name)).unwrap().split_whitespace().next().unwrap().to_string()
    }

    /// The volume line and mute of phonia's stream on the sink called `sink`. Tests run side by side
    /// and each has a sink of its own, so the sink says which stream is ours.
    fn server_volume(sink: &str) -> Option<(String, bool)> {
        let wanted = format!("Sink: {}", sink_index(sink));
        let text = pactl_output(&["list", "sink-inputs"]);
        let block = text
            .split("Sink Input #")
            .skip(1)
            .find(|block| block.contains("application.name = \"phonia\"") && block.lines().any(|line| line.trim() == wanted))?;
        let volume = block.lines().find(|line| line.trim_start().starts_with("Volume:"))?.to_string();
        let muted = block.lines().any(|line| line.trim() == "Mute: yes");
        Some((volume, muted))
    }

    /// The index of the stream playing on the sink called `sink`.
    fn stream_on(sink: &str) -> String {
        let sink = sink_index(sink);
        let streams = pactl_output(&["list", "short", "sink-inputs"]);
        streams.lines().find(|line| line.split_whitespace().nth(1) == Some(sink.as_str())).unwrap().split_whitespace().next().unwrap().to_string()
    }

    #[test]
    #[ignore = "needs a sound server and pactl"]
    fn the_volume_and_mute_reach_the_stream_and_a_new_stream_starts_at_them() {
        let sink_name = "phonia_test_volume";
        let _null = NullSink::load(sink_name);
        let spec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };
        let factory = SharedSinkFactory::new(Target::Named(sink_name.into()));
        let control = factory.volume().expect("a shared output has a volume");
        let mut sink = factory.open(spec).unwrap();
        write_all(&mut *sink, &ramp(4_800), 2);

        control.set(Volume { percent: 50, muted: false }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let (volume, muted) = server_volume(sink_name).expect("phonia's stream is on the server");
        assert!(volume.contains("50%") && !muted, "{volume}");

        control.set(Volume { percent: 50, muted: true }).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(server_volume(sink_name).unwrap().1, "muted");

        // The next track has another format: a new stream, at the same volume.
        drop(sink);
        let spec96 = SourceSpec { sample_rate: 96_000, ..spec };
        let mut second = factory.open(spec96).unwrap();
        write_all(&mut *second, &ramp(4_800), 2);
        std::thread::sleep(Duration::from_millis(300));
        let (volume, muted) = server_volume(sink_name).unwrap();
        assert!(volume.contains("50%") && muted, "{volume} muted={muted}");
    }

    #[test]
    #[ignore = "needs a sound server and pactl"]
    fn a_change_made_in_the_desktops_mixer_is_followed() {
        let sink_name = "phonia_test_mixer";
        let _null = NullSink::load(sink_name);
        let spec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };
        let factory = SharedSinkFactory::new(Target::Named(sink_name.into()));
        let control = factory.volume().unwrap();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let seen = heard.clone();
        control.on_change(Arc::new(move |volume| seen.lock().unwrap().push(volume)));
        let mut sink = factory.open(spec).unwrap();
        write_all(&mut *sink, &ramp(4_800), 2);

        let index = stream_on(sink_name);
        std::process::Command::new("pactl").args(["set-sink-input-volume", &index, "30%"]).status().unwrap();
        std::process::Command::new("pactl").args(["set-sink-input-mute", &index, "1"]).status().unwrap();
        for _ in 0..50 {
            if control.get() == (Volume { percent: 30, muted: true }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(control.get(), Volume { percent: 30, muted: true });
        assert!(!heard.lock().unwrap().is_empty(), "the handler was told");
    }
}
