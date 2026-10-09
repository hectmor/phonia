//! MPRIS2 integration (media keys, lock-screen widgets, `playerctl`...), #34.
//!
//! Split the same way `autoplay` is: [`model`] is pure policy -- no zbus, no D-Bus, no async,
//! fully unit-tested with plain `phonia_ipc` values. The zbus adapter that actually registers
//! `org.mpris.MediaPlayer2.phonia` on the session bus and wires this model to the daemon's own
//! event stream is a later part of #34, not added yet.

pub mod model;
