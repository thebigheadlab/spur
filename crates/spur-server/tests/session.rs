//! End-to-end tests: real sessions, clients speaking the protocol.

use std::time::Duration;

use serde_json::{Value, json};
use spur_core::editor::Editor;
use spur_core::{Actor, CommandId, CommandMeta, Core};
use spur_rpc::{
    Caps, ClientEnd, ClientKind, ClientMsg, CursorKind, MouseAction, Reply, ServerMsg, Size,
    SurfaceView, in_process,
};
use spur_server::{Session, SessionConfig};

const KEYS: &str = r#"
[mode.insert]
fallback = "editor.insert_text"

[[bind]]
mode = "insert normal"
keys = "<Left>"
command = "editor.move_left"

[[bind]]
mode = "insert normal"
keys = "<Right>"
command = "editor.move_right"

[[bind]]
mode = "insert normal"
keys = "<Down>"
command = "editor.move_down"

[[bind]]
mode = "insert normal"
keys = "<Up>"
command = "editor.move_up"

[[bind]]
mode = "insert"
keys = "<CR>"
command = "editor.insert_newline"

[[bind]]
mode = "insert"
keys = "<BS>"
command = "editor.delete_backward"

[[bind]]
mode = "insert normal"
keys = "<C-z>"
command = "editor.undo"

[[bind]]
mode = "insert normal"
keys = "<C-y>"
command = "editor.redo"

[[bind]]
mode = "insert"
keys = "<Esc>"
command = "editor.set_mode"
args = { mode = "normal" }

[[bind]]
mode = "normal"
keys = "i"
command = "editor.set_mode"
args = { mode = "insert" }

[[bind]]
mode = "normal"
keys = "x"
command = "editor.delete_forward"
when = "client.kind == frontend"
"#;

const WAIT: Duration = Duration::from_secs(10);

