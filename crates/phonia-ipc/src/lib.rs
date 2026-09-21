//! The protocol between the phonia daemon and its clients, and a client for it.
//!
//! This crate deliberately knows nothing about audio: it depends on `serde` and `tokio` only, so
//! a terminal UI, an MPRIS bridge or an agent server can talk to the daemon without building
//! ALSA or the decoders. The types here are the *wire* types; the daemon maps its own internal
//! ones onto them, so the wire format can outlive refactors.
//!
//! # Wire format
//!
//! A Unix stream socket carrying newline-delimited JSON: one compact JSON value per line, in both
//! directions (so `socat - UNIX-CONNECT:$socket` is a working client). The server speaks first,
//! with a [`ServerMessage::Hello`] banner; the client answers with a [`Request::Hello`], and only
//! then are other requests accepted. Every request carries an id, echoed by its response; events
//! arrive between responses, after a client has asked to [`Request::Subscribe`].
//!
//! # Versioning
//!
//! [`PROTOCOL`] is `major.minor`. A server accepts a client with the same major version; minors
//! only add things, and both sides ignore fields and variants they don't know (unknown enum
//! variants deserialize to an `Unknown` variant instead of failing).

pub mod client;
pub mod dto;
pub mod framing;
pub mod proto;
pub mod socket;
pub mod source;

pub use client::{Client, ClientError, EventStream};
pub use dto::*;
pub use proto::*;
