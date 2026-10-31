//! Every colour and text style of the interface, in one place.
//!
//! By default it only uses the terminal's own 16 colours, so it follows whatever palette the user
//! has chosen (solarized, gruvbox...) instead of imposing one, and it works everywhere. With
//! `NO_COLOR` set (see <https://no-color.org>) it uses no colour at all: emphasis comes from bold,
//! dim and reverse video alone. The one exception is a named [`Palette`] chosen in
//! `~/.config/phonia/theme.toml`, which uses that scheme's real colours instead, on purpose: see
//! [`Theme::load`]. Nothing outside this file names a colour.

use anyhow::{Context, Result, anyhow};
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Panel titles.
    pub title: Style,
    /// Ordinary text.
    pub text: Style,
    /// Less important text, and everything that is out of date.
    pub dim: Style,
    /// The thing to look at: the focused panel, the playing track.
    pub accent: Style,
    /// The row under the cursor.
    pub selected: Style,
    /// Something went wrong.
    pub error: Style,
    /// Not wrong, but not bit-perfect either (shared mode): worth noticing, not an error.
    pub warn: Style,
}

impl Theme {
    /// The theme for this terminal: colours unless `NO_COLOR` is set to something.
    pub fn detect() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        Self::new(!no_color)
    }

    pub fn new(color: bool) -> Self {
        let plain = Style::new();
        if color {
            Self {
                title: plain.fg(Color::Cyan).add_modifier(Modifier::BOLD),
                text: plain,
                dim: plain.fg(Color::DarkGray),
                accent: plain.fg(Color::Green).add_modifier(Modifier::BOLD),
                selected: plain.add_modifier(Modifier::REVERSED),
                error: plain.fg(Color::Red).add_modifier(Modifier::BOLD),
                warn: plain.fg(Color::Yellow),
            }
        } else {
            Self {
                title: plain.add_modifier(Modifier::BOLD),
                text: plain,
                dim: plain.add_modifier(Modifier::DIM),
                accent: plain.add_modifier(Modifier::BOLD),
                selected: plain.add_modifier(Modifier::REVERSED),
                error: plain.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                // No modifier: the SHARED/CONVERTED wording already says this is not an error.
                warn: plain,
            }
        }
    }

    /// Reads `~/.config/phonia/theme.toml` and returns the palette it names, or [`Theme::detect`]
    /// if there is no config directory, no such file, or the file does not set `palette`. Any
    /// other read failure, or a file that fails to parse (bad TOML, an unknown key, an unknown
    /// palette name), is an error: a typo should not be silently ignored.
    pub fn load() -> Result<Self> {
        let Some(dir) = dirs::config_dir() else {
            return Ok(Self::detect());
        };
        let path = dir.join("phonia").join("theme.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::detect());
            }
            Err(error) => {
                return Err(anyhow!(error))
                    .with_context(|| format!("reading the theme file {}", path.display()));
            }
        };
        let file: ThemeFile = toml::from_str(&text)
            .with_context(|| format!("in the theme file {}", path.display()))?;
        Ok(match file.palette {
            // shortcut: a chosen palette ignores NO_COLOR and is sent as 24-bit escapes
            // regardless of terminal support (no-color.org: a user's own config should win) — add
            // a non-truecolor fallback if that turns out to matter in practice.
            Some(palette) => palette.theme(),
            None => Self::detect(),
        })
    }
}

/// A built-in curated colour scheme, named by `theme.toml`'s `palette` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Palette {
    Gruvbox,
    Dracula,
    Nord,
}

