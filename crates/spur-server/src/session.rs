//! A session: one actor thread that owns the editor state and serves every attached
//! client. Each client has its own view, focus, keymap state, context keys and UI.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spur_core::editor::{DocId, Editor, ViewId, motion, register_editor_commands};
use spur_core::{
    Actor, ClientId, CommandError, CommandId, CommandMeta, Context, ContextLookup, Core, CtxValue,
    EventName, Invocation, Scoped, When, events, parse_args,
};
use spur_keymap::{Dispatch, Key, KeyCode, Keymap, Mods, parse_keys};
use spur_rpc::{
    ClientKind, ClientMsg, Cursor, CursorKind, PROTOCOL_VERSION, Pos, ServerEnd, ServerMsg, Size,
    Span, SurfaceFrame, Ui,
};
use spur_text::{Buffer, SelRange, Selection};

use crate::ask::{Ask, CommandEntry, Pick, title};
use crate::files::{Tree, index};
use crate::pack::Pack;
use crate::syntax::{highlight, language};
use crate::ui::{Env, ListData, resolve};

/// How long a notice stays up.
const TOAST: Duration = Duration::from_secs(4);
/// Lines kept between the cursor and the edge of the surface.
const SCROLL_MARGIN: usize = 3;
/// Largest file the server opens.
const MAX_FILE: u64 = 64 << 20;
/// Longest line prefix sent to a client, in chars.
const MAX_LINE: usize = 2000;

/// How to start a session.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Session name.
    pub name: String,
    /// Workspace folder.
    pub root: PathBuf,
    /// The experience every client gets.
    pub pack: Pack,
    /// File to open first.
    pub open: Option<PathBuf>,
    /// End the session when the last client leaves (a session without a daemon).
    pub exit_when_empty: bool,
    /// Shown to the first client as a notice (e.g. why the fallback pack is in use).
    pub notice: Option<String>,
}

enum In {
    Attach(ClientId, Sender<ServerMsg>),
    Msg(ClientId, ClientMsg),
    Gone(ClientId),
    Shutdown,
}

/// A running session. Dropping it shuts the session down.
#[derive(Debug)]
pub struct Session {
    tx: Sender<In>,
    next_client: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

/// Attaches clients to a session from other threads (an accept loop).
#[derive(Clone, Debug)]
pub struct Attacher {
    tx: Sender<In>,
    next_client: Arc<AtomicU64>,
}

impl Attacher {
    /// Attaches a client. Returns `false` if the session has ended.
    pub fn attach(&self, end: ServerEnd) -> bool {
        let id = ClientId(self.next_client.fetch_add(1, Ordering::Relaxed));
        let (out, input) = end.split();
        if self.tx.send(In::Attach(id, out)).is_err() {
            return false;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            for msg in input {
                if tx.send(In::Msg(id, msg)).is_err() {
                    return;
                }
            }
            let _ = tx.send(In::Gone(id));
        });
        true
    }
}

impl Session {
    /// Starts the session thread.
    ///
    /// # Errors
    ///
    /// Returns a message if the built-in commands cannot be registered.
    pub fn spawn(cfg: SessionConfig) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let mut state = State::new(cfg)?;
        let thread = std::thread::Builder::new()
            .name("spur-session".into())
            .spawn(move || state.run(&rx))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            tx,
            next_client: Arc::new(AtomicU64::new(1)),
            thread: Some(thread),
        })
    }

    /// A handle that attaches clients.
    pub fn attacher(&self) -> Attacher {
        Attacher {
            tx: self.tx.clone(),
            next_client: self.next_client.clone(),
        }
    }

    /// Attaches a client.
    pub fn attach(&self, end: ServerEnd) -> bool {
        self.attacher().attach(end)
    }

    /// Waits until the session ends by itself.
    pub fn join(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.tx.send(In::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Where a client's keys go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Editor,
    Files,
    Ask,
}

impl Focus {
    fn name(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::Files => "files",
            Self::Ask => "ask",
        }
    }
}

/// A UI request made by a command; the session applies it to the calling client.
#[derive(Clone, Debug)]
enum Op {
    Focus(Focus),
    ToggleFiles,
    AskOpen,
    AskClose,
    AskAccept,
    AskMove(i32),
    AskInsert(String),
    AskBackspace,
    FilesMove(i32),
    FilesActivate(Option<bool>),
    Page(i32),
    Open(PathBuf),
    Quit,
}

struct Toast {
    text: String,
    error: bool,
    until: Instant,
}

