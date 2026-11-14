//! Terminal frontend: a client of the session server that lays the UI tree out in
//! cells and draws it with crossterm.
//!
//! The client holds no look: colours come in `theme` messages, glyphs and text in
//! the tree, key bindings live in the server's pack. It turns terminal input into
//! protocol messages (keys as `spur-keymap` notation) and draws what the server
//! sends: one draw per batch of input, only changed cells written.

// Geometry reads best as x, y, w, h.
#![allow(clippy::many_single_char_names)]

mod clean;
mod color;
mod grid;
mod input;
mod layout;
mod paint;
mod term;

use std::io::{self, Write as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, Sender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use crossterm::event::{Event, MouseButton, MouseEventKind};
use spur_rpc::{
    Caps, ClientEnd, ClientKind, ClientMsg, Layout, MouseAction, PROTOCOL_VERSION, ServerMsg, Size,
    SurfaceSize,
};

use crate::color::Palette;
use crate::grid::Grid;
use crate::paint::{Frame, Screen, Target, paint};

pub use term::restore;

/// Name of this crate, used in diagnostics.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// Lines one wheel notch scrolls.
const WHEEL_LINES: i32 = 3;

/// A log line sink.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// How to run the client.
#[derive(Clone, Default)]
pub struct Options {
    /// Name sent in `hello`.
    pub name: String,
    /// 24-bit colour; `None` reads `COLORTERM`.
    pub truecolor: Option<bool>,
    /// Draw icons with their ASCII fallback.
    pub ascii_icons: bool,
    /// Where errors from the server and the client go.
    pub log: Option<Log>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("name", &self.name)
            .field("truecolor", &self.truecolor)
            .field("ascii_icons", &self.ascii_icons)
            .finish_non_exhaustive()
    }
}

/// Why [`Runner::run`] returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// The server said `bye` because the person quit.
    Quit,
    /// The server said `bye` for another reason.
    Bye(String),
    /// The connection closed.
    Disconnected,
    /// [`StopHandle::stop`] was called.
    Stopped,
}

enum Ev {
    Term(Event),
    Server(Vec<ServerMsg>),
    Closed,
    Stopped,
    Failed(String),
}

/// Stops a running client from another thread (a signal handler's thread).
#[derive(Clone, Debug)]
pub struct StopHandle(Sender<Ev>);

impl std::fmt::Debug for Ev {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ev")
    }
}

impl StopHandle {
    /// Asks the client to stop; `run` returns [`ExitReason::Stopped`].
    pub fn stop(&self) {
        let _ = self.0.send(Ev::Stopped);
    }
}

/// A client ready to run.
#[derive(Debug)]
pub struct Runner {
    tx: Sender<Ev>,
    rx: Receiver<Ev>,
}

/// Creates the client's event channel, so a stop handle exists before it runs.
pub fn prepare() -> (Runner, StopHandle) {
    let (tx, rx) = mpsc::channel();
    (Runner { tx: tx.clone(), rx }, StopHandle(tx))
}

static TUI_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);

/// The thread running [`Runner::run`], if any (for a panic hook).
pub fn tui_thread() -> Option<ThreadId> {
    TUI_THREAD.lock().ok().and_then(|t| *t)
}

/// Restores the terminal when `run` returns, however it returns.
struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
        if let Ok(mut t) = TUI_THREAD.lock() {
            *t = None;
        }
    }
}

fn spawn_guarded(name: &str, tx: Sender<Ev>, f: impl FnOnce(&Sender<Ev>) + Send + 'static) {
    let failed = tx.clone();
    let started = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            if let Err(p) = catch_unwind(AssertUnwindSafe(|| f(&tx))) {
                let msg = p
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| p.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "a client thread panicked".into());
                let _ = tx.send(Ev::Failed(msg));
            }
        });
    if let Err(e) = started {
        let _ = failed.send(Ev::Failed(format!("cannot start a client thread: {e}")));
    }
}