impl Palette {
    /// The theme this palette names, in that scheme's real colours (not the terminal's 16): see
    /// the 2026-10-23 entry in docs/DECISIONS.md. Only foregrounds are set — `text` and
    /// `selected` are untouched, so no palette paints the frame's background.
    fn theme(self) -> Theme {
        let plain = Style::new();
        let rgb = |hex: u32| Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
        match self {
            Palette::Gruvbox => Theme {
                title: plain.fg(rgb(0x8ec07c)).add_modifier(Modifier::BOLD),
                text: plain,
                dim: plain.fg(rgb(0x928374)),
                accent: plain.fg(rgb(0xb8bb26)).add_modifier(Modifier::BOLD),
                selected: plain.add_modifier(Modifier::REVERSED),
                error: plain.fg(rgb(0xfb4934)).add_modifier(Modifier::BOLD),
                warn: plain.fg(rgb(0xfabd2f)),
            },
            Palette::Dracula => Theme {
                title: plain.fg(rgb(0x8be9fd)).add_modifier(Modifier::BOLD),
                text: plain,
                dim: plain.fg(rgb(0x6272a4)),
                accent: plain.fg(rgb(0x50fa7b)).add_modifier(Modifier::BOLD),
                selected: plain.add_modifier(Modifier::REVERSED),
                error: plain.fg(rgb(0xff5555)).add_modifier(Modifier::BOLD),
                warn: plain.fg(rgb(0xf1fa8c)),
            },
            Palette::Nord => Theme {
                title: plain.fg(rgb(0x88c0d0)).add_modifier(Modifier::BOLD),
                text: plain,
                // Real nord3 (#4c566a) is near-invisible on dark backgrounds; brightened as the
                // usual comment colour in Nord ports instead.
                dim: plain.fg(rgb(0x616e88)),
                accent: plain.fg(rgb(0xa3be8c)).add_modifier(Modifier::BOLD),
                selected: plain.add_modifier(Modifier::REVERSED),
                error: plain.fg(rgb(0xbf616a)).add_modifier(Modifier::BOLD),
                warn: plain.fg(rgb(0xebcb8b)),
            },
        }
    }
}

/// The contents of `theme.toml`. Unknown keys are a hard error, not ignored, the same convention
/// `phonia_core::config::file::ConfigFile` uses for `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ThemeFile {
    palette: Option<Palette>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_colour_no_style_names_one() {
        let theme = Theme::new(false);
        for style in [
            theme.title,
            theme.text,
            theme.dim,
            theme.accent,
            theme.selected,
            theme.error,
            theme.warn,
        ] {
            assert_eq!(style.fg, None);
            assert_eq!(style.bg, None);
        }
    }

    #[test]
    fn with_colour_only_the_terminals_own_sixteen_are_used() {
        let theme = Theme::new(true);
        for style in [
            theme.title,
            theme.dim,
            theme.accent,
            theme.error,
            theme.warn,
        ] {
            let color = style.fg.expect("a colour");
            assert!(
                !matches!(color, Color::Rgb(..) | Color::Indexed(_)),
                "{color:?} is not one of the sixteen"
            );
        }
    }

    #[test]
    fn an_empty_or_comment_only_file_leaves_the_palette_unset() {
        let file: ThemeFile = toml::from_str("").unwrap();
        assert_eq!(file.palette, None);
        let file: ThemeFile = toml::from_str("# nothing here\n").unwrap();
        assert_eq!(file.palette, None);
    }

    #[test]
    fn every_builtin_palette_name_parses() {
        for (word, expected) in [
            ("gruvbox", Palette::Gruvbox),
            ("dracula", Palette::Dracula),
            ("nord", Palette::Nord),
        ] {
            let file: ThemeFile = toml::from_str(&format!("palette = \"{word}\"\n")).unwrap();
            assert_eq!(file.palette, Some(expected));
        }
    }

    #[test]
    fn an_unknown_palette_name_lists_the_real_ones() {
        let error = toml::from_str::<ThemeFile>("palette = \"solarized\"\n")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("gruvbox") && error.contains("dracula") && error.contains("nord"),
            "{error}"
        );
    }

    #[test]
    fn an_unknown_key_is_an_error() {
        let error = toml::from_str::<ThemeFile>("pallette = \"nord\"\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("pallette"), "{error}");
    }

    #[test]
    fn every_palette_sets_only_foregrounds_in_real_rgb() {
        for palette in [Palette::Gruvbox, Palette::Dracula, Palette::Nord] {
            let theme = palette.theme();
            for style in [
                theme.title,
                theme.dim,
                theme.accent,
                theme.error,
                theme.warn,
            ] {
                assert!(matches!(style.fg, Some(Color::Rgb(..))), "{style:?}");
                assert_eq!(style.bg, None);
            }
            assert_eq!(theme.text.fg, None);
            assert_eq!(theme.text.bg, None);
            assert_eq!(theme.selected.fg, None);
            assert_eq!(theme.selected.bg, None);
        }
    }
}
