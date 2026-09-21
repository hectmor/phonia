//! Bit-perfect ALSA sink.
//!
//! Opens the PCM device exactly as given by the caller (never `plughw`/`default` unless that is
//! literally the string passed in), negotiates a lossless integer hardware format, and packs
//! left-justified `i32` samples (see `crate::decode` for where that convention comes from) into
//! that format with pure bit shifts -- no rounding, no dithering, no resampling.

use alsa::pcm::{Access, Format, HwParams, PCM, State};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result, anyhow, bail};
use std::ffi::CString;
use std::sync::Arc;

use super::{AudioSink, SinkFactory};
use crate::decode::SourceSpec;

/// Target period/buffer sizes. Chosen as a reasonable phase-1-will-tune-this default: short
/// enough for responsive pause/seek/Ctrl+C, long enough not to underrun on a loaded system.
const PERIOD_TIME_US: u32 = 100_000;
const BUFFER_TIME_US: u32 = 500_000;

/// After this many consecutive zero-frame writes (each preceded by a `pcm.wait`), give up and
/// error out instead of silently discarding the rest of the chunk.
const MAX_CONSECUTIVE_ZERO_WRITES: u32 = 50;

/// Whether playback is running, and if not, how it was paused.
enum PauseState {
    Running,
    /// Paused with `snd_pcm_pause`: the device keeps its queued audio.
    Hardware,
    /// Paused with `snd_pcm_drop` because the device can't pause. `replay` holds the audio that
    /// had been queued but not yet played, to be written again on resume.
    Dropped { replay: Vec<u8> },
}

pub struct AlsaSink {
    pcm: PCM,
    device: String,
    format: Format,
    source: SourceSpec,
    period_frames: usize,
    can_pause: bool,
    scratch: Vec<u8>,
    /// The most recently written device-format bytes, at least as many as the device can hold
    /// queued. Needed to replay what a drop-based pause threw away.
    tail: TailBuffer,
    pause: PauseState,
    /// Told the bit-perfect verdict after the first successful write.
    on_report: Option<ReportHandler>,
}

impl AlsaSink {
    /// Opens `device` (e.g. `"hw:1,0"`) for playback and negotiates hardware parameters for
    /// `source`. Fails loudly and clearly if the device is busy, or if it cannot provide the
    /// exact sample rate / a lossless integer format for the source's bit depth.
    pub fn open(device: &str, source: SourceSpec) -> Result<Self> {
        let c_device = CString::new(device).context("invalid ALSA device name")?;

        let pcm = PCM::open(&c_device, Direction::Playback, false).map_err(|e| {
            if e.errno() == libc::EBUSY {
                anyhow!(
                    "device '{device}' is busy (EBUSY): PipeWire (or some other application) \
                     most likely has it open. Pause/disconnect playback to that DAC (e.g. \
                     `wpctl status`, or mute the card's profile in PipeWire) and try again.\n\
                     Original ALSA error: {e}"
                )
            } else {
                anyhow!("could not open ALSA device '{device}': {e}")
            }
        })?;

        // Scoped so every `HwParams` borrow of `pcm` (including the one behind
        // `hw_params_current()`) is dropped before `pcm` is moved into `AlsaSink` below.
        let (format, period_frames, buffer_frames, can_pause) = {
            let hwp =
                HwParams::any(&pcm).context("could not get the default hw_params")?;
            hwp.set_access(Access::RWInterleaved)
                .context("the device does not support interleaved access (RWInterleaved)")?;
            hwp.set_channels(source.channels)
                .with_context(|| format!("the device does not support {} channel(s)", source.channels))?;

            // Never let ALSA (or a plug layer above it) resample under us: bit-perfect means
            // the hardware runs at exactly the source's rate, or we fail loudly.
            hwp.set_rate_resample(false)
                .context("could not disable automatic resampling")?;
            hwp.set_rate(source.sample_rate, ValueOr::Nearest)
                .with_context(|| format!("could not request {} Hz", source.sample_rate))?;

            let format = pick_format(source.bits_per_sample, |f| hwp.test_format(f).is_ok())
                .with_context(|| format!("negotiating format for device '{device}'"))?;
            hwp.set_format(format).context("could not set the negotiated format")?;

            hwp.set_period_time_near(PERIOD_TIME_US, ValueOr::Nearest)
                .context("could not set the period size")?;
            hwp.set_buffer_time_near(BUFFER_TIME_US, ValueOr::Nearest)
                .context("could not set the buffer size")?;

            pcm.hw_params(&hwp).context("could not apply the hw_params")?;

            // Verify the rate that actually got committed to the hardware, since
            // ValueOr::Nearest may silently pick something else if the exact rate isn't
            // supported.
            let committed = pcm
                .hw_params_current()
                .context("could not read back the applied hw_params")?;
            let actual_rate = committed
                .get_rate()
                .context("could not read the applied sample rate")?;
            if actual_rate != source.sample_rate {
                bail!(
                    "device '{device}' does not support {} Hz natively (ALSA applied {} Hz instead); \
                     aborting to avoid losing the bit-perfect guarantee",
                    source.sample_rate,
                    actual_rate
                );
            }

            let period_frames = committed
                .get_period_size()
                .context("could not read the applied period size")?;
            let buffer_frames = committed
                .get_buffer_size()
                .context("could not read the applied buffer size")?;

            (format, period_frames.max(1) as usize, buffer_frames.max(1) as usize, committed.can_pause())
        };

        let bytes_per_frame = bytes_per_sample(format) * source.channels as usize;

        Ok(AlsaSink {
            pcm,
            device: device.to_string(),
            format,
            source,
            period_frames,
            can_pause,
            scratch: Vec::new(),
            tail: TailBuffer::new(buffer_frames * bytes_per_frame),
            pause: PauseState::Running,
            on_report: None,
        })
    }