impl Runner {
    /// Takes over the terminal and runs the client until the server says `bye`, the
    /// connection closes or the client is stopped. The terminal is restored on
    /// return.
    ///
    /// # Errors
    ///
    /// The terminal could not be set up, read or written, or a client thread
    /// panicked.
    pub fn run(self, end: ClientEnd, opts: Options) -> io::Result<ExitReason> {
        if let Ok(mut t) = TUI_THREAD.lock() {
            *t = Some(std::thread::current().id());
        }
        let _guard = Guard;
        term::setup()?;
        let (cols, rows) = crossterm::terminal::size()?;
        let truecolor = opts.truecolor.unwrap_or_else(|| {
            std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit")
        });
        let (out, from_server) = end.split();
        spawn_guarded("spur-tui-read", self.tx.clone(), |tx| {
            loop {
                match crossterm::event::read() {
                    Ok(ev) => {
                        if tx.send(Ev::Term(ev)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Ev::Failed(format!("terminal read: {e}")));
                        return;
                    }
                }
            }
        });
        spawn_guarded("spur-tui-recv", self.tx.clone(), move |tx| {
            for batch in from_server {
                if tx.send(Ev::Server(batch)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Ev::Closed);
        });
        // Never block: a server that left 1,024 batches unread is stuck, and a
        // blocked client would ignore its stop signal with the terminal still set up.
        let stall_log = opts.log.clone();
        let send = |msgs: Vec<ClientMsg>| match out.try_send(msgs) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                if let Some(l) = &stall_log {
                    l("the server stopped reading; leaving");
                }
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        };
        let hello = ClientMsg::Hello {
            protocols: vec![PROTOCOL_VERSION],
            kind: ClientKind::Frontend,
            name: opts.name.clone(),
            caps: Caps {
                truecolor,
                mouse: true,
                layout: Layout::Cells,
                ..Caps::default()
            },
            size: Size {
                cols: u32::from(cols),
                rows: u32::from(rows),
            },
        };
        if !send(vec![hello]) {
            return Ok(ExitReason::Disconnected);
        }
        let mut app = App::new(truecolor, opts, (u32::from(cols), u32::from(rows)));
        let mut stdout = io::stdout();
        loop {
            let Ok(first) = self.rx.recv() else {
                return Ok(ExitReason::Disconnected);
            };
            let mut evs = vec![first];
            while let Ok(ev) = self.rx.try_recv() {
                evs.push(ev);
            }
            let mut msgs = Vec::new();
            for ev in evs {
                if let Some(end) = app.event(ev, &mut msgs)? {
                    let _ = send(msgs);
                    return Ok(end);
                }
            }
            app.flush_keys(&mut msgs);
            if app.need_draw {
                let bytes = app.draw(&mut msgs);
                stdout.write_all(bytes.as_bytes())?;
                stdout.flush()?;
            }
            if !msgs.is_empty() && !send(msgs) {
                return Ok(ExitReason::Disconnected);
            }
        }
    }
}

/// The client's state between draws.
struct App {
    screen: Screen,
    truecolor: bool,
    opts: Options,
    size: (u32, u32),
    keys: String,
    frame: Option<Frame>,
    shown: Option<Grid>,
    sent_sizes: Vec<SurfaceSize>,
    ack: Option<u64>,
    need_draw: bool,
    /// A surface did not resolve: `refresh` was sent; row-only surfaces wait.
    awaiting_full: bool,
}

impl App {
    fn new(truecolor: bool, opts: Options, size: (u32, u32)) -> Self {
        Self {
            screen: Screen {
                ascii: opts.ascii_icons,
                ..Screen::default()
            },
            truecolor,
            opts,
            size,
            keys: String::new(),
            frame: None,
            shown: None,
            sent_sizes: Vec::new(),
            ack: None,
            need_draw: false,
            awaiting_full: false,
        }
    }

    fn log(&self, line: &str) {
        if let Some(l) = &self.opts.log {
            l(line);
        }
    }

