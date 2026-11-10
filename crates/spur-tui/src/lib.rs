//! The terminal frontend. It speaks [`spur_rpc`] and nothing else: the server says what
//! to show (a UI tree, theme tokens, surface frames), this crate lays it out on the
//! terminal grid and sends keys and mouse input back. It has no idea what a "files
//! panel" or an "Ask card" is.

mod grid;
mod input;
mod render;

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::mpsc;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    MouseButton, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use spur_rpc::{
    Caps, ClientEnd, ClientKind, ClientMsg, PROTOCOL_VERSION, ServerMsg, Size, Style, SurfaceFrame,
    Ui,
};

pub use grid::{Cell, Grid};
pub use input::notation;
pub use render::{Frame, Hit, Rect, Target, ViewState, char_col, display_col, render};

/// Name of this crate, used in diagnostics.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// Lines one wheel notch scrolls.
const WHEEL: i32 = 3;

enum Ev {
    Term(Event),
    Server(ServerMsg),
    Closed,
}

/// Puts the terminal into app mode and restores it when dropped.
struct Screen;

impl Screen {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        crossterm::execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        Ok(Self)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        // Show the cursor again with its default shape.
        let _ = io::stdout().write_all(b"\x1b[?25h\x1b[0 q");
        let _ = io::stdout().flush();
        let _ = disable_raw_mode();
    }
}

/// What the client knows: the last things the server sent, and the last frame drawn.
struct App {
    end_tx: mpsc::Sender<ClientMsg>,
    theme: BTreeMap<String, Style>,
    ui: Option<Ui>,
    surface: SurfaceFrame,
    size: (u16, u16),
    state: ViewState,
    drawn: Option<Frame>,
    reported: Option<(u16, u16)>,
    truecolor: bool,
}

impl App {
    fn send(&self, msg: ClientMsg) {
        let _ = self.end_tx.send(msg);
    }

    fn draw(&mut self, out: &mut impl Write) -> io::Result<()> {
        let Some(ui) = &self.ui else {
            return Ok(());
        };
        let frame = render(ui, &self.surface, &self.theme, self.size, &mut self.state);
        // Synchronized update, so the terminal never shows half a frame.
        out.write_all(b"\x1b[?2026h\x1b[?25l")?;
        frame
            .grid
            .write_diff(self.drawn.as_ref().map(|f| &f.grid), out, self.truecolor)?;
        if let Some((x, y)) = frame.cursor {
            // A steady bar: this is a typing cursor, not a block.
            write!(out, "\x1b[{};{}H\x1b[6 q\x1b[?25h", y + 1, x + 1)?;
        }
        out.write_all(b"\x1b[?2026l")?;
        out.flush()?;
        if let Some((rows, cols)) = frame.surface_size
            && self.reported != Some((rows, cols))
        {
            self.reported = Some((rows, cols));
            self.send(ClientMsg::SurfaceSize { rows, cols });
        }
        self.drawn = Some(frame);
        Ok(())
    }

    fn mouse(&self, kind: MouseEventKind, x: u16, y: u16) {
        let Some(frame) = &self.drawn else { return };
        let Some(hit) = frame.hit(x, y) else { return };
        let line = usize::from(y - hit.rect.y);
        match (kind, &hit.target) {
            (MouseEventKind::Down(MouseButton::Left), Target::Surface) => {
                let text: String = self
                    .surface
                    .lines
                    .get(line)
                    .map(|l| l.iter().map(|s| &*s.text).collect())
                    .unwrap_or_default();
                let col = char_col(&text, usize::from(x - hit.rect.x) + frame.hscroll);
                self.send(ClientMsg::Click {
                    node: hit.node.clone(),
                    row: self.surface.top_line + u32::try_from(line).unwrap_or(0),
                    col: u32::try_from(col).unwrap_or(0),
                });
            }
            (MouseEventKind::Down(MouseButton::Left), Target::List { rows }) => {
                if let Some(Some(row)) = rows.get(line) {
                    self.send(ClientMsg::Click {
                        node: hit.node.clone(),
                        row: *row,
                        col: 0,
                    });
                }
            }
            (MouseEventKind::ScrollDown | MouseEventKind::ScrollUp, t) if *t != Target::Nothing => {
                let down = kind == MouseEventKind::ScrollDown;
                // Lists move their selection one row per notch; text scrolls faster.
                let step = if *t == Target::Surface { WHEEL } else { 1 };
                self.send(ClientMsg::Scroll {
                    node: hit.node.clone(),
                    lines: if down { step } else { -step },
                });
            }
            _ => {}
        }
    }
}

/// Runs the terminal frontend on a connection until the server says goodbye or the
/// connection closes. Returns the server's reason for ending.
///
/// # Errors
///
/// Returns an error if the terminal cannot be set up or written to.
pub fn run(end: ClientEnd) -> io::Result<String> {
    let size = crossterm::terminal::size()?;
    let truecolor =
        std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"));
    let (end_tx, end_rx) = end.split();
    let (tx, rx) = mpsc::channel();
    let server_tx = tx.clone();
    std::thread::spawn(move || {
        for msg in end_rx {
            if server_tx.send(Ev::Server(msg)).is_err() {
                return;
            }
        }
        let _ = server_tx.send(Ev::Closed);
    });

    let mut app = App {
        end_tx,
        theme: BTreeMap::new(),
        ui: None,
        surface: SurfaceFrame::default(),
        size,
        state: ViewState::default(),
        drawn: None,
        reported: None,
        truecolor,
    };
    app.send(ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        kind: ClientKind::Frontend,
        name: "tui".into(),
        caps: Caps {
            cells: true,
            truecolor,
            mouse: true,
        },
        size: Size {
            cols: size.0,
            rows: size.1,
        },
    });

    let screen = Screen::enter()?;
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if tx.send(Ev::Term(ev)).is_err() {
                return;
            }
        }
    });

    let mut out = io::BufWriter::new(io::stdout());
    let mut reason = String::from("the connection closed");
    'main: while let Ok(first) = rx.recv() {
        // Handle everything that is waiting, then draw once.
        let mut redraw = false;
        for ev in std::iter::once(first).chain(rx.try_iter()) {
            match ev {
                Ev::Closed => break 'main,
                Ev::Server(ServerMsg::Bye { reason: r }) => {
                    reason = r;
                    break 'main;
                }
                Ev::Server(ServerMsg::Theme { tokens }) => app.theme = tokens,
                Ev::Server(ServerMsg::Ui { ui }) => app.ui = Some(ui),
                Ev::Server(ServerMsg::Surface { frame }) => app.surface = frame,
                Ev::Server(ServerMsg::Flush { frame }) => {
                    redraw = true;
                    app.send(ClientMsg::Ack { frame });
                }
                Ev::Term(Event::Key(key)) => {
                    if let Some(key) = notation(&key) {
                        app.send(ClientMsg::Key { key });
                    }
                }
                Ev::Term(Event::Paste(text)) => app.send(ClientMsg::Text { text }),
                Ev::Term(Event::Mouse(m)) => app.mouse(m.kind, m.column, m.row),
                Ev::Term(Event::Resize(cols, rows)) => {
                    app.size = (cols, rows);
                    app.drawn = None;
                    app.send(ClientMsg::Resize {
                        size: Size { cols, rows },
                    });
                    redraw = true;
                }
                Ev::Server(ServerMsg::Welcome { .. } | ServerMsg::Pong { .. }) | Ev::Term(_) => {}
            }
        }
        if redraw {
            app.draw(&mut out)?;
        }
    }
    drop(screen);
    Ok(reason)
}
