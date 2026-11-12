//! A session: one thread that owns the editor state and serves every attached client.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spur_core::editor::{DocId, Editor, ViewId, register_editor_commands};
use spur_core::{
    Actor, ClientId, CommandError, CommandId, Core, DEFAULT_QUEUE, Delivery, EventName, Filter,
    Invocation, Source, events,
};
use spur_keymap::{Keymap, Layer};
use spur_rpc::{
    ClientKind, ClientMsg, FrameReader, MAX_FRAME, Node, PROTOCOL_VERSION, QUEUE, Reply, ServerEnd,
    ServerMsg, SurfaceView, WireError, peek_hello, pump_writes,
};

use crate::client::{Client, SendFail, Shutter};
use crate::frame::{self, bare_ui};

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

/// How to start a session.
#[derive(Clone, Debug, Default)]
pub struct SessionConfig {
    /// Session name.
    pub name: String,
    /// Files to open; the first one is where new clients start.
    pub open: Vec<PathBuf>,
    /// Keymap layers, bottom first: (name, TOML in the `spur-keymap` layer format).
    pub keymap: Vec<(String, String)>,
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
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layer { name, message } => write!(f, "keymap layer {name}: {message}"),
            Self::Open { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Register(m) => write!(f, "cannot register commands: {m}"),
            Self::Thread(m) => write!(f, "cannot start the session thread: {m}"),
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
    Shutdown,
}

/// A running session. Dropping it shuts the session down.
pub struct Session {
    tx: Sender<In>,
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
    /// A [`SessionError`] if a layer does not parse, a file cannot be opened, or the
    /// thread cannot start.
    pub fn spawn(cfg: SessionConfig) -> Result<Self, SessionError> {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let state = State::new(cfg, samples.clone())?;
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("spur-session".into())
            .spawn(move || state.run(&rx))
            .map_err(|e| SessionError::Thread(e.to_string()))?;
        Ok(Self {
            tx,
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

    /// Shuts the session down and waits for its thread to end. Clients' connections
    /// close without a `bye`. Dropping the session does the same.
    pub fn join(mut self) {
        let _ = self.tx.send(In::Shutdown);
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
        })
    }

    fn run(mut self, rx: &Receiver<In>) {
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
            In::Shutdown => {}
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
        let mut client = Client::new(id, kind, name, caps, size, view, keymap, out);
        client.shutter = shutter;
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
            } => self.mouse(conn, &node, action, line, col, lines),
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
                    c.dirty = true;
                }
            }
            ClientMsg::Bye => self.drop_client(conn),
            ClientMsg::Unknown => {}
        }
    }

    /// Sends what is queued for the client, then removes it.
    fn drop_client(&mut self, conn: u64) {
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
        let peers: Vec<(u64, ViewId)> = self
            .clients
            .values()
            .filter(|c| c.draws())
            .map(|c| (c.id.0, c.active))
            .collect();
        let mut ponged = false;
        let mut broken = Vec::new();
        for (conn, c) in &mut self.clients {
            drain_events(c);
            if c.draws() && !c.dirty && shows_stale(&self.core.state, c) {
                // Text changed under the client by someone else (a tool editing by id).
                c.dirty = true;
            }
            if c.draws() && c.dirty && c.can_send() {
                let others: Vec<(u64, ViewId)> =
                    peers.iter().copied().filter(|(p, _)| p != conn).collect();
                let ed = &self.core.state;
                let built = catch_unwind(AssertUnwindSafe(|| {
                    let view = frame::surface(ed, c.active, c.rows(), &others);
                    // A new tree may have new surfaces: start them from a full frame.
                    let ui = bare_ui();
                    let new_ui = c.last_ui.as_ref() != Some(&ui);
                    let base = if new_ui { None } else { c.last_view.as_ref() };
                    let wire = (base != Some(&view)).then(|| frame::delta(base, &view));
                    (view, new_ui.then_some(ui), wire)
                }));
                match built {
                    Ok((view, ui, wire)) => render(c, view, ui, wire),
                    Err(_) => broken.push(*conn),
                }
            }
            let waiting_frame = c.draws() && c.dirty;
            if !waiting_frame {
                ponged |= c.pong();
            }
        }
        for conn in broken {
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

/// Queues what changed for a dirty client (a new tree, the surface as sent), then a
/// flush if anything did.
fn render(c: &mut Client, view: SurfaceView, ui: Option<Node>, wire: Option<SurfaceView>) {
    c.dirty = false;
    let changed = ui.is_some() || wire.is_some();
    if let Some(ui) = ui {
        c.batch.push(ServerMsg::Ui {
            root: ui.clone(),
            focus: Some(frame::SURFACE.into()),
        });
        c.last_ui = Some(ui);
    }
    if let Some(wire) = wire {
        // The client builds this frame from the last surface it got, which is
        // `last_view`: every surface queued is delivered in order, or the client is
        // dropped (a writer that cannot encode a message closes the connection).
        c.batch.push(ServerMsg::Surface {
            node: frame::SURFACE.into(),
            view: wire,
        });
        c.last_view = Some(view);
    }
    if changed {
        let frame = c.next_frame();
        c.batch.push(ServerMsg::Flush { frame });
    }
}