    fn flush_keys(&mut self, msgs: &mut Vec<ClientMsg>) {
        if !self.keys.is_empty() {
            msgs.push(ClientMsg::Key {
                key: std::mem::take(&mut self.keys),
            });
        }
    }

    fn event(&mut self, ev: Ev, msgs: &mut Vec<ClientMsg>) -> io::Result<Option<ExitReason>> {
        match ev {
            Ev::Term(e) => self.term(e, msgs),
            Ev::Server(batch) => {
                for m in batch {
                    if let Some(end) = self.server(m, msgs) {
                        return Ok(Some(end));
                    }
                }
            }
            Ev::Closed => return Ok(Some(ExitReason::Disconnected)),
            Ev::Stopped => return Ok(Some(ExitReason::Stopped)),
            Ev::Failed(m) => return Err(io::Error::other(m)),
        }
        Ok(None)
    }

    fn term(&mut self, e: Event, msgs: &mut Vec<ClientMsg>) {
        match e {
            Event::Key(k) => {
                if let Some(n) = input::notation(&k) {
                    self.keys.push_str(&n);
                }
            }
            Event::Paste(text) => {
                self.flush_keys(msgs);
                msgs.push(ClientMsg::Paste { text });
            }
            Event::Mouse(m) => {
                let (x, y) = (u32::from(m.column), u32::from(m.row));
                let Some(frame) = &self.frame else {
                    return;
                };
                let msg = match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => frame.target(x, y).map(press),
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        let lines = if m.kind == MouseEventKind::ScrollUp {
                            -WHEEL_LINES
                        } else {
                            WHEEL_LINES
                        };
                        frame.scroll_target(x, y).map(|node| ClientMsg::Mouse {
                            node: node.to_owned(),
                            action: MouseAction::Scroll,
                            line: 0,
                            col: 0,
                            lines,
                            item: None,
                        })
                    }
                    _ => None,
                };
                if let Some(msg) = msg {
                    self.flush_keys(msgs);
                    msgs.push(msg);
                }
            }
            Event::Resize(c, r) => {
                self.flush_keys(msgs);
                // A terminal may drop cells on any resize, even one that ends at the
                // size drawn last: redraw everything.
                self.shown = None;
                self.size = (u32::from(c), u32::from(r));
                msgs.push(ClientMsg::Resize {
                    size: Size {
                        cols: self.size.0,
                        rows: self.size.1,
                    },
                });
                self.need_draw = true;
            }
            _ => {}
        }
    }

    fn server(&mut self, m: ServerMsg, msgs: &mut Vec<ClientMsg>) -> Option<ExitReason> {
        match m {
            ServerMsg::Theme { tokens, .. } => {
                self.screen.palette = Palette::new(&tokens, self.truecolor);
            }
            ServerMsg::Ui { root, focus } => {
                let mut ids = std::collections::HashSet::new();
                collect_ids(&root, &mut ids);
                self.screen.lists.retain(|k, _| ids.contains(k.as_str()));
                self.screen.surfaces.retain(|k, _| ids.contains(k.as_str()));
                self.screen.root = Some(root);
                self.screen.focus = focus;
            }
            ServerMsg::List { node, list } => {
                self.screen.lists.insert(node, list);
            }
            ServerMsg::Surface { node, mut view } => {
                let full = view.rows.is_empty();
                if self.awaiting_full && !full {
                    return None;
                }
                let prev = self.screen.surfaces.get(&node).map(|s| &s.view);
                if view.resolve(prev).is_err() {
                    if !self.awaiting_full {
                        msgs.push(ClientMsg::Refresh);
                        self.awaiting_full = true;
                    }
                    return None;
                }
                self.awaiting_full = false;
                let s = self.screen.surfaces.entry(node).or_default();
                s.view = view;
            }
            ServerMsg::Flush { frame } => {
                self.ack = Some(frame);
                self.need_draw = true;
            }
            ServerMsg::Error { message } => self.log(&format!("server: {message}")),
            ServerMsg::Bye { reason } => {
                return Some(if reason == "quit" {
                    ExitReason::Quit
                } else {
                    ExitReason::Bye(reason)
                });
            }
            _ => {}
        }
        None
    }

    /// Draws, queues `layout` if sizes changed and the ack; returns the bytes.
    fn draw(&mut self, msgs: &mut Vec<ClientMsg>) -> String {
        self.need_draw = false;
        let frame = paint(&mut self.screen, self.size.0, self.size.1);
        let bytes = grid::diff(self.shown.as_ref(), &frame.grid, frame.cursor);
        if frame.sizes != self.sent_sizes {
            msgs.push(ClientMsg::Layout {
                surfaces: frame.sizes.clone(),
            });
            self.sent_sizes.clone_from(&frame.sizes);
        }
        if let Some(frame) = self.ack.take() {
            msgs.push(ClientMsg::Ack { frame });
        }
        self.shown = Some(frame.grid.clone());
        self.frame = Some(frame);
        bytes
    }
}

