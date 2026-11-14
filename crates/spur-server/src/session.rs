//! A session: one thread that owns the editor state and serves every attached client.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spur_core::editor::{DocId, Editor, ViewId, register_editor_commands};
use spur_core::{
    Actor, ClientId, CommandError, CommandId, Context, Core, DEFAULT_QUEUE, Delivery, EventName,
    Filter, Invocation, Scoped, Source, Subscription, events,
};
use spur_keymap::{Keymap, Layer};
use spur_rpc::{
    ClientKind, ClientMsg, FrameReader, MAX_FRAME, Node, PROTOCOL_VERSION, QUEUE, Reply, ServerEnd,
    ServerMsg, SurfaceView, WireError, peek_hello, pump_writes,
};

use crate::client::{Client, SendFail, Shutter};
use crate::frame::{self, SURFACE, bare_ui};
use crate::ux::commands::{UI_COMMANDS, ui_id};
use crate::ux::files::{Files, parent_of};
use crate::ux::pack::{Pack, PackError, PackSource};
use crate::ux::state::{self, ScopeInput, UiState};
use crate::ux::{Ux, eval};

/// Lines kept between the cursor and the edge of the surface.
pub const SCROLL_MARGIN: usize = 3;
/// Inbox batches handled before the next render.
const MAX_BATCHES: usize = 64;
/// Batches a client's outbox holds before the client counts as stalled.
const OUTBOX: usize = QUEUE;
/// Largest command result sent; a bigger one is replaced by a `too_large` error.
const MAX_RESULT: usize = MAX_FRAME - 4096;
/// Server-time samples kept for [`Session::stats`].
const MAX_SAMPLES: usize = 100_000;

/// Where a session logs: pack errors, failed `ui.*` commands and caught panics.
#[derive(Clone)]
pub struct LogSink(pub Arc<dyn Fn(&str) + Send + Sync>);

impl fmt::Debug for LogSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LogSink")
    }
}

/// A modified document: path (`None` for untitled) and text.
pub type ModifiedDoc = (Option<PathBuf>, String);
/// Modified documents.
pub type Modified = Vec<ModifiedDoc>;

/// Writes rescue copies of modified documents; called by the session thread if it
/// fails beyond recovery.
#[derive(Clone)]
pub struct RescueSink(pub Arc<RescueFn>);

/// The function a [`RescueSink`] wraps.
pub type RescueFn = dyn Fn(&[ModifiedDoc]) + Send + Sync;

impl fmt::Debug for RescueSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RescueSink")
    }
}

/// How to start a session.
#[derive(Clone, Debug, Default)]
pub struct SessionConfig {
    /// Session name.
    pub name: String,
    /// Files to open; the first one is where new clients start.
    pub open: Vec<PathBuf>,
    /// Keymap layers, bottom first: (name, TOML in the `spur-keymap` layer format).
    pub keymap: Vec<(String, String)>,
    /// The UX pack. Without one the session draws a single editor surface.
    pub pack: Option<PackSource>,
    /// Theme for the whole session, instead of the pack's default.
    pub theme: Option<String>,
    /// The workspace folder; empty means the current folder.
    pub root: PathBuf,
    /// Where the session logs.
    pub log: Option<LogSink>,
    /// Saves modified documents if the session thread fails.
    pub rescue: Option<RescueSink>,
    /// How often a pack folder is checked for changes; `None` for never. An
    /// embedded pack is never watched.
    pub watch: Option<Duration>,
    /// Runs on the core before the session starts (tests register commands here).
    #[doc(hidden)]
    pub setup: Option<fn(&mut Core<Editor>)>,
}

/// Why a session could not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionError {
    /// A keymap layer did not parse.
    Layer {
        /// Layer name.
        name: String,
        /// The parse error.
        message: String,
    },
    /// A file could not be opened.
    Open {
        /// The file.
        path: PathBuf,
        /// Why.
        message: String,
    },
    /// Built-in commands could not be registered.
    Register(String),
    /// The session thread could not start.
    Thread(String),
    /// The UX pack did not load: every problem found.
    Pack(Vec<PackError>),
    /// The pack has no theme of this name.
    Theme(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layer { name, message } => write!(f, "keymap layer {name}: {message}"),
            Self::Open { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Register(m) => write!(f, "cannot register commands: {m}"),
            Self::Thread(m) => write!(f, "cannot start the session thread: {m}"),
            Self::Pack(errors) => {
                f.write_str("the UX pack did not load:")?;
                for e in errors {
                    write!(f, "\n  {e}")?;
                }
                Ok(())
            }
            Self::Theme(name) => write!(f, "the UX pack has no theme {name:?}"),
        }
    }
}

impl std::error::Error for SessionError {}

type Probe = Box<dyn FnOnce(&mut Core<Editor>) + Send>;

/// Messages into the session thread.
enum In {
    Attach {
        conn: u64,
        out: SyncSender<Vec<ServerMsg>>,
        shutter: Option<Shutter>,
    },
    /// The first frame of a stream connection, undecoded, so the session can read its
    /// protocol versions whatever its shape.
    First {
        conn: u64,
        value: Value,
    },
    Msgs {
        conn: u64,
        msgs: Vec<ClientMsg>,
    },
    Gone {
        conn: u64,
    },
    Inspect(Probe),
    Modified(SyncSender<Modified>),
    /// The pack folder's files changed: their paths and text.
    PackFiles(Vec<(String, String)>),
    /// Panics outside the per-message guard (tests of the rescue path).
    Panic,
    Shutdown,
}

/// A running session. Dropping it shuts the session down.
pub struct Session {
    tx: Sender<In>,
    /// Dropping it stops the pack watcher.
    _watch: Option<Sender<()>>,
    next_conn: Arc<AtomicU64>,
    samples: Arc<Mutex<Vec<Duration>>>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

/// The session has ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ended;

impl fmt::Display for Ended {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the session has ended")
    }
}

