//! Detecting what the terminal can show a cover with, and the fallback when it can't.
//!
//! A real graphics protocol (Kitty, Sixel, iTerm2) is used when the terminal says it has one.
//! Short of that, "halfblocks" (▀/▄ characters coloured with arbitrary RGB) can stand in, but only
//! when the terminal says it has true colour: `theme.rs` otherwise sticks to the terminal's own 16
//! ANSI colours on purpose, and a halfblocks cover would be the one place that rule broke quietly.
//! With neither, no cover is shown at all, rather than something that looks wrong.

use ratatui_image::picker::{Picker, ProtocolType};
use std::fmt;
use std::str::FromStr;

/// How the user wants covers shown (`phonia tui --covers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoverMode {
    /// A real graphics protocol if the terminal has one; halfblocks if not but the terminal says
    /// it has true colour; otherwise no covers.
    #[default]
    Auto,
    /// Always halfblocks, even over a real graphics protocol that was detected.
    Halfblocks,
    /// Never show a cover, and never query the terminal for one.
    Off,
}

impl fmt::Display for CoverMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CoverMode::Auto => "auto",
            CoverMode::Halfblocks => "halfblocks",
            CoverMode::Off => "off",
        })
    }
}

impl FromStr for CoverMode {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "auto" => Ok(CoverMode::Auto),
            "halfblocks" => Ok(CoverMode::Halfblocks),
            "off" => Ok(CoverMode::Off),
            other => Err(format!(
                "unknown cover mode {other:?}: expected auto, halfblocks or off"
            )),
        }
    }
}

/// Whether `$COLORTERM` says the terminal supports 24-bit colour and `$NO_COLOR` doesn't say
/// otherwise.
pub fn truecolor_available(colorterm: Option<&str>, no_color: Option<&str>) -> bool {
    no_color.is_none() && matches!(colorterm, Some("truecolor") | Some("24bit"))
}

/// Which protocol to actually draw with, given what the terminal was detected to support and
/// what the user asked for. `None` means: show no covers at all.
pub fn choose_protocol(
    mode: CoverMode,
    detected: ProtocolType,
    truecolor: bool,
) -> Option<ProtocolType> {
    match mode {
        CoverMode::Off => None,
        CoverMode::Halfblocks => Some(ProtocolType::Halfblocks),
        CoverMode::Auto if detected != ProtocolType::Halfblocks => Some(detected),
        CoverMode::Auto if truecolor => Some(ProtocolType::Halfblocks),
        CoverMode::Auto => None,
    }
}

/// Queries the terminal and decides whether, and how, to show covers. `None` means `mode` was
/// `Off`, the query failed, or nothing usable was found.
///
/// This writes to and reads from stdio momentarily ([`Picker::from_query_stdio`]'s own warning
/// applies here too): call it after entering the alternate screen and before reading terminal
/// events, the same ordering `ratatui::init()` and the event stream already need relative to each
/// other.
pub fn setup(mode: CoverMode) -> Option<Picker> {
    if mode == CoverMode::Off {
        return None;
    }
    let mut picker = Picker::from_query_stdio().ok()?;
    let truecolor = truecolor_available(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("NO_COLOR").ok().as_deref(),
    );
    let chosen = choose_protocol(mode, picker.protocol_type(), truecolor)?;
    picker.set_protocol_type(chosen);
    Some(picker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_never_shows_a_cover_whatever_was_detected() {
        for detected in [ProtocolType::Kitty, ProtocolType::Halfblocks] {
            for truecolor in [true, false] {
                assert_eq!(choose_protocol(CoverMode::Off, detected, truecolor), None);
            }
        }
    }

    #[test]
    fn halfblocks_mode_is_always_halfblocks() {
        for detected in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
        ] {
            assert_eq!(
                choose_protocol(CoverMode::Halfblocks, detected, false),
                Some(ProtocolType::Halfblocks)
            );
        }
    }

    #[test]
    fn auto_prefers_a_real_protocol_over_halfblocks_whatever_the_colour_support() {
        for real in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
        ] {
            assert_eq!(choose_protocol(CoverMode::Auto, real, false), Some(real));
            assert_eq!(choose_protocol(CoverMode::Auto, real, true), Some(real));
        }
    }

    #[test]
    fn auto_falls_back_to_halfblocks_only_with_true_colour() {
        assert_eq!(
            choose_protocol(CoverMode::Auto, ProtocolType::Halfblocks, true),
            Some(ProtocolType::Halfblocks)
        );
        assert_eq!(
            choose_protocol(CoverMode::Auto, ProtocolType::Halfblocks, false),
            None,
            "no real protocol and no true colour: no cover, not a wrong-looking halfblock"
        );
    }

    #[test]
    fn true_colour_needs_the_right_value_and_no_no_color() {
        assert!(truecolor_available(Some("truecolor"), None));
        assert!(truecolor_available(Some("24bit"), None));
        assert!(!truecolor_available(Some("truecolor"), Some("1")));
        assert!(!truecolor_available(None, None));
        assert!(!truecolor_available(Some("yes"), None));
    }

    #[test]
    fn a_mode_parses_from_its_own_display_and_an_unknown_word_is_an_error() {
        for mode in [CoverMode::Auto, CoverMode::Halfblocks, CoverMode::Off] {
            assert_eq!(mode.to_string().parse::<CoverMode>(), Ok(mode));
        }
        assert!("sixel".parse::<CoverMode>().is_err());
    }

    #[test]
    fn off_mode_never_queries_the_terminal() {
        // If this queried stdio it would hang or misbehave under `cargo test`'s harness; that it
        // returns at all, with nothing, is the point.
        assert!(setup(CoverMode::Off).is_none());
    }
}
