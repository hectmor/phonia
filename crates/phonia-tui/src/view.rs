//! Painting a state onto the screen.

use crate::app::State;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

pub fn draw(_state: &State, theme: &Theme, frame: &mut Frame) {
    let block = Block::bordered()
        .title(Span::styled(" phonia ", theme.title))
        .border_style(theme.dim);
    let text = Paragraph::new(vec![
        Line::styled("phonia", theme.accent),
        Line::styled("q quits", theme.dim),
    ])
    .alignment(Alignment::Center)
    .block(block);
    frame.render_widget(text, frame.area());
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(&State::default(), &Theme::new(false), frame))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_first_screen_says_what_it_is_and_how_to_leave() {
        let screen = screen(30, 5);
        assert!(screen.contains("phonia"), "{screen}");
        assert!(screen.contains("q quits"), "{screen}");
    }

    #[test]
    fn a_tiny_terminal_does_not_panic() {
        for (w, h) in [(1, 1), (5, 2), (0, 0)] {
            let _ = screen(w, h);
        }
    }
}
