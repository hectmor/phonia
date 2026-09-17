//! Bit-perfect ALSA sink.
//!
//! Opens the PCM device exactly as given by the caller (never `plughw`/`default` unless that is
//! literally the string passed in), negotiates a lossless integer hardware format, and packs
//! left-justified `i32` samples (see `crate::decode` for where that convention comes from) into
//! that format with pure bit shifts -- no rounding, no dithering, no resampling.

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result, anyhow, bail};
use std::ffi::CString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::decode::{ChunkAction, SourceSpec};

/// Target period/buffer sizes. Chosen as a reasonable phase-1-will-tune-this default: short
/// enough for responsive Ctrl+C, long enough not to underrun on a loaded system.
const PERIOD_TIME_US: u32 = 100_000;
const BUFFER_TIME_US: u32 = 500_000;

/// After this many consecutive zero-frame writes (each preceded by a `pcm.wait`), give up and
/// error out instead of silently discarding the rest of the chunk.
const MAX_CONSECUTIVE_ZERO_WRITES: u32 = 50;

pub struct AlsaSink {
    pcm: PCM,
    device: String,
    format: Format,
    source: SourceSpec,
    total_duration: Option<Duration>,
    stop: Arc<AtomicBool>,
    scratch: Vec<u8>,
    frames_written: u64,
    started_at: Option<Instant>,
    diagnostics_printed: bool,
}

impl AlsaSink {
    /// Opens `device` (e.g. `"hw:1,0"`) for playback and negotiates hardware parameters for
    /// `source`. Fails loudly and clearly if the device is busy, or if it cannot provide the
    /// exact sample rate / a lossless integer format for the source's bit depth.
    pub fn open(
        device: &str,
        source: SourceSpec,
        total_duration: Option<Duration>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        let c_device = CString::new(device).context("nombre de dispositivo ALSA inválido")?;

        let pcm = PCM::open(&c_device, Direction::Playback, false).map_err(|e| {
            if e.errno() == libc::EBUSY {
                anyhow!(
                    "el dispositivo '{device}' está ocupado (EBUSY): lo más probable es que \
                     PipeWire (o alguna otra aplicación) lo tenga abierto. Pausa/desconecta la \
                     reproducción hacia ese DAC (ej. `wpctl status`/silenciar el perfil de la \
                     tarjeta en PipeWire) e inténtalo de nuevo.\nError original de ALSA: {e}"
                )
            } else {
                anyhow!("no se pudo abrir el dispositivo ALSA '{device}': {e}")
            }
        })?;

        // Scoped so every `HwParams` borrow of `pcm` (including the one behind
        // `hw_params_current()`) is dropped before `pcm` is moved into `AlsaSink` below.
        let format = {
            let hwp =
                HwParams::any(&pcm).context("no se pudieron obtener los hw_params por defecto")?;
            hwp.set_access(Access::RWInterleaved)
                .context("el dispositivo no soporta acceso entrelazado (RWInterleaved)")?;
            hwp.set_channels(source.channels)
                .with_context(|| format!("el dispositivo no soporta {} canal(es)", source.channels))?;

            // Never let ALSA (or a plug layer above it) resample under us: bit-perfect means
            // the hardware runs at exactly the source's rate, or we fail loudly.
            hwp.set_rate_resample(false)
                .context("no se pudo desactivar el resampling automático")?;
            hwp.set_rate(source.sample_rate, ValueOr::Nearest)
                .with_context(|| format!("no se pudo pedir {} Hz", source.sample_rate))?;

            let format = pick_format(source.bits_per_sample, |f| hwp.test_format(f).is_ok())
                .with_context(|| format!("negociando formato para el dispositivo '{device}'"))?;
            hwp.set_format(format).context("no se pudo fijar el formato negociado")?;

            hwp.set_period_time_near(PERIOD_TIME_US, ValueOr::Nearest)
                .context("no se pudo fijar el tamaño de periodo")?;
            hwp.set_buffer_time_near(BUFFER_TIME_US, ValueOr::Nearest)
                .context("no se pudo fijar el tamaño de buffer")?;

            pcm.hw_params(&hwp).context("no se pudieron aplicar los hw_params")?;

            // Verify the rate that actually got committed to the hardware, since
            // ValueOr::Nearest may silently pick something else if the exact rate isn't
            // supported.
            let committed = pcm
                .hw_params_current()
                .context("no se pudieron leer los hw_params ya aplicados")?;
            let actual_rate = committed
                .get_rate()
                .context("no se pudo leer la frecuencia de muestreo aplicada")?;
            if actual_rate != source.sample_rate {
                bail!(
                    "el dispositivo '{device}' no soporta {} Hz de forma nativa (ALSA aplicó {} Hz en su lugar); \
                     abortando para no perder la garantía de bit-perfect",
                    source.sample_rate,
                    actual_rate
                );
            }

            format
        };

        Ok(AlsaSink {
            pcm,
            device: device.to_string(),
            format,
            source,
            total_duration,
            stop,
            scratch: Vec::new(),
            frames_written: 0,
            started_at: None,
            diagnostics_printed: false,
        })
    }

