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
use std::time::Duration;
use symphonia::core::codecs::audio::AudioDecoder;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::units::{Time, TimeBase, Timestamp};

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
    duration: Option<Duration>,
    /// Timestamp units of the track; `None` means one tick per frame.
    time_base: Option<TimeBase>,
    /// Interleaved samples still to be dropped after a seek, because containers can only seek
    /// to a packet boundary at or before the requested time.
    skip_samples: usize,
}

impl Decoder {
    /// Probes `source` (using an optional file-extension hint, e.g. `"mp4"` for DASH or
    /// `"flac"`/`None` to let symphonia sniff it) and selects its default audio track and a
    /// matching decoder.
    pub fn open(source: impl MediaSource + 'static, extension_hint: Option<&str>) -> Result<Self> {
        Self::open_boxed(Box::new(source), extension_hint)
    }

    /// Like [`Decoder::open`], for a source that is already boxed.
    pub fn open_boxed(source: Box<dyn MediaSource>, extension_hint: Option<&str>) -> Result<Self> {
        let mss = MediaSourceStream::new(source, Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = extension_hint {
            hint.with_extension(ext);
        }

        let format_opts = Default::default();
        let metadata_opts = Default::default();

        let format = symphonia::default::get_probe()
            .probe(&hint, mss, format_opts, metadata_opts)
            .context("could not recognize the audio stream's format")?;

        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| anyhow!("no decodable audio track found"))?
            .clone();

        let codec_params = track
            .codec_params
            .as_ref()
            .ok_or_else(|| anyhow!("the audio track has no codec parameters"))?;
        let audio_params = codec_params
            .audio()
            .ok_or_else(|| anyhow!("the track is not audio"))?;

        let sample_rate = audio_params
            .sample_rate
            .ok_or_else(|| anyhow!("could not determine the source's sample rate"))?;
        let channels = audio_params
            .channels
            .as_ref()
            .map(|c| c.count() as u32)
            .ok_or_else(|| anyhow!("could not determine the source's channel count"))?;
        let bits_per_sample = audio_params.bits_per_sample.ok_or_else(|| {
            anyhow!(
                "could not determine the source's bits_per_sample (a FLAC should report 16 or 24)"
            )
        })?;

        let dec_opts = Default::default();
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(audio_params, &dec_opts)
            .context("unsupported audio codec")?;

        let duration = match (track.num_frames, track.duration, track.time_base) {
            (Some(frames), _, _) => Some(frames_to_duration(frames, sample_rate)),
            (None, Some(ticks), Some(time_base)) => time_base
                .calc_time(Timestamp::new(i64::try_from(ticks.get()).unwrap_or(i64::MAX)))
                .and_then(|time| u64::try_from(time.as_nanos()).ok())
                .map(Duration::from_nanos),
            _ => None,
        };

        Ok(Decoder {
            format,
            decoder,
            track_id: track.id,
            spec: SourceSpec { sample_rate, channels, bits_per_sample },
            duration,
            time_base: track.time_base,
            skip_samples: 0,
        })
    }

    pub fn spec(&self) -> SourceSpec {
        self.spec
    }

    /// The track's total length, if the container says so.
    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Converts a span of the track's timestamp ticks into frames.
    fn ticks_to_frames(&self, ticks: i64) -> u64 {
        let ticks = ticks.max(0) as i128;
        match self.time_base {
            Some(tb) => {
                let frames = ticks * i128::from(tb.numer.get()) * i128::from(self.spec.sample_rate)
                    / i128::from(tb.denom.get());
                frames as u64
            }
            None => ticks as u64,
        }
    }

    /// Moves to `to` (from the start of the track) and returns the position playback will
    /// actually resume from, which is `to` rounded down to a whole frame.
    ///
    /// The container can only seek to a packet at or before `to`, so the frames in between are
    /// decoded and dropped by [`Decoder::next_chunk_into`], making the seek frame-accurate.
    /// Fails if the source isn't seekable (e.g. a live network stream) or `to` is past the end.
    pub fn seek(&mut self, to: Duration) -> Result<Duration> {
        let nanos = u64::try_from(to.as_nanos()).unwrap_or(u64::MAX);
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time { time: Time::from_nanos_u64(nanos), track_id: Some(self.track_id) },
            )
            .context("seeking in the audio stream")?;
        self.decoder.reset();

        let skip_frames = self.ticks_to_frames(seeked.required_ts.get() - seeked.actual_ts.get());
        self.skip_samples = skip_frames as usize * self.spec.channels as usize;