struct Client {
    tx: Sender<ServerMsg>,
    /// `None` until the client said hello.
    kind: Option<ClientKind>,
    name: String,
    keymap: Keymap,
    view: ViewId,
    views: HashMap<DocId, ViewId>,
    focus: Focus,
    files_open: bool,
    tree: Tree,
    ask: Ask,
    toast: Option<Toast>,
    /// Key count at which quit was last refused because of unsaved changes.
    quit_armed: Option<u64>,
    keys: u64,
    surface: Size,
    ctx: Context,
    last_ui: Option<Ui>,
    last_surface: Option<SurfaceFrame>,
    /// View, cursor and size the surface was last scrolled for.
    scrolled_for: Option<(ViewId, usize, u16)>,
    frame: u64,
    /// Pings to answer after the next render.
    pings: Vec<u64>,
}

struct State {
    name: String,
    root: PathBuf,
    pack: Pack,
    exit_when_empty: bool,
    notice: Option<String>,
    core: Core<Editor>,
    ops: Arc<Mutex<Vec<Op>>>,
    clients: BTreeMap<ClientId, Client>,
    /// Document new clients start on.
    home: DocId,
    /// The editor's own first view; never removed, so `Editor::focus` is always valid.
    anchor_view: ViewId,
    had_client: bool,
}

fn cmd(id: &str) -> CommandId {
    CommandId::new(id).unwrap_or_else(|e| unreachable!("{e}"))
}

/// Formats key notation for people: `<C-Space>` becomes `ctrl space`.
fn pretty_keys(notation: &str) -> String {
    let Ok(keys) = parse_keys(notation) else {
        return notation.to_owned();
    };
    let one = |k: &Key| {
        let mut parts: Vec<String> = Vec::new();
        for (m, name) in [
            (Mods::CTRL, "ctrl"),
            (Mods::ALT, "alt"),
            (Mods::SHIFT, "shift"),
            (Mods::SUPER, "super"),
        ] {
            if k.mods.contains(m) {
                parts.push(name.into());
            }
        }
        parts.push(match k.code {
            KeyCode::Char(' ') => "space".into(),
            KeyCode::Char(c) if c.is_uppercase() => format!("shift {}", c.to_lowercase()),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Esc => "esc".into(),
            KeyCode::Enter => "enter".into(),
            KeyCode::Tab => "tab".into(),
            KeyCode::Backspace => "backspace".into(),
            KeyCode::Delete => "delete".into(),
            KeyCode::Insert => "insert".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::Home => "home".into(),
            KeyCode::End => "end".into(),
            KeyCode::PageUp => "pgup".into(),
            KeyCode::PageDown => "pgdn".into(),
            KeyCode::F(n) => format!("f{n}"),
            KeyCode::AnyChar => "…".into(),
        });
        parts.join(" ")
    };
    keys.iter().map(one).collect::<Vec<_>>().join(", ")
}

fn ctx_text(v: &CtxValue) -> String {
    match v {
        CtxValue::Bool(b) => b.to_string(),
        CtxValue::Int(i) => i.to_string(),
        CtxValue::Str(s) => s.to_string(),
        CtxValue::List(l) => l.join(", "),
    }
}

#[derive(serde::Deserialize)]
struct TextArgs {
    text: String,
}

#[derive(serde::Deserialize)]
struct PathArgs {
    path: PathBuf,
}

#[derive(serde::Deserialize)]
struct FocusArgs {
    target: String,
}

#[derive(serde::Deserialize)]
struct MoveToArgs {
    line: usize,
    #[serde(default)]
    col: usize,
    #[serde(default)]
    extend: bool,
}

