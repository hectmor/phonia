//! Symphonia decode loop: turns a FLAC or fragmented-MP4 (DASH) byte stream into interleaved
//! `i32` PCM, streamed to a callback so the whole track never has to sit in memory at once.
//!
//! ## FLAC integer scaling (bit-perfect path)
//!
//! `symphonia-bundle-flac` decodes each FLAC subframe into raw `bits_per_sample`-wide signed
//! integers, then **left-justifies** them into the internal `i32` buffer before handing them
//! back to the caller. See `symphonia-bundle-flac-0.6.1/src/decoder.rs:239-241`
//! (`FlacDecoder::decode_inner`):
//!
//! ```ignore
//! if bits_per_sample < 32 {
//!     let shift = 32 - bits_per_sample;
//!     self.buf.apply(|sample| sample << shift);
//! }
//! ```
//!
//! i.e. for 24-bit FLAC (`shift = 8`) the sample ends up in the top 24 bits of the `i32`, low 8
//! bits zero; for 16-bit FLAC (`shift = 16`) the sample is in the top 16 bits, low 16 bits zero.
//! This is exactly the "left-justified i32" convention this whole pipeline (and
//! `output/alsa.rs`'s packing functions) is built around.
//!
//! `GenericAudioBufferRef::copy_to_vec_interleaved::<i32>` (see
//! `symphonia-core-0.6.1/src/audio/generic.rs:503-509`) then copies out of that internal buffer
//! via `ConvertibleSample`/`FromSample`. For an `i32` source decoder this resolves to
//! `impl_convert!(i32, i32, s, s)` (`symphonia-core-0.6.1/src/audio/conv.rs:529`), i.e. a pure
//! identity copy -- no float round-trip, no extra shifting. So calling
//! `copy_to_vec_interleaved::<i32>` on a FLAC decoder's output is already bit-perfect and
//! already left-justified; we must simply never route samples through `f32`/`f64` ourselves.

use anyhow::{Context, Result, anyhow, bail};
use symphonia::core::codecs::audio::AudioDecoder;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatReader, TrackType};
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::{MediaSource, MediaSourceStream};

/// Sample format info about the decoded source (as reported by the codec), independent of
/// whatever the ALSA sink negotiates with the hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpec {
    pub sample_rate: u32,
    pub channels: u32,
    /// Bit depth as reported by the source codec (16 or 24 for FLAC). Samples handed to
    /// `on_chunk` are always left-justified 32-bit integers regardless of this value.
    pub bits_per_sample: u32,
}

/// Tells [`Decoder::run`] whether to keep decoding after a chunk was handed to the sink. Used to
/// implement cooperative cancellation (e.g. Ctrl+C) without unwinding through an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkAction {
    Continue,
    Stop,
}

/// A probed, ready-to-run decode session. Split from a single `decode_stream` function into
/// `open()` (which returns the source's [`SourceSpec`] once the container/codec have been
/// identified) and `run()` (which drives the packet loop) so the caller can open the ALSA sink
/// -- which needs to know sample rate/channels/bit depth -- *before* the first chunk is decoded,
/// instead of only after the whole track has been read.
pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    spec: SourceSpec,
}

impl Decoder {
    /// Probes `source` (using an optional file-extension hint, e.g. `"mp4"` for DASH or
    /// `"flac"`/`None` to let symphonia sniff it) and selects its default audio track and a
    /// matching decoder.
    pub fn open(source: impl MediaSource + 'static, extension_hint: Option<&str>) -> Result<Self> {
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = extension_hint {
            hint.with_extension(ext);
        }

        let format_opts = Default::default();
        let metadata_opts = Default::default();

        let format = symphonia::default::get_probe()
            .probe(&hint, mss, format_opts, metadata_opts)
            .context("no se pudo reconocer el formato del stream de audio")?;

        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| anyhow!("no se encontró ninguna pista de audio decodificable"))?
            .clone();

        let codec_params = track
            .codec_params
            .as_ref()
            .ok_or_else(|| anyhow!("la pista de audio no tiene parámetros de códec"))?;
        let audio_params = codec_params
            .audio()
            .ok_or_else(|| anyhow!("la pista no es de audio"))?;

        let sample_rate = audio_params
            .sample_rate
            .ok_or_else(|| anyhow!("no se pudo determinar la frecuencia de muestreo de la fuente"))?;
        let channels = audio_params
            .channels
            .as_ref()
            .map(|c| c.count() as u32)
            .ok_or_else(|| anyhow!("no se pudo determinar el número de canales de la fuente"))?;
        let bits_per_sample = audio_params.bits_per_sample.ok_or_else(|| {
            anyhow!(
                "no se pudo determinar bits_per_sample de la fuente (un FLAC debería reportar 16 o 24)"
            )
        })?;

        let dec_opts = Default::default();
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(audio_params, &dec_opts)
            .context("códec de audio no soportado")?;

        Ok(Decoder {
            format,
            decoder,
            track_id: track.id,
            spec: SourceSpec { sample_rate, channels, bits_per_sample },
        })
    }

    pub fn spec(&self) -> SourceSpec {
        self.spec
    }

    /// Drives the decode loop, calling `on_chunk` with each decoded packet's samples as
    /// left-justified, interleaved `i32` PCM, until the stream ends or `on_chunk` returns
    /// [`ChunkAction::Stop`].
    ///
    /// Packets belonging to any track other than the selected one are skipped; decode errors are
    /// reported and the affected packet is skipped; a mid-stream reset request is treated as an
    /// error (not supported in phase 0, see the symphonia `getting-started` example for how a
    /// fuller player would handle it).
    pub fn run(mut self, mut on_chunk: impl FnMut(&[i32]) -> Result<ChunkAction>) -> Result<()> {
        let mut interleaved: Vec<i32> = Vec::new();

        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(SymphoniaError::ResetRequired) => {
                    bail!(
                        "el stream requiere reiniciar el decodificador a mitad de la reproducción; no soportado en la fase 0"
                    );
                }
                Err(e) => return Err(e).context("error leyendo el siguiente paquete del contenedor"),
            };

            if packet.track_id != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::IoError(e)) => {
                    eprintln!("Aviso: paquete descartado por error de E/S: {e}");
                    continue;
                }
                Err(SymphoniaError::DecodeError(e)) => {
                    eprintln!("Aviso: paquete descartado por error de decodificación: {e}");
                    continue;
                }
                Err(e) => return Err(e).context("error irrecuperable del decodificador"),
            };

            // Integer path only: never route through f32/f64 here, that would break
            // bit-exactness (see the module doc for why this call is already bit-perfect).
            interleaved.clear();
            decoded.copy_to_vec_interleaved::<i32>(&mut interleaved);
            if on_chunk(&interleaved)? == ChunkAction::Stop {
                break;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_spec_is_plain_comparable_data() {
        // Smoke test: keeps `SourceSpec` a simple value type as the rest of the pipeline
        // (auth.rs/tidal.rs/output/alsa.rs) grows around it.
        let a = SourceSpec { sample_rate: 96_000, channels: 2, bits_per_sample: 24 };
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn opening_garbage_bytes_fails_cleanly() {
        let source = std::io::Cursor::new(vec![0u8; 64]);
        let result = Decoder::open(source, None);
        assert!(result.is_err());
    }
}
