//! The terminal interface of phonia.
//!
//! It is a client of the daemon and never touches audio, so it builds and tests without ALSA.
//! The shape is a small Elm: [`app::update`] turns a message into a change of the [`app::State`]
//! and a list of things to do, and [`view::draw`] paints a state. Both are pure, so they are
//! tested with plain values and ratatui's `TestBackend`; only [`run`] talks to the terminal.

pub mod app;
pub mod browse;
pub mod conn;
pub mod covers;
pub mod cursor;
pub mod graphics;
pub mod home;
pub mod input;
pub mod keymap;
pub mod library;
pub mod list;
pub mod lyrics;
pub mod search;
pub mod theme;
pub mod view;

use anyhow::{Result, bail};
use app::{Cmd, Msg, State, TICK};
use covers::{Covers, Outcome};
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
pub async fn run(socket: PathBuf, cover_mode: graphics::CoverMode) -> Result<()> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("phonia tui needs a terminal; use `phonia ctl` for scripts and pipes");
    }
    // Fails before the terminal is touched at all, so a bad theme.toml never flashes the
    // alternate screen just to quit right back out of it.
    let theme = Theme::load()?;
    // Installs a panic hook that restores the terminal before the message is printed.
    let mut terminal = ratatui::init();
    // Queries the terminal for graphics support, per `graphics::setup`'s own requirement: after
    // entering the alternate screen (just above), before reading any terminal event (below).
    let picker = graphics::setup(cover_mode);
    let result = event_loop(&mut terminal, socket, picker, theme).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    socket: PathBuf,
    picker: Option<ratatui_image::picker::Picker>,
    theme: Theme,
) -> Result<()> {
    let mut state = State::default();
    let mut covers = Covers::new(picker);
    let size = terminal.size()?;
    app::update(&mut state, Msg::Resize(size.width, size.height));
    let mut events = EventStream::new();
    let (messages, mut from_daemon) = mpsc::unbounded_channel();
    let (requests, requests_rx) = mpsc::unbounded_channel();
    let (cover_done, mut cover_results) =
        mpsc::unbounded_channel::<(String, ratatui::layout::Size, Outcome)>();
    let retry = Arc::new(Notify::new());
    // Stops the connection task however the loop ends.
    let _connection = AbortOnDrop(conn::spawn(
        connector(socket),
        messages,
        retry.clone(),
        requests_rx,
    ));
    let mut tick = tokio::time::interval(TICK);
    terminal.draw(|frame| view::draw(&state, &theme, &covers, frame))?;
    maybe_fetch_cover(&state, &mut covers, terminal.size()?, &cover_done);

    while !state.quit {
        let msg = tokio::select! {
            _ = tick.tick() => Msg::Tick,
            Some(msg) = from_daemon.recv() => msg,
            Some((url, cells, outcome)) = cover_results.recv() => {
                // Not an `app::Msg`: covers are a resource for drawing, not app state (see
                // `covers.rs`), so this redraws directly instead of going through `app::update`.
                covers.finish(url, cells, outcome);
                terminal.draw(|frame| view::draw(&state, &theme, &covers, frame))?;
                continue;
            }
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
                    let _ = requests.send((None, request));
                }
                Cmd::Request { tag, request } => {
                    let _ = requests.send((Some(tag), request));
                }
            }
        }
        maybe_fetch_cover(&state, &mut covers, terminal.size()?, &cover_done);
        // Painting is the expensive part: only when something changed.
        if effects.redraw && !state.quit {
            terminal.draw(|frame| view::draw(&state, &theme, &covers, frame))?;
        }
    }
    Ok(())
}

/// Starts fetching the cover the screen now wants, if there is one and it is not already in
/// flight, ready, or failed. The cover is sized against [`view::content_area`], the very rectangle
/// every draw path measures: the panel's border is load-bearing, not a rounding error, because
/// `Image` draws nothing at all, rather than clipping, when the encoded cover is larger than its
/// area, so one row of disagreement here makes the cover vanish. If the bytes of that image are
/// already held (a resize), they are encoded again instead of downloaded again.
fn maybe_fetch_cover(
    state: &State,
    covers: &mut Covers,
    terminal_size: ratatui::layout::Size,
    done: &mpsc::UnboundedSender<(String, ratatui::layout::Size, Outcome)>,
) {
    let full = ratatui::layout::Rect::new(0, 0, terminal_size.width, terminal_size.height);
    let Some(covers::Wanted { url, cells }) =
        covers::wanted(state, covers, view::content_area(full))
    else {
        return;
    };
    let Some(picker) = covers.picker().cloned() else {
        return;
    };
    if !covers.start(&url, cells) {
        return;
    }
    let held = covers.bytes(&url);
    let done = done.clone();
    tokio::spawn(async move {
        let outcome = covers::fetch(url.clone(), picker, cells, covers::http_source(), held).await;
        let _ = done.send((url, cells, outcome));
    });
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