    /// Reports the bit-perfect verdict (see [`AlsaSink::report`]) to `handler` once the first
    /// audio has been written, which is when the device reports the parameters it actually runs
    /// with.
    pub fn with_report_handler(mut self, handler: ReportHandler) -> Self {
        self.on_report = Some(handler);
        self
    }

    fn bytes_per_frame(&self) -> usize {
        bytes_per_sample(self.format) * self.source.channels as usize
    }

    /// Whether the device pauses in hardware. When it doesn't (typical for USB DACs), pausing
    /// drops the queue and replays it on resume instead.
    pub fn supports_hw_pause(&self) -> bool {
        self.can_pause
    }

    /// Reads back `/proc/asound/card<N>/pcm<D>p/sub0/hw_params` (only meaningful for `hw:N,D`
    /// devices), which is what the kernel says the running stream really uses, and reports it
    /// against what was asked for.
    pub fn report(&self) -> SinkReport {
        let proc = match parse_hw_device(&self.device) {
            None => ProcReading::NotHw,
            Some((card, device)) => {
                let path = format!("/proc/asound/card{card}/pcm{device}p/sub0/hw_params");
                match std::fs::read_to_string(&path) {
                    Ok(contents) => ProcReading::Read { path, contents },
                    Err(error) => ProcReading::Unreadable { path, error: error.to_string() },
                }
            }
        };
        SinkReport::new(self.device.clone(), self.source, self.format.to_string(), proc)
    }
}

/// What the kernel says about a running stream.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcReading {
    /// The device is not a raw `hw:N,D` one, so there is nothing to check (and a plug layer such
    /// as dmix or PipeWire may be resampling).
    NotHw,
    Unreadable { path: String, error: String },
    Read { path: String, contents: String },
}

/// Whether playback is bit-perfect, and the evidence: the device's own account of the format and
/// rate it runs at, compared with the source's.
#[derive(Debug, Clone, PartialEq)]
pub struct SinkReport {
    pub device: String,
    /// The format of the audio being played.
    pub source: SourceSpec,
    /// The ALSA sample format negotiated with the device, e.g. `S24_3LE`.
    pub negotiated_format: String,
    pub proc: ProcReading,
}

impl SinkReport {
    pub fn new(device: String, source: SourceSpec, negotiated_format: String, proc: ProcReading) -> Self {
        Self { device, source, negotiated_format, proc }
    }

    /// The sample rate the kernel reports for the running stream.
    pub fn device_rate(&self) -> Option<u32> {
        match &self.proc {
            ProcReading::Read { contents, .. } => extract_proc_rate(contents),
            _ => None,
        }
    }