impl std::error::Error for Ended {}

impl Session {
    /// Starts a session.
    ///
    /// # Errors
    ///
    /// A [`SessionError`] if a layer or the pack does not load, a file cannot be
    /// opened, or the thread cannot start.
    pub fn spawn(mut cfg: SessionConfig) -> Result<Self, SessionError> {
        let root = if cfg.root.as_os_str().is_empty() {
            std::env::current_dir()
        } else {
            std::path::absolute(&cfg.root)
        };
        cfg.root = root.map_err(|e| SessionError::Open {
            path: cfg.root.clone(),
            message: e.to_string(),
        })?;
        let samples = Arc::new(Mutex::new(Vec::new()));
        let watched = match (&cfg.pack, cfg.watch) {
            (Some(PackSource::Dir(dir)), Some(every)) => Some((dir.clone(), every)),
            _ => None,
        };
        let log = cfg.log.clone();
        // Hashed before the pack is loaded: an edit in between is seen as a change.
        let first = watched
            .as_ref()
            .and_then(|(dir, _)| crate::ux::watch::hash(dir));
        let state = State::new(cfg, samples.clone())?;
        let (tx, rx) = mpsc::channel();
        let watch = watched.and_then(|(dir, every)| {
            let (stop_tx, stop_rx) = mpsc::channel::<()>();
            let inbox = tx.clone();
            let spawn_log = log.clone();
            std::thread::Builder::new()
                .name("spur-pack-watch".into())
                .spawn(move || {
                    let log = |l: &str| {
                        if let Some(s) = &log {
                            (s.0)(l);
                        }
                    };
                    crate::ux::watch::watch(&dir, every, first, &stop_rx, &log, &|files| {
                        inbox.send(In::PackFiles(files)).is_ok()
                    });
                })
                .map_err(|e| {
                    if let Some(s) = &spawn_log {
                        (s.0)(&format!("pack watcher not started: {e}"));
                    }
                })
                .ok()
                .map(|_| stop_tx)
        });
        let thread = std::thread::Builder::new()
            .name("spur-session".into())
            .spawn(move || state.run(&rx))
            .map_err(|e| SessionError::Thread(e.to_string()))?;
        Ok(Self {
            tx,
            _watch: watch,
            next_conn: Arc::new(AtomicU64::new(1)),
            samples,
            thread: Some(thread),
        })
    }

