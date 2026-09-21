//! How the engine finds and opens tracks. The engine never grows a queue or knows about TIDAL:
//! it asks a [`TrackSupplier`], which the queue (#11) and the TIDAL client implement.

use super::types::{TrackMeta, TrackRef};
use crate::decode::SourceSpec;
use anyhow::Result;
use futures_util::future::BoxFuture;
use std::time::Duration;
use symphonia::core::io::MediaSource;

/// Which track to move to, relative to the one that just played or is playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// A track ended by itself; the supplier applies repeat/shuffle rules.
    Auto,
    /// The user asked to skip ahead.
    Next,
    /// The user asked to go back one track.
    Previous,
    /// The user asked to go back, but far enough into the track that it starts over: the supplier
    /// answers with the current track again.
    Restart,
}

/// The audio of an opened track.
pub enum TrackMedia {
    /// Encoded audio (FLAC, fMP4, ...) for the decoder.
    Encoded { source: Box<dyn MediaSource>, extension: Option<String> },
    /// Ready-made left-justified interleaved PCM, for tests and in-memory sources.
    RawPcm { samples: Vec<i32>, spec: SourceSpec },
}

/// How a loaded track can be moved around in, which depends on where its audio comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekMode {
    /// Not at all.
    None,
    /// The source can be repositioned: a local file.
    InPlace,
    /// One continuous stream that can't rewind (a single-URL download): moving ahead means
    /// reading and discarding, and going back is impossible.
    ForwardOnly,
    /// The stream can't rewind, but the supplier can open the track at a given position (a DASH
    /// stream reopened at the right segment). Seeking asks it to.
    Reopen,
}

pub struct LoadedTrack {
    pub meta: TrackMeta,
    pub media: TrackMedia,
    pub seek: SeekMode,
    /// Where in the track `media` begins. Zero unless the supplier was asked to open it further
    /// in and could only start at a boundary at or before that (the start of a DASH segment).
    /// The engine skips the difference itself, so a supplier that ignores the request is merely
    /// slow, never wrong.
    pub start: Duration,
}

impl LoadedTrack {
    /// A track that can't be seeked and starts at the beginning.
    pub fn new(meta: TrackMeta, media: TrackMedia) -> Self {
        Self { meta, media, seek: SeekMode::None, start: Duration::ZERO }
    }

    pub fn seekable_in_place(self) -> Self {
        Self { seek: SeekMode::InPlace, ..self }
    }

    pub fn forward_only(self) -> Self {
        Self { seek: SeekMode::ForwardOnly, ..self }
    }

    pub fn reopenable(self) -> Self {
        Self { seek: SeekMode::Reopen, ..self }
    }

    pub fn starting_at(self, start: Duration) -> Self {
        Self { start, ..self }
    }
}

/// Gets the audio of one track: a file, a TIDAL stream. It knows nothing about ordering; a queue
/// decides what plays next and asks an opener to open it.
pub trait TrackOpener: Send + Sync + 'static {
    /// Resolves and opens `track`, from `at` if the opener can (see [`LoadedTrack::start`] for
    /// what it reports back). `at` is zero for an ordinary start.
    fn open(&self, track: TrackRef, at: Duration) -> BoxFuture<'static, Result<LoadedTrack>>;
}

/// Runs on the async runtime, never on the audio thread, so it may take as long as a TIDAL
/// request needs without ever interrupting playback that is already going.
pub trait TrackSupplier: Send + Sync + 'static {
    /// Cheap and non-blocking: which track comes after the current one, if any.
    fn advance(&self, how: Advance) -> Option<TrackRef>;

    /// Resolves and opens `track`, from `at` if the supplier can (see [`LoadedTrack::start`] for
    /// what it reports back). `at` is zero for an ordinary start.
    fn open(&self, track: TrackRef, at: Duration) -> BoxFuture<'static, Result<LoadedTrack>>;
}