    /// The sample format the kernel reports for the running stream.
    pub fn device_format(&self) -> Option<String> {
        match &self.proc {
            ProcReading::Read { contents, .. } => extract_proc_field(contents, "format"),
            _ => None,
        }
    }

    /// True only when the kernel confirms the device runs at exactly the source's rate and the
    /// format that was negotiated. Anything less (including not being able to check) is not.
    pub fn bit_perfect(&self) -> bool {
        self.device_rate() == Some(self.source.sample_rate)
            && self.device_format().as_deref() == Some(self.negotiated_format.as_str())
    }

    /// Why playback is not known to be bit-perfect; `None` when it is.
    pub fn problem(&self) -> Option<String> {
        if self.bit_perfect() {
            return None;
        }
        Some(match (&self.proc, self.device_rate(), self.device_format()) {
            (ProcReading::NotHw, _, _) => {
                "the device is not hw:N,D (possible resampling/mixing via dmix/PipeWire)".to_string()
            }
            (_, None, _) | (_, _, None) => "could not read /proc/asound to confirm it".to_string(),
            (_, Some(rate), _) if rate != self.source.sample_rate => {
                format!("the card reports {rate} Hz instead of {} Hz", self.source.sample_rate)
            }
            (_, _, Some(format)) => {
                format!("the card reports format {format} instead of {}", self.negotiated_format)
            }
        })
    }

    /// The report as the terminal player shows it: the kernel's own account, then the verdict.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        match &self.proc {
            ProcReading::NotHw => text.push_str(&format!(
                "Device '{}' is not in hw:N,D form; skipping the /proc/asound check.\n",
                self.device
            )),
            ProcReading::Unreadable { path, error } => {
                text.push_str(&format!("Warning: could not read {path}: {error}\n"));
            }
            ProcReading::Read { path, contents } => text.push_str(&format!("--- {path} ---\n{contents}")),
        }

        let source = format!(
            "FLAC {}-bit/{} Hz {}ch",
            self.source.bits_per_sample, self.source.sample_rate, self.source.channels
        );
        match self.problem() {
            None => text.push_str(&format!(
                "{source} → {} {} {} Hz  \u{2714} BIT-PERFECT",
                self.device,
                self.negotiated_format,
                self.source.sample_rate
            )),
            Some(reason) => text.push_str(&format!(
                "{source} → {} {}  \u{2716} CONVERTED ({reason})",
                self.device, self.negotiated_format
            )),
        }
        text
    }
}

/// Called on the audio thread with the report of a sink that has just started playing, so it
/// must not block.
pub type ReportHandler = Arc<dyn Fn(SinkReport) + Send + Sync>;

/// Writes all of `bytes` (whole frames of `bytes_per_frame` each) to `pcm`, blocking as needed.
fn write_all(pcm: &PCM, device: &str, bytes_per_frame: usize, bytes: &[u8]) -> Result<()> {
    let mut offset = 0usize;
    let mut consecutive_zero_writes = 0u32;
    while offset < bytes.len() {
        let io = pcm.io_bytes();
        match io.writei(&bytes[offset..]) {
            Ok(0) => {
                // In blocking mode this shouldn't normally happen, but never silently drop the
                // rest of the data: wait for the device to accept more and retry, and only give
                // up (loudly) if it's stuck for an unreasonably long time.
                drop(io);
                consecutive_zero_writes += 1;
                if consecutive_zero_writes > MAX_CONSECUTIVE_ZERO_WRITES {
                    bail!(
                        "device '{device}' stopped accepting audio ({consecutive_zero_writes} consecutive \
                         0-frame writes); aborting instead of silently dropping the rest of the chunk"
                    );
                }
                pcm.wait(Some(100)).context("waiting for the ALSA device to accept more data")?;
            }
            Ok(frames) => {
                consecutive_zero_writes = 0;
                offset += frames * bytes_per_frame;
            }
            Err(e) if e.errno() == libc::EPIPE => {
                crate::warn!("\nWarning: underrun (EPIPE) on '{device}', recovering...");
                drop(io);
                pcm.try_recover(e, true).context("could not recover from an underrun")?;
            }
            Err(e) => return Err(e).context("error writing to the ALSA device"),
        }
    }
    Ok(())
}