    fn bytes_per_frame(&self) -> usize {
        bytes_per_sample(self.format) * self.source.channels as usize
    }

    /// Packs and writes one decoded chunk. Returns [`ChunkAction::Stop`] once Ctrl+C has been
    /// requested, so the caller's decode loop can unwind cleanly.
    pub fn write_chunk(&mut self, samples: &[i32]) -> Result<ChunkAction> {
        if self.stop.load(Ordering::Relaxed) {
            return Ok(ChunkAction::Stop);
        }
        if self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }

        self.scratch.clear();
        pack_frames(self.format, samples, &mut self.scratch);

        let bytes_per_frame = self.bytes_per_frame();
        let mut offset = 0usize;
        let mut consecutive_zero_writes = 0u32;
        while offset < self.scratch.len() {
            if self.stop.load(Ordering::Relaxed) {
                return Ok(ChunkAction::Stop);
            }

            let io = self.pcm.io_bytes();
            match io.writei(&self.scratch[offset..]) {
                Ok(0) => {
                    // In blocking mode this shouldn't normally happen, but never silently drop
                    // the rest of the chunk: wait for the device to accept more and retry, and
                    // only give up (loudly) if it's stuck for an unreasonably long time.
                    drop(io);
                    consecutive_zero_writes += 1;
                    if consecutive_zero_writes > MAX_CONSECUTIVE_ZERO_WRITES {
                        bail!(
                            "el dispositivo '{}' dejó de aceptar audio ({} escrituras de 0 frames seguidas); \
                             abortando en vez de descartar el resto del bloque en silencio",
                            self.device,
                            consecutive_zero_writes
                        );
                    }
                    self.pcm
                        .wait(Some(100))
                        .context("esperando a que el dispositivo ALSA acepte más datos")?;
                }
                Ok(frames) => {
                    consecutive_zero_writes = 0;
                    offset += frames * bytes_per_frame;
                    self.frames_written += frames as u64;
                }
                Err(e) if e.errno() == libc::EPIPE => {
                    eprintln!("\nAviso: underrun (EPIPE) en '{}', recuperando...", self.device);
                    drop(io);
                    self.pcm
                        .try_recover(e, true)
                        .context("no se pudo recuperar tras un underrun")?;
                }
                Err(e) => return Err(e).context("error escribiendo al dispositivo ALSA"),
            }
        }

        if !self.diagnostics_printed {
            self.print_first_write_diagnostics();
            self.diagnostics_printed = true;
        }
        self.print_progress();

