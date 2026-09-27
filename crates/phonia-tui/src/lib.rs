//! The terminal interface of phonia.
//!
//! It is a client of the daemon and never touches audio, so it builds and tests without ALSA.
//! The shape is a small Elm: [`app::update`] turns a message into a change of the [`app::State`]
//! and a list of things to do, and [`view::draw`] paints a state. Both are pure, so they are
//! tested with plain values and ratatui's `TestBackend`; only [`run`] talks to the terminal.

pub mod app;
pub mod conn;
pub mod cursor;
pub mod keymap;
pub mod theme;
pub mod view;

use anyhow::{Result, bail};
use app::{Cmd, Msg, State, TICK};
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use phonia_ipc::{Client, ClientInfo};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use theme::Theme;
use tokio::sync::{Notify, mpsc};

/// Runs the interface until the user quits. Takes over the terminal, and gives it back whatever
/// happens: on an error, and on a panic too.
pub async fn run(socket: PathBuf) -> Result<()> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("phonia tui needs a terminal; use `phonia ctl` for scripts and pipes");
    }
    // Installs a panic hook that restores the terminal before the message is printed.
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, socket).await;
    ratatui::restore();
    result
}

async fn event_loop(terminal: &mut ratatui::DefaultTerminal, socket: PathBuf) -> Result<()> {
    let theme = Theme::detect();
    let mut state = State::default();
    let size = terminal.size()?;
    app::update(&mut state, Msg::Resize(size.width, size.height));
    let mut events = EventStream::new();
    let (messages, mut from_daemon) = mpsc::unbounded_channel();
    let (requests, requests_rx) = mpsc::unbounded_channel();
    let retry = Arc::new(Notify::new());
    // Stops the connection task however the loop ends.
    let _connection = AbortOnDrop(conn::spawn(
        connector(socket),
        messages,
        retry.clone(),
        requests_rx,
    ));
    let mut tick = tokio::time::interval(TICK);
    terminal.draw(|frame| view::draw(&state, &theme, frame))?;

    while !state.quit {
        let msg = tokio::select! {
            _ = tick.tick() => Msg::Tick,
            Some(msg) = from_daemon.recv() => msg,
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => Msg::Key(key),
                Some(Ok(Event::Resize(columns, rows))) => Msg::Resize(columns, rows),
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
                Cmd::RetryNow => retry.notify_one(),
                // The connection task is gone if the daemon is unreachable; nothing to do then,
                // since the interface itself already refuses to send in that case.
                Cmd::Send(request) => {
                    let _ = requests.send(request);
                }
            }
        }
        // Painting is the expensive part: only when something changed.
        if effects.redraw && !state.quit {
            terminal.draw(|frame| view::draw(&state, &theme, frame))?;
        }
    }
    Ok(())
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Connects to the daemon's socket as `phonia-tui`.
fn connector(socket: PathBuf) -> conn::Connector {
    let info = ClientInfo {
        name: "phonia-tui".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    Arc::new(move || {
        let socket = socket.clone();
        let info = info.clone();
        Box::pin(async move { Client::connect(Some(&socket), info).await })
    })
}