impl AudioSink for AlsaSink {
    fn spec(&self) -> SourceSpec {
        self.source
    }

    fn write(&mut self, samples: &[i32]) -> Result<usize> {
        if !matches!(self.pause, PauseState::Running) {
            bail!("write() called on a paused ALSA sink");
        }

        let channels = self.source.channels as usize;
        let frames = (samples.len() / channels).min(self.period_frames);
        if frames == 0 {
            return Ok(0);
        }

        self.scratch.clear();
        pack_frames(self.format, &samples[..frames * channels], &mut self.scratch);
        write_all(&self.pcm, &self.device, self.bytes_per_frame(), &self.scratch)?;
        self.tail.push(&self.scratch);

        if let Some(handler) = self.on_report.take() {
            handler(self.report());
        }
        Ok(frames)
    }

    fn delay_frames(&mut self) -> Result<u64> {
        if let PauseState::Dropped { replay } = &self.pause {
            return Ok((replay.len() / self.bytes_per_frame()) as u64);
        }
        // Only a running (or hardware-paused) stream has audio in flight. In any other state
        // `snd_pcm_delay` can report a stale residual (a few hundred frames right after
        // drop+prepare on the HDA card), and it fails outright after an underrun.
        match self.pcm.state() {
            State::Running | State::Paused | State::Draining => {
                Ok(self.pcm.delay().map(|d| d.max(0) as u64).unwrap_or(0))
            }
            _ => Ok(0),
        }
    }