/// Registers the server's own commands. Most only queue an [`Op`], which the session
/// applies to the calling client once the command returns.
#[allow(clippy::too_many_lines)] // one table of commands
fn register_server_commands(
    core: &mut Core<Editor>,
    ops: &Arc<Mutex<Vec<Op>>>,
) -> Result<(), String> {
    type Make = fn(&Value) -> Result<Op, CommandError>;
    let text_schema = json!({
        "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"]
    });
    let table: [(&str, &str, Value, Make); 18] = [
        (
            "ui.focus",
            "Move keyboard focus to the editor or the files panel.",
            json!({ "type": "object", "properties": { "target": { "enum": ["editor", "files"] } }, "required": ["target"] }),
            |a| {
                let f: FocusArgs = parse_args(a)?;
                match f.target.as_str() {
                    "editor" => Ok(Op::Focus(Focus::Editor)),
                    "files" => Ok(Op::Focus(Focus::Files)),
                    other => Err(CommandError::bad_args(format!("unknown target {other:?}"))),
                }
            },
        ),
        (
            "ui.toggle_files",
            "Show and focus the files panel, or hide it.",
            Value::Null,
            |_| Ok(Op::ToggleFiles),
        ),
        (
            "ask.open",
            "Open Ask: find a command or a file.",
            Value::Null,
            |_| Ok(Op::AskOpen),
        ),
        ("ask.close", "Close Ask.", Value::Null, |_| Ok(Op::AskClose)),
        (
            "ask.accept",
            "Run the selected Ask row.",
            Value::Null,
            |_| Ok(Op::AskAccept),
        ),
        ("ask.next", "Select the next Ask row.", Value::Null, |_| {
            Ok(Op::AskMove(1))
        }),
        (
            "ask.prev",
            "Select the previous Ask row.",
            Value::Null,
            |_| Ok(Op::AskMove(-1)),
        ),
        ("ask.insert", "Type into Ask.", text_schema.clone(), |a| {
            let t: TextArgs = parse_args(a)?;
            Ok(Op::AskInsert(t.text))
        }),
        (
            "ask.backspace",
            "Delete the last char typed into Ask.",
            Value::Null,
            |_| Ok(Op::AskBackspace),
        ),
        ("files.next", "Select the next file.", Value::Null, |_| {
            Ok(Op::FilesMove(1))
        }),
        (
            "files.prev",
            "Select the previous file.",
            Value::Null,
            |_| Ok(Op::FilesMove(-1)),
        ),
        (
            "files.activate",
            "Open the selected file, or open/close the folder.",
            Value::Null,
            |_| Ok(Op::FilesActivate(None)),
        ),
        (
            "files.expand",
            "Open the selected folder.",
            Value::Null,
            |_| Ok(Op::FilesActivate(Some(true))),
        ),
        (
            "files.collapse",
            "Close the selected folder.",
            Value::Null,
            |_| Ok(Op::FilesActivate(Some(false))),
        ),
        (
            "view.page_down",
            "Move down one screen.",
            Value::Null,
            |_| Ok(Op::Page(1)),
        ),
        ("view.page_up", "Move up one screen.", Value::Null, |_| {
            Ok(Op::Page(-1))
        }),
        (
            "workspace.open",
            "Open a file in this client's view.",
            json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
            |a| {
                let p: PathArgs = parse_args(a)?;
                Ok(Op::Open(p.path))
            },
        ),
        ("client.quit", "Leave the session.", Value::Null, |_| {
            Ok(Op::Quit)
        }),
    ];
    for (id, desc, schema, make) in table {
        let ops = ops.clone();
        let meta = CommandMeta::new(cmd(id), desc).args_schema(schema);
        core.register_fn(meta, move |cx, args| {
            let op = make(args)?;
            if cx.client().is_none() {
                return Err(CommandError::failed("this command needs a client"));
            }
            if let Ok(mut q) = ops.lock() {
                q.push(op);
            }
            Ok(Value::Null)
        })
        .map_err(|e| e.to_string())?;
    }

    let meta = CommandMeta::new(
        cmd("editor.move_to"),
        "Put the cursor at a line and char column (from 0).",
    )
    .read_only()
    .args_schema(json!({
        "type": "object",
        "properties": { "line": { "type": "integer" }, "col": { "type": "integer" }, "extend": { "type": "boolean" } },
        "required": ["line"]
    }));
    core.register_fn(meta, |cx, args| {
        let a: MoveToArgs = parse_args(args)?;
        let snap = cx.state.snapshot();
        let line = a.line.min(snap.line_count().saturating_sub(1));
        let head = motion::offset_at_col(&snap, line, a.col);
        let id = cx.state.focus;
        let Some(view) = cx.state.views.get_mut(&id) else {
            return Err(CommandError::failed("no view"));
        };
        let anchor = if a.extend {
            view.selection.ranges[view.selection.primary].anchor
        } else {
            head
        };
        view.selection = Selection::cursor(head);
        view.selection.ranges[0] = SelRange { anchor, head };
        view.goals = vec![None];
        view.turn += 1;
        let name =
            EventName::new(events::SELECTION_CHANGED).unwrap_or_else(|e| unreachable!("{e}"));
        cx.emit(name, json!({ "view": id.0 }))?;
        Ok(Value::Null)
    })
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// The UI resolver's view of one client.
struct ClientEnv<'a> {
    client: &'a Client,
    global: &'a Context,
    mode: &'a str,
}

