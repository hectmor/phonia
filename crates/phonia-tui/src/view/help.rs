//! The help screen, drawn from the key table.

use crate::keymap::{BINDINGS, Group};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

/// One line of the help: what to press and what it does. Bindings with the same effect (`j` and
/// the down arrow) share a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub group: Group,
    pub keys: String,
    pub help: &'static str,
}

pub fn rows() -> Vec<Row> {
    let mut rows: Vec<(crate::keymap::Action, Row)> = Vec::new();
    for binding in BINDINGS {
        match rows
            .iter_mut()
            .find(|(action, row)| *action == binding.action && row.group == binding.group)
        {
            Some((_, row)) => {
                row.keys.push_str(" / ");
                row.keys.push_str(binding.label);
            }
            None => rows.push((
                binding.action,
                Row {
                    group: binding.group,
                    keys: binding.label.to_string(),
                    help: binding.help,
                },
            )),
        }
    }
    rows.into_iter().map(|(_, row)| row).collect()
}

fn lines(theme: &Theme) -> Vec<Line<'static>> {
    let rows = rows();
    let key_width = rows
        .iter()
        .map(|row| row.keys.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for group in [
        Group::General,
        Group::Movement,
        Group::Panels,
        Group::Playback,
    ] {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(group.title(), theme.title));
        for row in rows.iter().filter(|row| row.group == group) {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<key_width$}  ", row.keys), theme.accent),
                Span::styled(row.help, theme.text),
            ]));
        }
    }
    lines
}

/// A rectangle of `width` by `height` (or as much as fits) in the middle of `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    cell
}

pub fn draw(theme: &Theme, frame: &mut Frame) {
    let lines = lines(theme);
    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
    let height = lines.len() as u16 + 2;
    let area = centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_style(theme.accent)
                .title(Span::styled(" Keys ", theme.title)),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binding_is_in_the_help_and_nothing_else_is() {
        let text: String = rows()
            .iter()
            .map(|row| format!("{} {}", row.keys, row.help))
            .collect::<Vec<_>>()
            .join("\n");
        for binding in BINDINGS {
            assert!(
                text.contains(binding.label),
                "{} is not in the help",
                binding.label
            );
            assert!(
                text.contains(binding.help),
                "{:?} is not in the help",
                binding.help
            );
        }
        let listed: usize = rows().iter().map(|row| row.keys.split(" / ").count()).sum();
        assert_eq!(listed, BINDINGS.len(), "every key is listed exactly once");
    }

    #[test]
    fn keys_with_the_same_effect_share_a_line() {
        let rows = rows();
        let down = rows.iter().find(|row| row.help == "down").unwrap();
        assert_eq!(down.keys, "j / Down");
        let quit = rows.iter().find(|row| row.help == "quit").unwrap();
        assert_eq!(quit.keys, "q / Ctrl-c");
    }

    #[test]
    fn the_box_is_centered_and_never_bigger_than_the_screen() {
        let screen = Rect::new(0, 0, 100, 40);
        let boxed = centered(screen, 40, 20);
        assert_eq!((boxed.x, boxed.y), (30, 10));
        let small = centered(Rect::new(0, 0, 10, 5), 40, 20);
        assert_eq!((small.width, small.height), (10, 5));
    }
}
