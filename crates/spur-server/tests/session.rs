//! Drives a session the way a frontend does: over the protocol only. Every scenario
//! runs twice, over the in-process transport and over serialized frames.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use spur_rpc::{
    Caps, ClientEnd, ClientKind, ClientMsg, CursorKind, Node, NodeKind, PROTOCOL_VERSION, Selected,
    ServerEnd, ServerMsg, Size, Style, SurfaceFrame, Ui, framed, in_process,
};
use spur_server::{Pack, Session, SessionConfig};

const FRESH: &str = include_str!("../../../extensions/ux_fresh/pack.toml");
const FALLBACK: &str = include_str!("../../../extensions/fallback.toml");

#[derive(Clone, Copy, Debug)]
enum Transport {
    InProcess,
    Framed,
}

fn workspace(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spur-session-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {\n    let x = 1;\n}\n").unwrap();
    std::fs::write(dir.join("notes.md"), "# Notes\n").unwrap();
    dir
}

fn session(root: &Path, open: Option<&str>, pack: &str) -> Session {
    Session::spawn(SessionConfig {
        name: "test".into(),
        root: root.to_path_buf(),
        pack: Pack::parse(pack).unwrap(),
        open: open.map(PathBuf::from),
        exit_when_empty: false,
        notice: None,
    })
    .unwrap()
}

/// A scripted frontend: remembers the last theme, UI and surface it was sent.
struct Client {
    end: ClientEnd,
    theme: BTreeMap<String, Style>,
    ui: Option<Ui>,
    surface: SurfaceFrame,
    bye: Option<String>,
    pings: u64,
}

impl Client {
    fn attach(session: &Session, transport: Transport) -> Self {
        let end = match transport {
            Transport::InProcess => {
                let (client, server) = in_process();
                assert!(session.attach(server));
                client
            }
            Transport::Framed => {
                let (c2s_r, c2s_w) = std::io::pipe().unwrap();
                let (s2c_r, s2c_w) = std::io::pipe().unwrap();
                let server: ServerEnd = framed(c2s_r, s2c_w);
                assert!(session.attach(server));
                framed(s2c_r, c2s_w)
            }
        };
        let mut c = Self {
            end,
            theme: BTreeMap::new(),
            ui: None,
            surface: SurfaceFrame::default(),
            bye: None,
            pings: 0,
        };
        c.send(ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            kind: ClientKind::Frontend,
            name: "test".into(),
            caps: Caps::default(),
            size: Size {
                cols: 100,
                rows: 30,
            },
        });
        c.send(ClientMsg::SurfaceSize { rows: 10, cols: 60 });
        c.sync();
        c
    }

    fn send(&self, msg: ClientMsg) {
        assert!(self.end.send(msg));
    }

    /// Waits until the server has handled and drawn everything sent so far.
    fn sync(&mut self) {
        self.pings += 1;
        self.send(ClientMsg::Ping { id: self.pings });
        loop {
            match self.end.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(ServerMsg::Pong { id })) if id == self.pings => return,
                Ok(Some(ServerMsg::Theme { tokens })) => self.theme = tokens,
                Ok(Some(ServerMsg::Ui { ui })) => self.ui = Some(ui),
                Ok(Some(ServerMsg::Surface { frame })) => self.surface = frame,
                Ok(Some(ServerMsg::Bye { reason })) => {
                    self.bye = Some(reason);
                    return;
                }
                Ok(Some(_)) => {}
                Ok(None) => panic!("the server did not answer"),
                Err(()) => {
                    self.bye.get_or_insert_with(|| "closed".into());
                    return;
                }
            }
        }
    }

    fn keys(&mut self, keys: &[&str]) {
        for k in keys {
            self.send(ClientMsg::Key { key: (*k).into() });
        }
        self.sync();
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let key = match c {
                ' ' => "<Space>".to_owned(),
                '<' => "<lt>".to_owned(),
                c => c.to_string(),
            };
            self.send(ClientMsg::Key { key });
        }
        self.sync();
    }

    fn lines(&self) -> Vec<String> {
        self.surface
            .lines
            .iter()
            .map(|l| l.iter().map(|s| &*s.text).collect())
            .collect()
    }

    fn node(&self, id: &str) -> Option<&Node> {
        fn find<'a>(n: &'a Node, id: &str) -> Option<&'a Node> {
            if n.id == id {
                return Some(n);
            }
            match &n.kind {
                NodeKind::Row { children } | NodeKind::Column { children } => {
                    children.iter().find_map(|c| find(c, id))
                }
                _ => None,
            }
        }
        let ui = self.ui.as_ref()?;
        find(&ui.root, id).or_else(|| ui.floats.iter().find_map(|f| find(&f.node, id)))
    }

    fn text(&self, id: &str) -> String {
        match self.node(id).map(|n| &n.kind) {
            Some(NodeKind::Text { spans, .. }) => spans.iter().map(|s| &*s.text).collect(),
            _ => String::new(),
        }
    }

    fn list(&self, id: &str) -> (Vec<String>, Option<u32>) {
        match self.node(id).map(|n| &n.kind) {
            Some(NodeKind::List { rows, selected, .. }) => (
                rows.iter()
                    .map(|r| r.spans.iter().map(|s| &*s.text).collect())
                    .collect(),
                *selected,
            ),
            other => panic!("{id} is not a list: {other:?}"),
        }
    }
}

