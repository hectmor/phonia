//! How the engine finds and opens tracks. The engine never grows a queue or knows about TIDAL:
//! it asks a [`TrackSupplier`], which the queue (#11) and the TIDAL client implement.

use super::types::{TrackMeta, TrackRef};
use crate::decode::SourceSpec;
use anyhow::Result;
use futures_util::future::BoxFuture;
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

pub struct LoadedTrack {
    pub meta: TrackMeta,
    pub media: TrackMedia,
}

/// Runs on the async runtime, never on the audio thread, so it may take as long as a TIDAL
/// request needs without ever interrupting playback that is already going.
pub trait TrackSupplier: Send + Sync + 'static {
    /// Cheap and non-blocking: which track comes after the current one, if any.
    fn advance(&self, how: Advance) -> Option<TrackRef>;

    /// Resolves and opens `track`.
    fn open(&self, track: TrackRef) -> BoxFuture<'static, Result<LoadedTrack>>;
}