        Ok(frames_to_duration(self.ticks_to_frames(seeked.required_ts.get()), self.spec.sample_rate))
    }

    /// Decodes the next packet into `out` (replacing its contents) as left-justified,
    /// interleaved `i32` PCM. Returns `Ok(false)` at the end of the stream.
    ///
    /// Packets belonging to any track other than the selected one are skipped; decode errors are
    /// reported and the affected packet is skipped; a mid-stream reset request is treated as an
    /// error (see the symphonia `getting-started` example for how a fuller player would handle
    /// it).
    pub fn next_chunk_into(&mut self, out: &mut Vec<i32>) -> Result<bool> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(false),
                Err(SymphoniaError::ResetRequired) => {
                    bail!("the stream requires resetting the decoder mid-playback; not supported yet");
                }
                Err(e) => return Err(e).context("error reading the next packet from the container"),
            };

            if packet.track_id != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::IoError(e)) => {
                    eprintln!("Warning: packet dropped due to an I/O error: {e}");
                    continue;
                }
                Err(SymphoniaError::DecodeError(e)) => {
                    eprintln!("Warning: packet dropped due to a decode error: {e}");
                    continue;
                }
                Err(e) => return Err(e).context("unrecoverable decoder error"),
            };

            // Integer path only: never route through f32/f64 here, that would break
            // bit-exactness (see the module doc for why this call is already bit-perfect).
            out.clear();
            decoded.copy_to_vec_interleaved::<i32>(out);

            if self.skip_samples > 0 {
                let skipped = self.skip_samples.min(out.len());
                out.drain(..skipped);
                self.skip_samples -= skipped;
                if out.is_empty() {
                    continue;
                }
            }

            return Ok(true);
        }
    }

    /// Drives the decode loop, calling `on_chunk` with each decoded packet's samples as
    /// left-justified, interleaved `i32` PCM, until the stream ends or `on_chunk` returns
    /// [`ChunkAction::Stop`].
    pub fn run(mut self, mut on_chunk: impl FnMut(&[i32]) -> Result<ChunkAction>) -> Result<()> {
        let mut interleaved: Vec<i32> = Vec::new();
        while self.next_chunk_into(&mut interleaved)? {
            if on_chunk(&interleaved)? == ChunkAction::Stop {
                break;
            }
        }
        Ok(())
    }
}

pub(crate) fn frames_to_duration(frames: u64, sample_rate: u32) -> Duration {
    let sample_rate = u64::from(sample_rate.max(1));
    Duration::new(frames / sample_rate, ((frames % sample_rate) * 1_000_000_000 / sample_rate) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{RATE, expected, wav};

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

    fn decoder(frames: usize) -> Decoder {
        Decoder::open(std::io::Cursor::new(wav(frames)), Some("wav")).unwrap()
    }

    fn read_all(decoder: &mut Decoder) -> Vec<i32> {
        let (mut all, mut chunk) = (Vec::new(), Vec::new());
        while decoder.next_chunk_into(&mut chunk).unwrap() {
            all.extend_from_slice(&chunk);
        }
        all
    }

    #[test]
    fn reports_spec_and_duration() {
        let d = decoder(RATE as usize * 2);
        assert_eq!(d.spec(), SourceSpec { sample_rate: RATE, channels: 2, bits_per_sample: 16 });
        assert_eq!(d.duration(), Some(Duration::from_secs(2)));
    }

    #[test]
    fn next_chunk_into_yields_every_sample_left_justified_then_reports_the_end() {
        let frames = 30_000;
        let mut d = decoder(frames);
        assert_eq!(read_all(&mut d), expected(0, frames * 2));
        let mut chunk = Vec::new();
        assert!(!d.next_chunk_into(&mut chunk).unwrap(), "must keep reporting the end");
    }

    #[test]
    fn run_delivers_the_same_samples_as_next_chunk_into() {
        let frames = 30_000;
        let mut got = Vec::new();
        decoder(frames)
            .run(|samples| {
                got.extend_from_slice(samples);
                Ok(ChunkAction::Continue)
            })
            .unwrap();
        assert_eq!(got, expected(0, frames * 2));
    }

    #[test]
    fn run_stops_when_asked() {
        let mut calls = 0;
        decoder(30_000)
            .run(|_| {
                calls += 1;
                Ok(ChunkAction::Stop)
            })
            .unwrap();
        assert_eq!(calls, 1);
    }

    #[test]
    fn seek_lands_on_the_exact_frame() {
        let frames = RATE as usize * 2;
        let mut d = decoder(frames);
        let target = RATE as usize / 2; // 0.5 s
        let position = d.seek(Duration::from_millis(500)).unwrap();
        assert_eq!(position, Duration::from_millis(500));
        assert_eq!(read_all(&mut d), expected(target * 2, frames * 2));
    }

    #[test]
    fn seek_to_a_time_between_frames_rounds_down_and_reports_where_it_landed() {
        let frames = RATE as usize * 2;
        let mut d = decoder(frames);
        let position = d.seek(Duration::from_micros(123_456)).unwrap();

        let landed_frame = (position.as_secs_f64() * f64::from(RATE)).round() as usize;
        assert!(position <= Duration::from_micros(123_456));
        assert!(landed_frame.abs_diff(5444) <= 1, "landed on frame {landed_frame}");
        assert_eq!(read_all(&mut d), expected(landed_frame * 2, frames * 2));
    }

    #[test]
    fn seeking_backwards_after_reading_works() {
        let frames = RATE as usize;
        let mut d = decoder(frames);
        let mut chunk = Vec::new();
        for _ in 0..3 {
            assert!(d.next_chunk_into(&mut chunk).unwrap());
        }
        d.seek(Duration::ZERO).unwrap();
        assert_eq!(read_all(&mut d), expected(0, frames * 2));
    }

    #[test]
    fn seeking_past_the_end_is_an_error() {
        let mut d = decoder(RATE as usize);
        assert!(d.seek(Duration::from_secs(60)).is_err());
    }
}