    fn pause(&mut self) -> Result<()> {
        if !matches!(self.pause, PauseState::Running) {
            return Ok(());
        }

        if self.can_pause && self.pcm.pause(true).is_ok() {
            self.pause = PauseState::Hardware;
            return Ok(());
        }

        // The frames still queued in the device have not reached the DAC. Grab them from the
        // tail before dropping, so resuming can replay them instead of skipping that audio.
        let bytes_per_frame = self.bytes_per_frame();
        let pending_frames = self
            .pcm
            .delay()
            .map(|d| d.max(0) as usize)
            .unwrap_or(0)
            .min(self.tail.len() / bytes_per_frame);
        let replay = self.tail.take_last(pending_frames * bytes_per_frame);

        self.pcm.drop().context("could not drop() the ALSA device to pause")?;
        self.tail.clear();
        self.pause = PauseState::Dropped { replay };
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        match std::mem::replace(&mut self.pause, PauseState::Running) {
            PauseState::Running => {}
            PauseState::Hardware => self.pcm.pause(false).context("could not resume the ALSA device")?,
            PauseState::Dropped { replay } => {
                self.pcm.prepare().context("could not prepare() the ALSA device to resume")?;
                write_all(&self.pcm, &self.device, self.bytes_per_frame(), &replay)?;
                self.tail.push(&replay);
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.pcm.drop().context("could not drop() the ALSA device")?;
        self.pcm.prepare().context("could not prepare() the ALSA device")?;
        self.tail.clear();
        self.pause = PauseState::Running;
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        // A paused device would never finish draining.
        self.resume()?;
        self.pcm.drain().context("could not drain() the ALSA device")?;
        // Draining leaves the device in the setup state, where it refuses writes (EBADFD) until
        // it is prepared. The engine reuses the sink for the next track of the same format, so
        // hand it back ready for more audio.
        self.pcm.prepare().context("could not prepare() the ALSA device after draining")?;
        self.tail.clear();
        Ok(())
    }
}

/// Opens [`AlsaSink`]s on one device, for the playback engine.
pub struct AlsaSinkFactory {
    device: String,
    on_report: Option<ReportHandler>,
}

impl AlsaSinkFactory {
    pub fn new(device: impl Into<String>) -> Self {
        Self { device: device.into(), on_report: None }
    }

    /// Have every sink hand its bit-perfect verdict to `handler` when it starts playing.
    pub fn on_report(mut self, handler: ReportHandler) -> Self {
        self.on_report = Some(handler);
        self
    }
}

impl SinkFactory for AlsaSinkFactory {
    fn open(&self, spec: SourceSpec) -> Result<Box<dyn AudioSink>> {
        let sink = AlsaSink::open(&self.device, spec)?;
        Ok(Box::new(match &self.on_report {
            Some(handler) => sink.with_report_handler(handler.clone()),
            None => sink,
        }))
    }
}

/// The last `capacity` bytes written to the device, kept so audio thrown away by a drop-based
/// pause can be replayed. Only ever holds whole frames, so trimming from the front (`capacity`
/// is a multiple of the frame size) never splits one.
struct TailBuffer {
    capacity: usize,
    bytes: Vec<u8>,
}

impl TailBuffer {
    fn new(capacity: usize) -> Self {
        Self { capacity, bytes: Vec::new() }
    }

    fn len(&self) -> usize {
        self.bytes.len().min(self.capacity)
    }

    fn push(&mut self, data: &[u8]) {
        self.bytes.extend_from_slice(data);
        // Trim in batches rather than on every push.
        if self.bytes.len() > self.capacity * 2 {
            self.bytes.drain(..self.bytes.len() - self.capacity);
        }
    }

    /// The last `n` bytes (fewer if less is held).
    fn take_last(&self, n: usize) -> Vec<u8> {
        let n = n.min(self.len());
        self.bytes[self.bytes.len() - n..].to_vec()
    }

    fn clear(&mut self) {
        self.bytes.clear();
    }
}

/// Lists which sample formats and rates `device` accepts, using `HwParams::test_format` /
/// `test_rate`. This opens the device in playback mode (without ever writing to it) purely to
/// query its capabilities -- meant to be run by the user on their own hardware.
pub fn probe_device(device: &str) -> Result<()> {
    let c_device = CString::new(device).context("invalid ALSA device name")?;
    let pcm = PCM::open(&c_device, Direction::Playback, false)
        .with_context(|| format!("opening device {device}"))?;
    let hwp = HwParams::any(&pcm).context("could not get the default hw_params")?;

    println!("Formats supported on {device}:");
    for format in [Format::S16LE, Format::S243LE, Format::S24LE, Format::S32LE] {
        let ok = hwp.test_format(format).is_ok();
        println!("  {:<10} {}", format.to_string(), if ok { "yes" } else { "no" });
    }

    println!("\nRates supported on {device}:");
    for rate in [44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000] {
        let ok = hwp.test_rate(rate).is_ok();
        println!("  {:>7} Hz  {}", rate, if ok { "yes" } else { "no" });
    }

    Ok(())
}

/// Picks the first ALSA format the device accepts, in the priority order the phase-0 spec
/// requires for each source bit depth. Takes `test_format` as a closure so this can be unit
/// tested without opening a real device (the hard rule for this phase is: never open the
/// hardware ourselves).
fn pick_format(bits_per_sample: u32, mut test_format: impl FnMut(Format) -> bool) -> Result<Format> {
    let candidates: &[Format] = match bits_per_sample {
        24 => &[Format::S243LE, Format::S32LE, Format::S24LE],
        16 => &[Format::S16LE, Format::S32LE],
        other => bail!("unsupported bit depth in phase 0: {other} bits (only 16/24)"),
    };

    candidates
        .iter()
        .copied()
        .find(|f| test_format(*f))
        .ok_or_else(|| {
            anyhow!(
                "the device does not accept any lossless integer format for a {bits_per_sample}-bit source \
                 (tried: {candidates:?})"
            )
        })
}

fn bytes_per_sample(format: Format) -> usize {
    match format {
        Format::S16LE => 2,
        Format::S243LE => 3,
        Format::S24LE => 4,
        Format::S32LE => 4,
        other => unreachable!("pick_format should never choose {other}"),
    }
}

/// Packs one left-justified `i32` sample into `format`'s wire representation. All conversions
/// are pure bit shifts (no rounding), so they are lossless in both directions.
fn pack_sample(format: Format, sample: i32, out: &mut Vec<u8>) {
    match format {
        Format::S16LE => out.extend_from_slice(&pack_s16le(sample)),
        Format::S243LE => out.extend_from_slice(&pack_s24_3le(sample)),
        Format::S24LE => out.extend_from_slice(&pack_s24le(sample)),
        Format::S32LE => out.extend_from_slice(&pack_s32le(sample)),
        other => unreachable!("pick_format should never choose {other}"),
    }
}

fn pack_frames(format: Format, samples: &[i32], out: &mut Vec<u8>) {
    out.reserve(samples.len() * bytes_per_sample(format));
    for &s in samples {
        pack_sample(format, s, out);
    }
}

/// Left-justified i32 -> S16_LE: keep the top 16 bits.
fn pack_s16le(sample: i32) -> [u8; 2] {
    ((sample >> 16) as i16).to_le_bytes()
}

/// Left-justified i32 -> S24_3LE (3-byte packed container): keep the top 24 bits.
fn pack_s24_3le(sample: i32) -> [u8; 3] {
    let v = sample >> 8;
    let bytes = v.to_le_bytes();
    [bytes[0], bytes[1], bytes[2]]
}

/// Left-justified i32 -> S24_LE (4-byte container, low 3 bytes significant): keep the top 24
/// bits, in a 32-bit slot.
fn pack_s24le(sample: i32) -> [u8; 4] {
    (sample >> 8).to_le_bytes()
}

/// Left-justified i32 -> S32_LE: identity.
fn pack_s32le(sample: i32) -> [u8; 4] {
    sample.to_le_bytes()
}

/// Parses `"hw:N,D"` into `(N, D)`. Returns `None` for anything else (`"default"`, `"plughw:..."`,
/// etc.), since the `/proc/asound` diagnostic only makes sense for a raw hw device.
fn parse_hw_device(device: &str) -> Option<(u32, u32)> {
    let rest = device.strip_prefix("hw:")?;
    let (card, pcm) = rest.split_once(',')?;
    Some((card.parse().ok()?, pcm.parse().ok()?))
}

/// Extracts a `key: value` line from an ALSA `/proc/asound/.../hw_params`-style text blob.
fn extract_proc_field(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

/// The `rate` line looks like `rate: 96000 (96000/1)`; we only want the leading integer.
fn extract_proc_rate(contents: &str) -> Option<u32> {
    let value = extract_proc_field(contents, "rate")?;
    value.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_s24_3le_matches_the_documented_example() {
        // 24-bit sample 0x123456, left-justified in an i32 as 0x12345600.
        let left_justified = 0x1234_5600_u32 as i32;
        assert_eq!(pack_s24_3le(left_justified), [0x56, 0x34, 0x12]);
    }

    #[test]
    fn pack_s24_3le_handles_negative_values() {
        // -1 as a 24-bit two's complement value, left-justified: 0xFFFFFF00.
        let left_justified = 0xFFFF_FF00_u32 as i32;
        assert_eq!(pack_s24_3le(left_justified), [0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn pack_s16le_keeps_the_top_16_bits() {
        let left_justified = 0x1234_0000_u32 as i32;
        assert_eq!(pack_s16le(left_justified), [0x34, 0x12]);
    }

    #[test]
    fn pack_s16le_handles_negative_values() {
        let left_justified = 0xFFFF_0000_u32 as i32; // -1 as 16-bit, left-justified.
        assert_eq!(pack_s16le(left_justified), [0xFF, 0xFF]);
    }

    #[test]
    fn pack_s32le_is_an_identity_shift() {
        assert_eq!(pack_s32le(0x1234_5678), [0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn pack_s24le_keeps_top_24_bits_in_a_32_bit_slot() {
        let left_justified = 0x1234_5600_u32 as i32;
        assert_eq!(pack_s24le(left_justified), [0x56, 0x34, 0x12, 0x00]);
    }

    #[test]
    fn pick_format_prefers_s24_3le_for_24_bit_sources() {
        let format = pick_format(24, |_f| true).unwrap();
        assert_eq!(format, Format::S243LE);
    }

    #[test]
    fn pick_format_falls_back_when_preferred_formats_are_rejected() {
        let format = pick_format(24, |f| f == Format::S24LE).unwrap();
        assert_eq!(format, Format::S24LE);
    }

    #[test]
    fn pick_format_for_16_bit_prefers_s16le() {
        let format = pick_format(16, |_f| true).unwrap();
        assert_eq!(format, Format::S16LE);
    }

    #[test]
    fn pick_format_errors_on_unsupported_bit_depth() {
        assert!(pick_format(20, |_f| true).is_err());
    }

    #[test]
    fn pick_format_errors_when_device_accepts_nothing() {
        assert!(pick_format(24, |_f| false).is_err());
    }

    #[test]
    fn parse_hw_device_extracts_card_and_device() {
        assert_eq!(parse_hw_device("hw:1,0"), Some((1, 0)));
        assert_eq!(parse_hw_device("hw:0,2"), Some((0, 2)));
    }

    #[test]
    fn parse_hw_device_rejects_non_hw_strings() {
        assert_eq!(parse_hw_device("default"), None);
        assert_eq!(parse_hw_device("plughw:1,0"), None);
    }

    #[test]
    fn extract_proc_rate_parses_the_leading_integer() {
        let contents = "access: RW_INTERLEAVED\nformat: S24_3LE\nrate: 96000 (96000/1)\n";
        assert_eq!(extract_proc_rate(contents), Some(96_000));
    }

    #[test]
    fn extract_proc_field_reads_format_line() {
        let contents = "access: RW_INTERLEAVED\nformat: S24_3LE\nrate: 96000 (96000/1)\n";
        assert_eq!(extract_proc_field(contents, "format").as_deref(), Some("S24_3LE"));
    }

    #[test]
    fn tail_buffer_returns_the_most_recent_bytes() {
        let mut tail = TailBuffer::new(8);
        tail.push(&[1, 2, 3, 4]);
        tail.push(&[5, 6]);
        assert_eq!(tail.take_last(3), vec![4, 5, 6]);
        assert_eq!(tail.take_last(6), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn tail_buffer_never_reports_more_than_its_capacity() {
        let mut tail = TailBuffer::new(4);
        for chunk in 0u8..20 {
            tail.push(&[chunk * 2, chunk * 2 + 1]);
        }
        assert_eq!(tail.len(), 4);
        assert_eq!(tail.take_last(100), vec![36, 37, 38, 39]);
    }

    #[test]
    fn tail_buffer_take_last_on_an_empty_buffer_is_empty() {
        let tail = TailBuffer::new(8);
        assert!(tail.take_last(4).is_empty());
    }

    #[test]
    fn tail_buffer_clear_forgets_everything() {
        let mut tail = TailBuffer::new(8);
        tail.push(&[1, 2, 3, 4]);
        tail.clear();
        assert_eq!(tail.len(), 0);
        assert!(tail.take_last(4).is_empty());
    }

    /// Needs a real DAC that nothing else (e.g. PipeWire) has open. Writes silence only, so
    /// nothing is audible. Run with:
    /// `PHONIA_TEST_DEVICE=hw:1,0 cargo test -p phonia-core hardware -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a real, free ALSA device"]
    fn hardware_pause_resume_flush_accounting() {
        let device = std::env::var("PHONIA_TEST_DEVICE").unwrap_or_else(|_| "hw:1,0".into());
        let spec = SourceSpec { sample_rate: 48_000, channels: 2, bits_per_sample: 24 };
        let mut sink = AlsaSink::open(&device, spec).expect("opening the device");
        println!("{device}: hw pause supported = {}", sink.supports_hw_pause());

        let silence = vec![0i32; 48_000 / 10 * 2];
        let mut written = 0u64;
        for _ in 0..3 {
            let mut offset = 0;
            while offset < silence.len() {
                let frames = sink.write(&silence[offset..]).unwrap();
                assert!(frames > 0);
                offset += frames * 2;
                written += frames as u64;
            }
        }

        let before = sink.delay_frames().unwrap();
        assert!(before > 0 && before <= written, "delay {before} vs written {written}");

        sink.pause().unwrap();
        let paused = sink.delay_frames().unwrap();
        println!("delay before pause = {before}, while paused = {paused}");
        assert!(paused > 0, "a pause must not lose the queued audio");
        assert!(paused <= before + 1, "delay grew while paused: {paused} > {before}");
        assert!(sink.write(&silence).is_err(), "writing while paused must be rejected");

        sink.resume().unwrap();
        assert!(sink.delay_frames().unwrap() > 0, "resume must bring the queued audio back");

        sink.flush().unwrap();
        assert_eq!(sink.delay_frames().unwrap(), 0);
        assert!(sink.write(&silence).unwrap() > 0, "the sink must accept audio after a flush");

        sink.drain().unwrap();
        // A track that ends by itself drains the sink, and the next track of the same format
        // reuses it: writing after a drain has to work (it failed with EBADFD before).
        assert!(sink.write(&silence).unwrap() > 0, "the sink must accept audio after a drain");
        sink.drain().unwrap();
    }

    const HW_PARAMS_96K: &str = "access: RW_INTERLEAVED\nformat: S24_3LE\nsubformat: STD\nchannels: 2\nrate: 96000 (96000/1)\nperiod_size: 9600\nbuffer_size: 48000\n";
    const PROC_PATH: &str = "/proc/asound/card1/pcm0p/sub0/hw_params";

    fn report(device: &str, proc: ProcReading) -> SinkReport {
        let source = SourceSpec { sample_rate: 96_000, channels: 2, bits_per_sample: 24 };
        SinkReport::new(device.to_string(), source, "S24_3LE".to_string(), proc)
    }

    fn read(contents: &str) -> ProcReading {
        ProcReading::Read { path: PROC_PATH.to_string(), contents: contents.to_string() }
    }

    #[test]
    fn a_matching_device_is_bit_perfect_and_the_text_is_pinned() {
        let report = report("hw:1,0", read(HW_PARAMS_96K));
        assert!(report.bit_perfect());
        assert_eq!(report.problem(), None);
        assert_eq!(
            report.to_text(),
            format!("--- {PROC_PATH} ---\n{HW_PARAMS_96K}FLAC 24-bit/96000 Hz 2ch → hw:1,0 S24_3LE 96000 Hz  \u{2714} BIT-PERFECT")
        );
    }

    #[test]
    fn a_different_rate_is_converted() {
        let contents = HW_PARAMS_96K.replace("96000 (96000/1)", "48000 (48000/1)");
        let report = report("hw:1,0", read(&contents));
        assert!(!report.bit_perfect());
        assert_eq!(report.problem().as_deref(), Some("the card reports 48000 Hz instead of 96000 Hz"));
        assert!(report.to_text().ends_with(
            "FLAC 24-bit/96000 Hz 2ch → hw:1,0 S24_3LE  \u{2716} CONVERTED (the card reports 48000 Hz instead of 96000 Hz)"
        ));
    }

    #[test]
    fn a_different_format_is_converted() {
        let contents = HW_PARAMS_96K.replace("S24_3LE", "S32_LE");
        let report = report("hw:1,0", read(&contents));
        assert!(!report.bit_perfect());
        assert_eq!(report.problem().as_deref(), Some("the card reports format S32_LE instead of S24_3LE"));
    }

    #[test]
    fn a_device_that_is_not_hw_cannot_be_confirmed() {
        let report = report("default", ProcReading::NotHw);
        assert!(!report.bit_perfect());
        assert_eq!(
            report.to_text(),
            "Device 'default' is not in hw:N,D form; skipping the /proc/asound check.\n\
             FLAC 24-bit/96000 Hz 2ch → default S24_3LE  \u{2716} CONVERTED (the device is not hw:N,D (possible resampling/mixing via dmix/PipeWire))"
        );
    }

    #[test]
    fn an_unreadable_proc_file_is_reported_and_not_assumed_fine() {
        let report = report("hw:1,0", ProcReading::Unreadable { path: PROC_PATH.to_string(), error: "No such file".to_string() });
        assert!(!report.bit_perfect());
        assert_eq!(
            report.to_text(),
            format!("Warning: could not read {PROC_PATH}: No such file\nFLAC 24-bit/96000 Hz 2ch → hw:1,0 S24_3LE  ✖ CONVERTED (could not read /proc/asound to confirm it)")
        );
    }

    #[test]
    fn a_proc_file_missing_the_rate_line_cannot_confirm_either() {
        let report = report("hw:1,0", read("format: S24_3LE\n"));
        assert!(!report.bit_perfect());
        assert_eq!(report.problem().as_deref(), Some("could not read /proc/asound to confirm it"));
    }
}