impl Env for ClientEnv<'_> {
    fn get(&self, key: &str) -> Option<String> {
        Scoped(&[&self.client.ctx, self.global])
            .get(key)
            .map(ctx_text)
    }

    fn test(&self, when: &When) -> bool {
        when.eval(&Scoped(&[&self.client.ctx, self.global]))
    }

    fn key_hint(&self, command: &CommandId) -> Option<String> {
        self.client
            .keymap
            .bindings_of(command)
            .into_iter()
            .find(|(mode, ..)| &**mode == self.mode)
            .map(|(_, keys, _)| pretty_keys(&keys))
    }

    fn list(&self, source: &str) -> ListData {
        let c = self.client;
        let index = |i: usize| u32::try_from(i).ok();
        match source {
            "files" => ListData {
                rows: c.tree.list_rows(),
                selected: (!c.tree.rows.is_empty())
                    .then(|| index(c.tree.selected))
                    .flatten(),
            },
            "ask" => ListData {
                rows: c.ask.rows.clone(),
                selected: c.ask.selected.and_then(index),
            },
            _ => ListData {
                rows: Vec::new(),
                selected: None,
            },
        }
    }

    fn input(&self, source: &str) -> String {
        match source {
            "ask" => self.client.ask.query.clone(),
            _ => String::new(),
        }
    }
}

impl State {
    fn new(cfg: SessionConfig) -> Result<Self, String> {
        let editor = Editor::new();
        let anchor_view = editor.focus;
        let home = editor.view().doc;
        let mut core = Core::new(editor);
        register_editor_commands(&mut core).map_err(|e| e.to_string())?;
        let ops = Arc::new(Mutex::new(Vec::new()));
        register_server_commands(&mut core, &ops)?;
        let mut state = Self {
            name: cfg.name,
            root: cfg.root,
            pack: cfg.pack,
            exit_when_empty: cfg.exit_when_empty,
            notice: cfg.notice,
            core,
            ops,
            clients: BTreeMap::new(),
            home,
            anchor_view,
            had_client: false,
        };
        if let Some(path) = cfg.open {
            match state.load(&path) {
                Ok(doc) => state.home = doc,
                Err(e) => state.notice = Some(e),
            }
        }
        Ok(state)
    }

