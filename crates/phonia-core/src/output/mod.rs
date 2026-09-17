//! Audio output sinks. Phase 0 only has the bit-perfect ALSA sink, but this module exists so the
//! daemon (phase 1+) can add other sinks (e.g. a network sink) without touching `decode.rs`.

pub mod alsa;
