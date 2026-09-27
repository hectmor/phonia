//! Every colour and text style of the interface, in one place.
//!
//! It only uses the terminal's own 16 colours, so it follows whatever palette the user has chosen
//! (solarized, gruvbox...) instead of imposing one, and it works everywhere. With `NO_COLOR` set
//! (see <https://no-color.org>) it uses no colour at all: emphasis comes from bold, dim and
//! reverse video alone. Nothing outside this file names a colour.

use ratatui::style::{Color, Modifier, Style};

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
            }
        } else {
            Self {
                title: plain.add_modifier(Modifier::BOLD),
                text: plain,
                dim: plain.add_modifier(Modifier::DIM),
                accent: plain.add_modifier(Modifier::BOLD),
                selected: plain.add_modifier(Modifier::REVERSED),
                error: plain.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            }
        }
    }
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
        ] {
            assert_eq!(style.fg, None);
            assert_eq!(style.bg, None);
        }
    }

    #[test]
    fn with_colour_only_the_terminals_own_sixteen_are_used() {
        let theme = Theme::new(true);
        for style in [theme.title, theme.dim, theme.accent, theme.error] {
            let color = style.fg.expect("a colour");
            assert!(
                !matches!(color, Color::Rgb(..) | Color::Indexed(_)),
                "{color:?} is not one of the sixteen"
            );
        }
    }
}