        Ok(ChunkAction::Continue)
    }

    fn print_progress(&self) {
        let elapsed_secs = self.frames_written as f64 / self.source.sample_rate as f64;
        let total_secs = self.total_duration.map(|d| d.as_secs_f64());
        print!("\r{}", format_progress(elapsed_secs, total_secs));
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    /// Reads back `/proc/asound/card<N>/pcm<D>p/sub0/hw_params` (only meaningful for `hw:N,D`
    /// devices) and prints the bit-perfect verdict for this playback session.
    fn print_first_write_diagnostics(&self) {
        println!();
        let Some((card, device)) = parse_hw_device(&self.device) else {
            println!(
                "Dispositivo '{}' no tiene forma hw:N,D; omito la verificación de /proc/asound.",
                self.device
            );
            self.print_verdict(None, None, false);
            return;
        };

        let path = format!("/proc/asound/card{card}/pcm{device}p/sub0/hw_params");
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                println!("--- {path} ---");
                print!("{contents}");
                let proc_rate = extract_proc_rate(&contents);
                let proc_format = extract_proc_field(&contents, "format");
                self.print_verdict(proc_rate, proc_format, true);
            }
            Err(e) => {
                println!("Aviso: no se pudo leer {path}: {e}");
                self.print_verdict(None, None, true);
            }
        }
    }

    fn print_verdict(&self, proc_rate: Option<u32>, proc_format: Option<String>, is_hw: bool) {
        let fuente = format!(
            "FLAC {}-bit/{} Hz {}ch",
            self.source.bits_per_sample, self.source.sample_rate, self.source.channels
        );
        let negotiated_format = self.format.to_string();

        let bit_perfect = is_hw
            && proc_rate == Some(self.source.sample_rate)
            && proc_format.as_deref() == Some(negotiated_format.as_str());

        if bit_perfect {
            println!(
                "{fuente} → {} {} {} Hz  \u{2714} BIT-PERFECT",
                self.device,
                negotiated_format,
                proc_rate.unwrap()
            );
        } else {
            let reason = if !is_hw {
                "el dispositivo no es hw:N,D (posible resampling/mezcla vía dmix/PipeWire)".to_string()
            } else {
                match (proc_rate, &proc_format) {
                    (None, _) | (_, None) => {
                        "no se pudo leer /proc/asound para confirmarlo".to_string()
                    }
                    (Some(r), Some(f)) if r != self.source.sample_rate => {
                        format!("la tarjeta reporta {r} Hz en vez de {} Hz", self.source.sample_rate)
                    }
                    (Some(_), Some(f)) => {
                        format!("la tarjeta reporta el formato {f} en vez de {negotiated_format}")
                    }
                }
            };
            println!("{fuente} → {} {}  \u{2716} CONVERTED ({reason})", self.device, negotiated_format);
        }
    }

    /// Drains the device so the last period is fully played out before returning.
    pub fn finish(self) -> Result<()> {
        println!();
        self.pcm.drain().context("no se pudo hacer drain() del dispositivo ALSA")?;
        Ok(())
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
        other => bail!("profundidad de bits no soportada en la fase 0: {other} bits (solo 16/24)"),
    };

    candidates
        .iter()
        .copied()
        .find(|f| test_format(*f))
        .ok_or_else(|| {
            anyhow!(
                "el dispositivo no acepta ningún formato entero sin pérdidas para una fuente de {bits_per_sample} bits \
                 (probé: {candidates:?})"
            )
        })
}

fn bytes_per_sample(format: Format) -> usize {
    match format {
        Format::S16LE => 2,
        Format::S243LE => 3,
        Format::S24LE => 4,
        Format::S32LE => 4,
        other => unreachable!("pick_format nunca debería elegir {other}"),
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
        other => unreachable!("pick_format nunca debería elegir {other}"),
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

fn format_progress(elapsed_secs: f64, total_secs: Option<f64>) -> String {
    match total_secs {
        Some(total) => format!("{:>6.1}s / {:>6.1}s", elapsed_secs, total),
        None => format!("{:>6.1}s", elapsed_secs),
    }
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
    fn format_progress_without_total() {
        assert_eq!(format_progress(12.3, None), "  12.3s");
    }

    #[test]
    fn format_progress_with_total() {
        assert_eq!(format_progress(12.3, Some(205.1)), "  12.3s /  205.1s");
    }
}