    fn conn(&self) -> u64 {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    /// Attaches an in-process client (see [`spur_rpc::in_process`]).
    ///
    /// # Errors
    ///
    /// [`Ended`] if the session has ended.
    pub fn attach(&self, end: ServerEnd) -> Result<(), Ended> {
        let conn = self.conn();
        // The endpoint's own bounded sender is the outbox, so a client that stops
        // reading fills it and is dropped.
        let (out, from_client) = end.split();
        self.tx
            .send(In::Attach {
                conn,
                out,
                shutter: None,
            })
            .map_err(|_| Ended)?;
        let tx = self.tx.clone();
        self.reader(conn, "spur-attach-in", move || {
            for msgs in from_client {
                if tx.send(In::Msgs { conn, msgs }).is_err() {
                    return;
                }
            }
        });
        Ok(())
    }

    /// Starts a connection's reader thread, which reports the connection gone when `f`
    /// returns (or at once if the thread cannot start).
    fn reader(&self, conn: u64, name: &str, f: impl FnOnce() + Send + 'static) {
        let tx = self.tx.clone();
        let started = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                f();
                let _ = tx.send(In::Gone { conn });
            });
        if started.is_err() {
            let _ = self.tx.send(In::Gone { conn });
        }
    }

    /// Attaches a client over a byte stream. `close` shuts the stream down (a socket's
    /// `shutdown`). It runs once: after the last message is written, or on the session
    /// thread when the client stops reading, while writes may be pending. It must not
    /// block.
    ///
    /// # Errors
    ///
    /// [`Ended`] if the session has ended.
    pub fn attach_stream<R, W, F>(&self, reader: R, writer: W, close: F) -> Result<(), Ended>
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
        F: FnOnce() + Send + 'static,
    {
        let conn = self.conn();
        let (out, out_rx) = mpsc::sync_channel::<Vec<ServerMsg>>(OUTBOX);
        let shutter = Shutter::new(close);
        self.tx
            .send(In::Attach {
                conn,
                out,
                shutter: Some(shutter.clone()),
            })
            .map_err(|_| Ended)?;
        let writer_thread = std::thread::Builder::new()
            .name("spur-conn-write".into())
            .spawn(move || {
                pump_writes(&out_rx, writer);
                shutter.close();
            });
        if writer_thread.is_err() {
            let _ = self.tx.send(In::Gone { conn });
            return Ok(());
        }
        let tx = self.tx.clone();
        self.reader(conn, "spur-conn-read", move || {
            read_stream(conn, reader, &tx);
        });
        Ok(())
    }

    /// Attaches a client over a Unix socket, shut down when the client leaves.
    ///
    /// # Errors
    ///
    /// The socket's error if it cannot be cloned, or a `BrokenPipe` error wrapping
    /// [`Ended`] if the session has ended.
    #[cfg(unix)]
    pub fn attach_unix(&self, stream: std::os::unix::net::UnixStream) -> std::io::Result<()> {
        let reader = stream.try_clone()?;
        let closer = stream.try_clone()?;
        self.attach_stream(reader, stream, move || {
            let _ = closer.shutdown(std::net::Shutdown::Both);
        })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    }

    /// Server time per loop iteration that answered a ping: from taking the first
    /// message to handing the last batch to the writers.
    #[doc(hidden)]
    pub fn stats(&self) -> Vec<Duration> {
        self.samples.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Runs `f` on the session thread and returns its result (tests).
    #[doc(hidden)]
    pub fn inspect<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Core<Editor>) -> T + Send + 'static,
    ) -> Option<T> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(In::Inspect(Box::new(move |core| {
                let _ = tx.send(f(core));
            })))
            .ok()?;
        rx.recv().ok()
    }

    /// The modified documents, as the session thread sees them; `None` if it does
    /// not answer within `timeout` (it has ended or is stuck).
    pub fn modified_documents(&self, timeout: Duration) -> Option<Modified> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx.send(In::Modified(tx)).ok()?;
        rx.recv_timeout(timeout).ok()
    }

    /// Makes the session thread panic outside its per-message guard (tests).
    #[doc(hidden)]
    pub fn inject_panic(&self) {
        let _ = self.tx.send(In::Panic);
    }

    /// Shuts the session down and waits for its thread to end. Clients' connections
    /// close without a `bye`. Dropping the session does the same.
    pub fn join(mut self) {
        let _ = self.tx.send(In::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// How long dropping a session waits for its thread before leaving it running.
const DROP_WAIT: Duration = Duration::from_secs(2);

impl Drop for Session {
    /// Shuts the session down and waits for it, at most [`DROP_WAIT`]: a session
    /// stuck in a command must not keep the process from exiting.
    fn drop(&mut self) {
        let _ = self.tx.send(In::Shutdown);
        if let Some(t) = self.thread.take() {
            let end = Instant::now() + DROP_WAIT;
            while !t.is_finished() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(5));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }
}

/// Reads frames and pushes them into the inbox: the first undecoded, the rest in
/// batches of what arrived together.
fn read_stream<R: Read>(conn: u64, reader: R, tx: &Sender<In>) {
    let mut frames = FrameReader::new(reader);
    let Ok(Some(body)) = frames.read_body() else {
        return;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return;
    };
    if tx.send(In::First { conn, value }).is_err() {
        return;
    }
    while let Ok(Some(msgs)) = frames.read_batch::<ClientMsg>() {
        if tx.send(In::Msgs { conn, msgs }).is_err() {
            return;
        }
    }
}

/// What a client is told when the server caught a panic on its behalf.
const PANIC_MESSAGE: &str = "internal error while running a command; save your work to a new file";

fn cmd(id: &str) -> CommandId {
    CommandId::new(id).unwrap_or_else(|e| unreachable!("{e}"))
}

fn ev(name: &str) -> EventName {
    EventName::new(name).unwrap_or_else(|e| unreachable!("{e}"))
}

fn server_actor() -> Actor {
    Actor::System {
        source: "server".into(),
    }
}

fn wire_error(e: &CommandError) -> WireError {
    WireError {
        code: e.code.as_str().to_owned(),
        message: e.message.clone(),
        data: e.data.clone(),
    }
}

/// Everything the session thread owns.
pub(crate) struct State {
    name: String,
    pub(crate) core: Core<Editor>,
    layers: Vec<Layer>,
    /// Connections that have not said hello yet.
    waiting: BTreeMap<u64, (SyncSender<Vec<ServerMsg>>, Option<Shutter>)>,
    pub(crate) clients: BTreeMap<u64, Client>,
    /// The editor's own first view: `focus` rests here between commands.
    pub(crate) anchor: ViewId,
    home: DocId,
    samples: Arc<Mutex<Vec<Duration>>>,
    /// A client was removed since this was last cleared.
    removed: bool,
    /// The UX, when the session has a pack.
    pub(crate) ux: Option<Ux>,
    /// `buffer.saved` events, to read a saved file's folder again.
    saved: Option<Subscription>,
    log: Option<LogSink>,
    rescue: Option<RescueSink>,
}

impl State {
    fn new(cfg: SessionConfig, samples: Arc<Mutex<Vec<Duration>>>) -> Result<Self, SessionError> {
        let mut layers = Vec::new();
        for (name, toml) in &cfg.keymap {
            let source = Source::Extension {
                id: name.as_str().into(),
            };
            let layer = Layer::from_toml(name, source, toml).map_err(|e| SessionError::Layer {
                name: name.clone(),
                message: e.to_string(),
            })?;
            layers.push(layer);
        }
        let mut core = Core::new(Editor::new());
        register_editor_commands(&mut core).map_err(|e| SessionError::Register(e.to_string()))?;
        if let Some(setup) = cfg.setup {
            setup(&mut core);
        }
        let ux = match &cfg.pack {
            Some(src) => Some(load_ux(&core, src, &cfg)?),
            None => None,
        };
        let saved = if ux.is_some() {
            core.bus_mut()
                .subscribe(Filter::new([events::BUFFER_SAVED]), DEFAULT_QUEUE, None)
                .ok()
                .map(|(_, sub)| sub)
        } else {
            None
        };
        let anchor = core.state.focus;
        let mut home = core.state.views[&anchor].doc;
        for (i, path) in cfg.open.iter().enumerate() {
            let inv =
                Invocation::new(cmd("editor.open"), server_actor()).args(json!({ "path": path }));
            let opened = core.execute(inv).map_err(|e| SessionError::Open {
                path: path.clone(),
                message: e.message,
            })?;
            if i == 0 {
                home = DocId(opened["doc"].as_u64().unwrap_or(home.0));
            }
            core.state.focus = anchor;
        }
        Ok(Self {
            name: cfg.name,
            core,
            layers,
            waiting: BTreeMap::new(),
            clients: BTreeMap::new(),
            anchor,
            home,
            samples,
            removed: false,
            ux,
            saved,
            log: cfg.log,
            rescue: cfg.rescue,
        })
    }

    /// Loads changed pack files: on success swaps the pack in for every client; on
    /// failure keeps the old one and shows the first error.
    fn reload(&mut self, files: &[(String, String)]) {
        let Some(ux) = self.ux.as_mut() else {
            return;
        };
        let mut known: BTreeSet<String> = self
            .core
            .commands()
            .iter()
            .map(|m| m.id.to_string())
            .collect();
        known.extend(UI_COMMANDS.iter().map(|s| (*s).to_owned()));
        let loaded = Pack::load(files, &|id| known.contains(id)).and_then(|p| {
            match p.theme_named(ux.theme.as_deref()) {
                Some(_) => Ok(p),
                None => Err(vec![PackError {
                    file: "pack.toml".into(),
                    line: None,
                    path: "theme".into(),
                    message: format!(
                        "no theme {:?}",
                        ux.theme.clone().unwrap_or_else(|| p.theme.clone())
                    ),
                }]),
            }
        });
        match loaded {
            Ok(pack) => {
                // Migrate copies first: a panic here leaves every client on the
                // old pack, and the commit below only moves values.
                let mut next = Vec::new();
                for (id, c) in &self.clients {
                    let Some(ui) = c.ui.as_ref() else {
                        continue;
                    };
                    let (mut ui, mut scope) = (ui.clone(), c.scope.clone());
                    ui.migrate(&pack, &mut ux.files, &mut scope);
                    next.push((*id, ui, scope));
                }
                let old_layer = ux.pack.keys.name.clone();
                ux.pack = pack;
                ux.pack_error = None;
                ux.generation += 1;
                for (id, ui, scope) in next {
                    if let Some(c) = self.clients.get_mut(&id) {
                        c.keymap.remove_layer(&old_layer);
                        c.keymap.push_layer(ux.pack.keys.clone());
                        c.ui = Some(ui);
                        c.scope = scope;
                        c.dirty = true;
                    }
                }
                self.log("pack reloaded");
            }
            Err(errors) => {
                for e in &errors {
                    self.log(&format!("pack: {e}"));
                }
                if let Some(ux) = self.ux.as_mut() {
                    ux.pack_error = errors.first().map(ToString::to_string);
                }
                for c in self.clients.values_mut().filter(|c| c.ui.is_some()) {
                    c.dirty = true;
                }
            }
        }
        let conns: Vec<u64> = self.clients.keys().copied().collect();
        for conn in conns {
            self.check_focus(conn);
        }
    }

    /// Writes a line to the session log, if there is one.
    pub(crate) fn log(&self, line: &str) {
        if let Some(l) = &self.log {
            (l.0)(line);
        }
    }

    /// The surface node id: the pack's, or the bare tree's.
    pub(crate) fn surface_id(&self) -> &str {
        self.ux
            .as_ref()
            .map_or(SURFACE, |u| u.pack.surface.as_str())
    }

    /// Writes the client's context keys: the view keys, then the UI keys.
    pub(crate) fn refresh_client(&mut self, conn: u64) {
        let Some(c) = self.clients.get_mut(&conn) else {
            return;
        };
        c.refresh_scope(&self.core.state);
        let (Some(ux), Some(ui)) = (self.ux.as_ref(), c.ui.as_ref()) else {
            return;
        };
        let x = ScopeInput {
            ed: &self.core.state,
            view: c.active,
            cols: c.size.cols,
            rows: c.size.rows,
            pack: &ux.pack,
            workspace: &ux.workspace,
            pack_error: ux.pack_error.as_deref(),
            global: self.core.context(),
        };
        state::refresh(&mut c.scope, ui, &x);
    }

    /// Reads the folder of each saved file again, so a new file shows in the tree.
    fn drain_saved(&mut self) {
        let (Some(sub), Some(ux)) = (&self.saved, self.ux.as_mut()) else {
            return;
        };
        let before = ux.files.generation;
        while let Some(d) = sub.try_recv() {
            match d {
                Delivery::Event(e) => {
                    if let Some(path) = e.payload.get("path").and_then(Value::as_str)
                        && let Some(rel) = ux.files.rel(Path::new(path))
                    {
                        ux.files.reread(parent_of(&rel));
                    }
                }
                // Some saves were missed: read every folder again.
                Delivery::Lagged { .. } => ux.files.reread_all(),
            }
        }
        if ux.files.generation != before {
            for c in self.clients.values_mut().filter(|c| c.ui.is_some()) {
                c.dirty = true;
            }
        }
    }

    /// Runs the loop; a panic that escapes the per-message guards writes rescue
    /// copies, is logged, and ends the session.
    fn run(mut self, rx: &Receiver<In>) {
        let res = catch_unwind(AssertUnwindSafe(|| self.serve(rx)));
        if res.is_err() {
            self.core.state.focus = self.anchor;
            self.log("the session failed; writing rescue copies of modified documents");
            // Guarded again: the state that panicked may panic once more.
            let docs = catch_unwind(AssertUnwindSafe(|| self.modified())).unwrap_or_default();
            if let Some(r) = &self.rescue
                && !docs.is_empty()
                && catch_unwind(AssertUnwindSafe(|| (r.0)(&docs))).is_err()
            {
                self.log("writing rescue copies failed");
            }
        }
    }

    /// Modified documents, by id.
    fn modified(&self) -> Modified {
        let mut docs: Vec<_> = self
            .core
            .state
            .docs
            .iter()
            .filter(|(_, d)| d.modified())
            .collect();
        docs.sort_by_key(|(id, _)| **id);
        docs.into_iter()
            .map(|(_, d)| {
                let snap = d.buffer.snapshot();
                (
                    d.path.clone(),
                    snap.slice(0..snap.len()).unwrap_or_default(),
                )
            })
            .collect()
    }

    fn serve(&mut self, rx: &Receiver<In>) {
        loop {
            let first = match self.next_deadline() {
                Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => return,
                },
            };
            let started = Instant::now();
            let mut next = first;
            let mut handled = 0;
            while let Some(msg) = next {
                match msg {
                    In::Shutdown => return,
                    In::Panic => panic!("injected session panic"),
                    msg => self.handle_guarded(msg),
                }
                handled += 1;
                next = if handled < MAX_BATCHES {
                    rx.try_recv().ok()
                } else {
                    None
                };
            }
            self.tick(Instant::now());
            // Removing a client changes what its peers show, so render again until no
            // client leaves (each pass removes at least one, so this ends).
            let mut ponged = false;
            loop {
                self.removed = false;
                ponged |= self.render_all();
                self.send_all();
                if !self.removed {
                    break;
                }
            }
            if ponged {
                self.sample(started.elapsed());
            }
        }
    }

    fn sample(&self, d: Duration) {
        if let Ok(mut s) = self.samples.lock()
            && s.len() < MAX_SAMPLES
        {
            s.push(d);
        }
    }

    /// Handles one inbox message, guarding each client message on its own: a panic
    /// is reported to the client that sent it, and the rest of its batch and the
    /// session go on.
    fn handle_guarded(&mut self, msg: In) {
        match msg {
            In::Msgs { conn, msgs } => {
                for m in msgs {
                    self.guarded(Some(conn), |s| s.client_msg(conn, m));
                }
            }
            In::First { conn, .. } | In::Gone { conn } => {
                self.guarded(Some(conn), |s| s.handle(msg));
            }
            msg => self.guarded(None, |s| s.handle(msg)),
        }
    }

    fn guarded(&mut self, conn: Option<u64>, f: impl FnOnce(&mut Self)) {
        if catch_unwind(AssertUnwindSafe(|| f(self))).is_err() {
            self.core.state.focus = self.anchor;
            self.log("caught a panic while handling a client message");
            if let Some(conn) = conn {
                self.error(conn, PANIC_MESSAGE.into());
                self.mark_peers(conn, None);
            }
        }
    }

    /// One message from a connection: its hello, or anything after.
    fn client_msg(&mut self, conn: u64, m: ClientMsg) {
        if self.clients.contains_key(&conn) {
            self.message(conn, m);
        } else if self.waiting.contains_key(&conn) {
            self.hello(conn, m);
        }
    }

    fn handle(&mut self, msg: In) {
        match msg {
            In::Attach { conn, out, shutter } => {
                self.waiting.insert(conn, (out, shutter));
            }
            In::First { conn, value } => self.first(conn, &value),
            In::Msgs { conn, msgs } => {
                for m in msgs {
                    self.client_msg(conn, m);
                }
            }
            In::Gone { conn } => {
                self.waiting.remove(&conn);
                self.leave(conn);
            }
            In::Inspect(f) => f(&mut self.core),
            In::Modified(tx) => {
                let _ = tx.try_send(self.modified());
            }
            In::PackFiles(files) => {
                if catch_unwind(AssertUnwindSafe(|| self.reload(&files))).is_err() {
                    self.log("caught a panic while loading the pack; keeping the old one");
                    if let Some(ux) = self.ux.as_mut() {
                        ux.pack_error = Some("internal error while loading the pack".into());
                    }
                    for c in self.clients.values_mut().filter(|c| c.ui.is_some()) {
                        c.dirty = true;
                    }
                }
            }
            In::Panic | In::Shutdown => {}
        }
    }

    /// Refuses a connection that has not said hello.
    fn refuse(&mut self, conn: u64, reason: String) {
        if let Some((out, _)) = self.waiting.remove(&conn) {
            let _ = out.try_send(vec![ServerMsg::Bye { reason }]);
        }
    }

    fn first(&mut self, conn: u64, value: &Value) {
        let Some(versions) = peek_hello(value) else {
            self.refuse(conn, "the first message must be hello".into());
            return;
        };
        if !self.speaks(conn, &versions) {
            return;
        }
        match serde_json::from_value::<ClientMsg>(value.clone()) {
            Ok(msg) => self.hello(conn, msg),
            Err(e) => self.refuse(conn, format!("bad hello: {e}")),
        }
    }

    fn hello(&mut self, conn: u64, msg: ClientMsg) {
        let ClientMsg::Hello {
            protocols,
            kind,
            name,
            caps,
            size,
        } = msg
        else {
            self.refuse(conn, "the first message must be hello".into());
            return;
        };
        if !self.speaks(conn, &protocols) {
            return;
        }
        let Some((out, shutter)) = self.waiting.remove(&conn) else {
            return;
        };
        let kind = match kind {
            ClientKind::Frontend => ClientKind::Frontend,
            _ => ClientKind::Tool,
        };
        let id = ClientId(conn);
        let view = self.core.state.add_view_for(self.home, Some(id));
        let mut keymap = Keymap::new();
        for layer in &self.layers {
            keymap.push_layer(layer.clone());
        }
        let frontend = kind == ClientKind::Frontend;
        if frontend && let Some(ux) = &self.ux {
            keymap.push_layer(ux.pack.keys.clone());
        }
        let mut client = Client::new(id, kind, name, caps, size, view, keymap, out);
        client.shutter = shutter;
        if frontend && let Some(ux) = self.ux.as_mut() {
            let reveal = self
                .core
                .state
                .docs
                .get(&self.home)
                .and_then(|d| d.path.as_deref())
                .and_then(|p| ux.files.rel(p));
            client.ui = Some(UiState::new(&ux.pack, &mut ux.files, reveal.as_deref()));
        }
        client.batch.push(ServerMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            server: env!("CARGO_PKG_VERSION").into(),
            client: conn,
            session: self.name.clone(),
            features: Vec::new(),
        });
        self.core
            .state
            .scroll_view(view, client.rows(), SCROLL_MARGIN);
        let payload = json!({ "client": conn, "kind": kind, "name": client.name });
        self.clients.insert(conn, client);
        self.core
            .bus_mut()
            .publish(ev(events::CLIENT_ATTACHED), server_actor(), payload);
        self.mark_peers(conn, None);
    }

    /// True if the client speaks this server's protocol; refuses it otherwise.
    fn speaks(&mut self, conn: u64, versions: &[u32]) -> bool {
        if versions.contains(&PROTOCOL_VERSION) {
            return true;
        }
        self.refuse(
            conn,
            format!(
                "no common protocol: client speaks {versions:?}, server speaks [{PROTOCOL_VERSION}]"
            ),
        );
        false
    }

    /// Removes a client and every view it owns.
    fn leave(&mut self, conn: u64) {
        let Some(client) = self.clients.remove(&conn) else {
            return;
        };
        self.removed = true;
        let doc = self.core.state.views.get(&client.active).map(|v| v.doc);
        let owned: Vec<ViewId> = self
            .core
            .state
            .views
            .iter()
            .filter(|(_, v)| v.owner == Some(client.id))
            .map(|(id, _)| *id)
            .collect();
        for view in owned {
            if self.core.state.remove_view(view) {
                self.core.bus_mut().publish(
                    ev(events::VIEW_CLOSED),
                    server_actor(),
                    json!({ "view": view.0 }),
                );
            }
        }
        self.core.bus_mut().publish(
            ev(events::CLIENT_DETACHED),
            server_actor(),
            json!({ "client": conn, "kind": client.kind, "name": client.name }),
        );
        if let Some(doc) = doc {
            self.mark_doc(doc, conn);
        }
    }

    fn message(&mut self, conn: u64, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello { .. } => {
                if let Some(c) = self.clients.get_mut(&conn) {
                    c.batch.push(ServerMsg::Bye {
                        reason: "hello was already sent".into(),
                    });
                }
                self.drop_client(conn);
            }
            ClientMsg::Key { key } => self.key(conn, &key),
            ClientMsg::Paste { text } => self.paste(conn, &text),
            ClientMsg::Mouse {
                node,
                action,
                line,
                col,
                lines,
                item,
            } => self.mouse(conn, &node, action, (line, col, lines), item.as_deref()),
            ClientMsg::Resize { size } => self.resize(conn, size),
            ClientMsg::Layout { surfaces } => self.layout(conn, &surfaces),
            ClientMsg::Command { req, id, args } => self.command(conn, req, &id, args),
            ClientMsg::Subscribe { filters } => self.subscribe(conn, filters),
            ClientMsg::Ack { frame } => {
                if let Some(c) = self.clients.get_mut(&conn) {
                    c.on_ack(frame);
                }
            }
            ClientMsg::Ping { id } => {
                if let Some(c) = self.clients.get_mut(&conn) {
                    c.ping(id);
                }
            }
            ClientMsg::Refresh => {
                if let Some(c) = self.clients.get_mut(&conn) {
                    c.last_view = None;
                    c.last_ui = None;
                    if let Some(ui) = c.ui.as_mut() {
                        ui.sent_theme = None;
                        ui.sent_ui = None;
                        ui.sent_lists.clear();
                    }
                    c.dirty = true;
                }
            }
            ClientMsg::Bye => self.drop_client(conn),
            ClientMsg::Unknown => {}
        }
    }

    /// Sends what is queued for the client, then removes it.
    pub(crate) fn drop_client(&mut self, conn: u64) {
        if let Some(c) = self.clients.get_mut(&conn)
            && let Err(SendFail::Full) = c.send_batch()
            && let Some(s) = &c.shutter
        {
            // Its writer may be stuck on a peer that stopped reading.
            s.close();
        }
        self.leave(conn);
    }

    fn command(&mut self, conn: u64, req: u64, id: &str, args: Value) {
        let reply = match CommandId::new(id) {
            Err(e) => Reply::Error(WireError {
                code: "unknown_command".into(),
                message: format!("{id:?} is not a command id: {e}"),
                data: None,
            }),
            // `run_as` replaces the actor with the client's.
            Ok(id) => match self.run_as(conn, Invocation::new(id, Actor::User).args(args)) {
                Some(Ok(v)) => Reply::Ok(v),
                Some(Err(e)) => Reply::Error(wire_error(&e)),
                None => Reply::Error(WireError {
                    code: "internal".into(),
                    message: PANIC_MESSAGE.into(),
                    data: None,
                }),
            },
        };
        // Stream clients have a frame limit; in-process ones do not.
        let streamed = self.clients.get(&conn).is_some_and(|c| c.shutter.is_some());
        let reply = match streamed.then(|| serde_json::to_vec(&reply).map(|b| b.len())) {
            Some(Ok(len)) if len > MAX_RESULT => Reply::Error(WireError {
                code: "too_large".into(),
                message: format!("the result is over {MAX_RESULT} bytes"),
                data: None,
            }),
            Some(Err(e)) => Reply::Error(WireError {
                code: "internal".into(),
                message: format!("the result does not encode: {e}"),
                data: None,
            }),
            _ => reply,
        };
        if let Some(c) = self.clients.get_mut(&conn) {
            c.batch.push(ServerMsg::CommandResult { req, reply });
        }
    }

    fn subscribe(&mut self, conn: u64, filters: Vec<String>) {
        let sub = if filters.is_empty() {
            // `Filter::new([])` would match every event.
            None
        } else {
            match self
                .core
                .bus_mut()
                .subscribe(Filter::new(filters), DEFAULT_QUEUE, None)
            {
                Ok((_, s)) => Some(s),
                Err(e) => {
                    self.error(conn, format!("bad subscribe: {e}"));
                    None
                }
            }
        };
        if let Some(c) = self.clients.get_mut(&conn) {
            c.sub = sub;
        }
    }

    /// Runs `inv` as the client: in its view, with its actor and id. `None` if the
    /// client is gone or the command panicked (the client is told).
    pub(crate) fn run_as(
        &mut self,
        conn: u64,
        mut inv: Invocation,
    ) -> Option<Result<Value, CommandError>> {
        // `ui.*` runs outside the bookkeeping below: it may call `run_as` itself
        // (activating a file runs `editor.open`), and that view change must stand.
        if let Some(id) = ui_id(&inv) {
            self.clients.get(&conn)?;
            let id = id.to_string();
            let args = inv.args.clone();
            let res = catch_unwind(AssertUnwindSafe(|| self.ui_command(conn, &id, &args)));
            if let Ok(r) = res {
                if let Err(e) = &r {
                    self.log(&format!("{id}: {}", e.message));
                }
                return Some(r);
            }
            self.core.state.focus = self.anchor;
            self.log(&format!("caught a panic in {id}"));
            self.error(conn, PANIC_MESSAGE.into());
            return None;
        }
        let client = self.clients.get(&conn)?;
        let (id, view, rows) = (client.id, client.active, client.rows());
        inv.actor = client.actor.clone();
        inv.client = Some(id);
        let old_doc = self.core.state.views.get(&view).map(|v| v.doc);
        self.core.state.focus = view;
        let res = catch_unwind(AssertUnwindSafe(|| self.core.execute(inv)));
        let focus = std::mem::replace(&mut self.core.state.focus, self.anchor);
        // A command may move the client to another view it owns (`editor.open`).
        let mut active = view;
        if focus != view
            && self
                .core
                .state
                .views
                .get(&focus)
                .is_some_and(|v| v.owner == Some(id))
        {
            active = focus;
        }
        if !self.core.state.views.contains_key(&active) {
            active = self.core.state.add_view_for(self.home, Some(id));
        }
        self.core.state.scroll_view(active, rows, SCROLL_MARGIN);
        if let Some(c) = self.clients.get_mut(&conn) {
            c.active = active;
        }
        self.mark_peers(conn, old_doc);
        if let Ok(r) = res {
            return Some(r);
        }
        self.error(conn, PANIC_MESSAGE.into());
        None
    }

    /// Marks the client dirty, and every frontend showing its document (or `also`).
    pub(crate) fn mark_peers(&mut self, conn: u64, also: Option<DocId>) {
        let Some(c) = self.clients.get_mut(&conn) else {
            return;
        };
        c.dirty = true;
        let view = c.active;
        let doc = self.core.state.views.get(&view).map(|v| v.doc);
        for d in [doc, also].into_iter().flatten() {
            self.mark_doc(d, conn);
        }
    }

    fn mark_doc(&mut self, doc: DocId, except: u64) {
        for (k, c) in &mut self.clients {
            if *k != except && self.core.state.views.get(&c.active).map(|v| v.doc) == Some(doc) {
                c.dirty = true;
            }
        }
    }

    /// Delivers events, renders dirty frontends, answers pings. True if a pong went out.
    fn render_all(&mut self) -> bool {
        self.drain_saved();
        for c in self.clients.values_mut() {
            if c.draws() && !c.dirty && shows_stale(&self.core.state, c) {
                // Text changed under the client by someone else (a tool editing by id).
                c.dirty = true;
            }
        }
        if self.ux.is_some() {
            let dirty: Vec<u64> = self
                .clients
                .iter()
                .filter(|(_, c)| c.dirty && c.ui.is_some())
                .map(|(k, _)| *k)
                .collect();
            // The scope is evaluated next, so a frame never shows focus on a hidden
            // region.
            for conn in dirty {
                self.check_focus(conn);
            }
        }
        let peers: Vec<(u64, ViewId)> = self
            .clients
            .values()
            .filter(|c| c.draws())
            .map(|c| (c.id.0, c.active))
            .collect();
        let mut ponged = false;
        let mut broken = Vec::new();
        let ed = &self.core.state;
        let ctx = self.core.context();
        for (conn, c) in &mut self.clients {
            drain_events(c);
            if c.draws() && c.dirty && c.can_send() {
                let others: Vec<(u64, ViewId)> =
                    peers.iter().copied().filter(|(p, _)| p != conn).collect();
                let ux = &mut self.ux;
                let built = catch_unwind(AssertUnwindSafe(|| match ux {
                    Some(ux) if c.ui.is_some() => render_ux(c, ux, ed, ctx, &others),
                    _ => render_bare(c, ed, &others),
                }));
                if built.is_err() {
                    broken.push(*conn);
                }
            }
            let waiting_frame = c.draws() && c.dirty;
            if !waiting_frame {
                ponged |= c.pong();
            }
        }
        for conn in broken {
            self.log("caught a panic while drawing");
            if let Some(c) = self.clients.get_mut(&conn) {
                c.batch.push(ServerMsg::Bye {
                    reason: "internal error while drawing".into(),
                });
            }
            self.drop_client(conn);
        }
        ponged
    }

    fn send_all(&mut self) {
        let mut stalled = Vec::new();
        for (conn, c) in &mut self.clients {
            match c.send_batch() {
                Ok(()) => {}
                // A full outbox cannot carry a `bye`; the client is just dropped.
                Err(SendFail::Full | SendFail::Gone) => stalled.push(*conn),
            }
        }
        for conn in stalled {
            // A stream writer may be stuck writing to a peer that stopped reading.
            if let Some(s) = self.clients.get(&conn).and_then(|c| c.shutter.clone()) {
                s.close();
            }
            self.leave(conn);
        }
    }
}

