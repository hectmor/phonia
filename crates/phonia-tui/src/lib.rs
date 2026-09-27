//! The terminal interface of phonia.
//!
//! It is a client of the daemon and never touches audio, so it builds and tests without ALSA.
//! The shape is a small Elm: [`app::update`] turns a message into a change of the [`app::State`]
//! and a list of things to do, and [`view::draw`] paints a state. Both are pure, so they are
//! tested with plain values and ratatui's `TestBackend`; only [`run`] talks to the terminal.

pub mod app;
pub mod theme;
pub mod view;

use anyhow::{Result, bail};
use app::{Cmd, Msg, State};
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use std::io::IsTerminal;
use std::time::Duration;
use theme::Theme;

/// How often the interface wakes up on its own: it will move the playing position between the
/// daemon's reports.
const TICK: Duration = Duration::from_millis(250);

/// Runs the interface until the user quits. Takes over the terminal, and gives it back whatever
/// happens: on an error, and on a panic too.
pub async fn run() -> Result<()> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("phonia tui needs a terminal; use `phonia ctl` for scripts and pipes");
    }
    // Installs a panic hook that restores the terminal before the message is printed.
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal).await;
    ratatui::restore();
    result
}

async fn event_loop(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let theme = Theme::detect();
    let mut state = State::default();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    terminal.draw(|frame| view::draw(&state, &theme, frame))?;

    while !state.quit {
        let msg = tokio::select! {
            _ = tick.tick() => Msg::Tick,
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => Msg::Key(key),
                Some(Ok(Event::Resize(..))) => Msg::Resize,
                Some(Ok(_)) => continue,
                Some(Err(error)) => return Err(error.into()),
                // The terminal closed under us.
                None => break,
            },
        };
        let effects = app::update(&mut state, msg);
        for command in effects.commands {
            match command {
                Cmd::Quit => state.quit = true,
            }
        }
        // Painting is the expensive part: only when something changed.
        if effects.redraw && !state.quit {
            terminal.draw(|frame| view::draw(&state, &theme, frame))?;
        }
    }
    Ok(())
}