fn both(test: impl Fn(Transport)) {
    test(Transport::InProcess);
    test(Transport::Framed);
}

#[test]
fn edit_save_and_undo_over_the_protocol() {
    both(|t| {
        let root = workspace(&format!("edit-{t:?}"));
        let s = session(&root, Some("src/main.rs"), FRESH);
        let mut c = Client::attach(&s, t);
        assert!(c.theme.contains_key("base"), "the theme arrives as tokens");
        assert_eq!(c.lines()[0], "fn main() {");
        assert_eq!(
            c.surface.lines[0][0].style.as_deref(),
            Some("syntax.keyword"),
            "spans carry semantic names, not colours"
        );
        assert!(
            c.text("crumbs").contains("src  /  main.rs"),
            "{}",
            c.text("crumbs")
        );
        assert!(!c.text("crumbs").contains('●'));

        c.type_text("// hi<");
        c.keys(&["<CR>"]);
        assert_eq!(c.lines()[..2], ["// hi<", "fn main() {"]);
        assert!(
            c.text("crumbs").contains('●'),
            "modified shows in the breadcrumb"
        );
        assert!(
            c.text("position").contains("Ln 2, Col 1"),
            "{}",
            c.text("position")
        );
        assert!(c.text("position").contains("Rust"));

        c.keys(&["<C-s>"]);
        assert_eq!(
            std::fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "// hi<\nfn main() {\n    let x = 1;\n}\n"
        );
        assert!(!c.text("crumbs").contains('●'));
        assert!(c.text("toast_text").contains("Saved main.rs"));

        c.keys(&["<C-z>"]);
        assert_eq!(c.lines()[0], "fn main() {", "typing is one undo step");
        c.keys(&["<S-Right>", "<S-Right>"]);
        assert_eq!(c.surface.selections.len(), 1);
        c.send(ClientMsg::Text {
            text: "pasted ".into(),
        });
        c.sync();
        assert_eq!(c.lines()[0], "pasted  main() {");
        std::fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn ask_finds_commands_and_files() {
    both(|t| {
        let root = workspace(&format!("ask-{t:?}"));
        let s = session(&root, None, FRESH);
        let mut c = Client::attach(&s, t);
        assert!(
            c.node("ask").is_none(),
            "the card is hidden until asked for"
        );
        assert!(
            c.text("hints").contains("ctrl space Ask"),
            "{}",
            c.text("hints")
        );

        c.keys(&["<C-Space>"]);
        let ui = c.ui.clone().unwrap();
        assert_eq!(ui.focus, Selected::Node("ask_input".into()));
        assert!(ui.floats.iter().any(|f| f.node.id == "ask" && f.scrim));
        let (rows, selected) = c.list("ask_list");
        assert_eq!(rows[0], "Commands");
        assert_eq!(selected, Some(1));
        assert!(c.text("ask_hints").contains("enter Run"));
        assert!(
            !c.text("hints").contains("Save"),
            "editor keys are not offered in Ask"
        );

        c.type_text("main");
        let (rows, selected) = c.list("ask_list");
        assert_eq!(rows, ["Files", "src/main.rs"]);
        assert_eq!(selected, Some(1));
        c.keys(&["<CR>"]);
        assert!(c.node("ask").is_none());
        assert_eq!(c.lines()[0], "fn main() {");
        assert_eq!(c.ui.as_ref().unwrap().focus, Selected::Surface);
        let (files, selected) = c.list("files");
        assert_eq!(
            files[selected.unwrap() as usize],
            "main.rs",
            "the tree follows"
        );

        // A command, through the same card.
        c.type_text("x");
        c.keys(&["<C-p>"]);
        c.type_text("undo");
        assert_eq!(c.list("ask_list").0[1], "Undo");
        c.keys(&["<CR>"]);
        assert_eq!(c.lines()[0], "fn main() {");

        c.keys(&["<C-Space>", "<Esc>"]);
        assert!(c.node("ask").is_none());
        std::fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn files_panel_opens_files_and_hides() {
    both(|t| {
        let root = workspace(&format!("files-{t:?}"));
        let s = session(&root, None, FRESH);
        let mut c = Client::attach(&s, t);
        assert_eq!(c.list("files").0, ["src", "notes.md"]);
        c.keys(&["<C-e>"]);
        assert_eq!(c.ui.as_ref().unwrap().focus, Selected::Node("files".into()));
        assert!(
            c.text("hints").contains("enter Open"),
            "{}",
            c.text("hints")
        );
        c.keys(&["<CR>"]);
        assert_eq!(c.list("files").0, ["src", "main.rs", "notes.md"]);
        c.keys(&["<Down>", "<CR>"]);
        assert_eq!(c.lines()[0], "fn main() {");
        assert_eq!(c.ui.as_ref().unwrap().focus, Selected::Surface);

        // Typing in the panel does not edit the document.
        c.keys(&["<C-e>", "z"]);
        assert_eq!(c.lines()[0], "fn main() {");
        c.keys(&["<C-e>"]);
        assert!(
            c.node("files_panel").is_none(),
            "second press hides the panel"
        );

        // Mouse: clicks arrive as node + row, never as screen coordinates.
        c.keys(&["<C-e>"]);
        c.send(ClientMsg::Click {
            node: "files".into(),
            row: 2,
            col: 0,
        });
        c.sync();
        assert_eq!(c.lines()[0], "# Notes");
        c.send(ClientMsg::Click {
            node: "files".into(),
            row: 99,
            col: 0,
        });
        c.send(ClientMsg::Click {
            node: "editor".into(),
            row: 0,
            col: 2,
        });
        c.sync();
        assert!(
            c.text("position").contains("Ln 1, Col 3"),
            "{}",
            c.text("position")
        );
        std::fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn two_clients_share_a_document_but_not_a_cursor() {
    both(|t| {
        let root = workspace(&format!("two-{t:?}"));
        let s = session(&root, Some("src/main.rs"), FRESH);
        let mut a = Client::attach(&s, t);
        let mut b = Client::attach(&s, t);
        a.sync();
        assert!(
            a.text("attached").contains("2 attached"),
            "{}",
            a.text("attached")
        );

        b.keys(&["<Down>", "<End>"]);
        a.type_text("AB");
        b.sync();
        assert_eq!(b.lines()[0], "ABfn main() {", "b sees a's edit");
        assert!(
            b.text("position").contains("Ln 2, Col 15"),
            "b's cursor stays put"
        );
        let peer = b
            .surface
            .cursors
            .iter()
            .find(|c| c.kind == CursorKind::Peer)
            .unwrap();
        assert_eq!((peer.pos.line, peer.pos.col), (0, 2), "b sees where a is");

        // Each client has its own focus and UI.
        a.keys(&["<C-Space>"]);
        b.sync();
        assert!(a.node("ask").is_some() && b.node("ask").is_none());
        b.type_text("!");
        a.type_text("q");
        b.sync();
        assert_eq!(
            b.lines()[1],
            "    let x = 1;!",
            "a's typing went to its Ask card"
        );

        // A leaves; the session and b carry on.
        a.keys(&["<Esc>", "<C-q>"]);
        assert_eq!(a.bye.as_deref(), Some("quit"));
        b.sync();
        assert!(!b.text("attached").contains("attached"));
        assert!(b.surface.cursors.iter().all(|c| c.kind != CursorKind::Peer));
        std::fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn scrolling_follows_the_cursor_and_the_wheel() {
    let root = workspace("scroll");
    let text = (1..=100).fold(String::new(), |acc, i| format!("{acc}line {i}\n"));
    std::fs::write(root.join("long.txt"), text).unwrap();
    let s = session(&root, Some("long.txt"), FRESH);
    let mut c = Client::attach(&s, Transport::InProcess);
    assert_eq!(c.surface.line_count, 101);
    assert_eq!(c.lines().len(), 10);
    c.keys(&["<PageDown>"]);
    assert!(c.text("position").contains("Ln 10,"));
    assert!(c.surface.top_line > 0);
    c.send(ClientMsg::Scroll {
        node: "editor".into(),
        lines: 30,
    });
    c.sync();
    let top = c.surface.top_line;
    assert!(top >= 30, "the wheel scrolls without moving the cursor");
    assert!(c.text("position").contains("Ln 10,"));
    c.keys(&["<Down>"]);
    assert!(
        c.surface.top_line < top,
        "moving the cursor brings it back into view"
    );
    c.send(ClientMsg::SurfaceSize { rows: 4, cols: 20 });
    c.sync();
    assert_eq!(c.lines().len(), 4);
    c.keys(&["<C-End>"]);
    assert_eq!(c.lines().last().map(String::as_str), Some(""));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_look_and_keys_come_from_the_pack() {
    let root = workspace("pack");
    // One edited value changes a key and its hint; nothing is hard-coded in the server.
    let edited = FRESH.replace("keys = \"<C-s>\"", "keys = \"<F2>\"");
    let s = session(&root, Some("notes.md"), &edited);
    let mut c = Client::attach(&s, Transport::InProcess);
    assert!(c.text("hints").contains("f2 Save"), "{}", c.text("hints"));
    c.type_text("x");
    c.keys(&["<C-s>"]);
    assert!(c.text("crumbs").contains('●'), "the old key does nothing");
    c.keys(&["<F2>"]);
    assert!(!c.text("crumbs").contains('●'));
    drop(s);

    // The fallback pack is a different UI over the same server.
    let s = session(&root, Some("notes.md"), FALLBACK);
    let mut c = Client::attach(&s, Transport::InProcess);
    assert!(c.node("files_panel").is_none() && c.node("status").is_some());
    assert!(c.text("status").contains("notes.md"));
    c.type_text("y");
    assert_eq!(c.lines()[0], "yx# Notes");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn bad_clients_are_turned_away() {
    let root = workspace("bad");
    let s = session(&root, None, FRESH);
    let (c, server) = in_process();
    assert!(s.attach(server));
    c.send(ClientMsg::Key { key: "a".into() });
    assert!(matches!(c.recv(), Some(ServerMsg::Bye { reason }) if reason.contains("hello")));

    let (c, server) = in_process();
    assert!(s.attach(server));
    c.send(ClientMsg::Hello {
        protocol: 99,
        kind: ClientKind::Frontend,
        name: "new".into(),
        caps: Caps::default(),
        size: Size::default(),
    });
    assert!(matches!(c.recv(), Some(ServerMsg::Bye { reason }) if reason.contains("99")));

    // Nonsense input is ignored, not fatal.
    let mut ok = Client::attach(&s, Transport::InProcess);
    ok.send(ClientMsg::Key {
        key: "<Bogus>".into(),
    });
    ok.send(ClientMsg::Click {
        node: "nope".into(),
        row: 1,
        col: 1,
    });
    ok.send(ClientMsg::Scroll {
        node: "files".into(),
        lines: i32::MIN,
    });
    ok.send(ClientMsg::Command {
        id: "not a command".into(),
        args: serde_json::Value::Null,
    });
    ok.send(ClientMsg::Command {
        id: "workspace.open".into(),
        args: serde_json::json!({"path": "src"}),
    });
    ok.sync();
    assert!(
        ok.text("toast_text").contains("is a folder"),
        "{}",
        ok.text("toast_text")
    );
    ok.type_text("still works");
    assert_eq!(ok.lines()[0], "still works");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn quitting_with_unsaved_changes_asks_once() {
    let root = workspace("quit");
    let s = Session::spawn(SessionConfig {
        name: "solo".into(),
        root: root.clone(),
        pack: Pack::parse(FRESH).unwrap(),
        open: Some("notes.md".into()),
        exit_when_empty: true,
        notice: Some("hello".into()),
    })
    .unwrap();
    let mut c = Client::attach(&s, Transport::InProcess);
    assert!(c.text("toast_text").contains("hello"));
    c.type_text("x");
    c.keys(&["<C-q>"]);
    assert!(c.bye.is_none());
    assert!(c.text("toast_text").contains("Unsaved"));
    c.keys(&["<Left>", "<C-q>"]);
    assert!(c.bye.is_none(), "another key in between disarms it");
    c.keys(&["<C-q>"]);
    assert_eq!(c.bye.as_deref(), Some("quit"));
    s.join();
    std::fs::remove_dir_all(root).unwrap();
}

/// Latency probe, not a pass/fail test: `cargo test -p spur-server --release -- --ignored
/// --nocapture`. Types into a 2,000-line file and times key → drawn frame.
#[test]
#[ignore = "prints timings; run by hand"]
fn keystroke_round_trip_timing() {
    let root = workspace("timing");
    let text = (1..=2000).fold(String::new(), |acc, i| {
        format!("{acc}    let value_{i} = {i};\n")
    });
    std::fs::write(root.join("src/big.rs"), text).unwrap();
    for t in [Transport::InProcess, Transport::Framed] {
        let s = session(&root, Some("src/big.rs"), FRESH);
        let mut c = Client::attach(&s, t);
        c.send(ClientMsg::SurfaceSize {
            rows: 50,
            cols: 120,
        });
        c.sync();
        let mut times = Vec::new();
        for i in 0..2000 {
            let key = if i % 40 == 39 { "<CR>" } else { "x" };
            let start = std::time::Instant::now();
            c.keys(&[key]);
            times.push(start.elapsed());
        }
        times.sort();
        eprintln!(
            "{t:?}: p50 {:?}  p99 {:?}  max {:?}",
            times[times.len() / 2],
            times[times.len() * 99 / 100],
            times[times.len() - 1]
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