    fn run(&mut self, rx: &Receiver<In>) {
        loop {
            let now = Instant::now();
            let deadline = self
                .clients
                .values()
                .flat_map(|c| [c.keymap.deadline(), c.toast.as_ref().map(|t| t.until)])
                .flatten()
                .min();
            let msg = match deadline {
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(d) => rx.recv_timeout(d.saturating_duration_since(now)),
            };
            match msg {
                Ok(In::Attach(id, tx)) => self.attach(id, tx),
                Ok(In::Msg(id, m)) => self.message(id, m),
                Ok(In::Gone(id)) => self.remove(id, None),
                Ok(In::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
            self.render_all();
            if self.exit_when_empty && self.had_client && self.clients.is_empty() {
                break;
            }
        }
        for c in self.clients.values() {
            let _ = c.tx.send(ServerMsg::Bye {
                reason: "the session ended".into(),
            });
        }
    }

    fn attach(&mut self, id: ClientId, tx: Sender<ServerMsg>) {
        let mut keymap = Keymap::new();
        keymap.push_layer(self.pack.layer.clone());
        let view = self.core.state.add_view(self.home);
        let mut tree = Tree::new(&self.root);
        if let Some(path) = &self.core.state.docs[&self.home].path {
            tree.reveal(path);
        }
        self.clients.insert(
            id,
            Client {
                tx,
                kind: None,
                name: String::new(),
                keymap,
                view,
                views: HashMap::from([(self.home, view)]),
                focus: Focus::Editor,
                files_open: true,
                tree,
                ask: Ask::default(),
                toast: None,
                quit_armed: None,
                keys: 0,
                surface: Size::default(),
                ctx: Context::new(),
                last_ui: None,
                last_surface: None,
                scrolled_for: None,
                frame: 0,
                pings: Vec::new(),
            },
        );
    }

    /// Drops a client and its views. `reason` is sent as a goodbye if given.
    fn remove(&mut self, id: ClientId, reason: Option<&str>) {
        let Some(c) = self.clients.remove(&id) else {
            return;
        };
        if let Some(reason) = reason {
            let _ = c.tx.send(ServerMsg::Bye {
                reason: reason.into(),
            });
        }
        self.core.state.focus = self.anchor_view;
        for view in c.views.values() {
            self.core.state.views.remove(view);
        }
    }

    fn message(&mut self, id: ClientId, msg: ClientMsg) {
        let Some(c) = self.clients.get_mut(&id) else {
            return;
        };
        if c.kind.is_none() {
            let ClientMsg::Hello {
                protocol,
                kind,
                name,
                size,
                ..
            } = msg
            else {
                self.remove(id, Some("expected hello first"));
                return;
            };
            if protocol != PROTOCOL_VERSION {
                let reason = format!(
                    "protocol {protocol} is not supported (server speaks {PROTOCOL_VERSION})"
                );
                self.remove(id, Some(&reason));
                return;
            }
            c.kind = Some(kind);
            c.name = name.chars().take(64).collect();
            // Until the client reports its layout, assume the surface fills the screen.
            c.surface = size;
            let _ = c.tx.send(ServerMsg::Welcome {
                protocol: PROTOCOL_VERSION,
                server: env!("CARGO_PKG_VERSION").into(),
                client: id.0,
                session: self.name.clone(),
            });
            let _ = c.tx.send(ServerMsg::Theme {
                tokens: self.pack.theme.clone(),
            });
            self.had_client = true;
            if let Some(n) = self.notice.take() {
                self.toast(id, n, true);
            }
            return;
        }
        match msg {
            ClientMsg::Hello { .. } => self.remove(id, Some("unexpected second hello")),
            ClientMsg::Key { key } => self.key(id, &key),
            ClientMsg::Text { text } => {
                if c.focus == Focus::Ask {
                    c.ask.insert(&text);
                } else if c.focus == Focus::Editor {
                    self.exec_id(id, "editor.insert_text", json!({ "text": text }));
                }
            }
            ClientMsg::Resize { .. } | ClientMsg::Ack { .. } => {}
            ClientMsg::SurfaceSize { rows, cols } => {
                c.surface = Size { cols, rows };
            }
            ClientMsg::Click { node, row, col } => self.click(id, &node, row, col),
            ClientMsg::Scroll { node, lines } => self.scroll(id, &node, lines),
            ClientMsg::Command { id: command, args } => match CommandId::new(&command) {
                Ok(command) => self.exec(id, Invocation::new(command, Actor::User).args(args)),
                Err(e) => self.toast(id, e.to_string(), true),
            },
            ClientMsg::Ping { id: ping } => c.pings.push(ping),
            ClientMsg::Bye => self.remove(id, None),
        }
    }

    /// The source a node shows, if it is a list.
    fn list_source(&self, node: &str) -> Option<&str> {
        match &self.pack.nodes.get(node)?.kind {
            crate::pack::KindDef::List { source } => Some(source),
            _ => None,
        }
    }

    fn is_surface(&self, node: &str) -> bool {
        matches!(
            self.pack.nodes.get(node).map(|n| &n.kind),
            Some(crate::pack::KindDef::Surface)
        )
    }

    fn click(&mut self, id: ClientId, node: &str, row: u32, col: u32) {
        if self.is_surface(node) {
            self.apply(id, Op::Focus(Focus::Editor));
            self.exec_id(id, "editor.move_to", json!({ "line": row, "col": col }));
            return;
        }
        let source = self.list_source(node).map(str::to_owned);
        let Some(c) = self.clients.get_mut(&id) else {
            return;
        };
        let row = row as usize;
        match source.as_deref() {
            Some("files") if row < c.tree.rows.len() => {
                c.focus = Focus::Files;
                c.tree.selected = row;
                self.apply(id, Op::FilesActivate(None));
            }
            Some("ask") if c.focus == Focus::Ask && c.ask.select(row) => {
                self.apply(id, Op::AskAccept);
            }
            _ => {}
        }
    }

    fn scroll(&mut self, id: ClientId, node: &str, lines: i32) {
        if self.is_surface(node) {
            let Some(c) = self.clients.get(&id) else {
                return;
            };
            let ed = &mut self.core.state;
            let Some(view) = ed.views.get_mut(&c.view) else {
                return;
            };
            let last = ed.docs[&view.doc]
                .buffer
                .snapshot()
                .line_count()
                .saturating_sub(1);
            let step = lines.unsigned_abs() as usize;
            view.top_line = if lines < 0 {
                view.top_line.saturating_sub(step)
            } else {
                (view.top_line + step).min(last)
            };
            return;
        }
        match self.list_source(node) {
            Some("files") => self.apply(id, Op::FilesMove(lines)),
            Some("ask") => self.apply(id, Op::AskMove(lines)),
            _ => {}
        }
    }

    /// The keymap mode for a client: the view's mode in the editor, else the focus name.
    fn mode(&self, c: &Client) -> Arc<str> {
        match c.focus {
            Focus::Editor => self
                .core
                .state
                .views
                .get(&c.view)
                .map_or_else(|| "insert".into(), |v| v.mode.clone()),
            other => other.name().into(),
        }
    }

    fn key(&mut self, id: ClientId, notation: &str) {
        let Ok(keys) = parse_keys(notation) else {
            return;
        };
        for key in keys {
            self.sync_context(id);
            let Some(c) = self.clients.get(&id) else {
                return;
            };
            let mode = self.mode(c);
            let Some(c) = self.clients.get_mut(&id) else {
                return;
            };
            c.keys += 1;
            let scope = [&c.ctx, self.core.context()];
            let feed = c.keymap.feed(key, &mode, &Scoped(&scope), Instant::now());
            self.dispatch(id, &feed.run);
        }
    }

    fn dispatch(&mut self, id: ClientId, run: &[Dispatch]) {
        for d in run {
            for inv in d.invocations(&Actor::User) {
                self.exec(id, inv);
            }
        }
    }

    fn exec_id(&mut self, id: ClientId, command: &str, args: Value) {
        self.exec(id, Invocation::new(cmd(command), Actor::User).args(args));
    }

    /// Runs a command as a client: on its view, then applies the UI requests it made.
    fn exec(&mut self, id: ClientId, inv: Invocation) {
        let Some(c) = self.clients.get(&id) else {
            return;
        };
        self.core.state.focus = c.view;
        let actor = match c.kind {
            Some(ClientKind::Tool) => Actor::Remote {
                client: c.name.as_str().into(),
            },
            _ => inv.actor.clone(),
        };
        let inv = Invocation {
            actor,
            ..inv.client(id)
        };
        let result = self.core.execute(inv);
        // A command may not move another client's focus; keep `focus` valid regardless.
        if !self.core.state.views.contains_key(&self.core.state.focus) {
            self.core.state.focus = self.anchor_view;
        }
        let ops: Vec<Op> = self
            .ops
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        match result {
            Ok(v) => {
                if let Some(path) = v.get("path").and_then(Value::as_str) {
                    let name = Path::new(path)
                        .file_name()
                        .map_or_else(|| path.into(), |n| n.to_string_lossy());
                    self.toast(id, format!("Saved {name}"), false);
                    for c in self.clients.values_mut() {
                        c.tree.refresh();
                    }
                }
            }
            Err(e) => self.toast(id, e.message, true),
        }
        for op in ops {
            self.apply(id, op);
        }
    }

    fn toast(&mut self, id: ClientId, text: String, error: bool) {
        if let Some(c) = self.clients.get_mut(&id) {
            c.toast = Some(Toast {
                text,
                error,
                until: Instant::now() + TOAST,
            });
        }
    }

    fn apply(&mut self, id: ClientId, op: Op) {
        let Some(c) = self.clients.get_mut(&id) else {
            return;
        };
        match op {
            Op::Focus(f) => {
                if f == Focus::Files {
                    c.files_open = true;
                }
                if c.focus == Focus::Ask && f != Focus::Ask {
                    c.ask.close();
                }
                c.focus = f;
            }
            Op::ToggleFiles => {
                if c.focus == Focus::Files {
                    c.files_open = false;
                    c.focus = Focus::Editor;
                } else {
                    c.files_open = true;
                    c.focus = Focus::Files;
                    c.tree.refresh();
                    c.ask.close();
                }
            }
            Op::AskOpen => {
                let commands = ask_commands(&self.core, &c.keymap);
                c.ask.open(commands, index(&self.root), &self.pack.featured);
                c.focus = Focus::Ask;
            }
            Op::AskClose => {
                c.ask.close();
                c.focus = Focus::Editor;
            }
            Op::AskMove(d) => c.ask.move_by(d),
            Op::AskInsert(t) => c.ask.insert(&t),
            Op::AskBackspace => c.ask.backspace(),
            Op::AskAccept => {
                let pick = c.ask.pick();
                c.ask.close();
                c.focus = Focus::Editor;
                match pick {
                    Some(Pick::Command(command)) => {
                        self.exec(id, Invocation::new(command, Actor::User));
                    }
                    Some(Pick::File(rel)) => self.open(id, Path::new(&rel)),
                    None => {}
                }
            }
            Op::FilesMove(d) => c.tree.move_by(d),
            Op::FilesActivate(open) => {
                if let Some(path) = c.tree.activate(open) {
                    self.open(id, &path);
                }
            }
            Op::Page(dir) => {
                let rows = u32::from(c.surface.rows.saturating_sub(1).max(1));
                let command = if dir < 0 {
                    "editor.move_up"
                } else {
                    "editor.move_down"
                };
                self.exec(id, Invocation::new(cmd(command), Actor::User).count(rows));
            }
            Op::Open(path) => self.open(id, &path),
            Op::Quit => {
                let unsaved = self
                    .core
                    .state
                    .docs
                    .values()
                    .any(spur_core::editor::Doc::modified);
                let last = self.clients.len() == 1 && self.exit_when_empty;
                let Some(c) = self.clients.get_mut(&id) else {
                    return;
                };
                if unsaved && last && c.quit_armed != Some(c.keys.saturating_sub(1)) {
                    c.quit_armed = Some(c.keys);
                    self.toast(
                        id,
                        "Unsaved changes. Save first, or quit again to discard them.".into(),
                        true,
                    );
                } else {
                    self.remove(id, Some("quit"));
                }
            }
        }
    }

    /// Loads `path` (relative to the workspace root) or finds it among the open documents.
    fn load(&mut self, path: &Path) -> Result<DocId, String> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let ed = &mut self.core.state;
        if let Some((id, _)) = ed
            .docs
            .iter()
            .find(|(_, d)| d.path.as_deref() == Some(&*path))
        {
            return Ok(*id);
        }
        let shown = path.display();
        let text = match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => return Err(format!("{shown} is a folder")),
            Ok(m) if m.len() > MAX_FILE => return Err(format!("{shown} is too large to open")),
            Ok(_) => {
                let bytes = std::fs::read(&path).map_err(|e| format!("{shown}: {e}"))?;
                String::from_utf8(bytes).map_err(|_| format!("{shown} is not UTF-8 text"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{shown}: {e}")),
        };
        Ok(ed.add_doc(Buffer::new(&text), Some(path)))
    }

    /// Shows a file in this client's own view of it.
    fn open(&mut self, id: ClientId, path: &Path) {
        let doc = match self.load(path) {
            Ok(doc) => doc,
            Err(e) => {
                self.toast(id, e, true);
                return;
            }
        };
        let full = self.core.state.docs[&doc].path.clone();
        let Some(c) = self.clients.get_mut(&id) else {
            return;
        };
        let ed = &mut self.core.state;
        c.view = *c.views.entry(doc).or_insert_with(|| ed.add_view(doc));
        c.focus = Focus::Editor;
        if let Some(p) = full {
            c.tree.reveal(&p);
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for id in ids {
            let Some(c) = self.clients.get_mut(&id) else {
                continue;
            };
            if c.toast.as_ref().is_some_and(|t| t.until <= now) {
                c.toast = None;
            }
            if let Some(d) = c.keymap.tick(now) {
                self.dispatch(id, &[d]);
            }
        }
    }

    /// Publishes this client's context keys, which `when` clauses and text bindings read.
    fn sync_context(&mut self, id: ClientId) {
        let attached = self.clients.values().filter(|c| c.kind.is_some()).count();
        let Some(c) = self.clients.get_mut(&id) else {
            return;
        };
        let ed = &self.core.state;
        let Some(view) = ed.views.get(&c.view) else {
            return;
        };
        let doc = &ed.docs[&view.doc];
        let snap = doc.buffer.snapshot();
        let primary = view.selection.ranges[view.selection.primary];
        let rel = doc
            .path
            .as_deref()
            .map(|p| p.strip_prefix(&self.root).unwrap_or(p));
        let name = rel
            .and_then(Path::file_name)
            .map_or_else(|| "untitled".into(), |n| n.to_string_lossy().into_owned());
        let dir = rel
            .and_then(Path::parent)
            .map(|d| {
                d.components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default();
        let workspace = self
            .root
            .file_name()
            .map_or_else(|| "/".into(), |n| n.to_string_lossy().into_owned());
        let int = |n: usize| CtxValue::Int(i64::try_from(n).unwrap_or(i64::MAX));
        let (toast, toast_error) = c
            .toast
            .as_ref()
            .map_or((String::new(), false), |t| (t.text.clone(), t.error));
        let pairs: [(&str, CtxValue); 19] = [
            ("buffer.name", name.as_str().into()),
            ("buffer.dir", dir.into()),
            ("buffer.modified", doc.modified().into()),
            ("buffer.language", language(&name).name.into()),
            ("buffer.lines", int(snap.line_count())),
            ("cursor.line", int(motion::line_of(&snap, primary.head) + 1)),
            ("cursor.col", int(motion::char_col(&snap, primary.head) + 1)),
            ("cursor.count", int(view.selection.ranges.len())),
            ("selection.active", (primary.anchor != primary.head).into()),
            ("editor.mode", (&*view.mode).into()),
            ("workspace.name", workspace.into()),
            ("session.name", self.name.as_str().into()),
            ("session.clients", int(attached)),
            ("session.shared", (!self.exit_when_empty).into()),
            ("pack.name", self.pack.name.as_str().into()),
            ("ui.focus", c.focus.name().into()),
            ("ui.files_open", c.files_open.into()),
            ("ui.toast", toast.into()),
            ("ui.toast_error", toast_error.into()),
        ];
        for (k, v) in pairs {
            // Keys are valid names, so `set` cannot fail.
            let _ = c.ctx.set(k, v);
        }
        let _ = c.ctx.set("ui.pending", pretty_pending(&c.keymap));
    }

    fn render_all(&mut self) {
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for id in ids {
            self.sync_context(id);
            self.render(id);
        }
    }

    fn render(&mut self, id: ClientId) {
        let Some(c) = self.clients.get(&id) else {
            return;
        };
        if c.kind != Some(ClientKind::Frontend) {
            self.pong(id);
            return;
        }
        let mode = self.mode(c);
        let env = ClientEnv {
            client: c,
            global: self.core.context(),
            mode: &mode,
        };
        let focused = match c.focus {
            Focus::Editor => None,
            other => Some(other.name()),
        };
        let ui = resolve(&self.pack, &env, focused);
        let surface = self.surface(id);
        let Some(c) = self.clients.get_mut(&id) else {
            return;
        };
        let mut dirty = false;
        if c.last_ui.as_ref() != Some(&ui) {
            c.last_ui = Some(ui.clone());
            dirty |= c.tx.send(ServerMsg::Ui { ui }).is_ok();
        }
        if let Some(frame) = surface
            && c.last_surface.as_ref() != Some(&frame)
        {
            c.last_surface = Some(frame.clone());
            dirty |= c.tx.send(ServerMsg::Surface { frame }).is_ok();
        }
        if dirty {
            c.frame += 1;
            let _ = c.tx.send(ServerMsg::Flush { frame: c.frame });
        }
        self.pong(id);
    }

    fn pong(&mut self, id: ClientId) {
        if let Some(c) = self.clients.get_mut(&id) {
            for ping in c.pings.drain(..) {
                let _ = c.tx.send(ServerMsg::Pong { id: ping });
            }
        }
    }

    /// The visible lines of a client's view, scrolled so its cursor stays in sight.
    fn surface(&mut self, id: ClientId) -> Option<SurfaceFrame> {
        let c = self.clients.get(&id)?;
        let rows = c.surface.rows;
        if rows == 0 {
            return None;
        }
        let view_id = c.view;
        let ed = &mut self.core.state;
        let view = ed.views.get(&view_id)?;
        let doc_id = view.doc;
        let head = view.selection.ranges[view.selection.primary].head;
        ed.focus = view_id;
        let key = (view_id, head, rows);
        if c.scrolled_for != Some(key) {
            ed.scroll_to_cursor(usize::from(rows), SCROLL_MARGIN);
        }
        let vp = ed.viewport(usize::from(rows));
        let doc = &ed.docs[&doc_id];
        let lang = language(
            &doc.path
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy())
                .unwrap_or_default(),
        );
        let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
        let pos = |p: spur_text::Point| Pos {
            line: n(p.line),
            col: n(p.column),
        };
        let lines: Vec<Vec<Span>> = vp
            .lines
            .iter()
            .map(|l| {
                let cut = l.char_indices().nth(MAX_LINE).map_or(l.len(), |(i, _)| i);
                highlight(lang, &l[..cut])
            })
            .collect();
        let mut cursors: Vec<Cursor> = vp
            .cursors
            .iter()
            .map(|(p, primary)| Cursor {
                pos: pos(*p),
                kind: if *primary {
                    CursorKind::Primary
                } else {
                    CursorKind::Secondary
                },
            })
            .collect();
        let snap = doc.buffer.snapshot();
        for (other_id, other) in &self.clients {
            if *other_id == id || other.kind != Some(ClientKind::Frontend) {
                continue;
            }
            let Some(v) = self.core.state.views.get(&other.view) else {
                continue;
            };
            if v.doc == doc_id {
                let head = v.selection.ranges[v.selection.primary].head;
                cursors.push(Cursor {
                    pos: Pos {
                        line: n(motion::line_of(&snap, head)),
                        col: n(motion::char_col(&snap, head)),
                    },
                    kind: CursorKind::Peer,
                });
            }
        }
        let frame = SurfaceFrame {
            top_line: n(vp.top_line),
            line_count: n(vp.line_count),
            lines,
            cursors,
            selections: vp
                .selections
                .iter()
                .map(|(a, b)| (pos(*a), pos(*b)))
                .collect(),
        };
        if let Some(c) = self.clients.get_mut(&id) {
            c.scrolled_for = Some(key);
        }
        Some(frame)
    }
}

/// Commands Ask can run: everything that needs no args, except Ask's and the files
/// panel's own keys.
fn ask_commands(core: &Core<Editor>, keymap: &Keymap) -> Vec<CommandEntry> {
    core.commands()
        .into_iter()
        .filter(|m| {
            let required = m
                .args_schema
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|r| !r.is_empty());
            !required && !matches!(m.id.namespace(), "ask" | "files")
        })
        .map(|m| CommandEntry {
            id: m.id.clone(),
            title: title(&m.id),
            hint: keymap
                .bindings_of(&m.id)
                .first()
                .map(|(_, keys, _)| pretty_keys(keys))
                .unwrap_or_default(),
        })
        .collect()
}

fn pretty_pending(keymap: &Keymap) -> String {
    let pending = keymap.pending_keys();
    if pending.is_empty() {
        String::new()
    } else {
        pretty_keys(&pending)
    }
}
