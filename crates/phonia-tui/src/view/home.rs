//! The Home section: just the "Continue" row for now (see #139's later parts for the rest).

use crate::app::State;
use crate::home;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

pub fn draw(state: &State, theme: &Theme, focused: bool, frame: &mut Frame, area: Rect) {
    let continue_ = home::continuation(state);
    let style = if focused { theme.selected } else { theme.text };
    frame.render_widget(
        Paragraph::new(Line::styled(continue_.text().to_string(), style)),
        area,
    );
}
