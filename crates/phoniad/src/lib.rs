//! The phonia daemon: owns the playback engine and the queue, and lets clients drive them over a
//! Unix socket (see the `phonia-ipc` crate for the protocol).

mod autoplay;
pub mod convert;
pub mod daemon;
pub mod mpris;
pub mod outputs;
pub mod server;
pub mod socket;