fn temp_file(name: &str, text: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("spur-server-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("notes.txt");
    std::fs::write(&path, text).unwrap();
    path
}

fn session(name: &str, text: &str) -> Session {
    Session::spawn(SessionConfig {
        name: "test".into(),
        open: vec![temp_file(name, text)],
        keymap: vec![("test".into(), KEYS.into())],
        setup: Some(setup),
        ..Default::default()
    })
    .unwrap()
}

fn setup(core: &mut Core<Editor>) {
    let meta = CommandMeta::new(CommandId::new("test.panic").unwrap(), "Panics.");
    core.register_fn(meta, |_, _| panic!("test panic")).unwrap();
}

/// A protocol client that keeps the latest surface and acks every flush.
struct Client {
    end: ClientEnd,
    view: Option<SurfaceView>,
    log: Vec<ServerMsg>,
    ack: bool,
    ping: u64,
    id: u64,
}

impl Client {
    fn attach(s: &Session, kind: ClientKind) -> Self {
        let (end, server) = in_process();
        s.attach(server).unwrap();
        let mut c = Self::raw(end);
        c.hello(kind);
        c
    }

    fn raw(end: ClientEnd) -> Self {
        Self {
            end,
            view: None,
            log: Vec::new(),
            ack: true,
            ping: 0,
            id: 0,
        }
    }

    fn hello(&mut self, kind: ClientKind) {
        self.send(ClientMsg::Hello {
            protocols: vec![0],
            kind,
            name: "t".into(),
            caps: Caps::default(),
            size: Size { cols: 80, rows: 10 },
        });
        self.sync();
        let Some(ServerMsg::Welcome { client, .. }) = self.log.first() else {
            panic!("no welcome: {:?}", self.log);
        };
        self.id = *client;
    }

    fn send(&self, m: ClientMsg) {
        self.end.send(m).unwrap();
    }

    fn take(&mut self, m: ServerMsg) {
        match &m {
            ServerMsg::Surface { view, .. } => {
                let mut full = view.clone();
                full.resolve(self.view.as_ref())
                    .unwrap_or_else(|e| panic!("{e}: {view:?}"));
                self.view = Some(full);
            }
            ServerMsg::Flush { frame } if self.ack => {
                let _ = self.end.send(ClientMsg::Ack { frame: *frame });
            }
            _ => {}
        }
        self.log.push(m);
    }

    /// Sends a ping and reads until its pong; returns the messages before it.
    fn sync(&mut self) -> Vec<ServerMsg> {
        self.ping += 1;
        let id = self.ping;
        self.send(ClientMsg::Ping { id });
        let start = self.log.len();
        loop {
            match self.end.recv_timeout(WAIT) {
                Ok(Some(ServerMsg::Pong { id: got, .. })) if got == id => break,
                Ok(Some(m)) => self.take(m),
                other => panic!("no pong: {other:?} after {:?}", &self.log[start..]),
            }
        }
        self.log[start..].to_vec()
    }

    fn keys(&mut self, keys: &str) {
        self.send(ClientMsg::Key { key: keys.into() });
        self.sync();
    }

    fn text(&self) -> String {
        let v = self.view.as_ref().unwrap();
        v.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn cursor(&self, kind: CursorKind) -> Option<(u32, u32, Option<u64>)> {
        self.view
            .as_ref()?
            .cursors
            .iter()
            .find(|c| c.kind == kind)
            .map(|c| (c.line, c.col, c.peer))
    }

    fn command(&mut self, id: &str, args: Value) -> Reply {
        self.send(ClientMsg::Command {
            req: 77,
            id: id.into(),
            args,
        });
        let got = self.sync();
        got.into_iter()
            .find_map(|m| match m {
                ServerMsg::CommandResult { req: 77, reply } => Some(reply),
                _ => None,
            })
            .expect("a result")
    }
}

#[test]
fn handshake_sends_welcome_then_the_first_frame() {
    let s = session("hello", "one\ntwo\n");
    let c = Client::attach(&s, ClientKind::Frontend);
    let kinds: Vec<&str> = c
        .log
        .iter()
        .map(|m| match m {
            ServerMsg::Welcome { .. } => "welcome",
            ServerMsg::Ui { .. } => "ui",
            ServerMsg::Surface { .. } => "surface",
            ServerMsg::Flush { .. } => "flush",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["welcome", "ui", "surface", "flush"]);
    let ServerMsg::Welcome {
        protocol, session, ..
    } = &c.log[0]
    else {
        unreachable!()
    };
    assert_eq!((*protocol, session.as_str()), (0, "test"));
    assert_eq!(c.text(), "one\ntwo\n");
    let v = c.view.as_ref().unwrap();
    assert_eq!(
        v.path.as_deref().map(|p| p.ends_with("notes.txt")),
        Some(true)
    );
    assert_eq!(v.mode, "insert");
    assert_eq!(c.cursor(CursorKind::Primary), Some((0, 0, None)));
}

#[test]
fn messages_before_hello_or_a_second_hello_are_refused() {
    let s = session("before", "");
    let (end, server) = in_process();
    s.attach(server).unwrap();
    let mut c = Client::raw(end);
    c.send(ClientMsg::Key { key: "a".into() });
    let Ok(Some(ServerMsg::Bye { reason })) = c.end.recv_timeout(WAIT) else {
        panic!("expected bye");
    };
    assert!(reason.contains("hello"), "{reason}");

    let mut c = Client::attach(&s, ClientKind::Frontend);
    c.send(ClientMsg::Hello {
        protocols: vec![0],
        kind: ClientKind::Frontend,
        name: String::new(),
        caps: Caps::default(),
        size: Size::default(),
    });
    let mut got = None;
    while let Ok(Some(m)) = c.end.recv_timeout(WAIT) {
        if let ServerMsg::Bye { reason } = m {
            got = Some(reason);
            break;
        }
    }
    assert!(got.unwrap().contains("already"));
}

#[test]
fn typing_shows_up_in_the_next_frame_and_pong_follows_it() {
    let s = session("typing", "");
    let mut c = Client::attach(&s, ClientKind::Frontend);
    c.send(ClientMsg::Key { key: "h".into() });
    c.send(ClientMsg::Key { key: "i".into() });
    let got = c.sync();
    assert!(
        matches!(got.last(), Some(ServerMsg::Flush { .. })),
        "{got:?}"
    );
    assert_eq!(c.text(), "hi");
    assert_eq!(c.cursor(CursorKind::Primary), Some((0, 2, None)));
    assert!(c.view.as_ref().unwrap().modified);
    // Nothing changed: a ping gets a pong and no frame.
    assert!(c.sync().is_empty());
    // A paste is one edit and never read as keys.
    c.send(ClientMsg::Paste {
        text: "<Esc>x".into(),
    });
    c.sync();
    assert_eq!(c.text(), "hi<Esc>x");
    c.keys("<C-z>");
    assert_eq!(c.text(), "hi");
}

#[test]
fn two_clients_see_each_other() {
    let s = session("two", "\n\n\n\n");
    let mut a = Client::attach(&s, ClientKind::Frontend);
    let mut b = Client::attach(&s, ClientKind::Frontend);
    b.keys("<Down>");
    b.keys("<Down>");
    a.sync();
    // A sees B's cursor on line 2 without B typing anything.
    assert_eq!(a.cursor(CursorKind::Peer), Some((2, 0, Some(b.id))));
    for (wa, wb) in ["he", "llo"].iter().zip(["wor", "ld"]) {
        for ch in wa.chars() {
            a.keys(&ch.to_string());
        }
        for ch in wb.chars() {
            b.keys(&ch.to_string());
        }
    }
    a.sync();
    b.sync();
    assert_eq!(a.text(), "hello\n\nworld\n\n");
    assert_eq!(b.text(), a.text());
    assert_eq!(a.cursor(CursorKind::Primary), Some((0, 5, None)));
    assert_eq!(a.cursor(CursorKind::Peer), Some((2, 5, Some(b.id))));
    assert_eq!(b.cursor(CursorKind::Primary), Some((2, 5, None)));
    assert_eq!(b.cursor(CursorKind::Peer), Some((0, 5, Some(a.id))));

    // A newline from A above B moves B's cursor down with its text.
    a.keys("<CR>");
    b.sync();
    assert_eq!(b.cursor(CursorKind::Primary), Some((3, 5, None)));

    // Undo from A takes A's newline and leaves B's cursor on its text.
    a.keys("<C-z>");
    b.sync();
    assert_eq!(b.text(), "hello\n\nworld\n\n");
    assert_eq!(b.cursor(CursorKind::Primary), Some((2, 5, None)));
    // Undo is per window: A's next undo takes A's own "llo", not B's newer "ld".
    a.keys("<C-z>");
    assert_eq!(a.text(), "he\n\nworld\n\n");
    assert_eq!(a.cursor(CursorKind::Primary), Some((0, 2, None)));
    // B's undo takes B's "ld"; A's redo brings back "llo".
    b.keys("<C-z>");
    a.keys("<C-y>");
    b.sync();
    assert_eq!(b.text(), "hello\n\nwor\n\n");
}

#[test]
fn modes_and_when_clauses_are_per_client() {
    let s = session("modes", "abc\n");
    let mut a = Client::attach(&s, ClientKind::Frontend);
    let mut b = Client::attach(&s, ClientKind::Frontend);
    a.keys("<Esc>");
    assert_eq!(a.view.as_ref().unwrap().mode, "normal");
    b.sync();
    assert_eq!(b.view.as_ref().unwrap().mode, "insert");
    // `x` deletes in A (normal mode, `when client.kind == frontend`), types in B.
    a.keys("x");
    b.keys("x");
    a.sync();
    assert_eq!(a.text(), "xbc\n");
}

#[test]
fn opening_a_file_another_client_shows_gives_its_own_view() {
    let s = session("open", "a\nb\nc\n");
    let mut a = Client::attach(&s, ClientKind::Frontend);
    let mut b = Client::attach(&s, ClientKind::Frontend);
    let path = a.view.as_ref().unwrap().path.clone().unwrap();
    let path = std::fs::canonicalize(&path)
        .or_else(|_| {
            std::env::current_dir()
                .map(|d| d.join(&path))
                .and_then(std::fs::canonicalize)
        })
        .unwrap_or_else(|_| path.into());
    a.keys("<Down>");
    let r = b.command("editor.open", json!({ "path": path }));
    assert!(matches!(r, Reply::Ok(_)), "{r:?}");
    b.sync();
    // B kept its own cursor; A's is a peer cursor.
    assert_eq!(b.cursor(CursorKind::Primary), Some((0, 0, None)));
    assert_eq!(b.cursor(CursorKind::Peer), Some((1, 0, Some(a.id))));
}

#[test]
fn tools_run_commands_as_their_own_principal_and_draw_nothing() {
    let s = session("tool", "x\n");
    let mut f = Client::attach(&s, ClientKind::Frontend);
    let mut t = Client::attach(&s, ClientKind::Tool);
    assert!(
        t.log.iter().all(|m| matches!(m, ServerMsg::Welcome { .. })),
        "{:?}",
        t.log
    );
    let r = t.command("editor.insert_text", json!({ "text": "T" }));
    assert_eq!(r, Reply::Ok(json!(1)));
    let r = t.command("nope.nothing", Value::Null);
    let Reply::Error(err) = r else {
        panic!("{r:?}")
    };
    assert_eq!(err.code, "unknown_command");
    let r = t.command("Not An Id", Value::Null);
    assert!(matches!(r, Reply::Error(_)));
    f.sync();
    assert_eq!(f.text(), "Tx\n");
    // The tool is no peer cursor, and its edit is not the human's.
    assert_eq!(f.cursor(CursorKind::Peer), None);
    let actor = s
        .inspect(|core| {
            core.audit()
                .records()
                .rev()
                .find(|r| r.command.as_ref().map(CommandId::as_str) == Some("editor.insert_text"))
                .map(|r| r.actor.clone())
        })
        .flatten();
    assert_eq!(
        actor,
        Some(Actor::Remote {
            client: format!("t#{}", t.id).into()
        })
    );
}

#[test]
fn subscribed_events_arrive_before_the_frame() {
    let s = session("events", "");
    let mut a = Client::attach(&s, ClientKind::Frontend);
    let mut t = Client::attach(&s, ClientKind::Tool);
    a.send(ClientMsg::Subscribe {
        filters: vec!["buffer.*".into()],
    });
    t.send(ClientMsg::Subscribe {
        filters: vec!["buffer.changed".into()],
    });
    t.sync();
    a.send(ClientMsg::Key { key: "z".into() });
    let got = a.sync();
    let names: Vec<&str> = got
        .iter()
        .map(|m| match m {
            ServerMsg::Event { name, .. } => name.as_str(),
            ServerMsg::Surface { .. } => "surface",
            ServerMsg::Flush { .. } => "flush",
            _ => "other",
        })
        .collect();
    assert_eq!(names, ["buffer.changed", "surface", "flush"], "{got:?}");
    let got = t.sync();
    assert!(
        got.iter()
            .any(|m| matches!(m, ServerMsg::Event { name, .. } if name == "buffer.changed")),
        "{got:?}"
    );
    // An empty list unsubscribes.
    a.send(ClientMsg::Subscribe { filters: vec![] });
    a.keys("z");
    assert!(
        !a.log
            .iter()
            .rev()
            .take(3)
            .any(|m| matches!(m, ServerMsg::Event { .. }))
    );
}

#[test]
fn a_client_that_lost_its_rows_gets_a_full_frame_after_refresh() {
    let s = session("refresh", "ab\ncd\n");
    let mut c = Client::attach(&s, ClientKind::Frontend);
    c.keys("x");
    // A diff may already be on its way when the client drops its copy.
    c.send(ClientMsg::Key { key: "y".into() });
    c.send(ClientMsg::Refresh);
    c.view = None;
    c.send(ClientMsg::Ping { id: 99 });
    loop {
        match c.end.recv_timeout(WAIT).unwrap().unwrap() {
            ServerMsg::Surface { view, .. } if !view.rows.is_empty() => {
                assert!(view.clone().resolve(None).is_err());
            }
            ServerMsg::Pong { id: 99, .. } => break,
            m => c.take(m),
        }
    }
    assert_eq!(c.text(), "xyab\ncd\n");
    // From there on, diffs build on the full frame.
    c.keys("z");
    assert_eq!(c.text(), "xyzab\ncd\n");
}

#[test]
fn a_client_that_does_not_ack_gets_at_most_two_frames() {
    let s = session("flow", "");
    let mut slow = Client::attach(&s, ClientKind::Frontend);
    slow.ack = false;
    // The welcome frame was acked; now stop acking. Each of the first two keys gets
    // its own frame.
    let mut flushes = Vec::new();
    for ch in ["a", "b"] {
        slow.send(ClientMsg::Key { key: ch.into() });
        loop {
            let m = slow.end.recv_timeout(WAIT).unwrap().unwrap();
            let done = matches!(m, ServerMsg::Flush { .. });
            if let ServerMsg::Flush { frame } = &m {
                flushes.push(*frame);
            }
            slow.take(m);
            if done {
                break;
            }
        }
    }
    assert_eq!(slow.text(), "ab");
    // At the limit: more keys change the text but send no frame.
    slow.send(ClientMsg::Key { key: "c".into() });
    slow.send(ClientMsg::Key { key: "d".into() });
    let mut a = Client::attach(&s, ClientKind::Frontend);
    // The slow client's keys arrive on its own connection; wait until they are in.
    for _ in 0..1000 {
        a.sync();
        if a.text() == "abcd" {
            break;
        }
    }
    assert_eq!(a.text(), "abcd");
    assert!(matches!(slow.end.try_recv(), Ok(None)));
    // One cumulative ack releases the latest state.
    slow.ack = true;
    slow.send(ClientMsg::Ack {
        frame: *flushes.last().unwrap(),
    });
    slow.sync();
    assert_eq!(slow.text(), "abcd");
}

#[test]
fn leaving_removes_the_clients_views_and_peer_cursor() {
    let s = session("leave", "abc\n");
    let views = || s.inspect(|core| core.state.views.len()).unwrap();
    let before = views();
    let mut a = Client::attach(&s, ClientKind::Frontend);
    let mut b = Client::attach(&s, ClientKind::Frontend);
    // B owns a second view, on another file, but stays on A's document.
    let path = temp_file("leave-2", "other\n");
    b.command("editor.open", json!({ "path": path }));
    assert_eq!(views(), before + 3);
    let first = temp_file("leave", "abc\n");
    b.command("editor.open", json!({ "path": first }));
    a.sync();
    assert!(a.cursor(CursorKind::Peer).is_some());
    b.send(ClientMsg::Bye);
    // The server closes B's connection once B is gone.
    let mut closed = false;
    for _ in 0..1000 {
        match b.end.recv_timeout(WAIT) {
            Err(_) => {
                closed = true;
                break;
            }
            Ok(None) => break,
            Ok(Some(_)) => {}
        }
    }
    assert!(closed, "the server never closed B's connection");
    drop(b);
    a.sync();
    assert_eq!(views(), before + 1);
    assert_eq!(a.cursor(CursorKind::Peer), None);
}

#[test]
fn mouse_presses_move_and_scrolls_scroll() {
    let body: String = (0..50).fold(String::new(), |s, i| s + &format!("line {i}\n"));
    let s = session("mouse", &body);
    let mut a = Client::attach(&s, ClientKind::Frontend);
    a.send(ClientMsg::Mouse {
        node: "editor".into(),
        action: MouseAction::Press,
        line: 4,
        col: 2,
        lines: 0,
        item: None,
    });
    a.sync();
    assert_eq!(a.cursor(CursorKind::Primary), Some((4, 2, None)));
    a.send(ClientMsg::Mouse {
        node: "editor".into(),
        action: MouseAction::Scroll,
        line: 0,
        col: 0,
        lines: 20,
        item: None,
    });
    a.sync();
    let v = a.view.as_ref().unwrap();
    assert_eq!(v.top_line, 20);
    assert_eq!(v.lines[0].spans[0].text, "line 20");
    // A smaller surface shows fewer rows.
    a.send(ClientMsg::Layout {
        surfaces: vec![spur_rpc::SurfaceSize {
            node: "editor".into(),
            rows: 3,
            cols: 80,
        }],
    });
    a.sync();
    assert_eq!(a.view.as_ref().unwrap().lines.len(), 3);
}

#[test]
fn bad_input_and_panics_are_reported_and_the_session_goes_on() {
    let s = session("errors", "");
    let mut a = Client::attach(&s, ClientKind::Frontend);
    a.send(ClientMsg::Key {
        key: "<Nope>".into(),
    });
    let got = a.sync();
    assert!(
        got.iter().any(|m| matches!(m, ServerMsg::Error { .. })),
        "{got:?}"
    );
    let got_panic = {
        a.send(ClientMsg::Command {
            req: 1,
            id: "test.panic".into(),
            args: Value::Null,
        });
        a.sync()
    };
    assert!(
        got_panic
            .iter()
            .any(|m| matches!(m, ServerMsg::Error { message } if message.contains("internal"))),
        "{got_panic:?}"
    );
    // The request still gets its result, so a caller waiting on it is not stuck.
    assert!(
        got_panic.iter().any(|m| matches!(
            m,
            ServerMsg::CommandResult { req: 1, reply: Reply::Error(e) } if e.code == "internal"
        )),
        "{got_panic:?}"
    );
    let focus_is_anchor = s
        .inspect(|core| core.state.views[&core.state.focus].owner.is_none())
        .unwrap();
    assert!(focus_is_anchor);
    a.keys("k");
    assert_eq!(a.text(), "k");
    // Unknown messages are ignored.
    a.send(ClientMsg::Unknown);
    a.sync();
}

#[test]
fn long_lines_are_cut() {
    let s = session("long", &"x".repeat(5000));
    let a = Client::attach(&s, ClientKind::Frontend);
    let line = &a.view.as_ref().unwrap().lines[0];
    assert!(line.truncated);
    assert_eq!(line.spans[0].text.chars().count(), 4000);
}

#[cfg(unix)]
mod over_sockets {
    use std::os::unix::net::UnixStream;

    use super::*;

    fn socket_client(s: &Session) -> ClientEnd {
        let (mine, theirs) = UnixStream::pair().unwrap();
        s.attach_unix(theirs).unwrap();
        ClientEnd::unix(mine).unwrap()
    }

    #[test]
    fn two_clients_over_unix_sockets() {
        let s = session("unix", "\n");
        let mut a = Client::raw(socket_client(&s));
        a.hello(ClientKind::Frontend);
        let mut b = Client::raw(socket_client(&s));
        b.hello(ClientKind::Frontend);
        a.keys("A");
        b.keys("<Down>");
        b.keys("B");
        a.sync();
        assert_eq!(a.text(), "A\nB");
        assert_eq!(a.cursor(CursorKind::Peer), Some((1, 1, Some(b.id))));
    }

    fn first_reply(s: &Session, hello: &[u8]) -> ServerMsg {
        let (mut mine, theirs) = UnixStream::pair().unwrap();
        s.attach_unix(theirs).unwrap();
        let mut frame = u32::try_from(hello.len()).unwrap().to_le_bytes().to_vec();
        frame.extend_from_slice(hello);
        std::io::Write::write_all(&mut mine, &frame).unwrap();
        spur_rpc::read_frame(&mut mine).unwrap().unwrap()
    }

    #[test]
    fn a_version_mismatch_gets_a_readable_bye() {
        let s = session("version", "");
        let m = first_reply(&s, br#"{"type":"hello","protocols":[1],"kind":"frontend"}"#);
        let ServerMsg::Bye { reason } = m else {
            panic!("{m:?}")
        };
        assert_eq!(
            reason,
            "no common protocol: client speaks [1], server speaks [0]"
        );
        // A future hello of another shape still gets a reason.
        let m = first_reply(
            &s,
            br#"{"type":"hello","protocols":[9],"kind":{"v":2},"who":[]}"#,
        );
        assert!(matches!(m, ServerMsg::Bye { reason } if reason.contains("[9]")));
        // Version 0 but an undecodable rest.
        let m = first_reply(&s, br#"{"type":"hello","protocols":[0],"kind":7}"#);
        assert!(matches!(m, ServerMsg::Bye { reason } if reason.starts_with("bad hello")));
    }

    #[test]
    fn the_server_closes_after_bye() {
        let s = session("close", "");
        let (mine, theirs) = UnixStream::pair().unwrap();
        s.attach_unix(theirs).unwrap();
        let mut end = ClientEnd::unix(mine).unwrap();
        end.send(ClientMsg::Key { key: "a".into() }).unwrap();
        assert!(matches!(
            end.recv_timeout(WAIT),
            Ok(Some(ServerMsg::Bye { .. }))
        ));
        assert_eq!(end.recv_timeout(WAIT), Err(spur_rpc::Closed));
    }

    #[test]
    fn a_client_that_stops_reading_is_dropped_and_its_socket_closed() {
        let s = session("stall", "");
        let views = || s.inspect(|core| core.state.views.len()).unwrap();
        let before = views();
        let (mut tool, theirs) = UnixStream::pair().unwrap();
        s.attach_unix(theirs).unwrap();
        for msg in [
            r#"{"type":"hello","protocols":[0],"kind":"tool","name":"t","size":{"cols":1,"rows":1}}"#,
            r#"{"type":"subscribe","filters":["buffer.*"]}"#,
        ] {
            let mut frame = u32::try_from(msg.len()).unwrap().to_le_bytes().to_vec();
            frame.extend_from_slice(msg.as_bytes());
            std::io::Write::write_all(&mut tool, &frame).unwrap();
        }
        let mut a = Client::attach(&s, ClientKind::Frontend);
        while views() < before + 2 {
            a.sync();
        }
        let keys = format!("{}<CR>", "x".repeat(49));
        // The tool reads nothing while edits keep producing events for it.
        let mut dropped = false;
        for _ in 0..5000 {
            a.keys(&keys);
            if views() == before + 1 {
                dropped = true;
                break;
            }
        }
        assert!(dropped, "the stalled tool was never dropped");
        // Its socket was shut down: reading drains what was sent, then ends.
        tool.set_read_timeout(Some(WAIT)).unwrap();
        let mut rest = Vec::new();
        std::io::Read::read_to_end(&mut tool, &mut rest).unwrap();
    }

    #[test]
    fn edits_nobody_announced_still_reach_frontends() {
        let s = session("stale", "abc\n");
        let mut a = Client::attach(&s, ClientKind::Frontend);
        let view = s
            .inspect(|core| {
                core.state
                    .views
                    .iter()
                    .find(|(_, v)| v.owner.is_some())
                    .map(|(id, _)| *id)
            })
            .unwrap()
            .unwrap();
        // An edit made on the session thread with no client behind it.
        s.inspect(move |core| {
            let anchor = core.state.focus;
            core.state.focus = view;
            let id = CommandId::new("editor.insert_text").unwrap();
            let actor = Actor::System {
                source: "test".into(),
            };
            core.execute(spur_core::Invocation::new(id, actor).args(json!({ "text": "Z" })))
                .unwrap();
            core.state.focus = anchor;
        });
        a.sync();
        assert_eq!(a.text(), "Zabc\n");
    }
}
