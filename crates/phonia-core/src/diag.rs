//! What the library says about itself while it works (a stream buffering, a packet dropped, an
//! underrun), as opposed to what a command asks it to print.
//!
//! A terminal player wants these lines; a daemon must not write to stdout at all. Every such line
//! goes through [`note!`] or [`warn!`], so one call to [`silence`] turns them all off. This is
//! the single place to swap in a real logging crate if that ever pays for itself.

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

/// How much the library prints. Ordered: each level includes the ones below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Silent = 0,
    /// Only warnings, on stderr.
    Warnings = 1,
    /// Warnings and progress notes (the default).
    All = 2,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::All as u8);

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        0 => Level::Silent,
        1 => Level::Warnings,
        _ => Level::All,
    }
}

/// Stops the library printing anything. What a daemon calls first thing.
pub fn silence() {
    set_level(Level::Silent);
}

/// A progress note on stdout. Use [`note!`].
pub fn note(args: fmt::Arguments<'_>) {
    if level() >= Level::All {
        println!("{args}");
    }
}

/// A warning on stderr. Use [`warn!`].
pub fn warn(args: fmt::Arguments<'_>) {
    if level() >= Level::Warnings {
        eprintln!("{args}");
    }
}

/// Prints a progress note (stdout) unless the library has been silenced.
#[macro_export]
macro_rules! note {
    ($($arg:tt)*) => { $crate::diag::note(format_args!($($arg)*)) };
}

/// Prints a warning (stderr) unless the library has been silenced.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::diag::warn(format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Levels are process-wide, so one test walks through all of them and restores the default.
    #[test]
    fn levels_are_ordered_and_settable() {
        assert!(Level::Silent < Level::Warnings && Level::Warnings < Level::All);
        assert_eq!(level(), Level::All, "the library talks by default");

        silence();
        assert_eq!(level(), Level::Silent);
        set_level(Level::Warnings);
        assert_eq!(level(), Level::Warnings);

        set_level(Level::All);
        assert_eq!(level(), Level::All);
    }
}