fn press(t: Target) -> ClientMsg {
    let (node, line, col, item) = match t {
        Target::Surface { node, line, col } => (node, line, col, None),
        Target::List { node, item } => (node, 0, 0, item),
        Target::Click(node) => (node, 0, 0, None),
    };
    ClientMsg::Mouse {
        node,
        action: MouseAction::Press,
        line,
        col,
        lines: 0,
        item,
    }
}

fn collect_ids<'a>(n: &'a spur_rpc::Node, out: &mut std::collections::HashSet<&'a str>) {
    out.insert(n.id.as_str());
    for c in &n.children {
        collect_ids(c, out);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(super::NAME, "spur-tui");
    }

    #[test]
    fn restore_twice_is_harmless() {
        super::restore();
        super::restore();
    }
}

#[cfg(test)]
mod shell_tests {
    use super::*;
    use crate::layout::{Rect, layout};

    fn fixture(name: &str) -> Vec<ServerMsg> {
        let path = format!(
            "{}/../spur-server/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn app_for(name: &str, w: u32, h: u32) -> App {
        let mut app = App::new(true, Options::default(), (w, h));
        let mut msgs = Vec::new();
        for m in fixture(name) {
            assert!(app.server(m, &mut msgs).is_none());
        }
        assert!(msgs.is_empty(), "{msgs:?}");
        app
    }

    fn snapshot(name: &str, got: &str) {
        let path = format!("{}/tests/snapshots/{name}", env!("CARGO_MANIFEST_DIR"));
        if std::env::var_os("SPUR_BLESS").is_some() {
            std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
            std::fs::write(&path, got).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{path}: {e} (run with SPUR_BLESS=1)"));
        assert!(
            got == want,
            "{name} differs (run with SPUR_BLESS=1):\n{got}"
        );
    }

    fn rect(app: &App, id: &str) -> Rect {
        let root = app.screen.root.as_ref().unwrap();
        let (w, h) = app.size;
        layout(root, Rect { x: 0, y: 0, w, h })
            .into_iter()
            .find(|p| p.node.id == id)
            .unwrap_or_else(|| panic!("no {id}"))
            .rect
    }

    #[test]
    fn the_first_frames_paint_as_the_snapshots() {
        for (name, w, h) in [
            ("first_frame_160x45", 160, 45),
            ("first_frame_80x24", 80, 24),
        ] {
            let mut app = app_for(&format!("{name}.json"), w, h);
            let mut msgs = Vec::new();
            let bytes = app.draw(&mut msgs);
            assert!(bytes.contains("\x1b[?2026h"));
            // A layout message and the ack follow the first draw.
            assert!(
                matches!(msgs.first(), Some(ClientMsg::Layout { .. })),
                "{msgs:?}"
            );
            assert!(matches!(msgs.last(), Some(ClientMsg::Ack { .. })));
            let frame = app.frame.as_ref().unwrap();
            assert!(frame.cursor.is_some(), "the code surface has focus");
            snapshot(
                &format!("{name}.txt"),
                &(frame.grid.lines().join("\n") + "\n"),
            );
        }
    }

    #[test]
    fn the_shell_layout_at_160_and_80_columns() {
        let app = app_for("first_frame_160x45.json", 160, 45);
        assert_eq!(
            rect(&app, "topbar"),
            Rect {
                x: 0,
                y: 0,
                w: 160,
                h: 1
            }
        );
        assert_eq!(
            rect(&app, "rail"),
            Rect {
                x: 1,
                y: 1,
                w: 3,
                h: 44
            }
        );
        assert_eq!(
            rect(&app, "sidebar"),
            Rect {
                x: 5,
                y: 1,
                w: 28,
                h: 44
            }
        );
        assert_eq!(
            rect(&app, "main"),
            Rect {
                x: 34,
                y: 1,
                w: 81,
                h: 44
            }
        );
        assert_eq!(
            rect(&app, "agent"),
            Rect {
                x: 116,
                y: 1,
                w: 43,
                h: 44
            }
        );
        let app = app_for("first_frame_80x24.json", 80, 24);
        assert_eq!(
            rect(&app, "main"),
            Rect {
                x: 5,
                y: 1,
                w: 74,
                h: 23
            }
        );
    }

    #[test]
    fn the_code_column_keeps_64_cells_at_the_thresholds() {
        let full = app_for("first_frame_160x45.json", 160, 45);
        let mut no_agent = full.screen.root.clone().unwrap();
        let body = no_agent
            .children
            .iter_mut()
            .find(|c| c.id == "body")
            .unwrap();
        body.children.retain(|c| c.id != "agent");
        for (root, w) in [(full.screen.root.clone().unwrap(), 144), (no_agent, 100)] {
            let main = layout(
                &root,
                Rect {
                    x: 0,
                    y: 0,
                    w,
                    h: 30,
                },
            )
            .into_iter()
            .find(|p| p.node.id == "main")
            .unwrap()
            .rect;
            assert!(main.w >= 64, "{w}: {}", main.w);
        }
    }

    #[test]
    fn clicks_map_to_nodes_items_and_positions() {
        let mut app = app_for("first_frame_160x45.json", 160, 45);
        let mut msgs = Vec::new();
        app.draw(&mut msgs);
        let frame = app.frame.as_ref().unwrap();
        let rail = rect(&app, "rail_files");
        assert_eq!(
            frame.target(rail.x + 1, rail.y),
            Some(Target::Click("rail_files".into()))
        );
        let list = rect(&app, "files_list");
        let Some(Target::List { node, item }) = frame.target(list.x + 3, list.y) else {
            panic!("no list target");
        };
        assert_eq!(node, "files_list");
        assert_eq!(item.as_deref(), Some("docs"));
        let code = rect(&app, "code");
        // Gutter of 4 cells: column 2 of the first line.
        let t = frame.target(code.x + 4 + 2, code.y);
        assert_eq!(
            t,
            Some(Target::Surface {
                node: "code".into(),
                line: 0,
                col: 2
            })
        );
        assert_eq!(frame.scroll_target(code.x + 10, code.y + 5), Some("code"));
        assert_eq!(frame.scroll_target(rail.x, rail.y), None);
    }

    #[test]
    fn a_full_redraw_of_300x100_is_quick() {
        let mut app = app_for("first_frame_160x45.json", 300, 100);
        let start = std::time::Instant::now();
        let mut msgs = Vec::new();
        let bytes = app.draw(&mut msgs);
        let took = start.elapsed();
        assert!(!bytes.is_empty());
        // The budget is 5 ms in a release build (about 3.4 ms measured); debug
        // builds, which CI runs, get ten times that.
        let budget = if cfg!(debug_assertions) { 50 } else { 5 };
        assert!(took < std::time::Duration::from_millis(budget), "{took:?}");
    }
}
