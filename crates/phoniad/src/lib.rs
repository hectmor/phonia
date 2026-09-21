//! The phonia daemon: owns the playback engine and the queue, and lets clients drive them over a
//! Unix socket (see the `phonia-ipc` crate for the protocol).

pub mod convert;
pub mod daemon;
pub mod server;
pub mod socket;