/// The client's last frame shows another document or an older version of it.
fn shows_stale(ed: &Editor, c: &Client) -> bool {
    let Some(last) = &c.last_view else {
        return false;
    };
    let Some(doc) = ed
        .views
        .get(&c.active)
        .and_then(|v| ed.docs.get(&v.doc).map(|d| (v.doc, d)))
    else {
        return false;
    };
    last.doc != doc.0.0 || last.version != doc.1.buffer.version().0
}

fn drain_events(c: &mut Client) {
    let Some(sub) = &c.sub else {
        return;
    };
    while let Some(d) = sub.try_recv() {
        c.batch.push(match d {
            Delivery::Event(e) => ServerMsg::Event {
                seq: e.seq,
                name: e.name.to_string(),
                actor: serde_json::to_value(&e.actor).unwrap_or(Value::Null),
                payload: e.payload.clone(),
            },
            Delivery::Lagged {
                missed,
                first_missed_seq,
            } => ServerMsg::Lagged {
                missed,
                first_missed_seq,
            },
        });
    }
}

/// Loads the session's pack and checks its theme.
fn load_ux(core: &Core<Editor>, src: &PackSource, cfg: &SessionConfig) -> Result<Ux, SessionError> {
    let mut known: BTreeSet<String> = core.commands().iter().map(|m| m.id.to_string()).collect();
    known.extend(UI_COMMANDS.iter().map(|s| (*s).to_owned()));
    let pack = Pack::from_source(src, &|id| known.contains(id)).map_err(SessionError::Pack)?;
    if pack.theme_named(cfg.theme.as_deref()).is_none() {
        let name = cfg.theme.clone().unwrap_or_else(|| pack.theme.clone());
        return Err(SessionError::Theme(name));
    }
    let workspace = cfg.root.file_name().map_or_else(
        || cfg.root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    Ok(Ux {
        pack,
        theme: cfg.theme.clone(),
        files: Files::new(cfg.root.clone()),
        workspace,
        pack_error: None,
        generation: 1,
    })
}

/// True if `id` is `n` or one of its descendants.
pub(crate) fn has_node(n: &Node, id: &str) -> bool {
    n.id == id || n.children.iter().any(|c| has_node(c, id))
}

/// Renders a client of a session without a pack: one surface filling its area.
fn render_bare(c: &mut Client, ed: &Editor, others: &[(u64, ViewId)]) {
    let view = frame::surface(ed, c.active, c.rows(), others);
    let ui = bare_ui();
    let new_ui = c.last_ui.as_ref() != Some(&ui);
    // A new tree may have new surfaces: start them from a full frame.
    let base = if new_ui { None } else { c.last_view.as_ref() };
    let wire = (base != Some(&view)).then(|| frame::delta(base, &view));
    let mut msgs = Vec::new();
    if new_ui {
        msgs.push(ServerMsg::Ui {
            root: ui.clone(),
            focus: Some(SURFACE.into()),
        });
        c.last_ui = Some(ui);
    }
    finish(c, msgs, SURFACE, Some((view, wire)));
}

/// Renders a client with UI state: theme, tree, lists and surface, each when changed.
fn render_ux(c: &mut Client, ux: &mut Ux, ed: &Editor, ctx: &Context, others: &[(u64, ViewId)]) {
    let built = eval::tree(&ux.pack, &Scoped(&[ctx, &c.scope]));
    let (rows, hint) = (c.rows(), c.rows_hint());
    let Some(ui) = c.ui.as_mut() else {
        return;
    };
    let mut msgs = Vec::new();
    if ui.sent_theme != Some(ux.generation) {
        if let Some((name, theme)) = ux.theme() {
            msgs.push(ServerMsg::Theme {
                name: name.to_owned(),
                dark: theme.dark,
                tokens: eval::theme_tokens(theme),
            });
        }
        ui.sent_theme = Some(ux.generation);
    }
    let surface_id = ux.pack.surface.as_str();
    let focus = ux
        .pack
        .regions
        .get(&ui.focus)
        .map_or_else(|| surface_id.to_owned(), |r| r.node.clone());
    let surface_was_shown = ui
        .sent_ui
        .as_ref()
        .is_some_and(|(n, _)| has_node(n, surface_id));
    let surface_shown = has_node(&built.root, surface_id);
    let tree = (built.root, focus);
    if ui.sent_ui.as_ref() != Some(&tree) {
        msgs.push(ServerMsg::Ui {
            root: tree.0.clone(),
            focus: Some(tree.1.clone()),
        });
        ui.sent_ui = Some(tree);
    }
    // A list that left the tree is sent again in full when it comes back.
    ui.sent_lists.retain(|k, _| built.lists.contains(k));
    for id in &built.lists {
        let height = ui.list_rows.get(id).copied().unwrap_or(hint);
        let Some(t) = ui.trees.get_mut(id) else {
            continue;
        };
        let window = t.window(&mut ux.files, height, &ux.pack);
        if ui.sent_lists.get(id) != Some(&window) {
            msgs.push(ServerMsg::List {
                node: id.clone(),
                list: window.clone(),
            });
            ui.sent_lists.insert(id.clone(), window);
        }
    }
    let surface = surface_shown.then(|| {
        let view = frame::surface(ed, c.active, rows, others);
        // A surface new in the tree starts from a full frame, and so does a new
        // path (a save under another name may mean another language, so no row
        // coloured for the old one is copied).
        let base = if surface_was_shown {
            c.last_view.as_ref().filter(|b| b.path == view.path)
        } else {
            None
        };
        // `last_view` keeps plain text; only the rows sent are coloured.
        let wire = (base != Some(&view)).then(|| {
            let mut wire = frame::delta(base, &view);
            let path = ed
                .views
                .get(&c.active)
                .and_then(|v| ed.docs.get(&v.doc))
                .and_then(|d| d.path.as_deref());
            crate::highlight::apply(crate::lang::of_path(path), &mut wire.lines);
            wire
        });
        (view, wire)
    });
    finish(c, msgs, surface_id, surface);
}

/// Queues `msgs`, then the surface as sent if it changed, then a flush if anything was
/// queued.
fn finish(
    c: &mut Client,
    msgs: Vec<ServerMsg>,
    node: &str,
    surface: Option<(SurfaceView, Option<SurfaceView>)>,
) {
    c.dirty = false;
    let mut changed = !msgs.is_empty();
    c.batch.extend(msgs);
    if let Some((view, Some(mut wire))) = surface {
        changed = true;
        c.surface_seq += 1;
        wire.base = (!wire.rows.is_empty()).then_some(c.surface_seq - 1);
        wire.seq = c.surface_seq;
        // The client builds this frame from the last surface it got, which is
        // `last_view`: every surface queued is delivered in order, or the client is
        // dropped (a writer that cannot encode a message closes the connection).
        c.batch.push(ServerMsg::Surface {
            node: node.into(),
            view: wire,
        });
        c.last_view = Some(view);
    }
    if changed {
        let frame = c.next_frame();
        c.batch.push(ServerMsg::Flush { frame });
    }
}
