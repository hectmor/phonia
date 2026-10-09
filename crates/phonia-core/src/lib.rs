pub mod auth;
pub mod catalog;
pub mod config;
pub mod control;
pub mod dash;
pub mod decode;
pub mod diag;
pub mod engine;
pub mod openers;
pub mod output;
pub mod play_log;
pub mod queue;
pub mod recent;
pub mod replaygain;
mod session;
pub mod stream;
pub mod tidal;

#[cfg(test)]
mod testutil;
