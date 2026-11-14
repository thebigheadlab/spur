//! The shell over the protocol: a session with the `ux_fresh` pack and a client that
//! keeps what a terminal client would (theme, tree, focus, lists, surface).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use spur_rpc::{
    Caps, ClientEnd, ClientKind, ClientMsg, ItemKind, ListView, MouseAction, Node, Reply,
    ServerMsg, Size, SurfaceSize, SurfaceView, in_process,
};
use spur_server::{PackSource, Session, SessionConfig};

const WAIT: Duration = Duration::from_secs(10);

fn pack_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/ux_fresh")
}

/// A workspace folder named `demo` with a few files, unique per test.
fn workspace(test: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("spur-shell-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("demo");
    for (path, text) in [
        ("README.md", "# demo\n"),
        ("notes.txt", "one\ntwo\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("src/lib.rs", "pub fn f() {}\n"),
        ("src/ui/mod.rs", "\n"),
        ("docs/guide.md", "guide\n"),
    ] {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    root
}

fn session_in(root: &Path, open: &[&str]) -> Session {
    Session::spawn(SessionConfig {
        name: "shell".into(),
        open: open.iter().map(|p| root.join(p)).collect(),
        pack: Some(PackSource::Dir(pack_dir())),
        root: root.to_path_buf(),
        ..Default::default()
    })
    .unwrap()
}

/// A protocol client that keeps the latest theme, tree, lists and surface.
struct Client {
    end: ClientEnd,
    log: Vec<ServerMsg>,
    theme: Option<String>,
    app_bg: Option<String>,
    root: Option<Node>,
    focus: Option<String>,
    lists: BTreeMap<String, ListView>,
    view: Option<SurfaceView>,
    bye: Option<String>,
    ping: u64,
}

impl Client {
    fn attach(s: &Session, cols: u32, rows: u32) -> Self {
        let (end, server) = in_process();
        s.attach(server).unwrap();
        let mut c = Self {
            end,
            log: Vec::new(),
            theme: None,
            app_bg: None,
            root: None,
            focus: None,
            lists: BTreeMap::new(),
            view: None,
            bye: None,
            ping: 0,
        };
        c.send(ClientMsg::Hello {
            protocols: vec![0],
            kind: ClientKind::Frontend,
            name: "t".into(),
            caps: Caps::default(),
            size: Size { cols, rows },
        });
        c.sync();
        c
    }

    fn send(&self, m: ClientMsg) {
        self.end.send(m).unwrap();
    }

    fn take(&mut self, m: ServerMsg) {
        match &m {
            ServerMsg::Theme { name, tokens, .. } => {
                self.theme = Some(name.clone());
                self.app_bg = tokens.get("app").and_then(|t| t.bg.clone());
            }
            ServerMsg::Ui { root, focus } => {
                self.lists.retain(|k, _| find(root, k).is_some());
                self.root = Some(root.clone());
                self.focus.clone_from(focus);
            }
            ServerMsg::List { node, list } => {
                self.lists.insert(node.clone(), list.clone());
            }
            ServerMsg::Surface { view, .. } => {
                let mut full = view.clone();
                full.resolve(self.view.as_ref())
                    .unwrap_or_else(|e| panic!("{e}: {view:?}"));
                self.view = Some(full);
            }
            ServerMsg::Flush { frame } => {
                let _ = self.end.send(ClientMsg::Ack { frame: *frame });
            }
            ServerMsg::Bye { reason } => self.bye = Some(reason.clone()),
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
                Ok(Some(m)) => {
                    let bye = matches!(m, ServerMsg::Bye { .. });
                    self.take(m);
                    if bye {
                        break;
                    }
                }
                other => panic!("no pong: {other:?} after {:?}", &self.log[start..]),
            }
        }
        self.log[start..].to_vec()
    }

    fn keys(&mut self, keys: &str) -> Vec<ServerMsg> {
        self.send(ClientMsg::Key { key: keys.into() });
        self.sync()
    }

    fn press(&mut self, node: &str, item: Option<&str>) -> Vec<ServerMsg> {
        self.send(ClientMsg::Mouse {
            node: node.into(),
            action: MouseAction::Press,
            line: 0,
            col: 0,
            lines: 0,
            item: item.map(Into::into),
        });
        self.sync()
    }

    fn scroll(&mut self, node: &str, lines: i32) -> Vec<ServerMsg> {
        self.send(ClientMsg::Mouse {
            node: node.into(),
            action: MouseAction::Scroll,
            line: 0,
            col: 0,
            lines,
            item: None,
        });
        self.sync()
    }

    fn resize(&mut self, cols: u32, rows: u32) {
        self.send(ClientMsg::Resize {
            size: Size { cols, rows },
        });
        self.sync();
    }

    fn command(&mut self, id: &str, args: Value) -> Reply {
        self.send(ClientMsg::Command {
            req: 9,
            id: id.into(),
            args,
        });
        self.sync()
            .into_iter()
            .find_map(|m| match m {
                ServerMsg::CommandResult { req: 9, reply } => Some(reply),
                _ => None,
            })
            .expect("a result")
    }

    fn shows(&self, id: &str) -> bool {
        self.root.as_ref().and_then(|r| find(r, id)).is_some()
    }

    fn node(&self, id: &str) -> &Node {
        self.root
            .as_ref()
            .and_then(|r| find(r, id))
            .unwrap_or_else(|| panic!("no node {id}"))
    }

    fn focus(&self) -> &str {
        self.focus.as_deref().unwrap_or("")
    }

    fn notice(&self) -> Option<String> {
        self.shows("notice")
            .then(|| self.node("notice").text.clone().unwrap_or_default())
    }

    fn text(&self) -> String {
        let v = self.view.as_ref().unwrap();
        v.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn files(&self) -> &ListView {
        &self.lists["files_list"]
    }

    fn selected(&self) -> String {
        let l = self.files();
        let i = l.selected.expect("a selection") - l.offset;
        l.items[i as usize].id.clone()
    }

    fn item_ids(&self) -> Vec<&str> {
        self.files().items.iter().map(|i| i.id.as_str()).collect()
    }
}

fn find<'a>(n: &'a Node, id: &str) -> Option<&'a Node> {
    if n.id == id {
        return Some(n);
    }
    n.children.iter().find_map(|c| find(c, id))
}

fn kinds(msgs: &[ServerMsg]) -> Vec<&'static str> {
    msgs.iter()
        .map(|m| match m {
            ServerMsg::Welcome { .. } => "welcome",
            ServerMsg::Theme { .. } => "theme",
            ServerMsg::Ui { .. } => "ui",
            ServerMsg::List { .. } => "list",
            ServerMsg::Surface { .. } => "surface",
            ServerMsg::Flush { .. } => "flush",
            ServerMsg::Bye { .. } => "bye",
            ServerMsg::Error { .. } => "error",
            ServerMsg::CommandResult { .. } => "result",
            _ => "other",
        })
        .collect()
}

/// Compares the first frame with a fixture, the workspace path replaced by
/// `<root>`; `SPUR_BLESS=1` rewrites it.
fn check_fixture(name: &str, root: &Path, msgs: &[ServerMsg]) {
    let frame: Vec<&ServerMsg> = msgs
        .iter()
        .filter(|m| !matches!(m, ServerMsg::Welcome { .. }))
        .collect();
    let mut value = serde_json::to_value(&frame).unwrap();
    scrub(&mut value, &root.display().to_string());
    let got = serde_json::to_string_pretty(&value).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    if std::env::var_os("SPUR_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with SPUR_BLESS=1)", path.display()));
    assert!(
        got == want,
        "{name} differs from the fixture (run with SPUR_BLESS=1 to update)"
    );
}

/// Replaces the workspace path in every string with `<root>`, with `/` separators.
fn scrub(v: &mut Value, root: &str) {
    match v {
        Value::String(s) => {
            if let Some(rest) = s.strip_prefix(root) {
                *s = format!("<root>{}", rest.replace('\\', "/"));
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|v| scrub(v, root)),
        Value::Object(o) => o.values_mut().for_each(|v| scrub(v, root)),
        _ => {}
    }
}

#[test]
fn the_first_frame_is_theme_ui_list_surface_flush() {
    for (cols, rows, fixture) in [
        (160, 45, "first_frame_160x45.json"),
        (80, 24, "first_frame_80x24.json"),
    ] {
        let root = workspace(&format!("first{cols}"));
        let s = session_in(&root, &["src/main.rs"]);
        let c = Client::attach(&s, cols, rows);
        let k = kinds(&c.log);
        if cols >= 100 {
            assert_eq!(
                k,
                ["welcome", "theme", "ui", "list", "surface", "flush"],
                "{cols}"
            );
        } else {
            // No room for the sidebar: no list.
            assert_eq!(k, ["welcome", "theme", "ui", "surface", "flush"], "{cols}");
        }
        assert_eq!(c.theme.as_deref(), Some("graphite"));
        assert_eq!(c.focus(), "code");
        assert_eq!(c.node("code_title").text.as_deref(), Some("main.rs"));
        assert_eq!(c.node("code_lang").text.as_deref(), Some("Rust"));
        assert_eq!(c.node("workspace").text.as_deref(), Some("demo"));
        if cols >= 100 {
            // The first open file is revealed and selected.
            assert_eq!(c.selected(), "src/main.rs");
            assert_eq!(
                c.item_ids(),
                [
                    "docs",
                    "src",
                    "src/ui",
                    "src/lib.rs",
                    "src/main.rs",
                    "notes.txt",
                    "README.md"
                ]
            );
        }
        check_fixture(fixture, &root, &c.log);
    }
}

#[test]
fn which_columns_show_depends_on_width() {
    let root = workspace("widths");
    let s = session_in(&root, &["notes.txt"]);
    for (cols, sidebar, agent) in [
        (80, false, false),
        (99, false, false),
        (100, true, false),
        (143, true, false),
        (144, true, true),
        (200, true, true),
    ] {
        let c = Client::attach(&s, cols, 30);
        assert_eq!(c.shows("sidebar"), sidebar, "sidebar at {cols}");
        assert_eq!(c.shows("agent"), agent, "agent at {cols}");
        assert!(c.shows("code"));
    }
}

#[test]
fn typing_sends_one_row_and_a_save_then_edit_sends_the_tree() {
    let root = workspace("typing");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    let got = c.keys("x");
    // The modified dot appears: one new tree, and the one changed row.
    assert_eq!(kinds(&got), ["ui", "surface", "flush"]);
    assert!(c.shows("code_modified"));
    let got = c.keys("y");
    assert_eq!(kinds(&got), ["surface", "flush"]);
    let ServerMsg::Surface { view, .. } = &got[0] else {
        unreachable!()
    };
    assert!(view.base.is_some(), "a delta, not a full surface");
    assert_eq!(c.text().lines().next(), Some("xyone"));
    let got = c.keys("<C-s>");
    assert!(kinds(&got).contains(&"ui"), "the dot goes away");
    assert!(!c.shows("code_modified"));
    let got = c.keys("z");
    assert_eq!(kinds(&got), ["ui", "surface", "flush"]);
    let ServerMsg::Surface { view, .. } = &got[1] else {
        unreachable!()
    };
    assert!(view.base.is_some(), "only the changed rows");
}

#[test]
fn ctrl_b_hides_a_focused_sidebar_and_typing_goes_to_the_code() {
    let root = workspace("ctrlb");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    c.keys("<C-S-e>");
    assert_eq!(c.focus(), "sidebar");
    c.keys("<C-b>");
    assert!(!c.shows("sidebar"));
    assert_eq!(c.focus(), "code");
    c.keys("q");
    assert_eq!(c.text().lines().next(), Some("qone"));
    // ctrl+b twice: closed, then open again with the tree sent in full.
    c.keys("<C-b>");
    assert!(c.shows("sidebar"));
    c.keys("<C-b>");
    assert!(!c.shows("sidebar"));
    assert!(!c.lists.contains_key("files_list"));
    let got = c.keys("<C-b>");
    assert!(c.shows("sidebar"));
    assert_eq!(c.focus(), "code", "rule 2 keeps focus");
    assert!(kinds(&got).contains(&"list"));
    assert!(!c.files().items.is_empty());
}

#[test]
fn narrow_terminals_show_columns_on_focus() {
    let root = workspace("narrow");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 80, 24);
    assert!(!c.shows("sidebar"));
    for (keys, view) in [
        ("<C-S-e>", "files_view"),
        ("<A-2>", "sessions_view"),
        ("<C-k>f", "search_view"),
    ] {
        c.keys(keys);
        assert_eq!(c.focus(), "sidebar", "{keys}");
        assert!(c.shows("sidebar") && c.shows(view), "{keys}");
        c.keys("<Esc>");
        assert_eq!(c.focus(), "code", "{keys}");
        assert!(!c.shows("sidebar"), "{keys}");
    }
    c.press("rail_git", None);
    assert_eq!(c.focus(), "sidebar");
    assert!(c.shows("git_view"));
    c.keys("<Esc>");
    assert!(!c.shows("sidebar"));
    c.keys("<C-A-b>");
    assert_eq!(c.focus(), "agent");
    assert!(c.shows("agent") && c.shows("chat_view"));
    c.keys("<Right>");
    assert!(c.shows("threads_view"));
    c.keys("<Left>");
    c.keys("<Left>");
    assert!(c.shows("memory_view"), "wraps");
    c.keys("<Esc>");
    assert!(!c.shows("agent"));
    assert_eq!(c.focus(), "code");
}

#[test]
fn the_agent_column_at_120_columns() {
    let root = workspace("w120");
    let s = session_in(&root, &["notes.txt"]);
    let mut a = Client::attach(&s, 160, 30);
    a.resize(120, 30);
    let mut b = Client::attach(&s, 120, 30);
    for c in [&mut a, &mut b] {
        assert!(c.shows("sidebar") && !c.shows("agent"));
        c.keys("<C-b>");
        assert!(!c.shows("sidebar") && !c.shows("agent"));
        c.keys("<C-A-b>");
        assert!(c.shows("agent"));
        assert_eq!(c.focus(), "agent");
        c.keys("<Esc>");
        assert!(!c.shows("agent"));
        c.keys("<C-A-b>");
        assert_eq!(c.focus(), "agent");
        c.keys("<C-A-b>");
        assert!(!c.shows("agent"), "closed");
        c.keys("<C-A-b>");
        assert!(c.shows("agent"), "shown again");
        assert_eq!(c.focus(), "agent");
    }
}

#[test]
fn the_rail_and_view_keys_toggle() {
    let root = workspace("rail");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    assert!(c.node("rail_files").clickable);
    assert_eq!(
        c.node("rail_files").style.as_deref(),
        Some("rail.item.active")
    );
    c.press("rail_files", None);
    assert!(!c.shows("sidebar"), "the active icon closes it");
    c.press("rail_files", None);
    assert!(c.shows("sidebar"));
    assert_eq!(c.focus(), "sidebar");
    c.keys("<Esc>");
    c.keys("<C-S-e>");
    assert_eq!(c.focus(), "sidebar");
    c.keys("<C-S-e>");
    assert_eq!(c.focus(), "code");
    assert!(c.shows("sidebar"));
}

#[test]
fn the_files_tree_moves_expands_and_opens() {
    let root = workspace("tree");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    assert_eq!(c.selected(), "notes.txt");
    assert_eq!(c.item_ids(), ["docs", "src", "notes.txt", "README.md"]);
    assert!(c.files().items[2].current);
    c.keys("<C-S-e>");
    c.keys("<Up>");
    assert_eq!(c.selected(), "src");
    c.keys("<Up>");
    assert_eq!(c.selected(), "docs");
    c.keys("<Up>");
    assert_eq!(c.selected(), "docs", "clamped");
    c.keys("<PageDown>");
    assert_eq!(c.selected(), "README.md", "clamped at the end");
    c.keys("<Up>");
    c.keys("<Up>");
    assert_eq!(c.selected(), "src");
    c.keys("<Right>");
    assert_eq!(
        c.item_ids(),
        [
            "docs",
            "src",
            "src/ui",
            "src/lib.rs",
            "src/main.rs",
            "notes.txt",
            "README.md"
        ]
    );
    c.keys("<Down>");
    c.keys("<Down>");
    c.keys("<Down>");
    assert_eq!(c.selected(), "src/main.rs");
    // Left on a file selects its folder; Left again collapses it.
    c.keys("<Left>");
    assert_eq!(c.selected(), "src");
    c.keys("<Left>");
    assert!(!c.item_ids().contains(&"src/main.rs"));
    // Enter on a folder toggles it; on a file opens it and focuses the code.
    c.keys("<CR>");
    assert!(c.item_ids().contains(&"src/main.rs"));
    c.keys("<Down>");
    c.keys("<Down>");
    c.keys("<Down>");
    c.keys("<CR>");
    assert_eq!(c.focus(), "code");
    assert_eq!(c.node("code_title").text.as_deref(), Some("main.rs"));
    let current: Vec<&str> = c
        .files()
        .items
        .iter()
        .filter(|i| i.current)
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(current, ["src/main.rs"], "the open file's row is marked");
    c.keys("k");
    assert_eq!(c.text().lines().next(), Some("kfn main() {}"));
    let main = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert_eq!(main, "fn main() {}\n", "not saved");
}

#[test]
fn clicks_open_items_by_id_and_scrolling_keeps_the_selection() {
    let root = workspace("click");
    for i in 0..40 {
        std::fs::write(root.join(format!("docs/page{i:02}.md")), "p\n").unwrap();
    }
    let s = session_in(&root, &["docs/page00.md"]);
    let mut c = Client::attach(&s, 160, 30);
    c.send(ClientMsg::Layout {
        surfaces: vec![SurfaceSize {
            node: "files_list".into(),
            rows: 10,
            cols: 24,
        }],
    });
    c.sync();
    assert_eq!(c.files().items.len(), 10);
    assert_eq!(c.selected(), "docs/page00.md");
    c.scroll("files_list", 20);
    assert_eq!(c.files().offset, 20);
    assert_eq!(c.files().selected, Some(2), "the selection stays");
    // An item in the scrolled window opens by id.
    let id = c.files().items[3].id.clone();
    c.press("files_list", Some(&id));
    assert_eq!(c.focus(), "code");
    let name = id.rsplit('/').next().unwrap().to_owned();
    assert_eq!(c.node("code_title").text.as_deref(), Some(name.as_str()));
    assert_eq!(c.selected(), id);
    // An unknown id is ignored, but the press still focuses the sidebar.
    c.press("files_list", Some("nope.rs"));
    assert_eq!(c.selected(), id);
    assert_eq!(c.node("code_title").text.as_deref(), Some(name.as_str()));
    assert_eq!(c.focus(), "sidebar");
    // A click on a folder toggles it.
    c.press("files_list", Some("docs"));
    assert!(!c.item_ids().iter().any(|i| i.starts_with("docs/")));
    assert_eq!(c.files().items.first().map(|i| i.kind), Some(ItemKind::Dir));
}

#[test]
fn a_save_shows_a_new_file_in_the_tree() {
    let root = workspace("saveas");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    let path = root.join("zeta.txt");
    let r = c.command("buffer.save", json!({ "path": path }));
    assert!(matches!(r, Reply::Ok(_)), "{r:?}");
    c.sync();
    assert!(c.item_ids().contains(&"zeta.txt"));
}

#[test]
fn a_failed_command_shows_until_the_next_key() {
    let root = workspace("status");
    let s = session_in(&root, &[]);
    let mut c = Client::attach(&s, 160, 30);
    assert_eq!(c.notice(), None);
    assert_eq!(c.node("code_title").text.as_deref(), Some("untitled"));
    c.keys("a");
    c.keys("<C-s>");
    let notice = c.notice().expect("a notice");
    assert!(notice.starts_with("buffer.save: "), "{notice}");
    c.keys("<Right>");
    assert_eq!(c.notice(), None);
    // The quit prompt hides the status.
    c.keys("<C-s>");
    c.keys("<C-q>");
    let notice = c.notice().expect("the prompt");
    assert!(
        notice.starts_with("Unsaved changes in untitled"),
        "{notice}"
    );
    // The wire command message returns its error and sets no notice.
    c.keys("<Right>");
    let r = c.command("buffer.save", json!({}));
    assert!(matches!(r, Reply::Error(_)));
    assert_eq!(c.notice(), None);
}

#[test]
fn quit_asks_once_when_there_are_changes() {
    let root = workspace("quit");
    let s = session_in(&root, &["notes.txt"]);
    let mut clean = Client::attach(&s, 160, 30);
    clean.keys("<C-q>");
    assert_eq!(clean.bye.as_deref(), Some("quit"));

    let mut c = Client::attach(&s, 160, 30);
    c.keys("x");
    c.keys("<C-q>");
    assert_eq!(c.bye, None);
    assert!(c.notice().is_some());
    c.keys("<Right>");
    assert_eq!(c.notice(), None, "another key clears the prompt");
    c.keys("<C-q>");
    assert_eq!(c.bye, None);
    c.keys("<C-q>");
    assert_eq!(c.bye.as_deref(), Some("quit"));
}

#[test]
fn paste_while_the_sidebar_has_focus_does_nothing() {
    let root = workspace("paste");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    c.keys("<C-S-e>");
    c.send(ClientMsg::Paste { text: "zz".into() });
    c.sync();
    assert_eq!(c.text().lines().next(), Some("one"));
    c.keys("<Esc>");
    c.send(ClientMsg::Paste { text: "zz".into() });
    c.sync();
    assert_eq!(c.text().lines().next(), Some("zzone"));
}

#[test]
fn ui_commands_check_their_args_and_callers() {
    let root = workspace("uicmd");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    let code = |r: Reply| match r {
        Reply::Error(e) => e.code,
        Reply::Ok(v) => format!("ok {v}"),
        Reply::Unknown => "unknown".into(),
    };
    assert_eq!(code(c.command("ui.nope", json!({}))), "unknown_command");
    assert_eq!(
        code(c.command("ui.set", json!({ "key": "ui.sidebar", "value": "nope" }))),
        "bad_args"
    );
    assert_eq!(
        code(c.command("ui.set", json!({ "key": "ui.sidebar_open", "value": 3 }))),
        "bad_args"
    );
    assert_eq!(
        code(c.command("ui.toggle", json!({ "key": "ui.sidebar" }))),
        "bad_args"
    );
    assert_eq!(
        code(c.command("ui.focus", json!({ "region": "x" }))),
        "bad_args"
    );
    assert_eq!(
        code(c.command("ui.cycle", json!({ "key": "ui.sidebar", "by": 5 }))),
        "ok null"
    );
    c.sync();
    assert!(c.shows("sessions_view"), "files + 5 wraps to sessions");
    assert_eq!(
        code(c.command("ui.focus", json!({ "region": "agent" }))),
        "ok null"
    );
    c.sync();
    assert_eq!(c.focus(), "agent");

    // A tool has no UI state.
    let (mut end, server) = in_process();
    s.attach(server).unwrap();
    end.send(ClientMsg::Hello {
        protocols: vec![0],
        kind: ClientKind::Tool,
        name: "tool".into(),
        caps: Caps::default(),
        size: Size { cols: 0, rows: 0 },
    })
    .unwrap();
    end.send(ClientMsg::Command {
        req: 1,
        id: "ui.toggle".into(),
        args: json!({ "key": "ui.agent_open" }),
    })
    .unwrap();
    let reply = loop {
        match end.recv_timeout(WAIT) {
            Ok(Some(ServerMsg::CommandResult { reply, .. })) => break reply,
            Ok(Some(_)) => {}
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(code(reply), "failed");
}

#[test]
fn a_session_without_a_pack_still_draws_the_bare_tree() {
    let root = workspace("bare");
    let s = Session::spawn(SessionConfig {
        open: vec![root.join("notes.txt")],
        ..Default::default()
    })
    .unwrap();
    let c = Client::attach(&s, 80, 10);
    assert_eq!(kinds(&c.log), ["welcome", "ui", "surface", "flush"]);
    assert_eq!(c.focus(), "editor");
}

#[test]
fn bad_packs_and_themes_stop_the_session() {
    let root = workspace("badpack");
    let err = Session::spawn(SessionConfig {
        pack: Some(PackSource::Dir(pack_dir())),
        theme: Some("nope".into()),
        root: root.clone(),
        ..Default::default()
    })
    .unwrap_err();
    assert_eq!(err.to_string(), "the UX pack has no theme \"nope\"");
    let s = Session::spawn(SessionConfig {
        pack: Some(PackSource::Dir(pack_dir())),
        theme: Some("paper".into()),
        root,
        ..Default::default()
    })
    .unwrap();
    let c = Client::attach(&s, 80, 10);
    assert_eq!(c.theme.as_deref(), Some("paper"));
    let err = Session::spawn(SessionConfig {
        pack: Some(PackSource::Embedded(&[("pack.toml", "id = 1\n")])),
        ..Default::default()
    })
    .unwrap_err();
    assert!(
        err.to_string().starts_with("the UX pack did not load:"),
        "{err}"
    );
}

#[test]
fn keys_in_one_message_see_the_state_the_earlier_ones_made() {
    let root = workspace("batched");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 80, 24);
    // The sidebar is hidden: the list it shows appears only with the first key.
    c.keys("<C-S-e><Up>");
    assert_eq!(c.focus(), "sidebar");
    assert_eq!(c.selected(), "src");
    // Switching to Sessions hides the Files list: Down then moves nothing.
    c.keys("<A-2><Down><A-1>");
    assert_eq!(c.selected(), "src");
}

#[test]
fn an_unreadable_folder_shows_one_error_row() {
    let base = std::env::temp_dir().join(format!("spur-shell-{}-missing", std::process::id()));
    let s = Session::spawn(SessionConfig {
        pack: Some(PackSource::Dir(pack_dir())),
        root: base.join("not-there"),
        ..Default::default()
    })
    .unwrap();
    let c = Client::attach(&s, 160, 30);
    let items = &c.files().items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "#error:");
    assert_eq!(items[0].kind, ItemKind::Error);
    assert!(
        items[0].text.starts_with("cannot read: "),
        "{}",
        items[0].text
    );
}

#[test]
fn refresh_sends_everything_again() {
    let root = workspace("refresh");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    c.send(ClientMsg::Refresh);
    let got = c.sync();
    assert_eq!(kinds(&got), ["theme", "ui", "list", "surface", "flush"]);
    let ServerMsg::Surface { view, .. } = &got[3] else {
        unreachable!()
    };
    assert_eq!(view.base, None, "a full surface");
}

#[test]
fn the_quit_prompt_is_confirmed_only_by_the_next_quit_key() {
    let root = workspace("quitwire");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    c.keys("x");
    c.keys("<C-q>");
    c.keys("<Right>");
    // The prompt is gone: a quit from elsewhere asks again.
    let r = c.command("ui.quit", json!({}));
    assert!(matches!(r, Reply::Ok(_)), "{r:?}");
    assert_eq!(c.bye, None);
    assert!(c.notice().is_some());
}

#[test]
fn presses_on_nodes_the_client_was_not_shown_are_ignored() {
    let root = workspace("hidden");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 80, 24);
    assert!(!c.shows("agent"));
    c.press("tab_threads", None);
    c.keys("<C-A-b>");
    assert!(c.shows("chat_view"), "the hidden tab was not clicked");
    c.press("tab_threads", None);
    assert!(c.shows("threads_view"));
}

#[cfg(unix)]
#[test]
fn a_symlink_to_a_folder_is_listed_but_not_opened() {
    let root = workspace("symlink");
    std::os::unix::fs::symlink(root.join("src"), root.join("link")).unwrap();
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    let item = c.files().items.iter().find(|i| i.id == "link").unwrap();
    assert_eq!(item.kind, ItemKind::Entry);
    c.press("files_list", Some("link"));
    assert_eq!(c.node("code_title").text.as_deref(), Some("notes.txt"));
    assert_eq!(c.notice(), None);
}

#[test]
fn a_session_that_fails_writes_rescue_copies() {
    use std::sync::{Arc, Mutex};
    let root = workspace("rescue");
    let got: Arc<Mutex<spur_server::Modified>> = Arc::default();
    let sink = got.clone();
    let s = Session::spawn(SessionConfig {
        open: vec![root.join("notes.txt")],
        pack: Some(PackSource::Dir(pack_dir())),
        root: root.clone(),
        rescue: Some(spur_server::RescueSink(Arc::new(move |docs| {
            sink.lock().unwrap().extend_from_slice(docs);
        }))),
        ..Default::default()
    })
    .unwrap();
    let mut c = Client::attach(&s, 160, 30);
    c.keys("x");
    let docs = s.modified_documents(WAIT).unwrap();
    assert_eq!(
        docs,
        [(Some(root.join("notes.txt")), "xone\ntwo\n".to_owned())]
    );
    s.inject_panic();
    assert_eq!(s.modified_documents(Duration::from_millis(200)), None);
    assert_eq!(
        *got.lock().unwrap(),
        [(Some(root.join("notes.txt")), "xone\ntwo\n".to_owned())]
    );
}

/// A copy of `ux_fresh` to edit.
fn pack_copy(test: &str) -> PathBuf {
    let dst = std::env::temp_dir().join(format!("spur-pack-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dst);
    for sub in ["", "components", "themes"] {
        std::fs::create_dir_all(dst.join(sub)).unwrap();
        for e in std::fs::read_dir(pack_dir().join(sub)).unwrap().flatten() {
            if e.path().is_file() {
                std::fs::copy(e.path(), dst.join(sub).join(e.file_name())).unwrap();
            }
        }
    }
    dst
}

fn edit(path: &Path, from: &str, to: &str) {
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains(from), "{from}");
    std::fs::write(path, text.replacen(from, to, 1)).unwrap();
}

/// Syncs until `f` holds.
fn until(c: &mut Client, what: &str, mut f: impl FnMut(&mut Client) -> bool) {
    let end = std::time::Instant::now() + WAIT;
    while !f(c) {
        assert!(
            std::time::Instant::now() < end,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
        c.sync();
    }
}

#[test]
fn the_pack_reloads_when_its_files_change() {
    let root = workspace("reload");
    let pack = pack_copy("reload");
    let s = Session::spawn(SessionConfig {
        open: vec![root.join("notes.txt")],
        pack: Some(PackSource::Dir(pack.clone())),
        root: root.clone(),
        watch: Some(Duration::from_millis(10)),
        ..Default::default()
    })
    .unwrap();
    let mut c = Client::attach(&s, 160, 30);
    assert_eq!(c.app_bg.as_deref(), Some("#111215"));
    // A same-size colour edit.
    edit(
        &pack.join("themes/graphite.toml"),
        "bg = \"#111215\"",
        "bg = \"#121215\"",
    );
    until(&mut c, "the new colour", |c| {
        c.app_bg.as_deref() == Some("#121215")
    });

    // A new state key appears; the old values stay.
    c.keys("<C-b>");
    assert!(!c.shows("sidebar"));
    edit(
        &pack.join("pack.toml"),
        "[state]",
        "[state]\n\"ui.extra\" = true",
    );
    until(&mut c, "the new key", |c| {
        matches!(
            c.command("ui.toggle", json!({ "key": "ui.extra" })),
            Reply::Ok(_)
        )
    });
    assert!(!c.shows("sidebar"), "kept ui.sidebar_open = false");

    // A broken edit keeps the old pack and shows the error, also to a new client.
    edit(&pack.join("layout.toml"), "width = 28", "width = \"wide\"");
    until(&mut c, "the error notice", |c| c.notice().is_some());
    let notice = c.notice().unwrap();
    assert!(notice.starts_with("layout.toml:"), "{notice}");
    let late = Client::attach(&s, 160, 30);
    assert_eq!(late.notice(), Some(notice));
    assert_eq!(late.app_bg.as_deref(), Some("#121215"));
    // Fixing it clears the notice; removing the key removes it.
    edit(&pack.join("layout.toml"), "width = \"wide\"", "width = 28");
    edit(&pack.join("pack.toml"), "\n\"ui.extra\" = true", "");
    until(&mut c, "the key to go", |c| {
        matches!(
            c.command("ui.toggle", json!({ "key": "ui.extra" })),
            Reply::Error(_)
        )
    });
    assert_eq!(c.notice(), None);
}

#[test]
fn a_reload_keeps_the_theme_override_and_swaps_the_keys() {
    let root = workspace("reload-theme");
    let pack = pack_copy("reload-theme");
    let s = Session::spawn(SessionConfig {
        open: vec![root.join("notes.txt")],
        pack: Some(PackSource::Dir(pack.clone())),
        theme: Some("paper".into()),
        root: root.clone(),
        watch: Some(Duration::from_millis(10)),
        ..Default::default()
    })
    .unwrap();
    let mut c = Client::attach(&s, 160, 30);
    assert_eq!(c.app_bg.as_deref(), Some("#f3f3f1"));
    // The override survives a reload of the theme it names.
    edit(
        &pack.join("themes/paper.toml"),
        "bg = \"#f3f3f1\"",
        "bg = \"#f3f3f2\"",
    );
    until(&mut c, "the paper edit", |c| {
        c.app_bg.as_deref() == Some("#f3f3f2")
    });
    // A new binding works after the reload; the old ones still do.
    let keys = pack.join("keys.toml");
    let mut text = std::fs::read_to_string(&keys).unwrap();
    text.push_str(
        "\n[[bind]]\nmode = \"insert\"\nkeys = \"<C-t>\"\ncommand = \"ui.set\"\n\
         args = { key = \"ui.sidebar_open\", value = false }\n",
    );
    std::fs::write(&keys, text).unwrap();
    until(&mut c, "the new binding", |c| {
        c.keys("<C-t>");
        !c.shows("sidebar")
    });
    c.keys("x");
    assert!(c.text().starts_with('x'), "{}", c.text());
    // Removing the theme the session asked for is a load error.
    std::fs::remove_file(pack.join("themes/paper.toml")).unwrap();
    until(&mut c, "the notice", |c| c.notice().is_some());
    let notice = c.notice().unwrap();
    assert!(notice.contains("paper"), "{notice}");
    assert_eq!(c.app_bg.as_deref(), Some("#f3f3f2"));
}

#[test]
fn code_is_coloured_and_copied_rows_keep_their_colours() {
    let root = workspace("colours");
    let s = session_in(&root, &["notes.txt", "src/lib.rs"]);
    let mut c = Client::attach(&s, 160, 30);
    let hl = |c: &Client, row: usize| -> Vec<(String, Option<String>)> {
        c.view.as_ref().unwrap().lines[row]
            .spans
            .iter()
            .map(|s| (s.text.clone(), s.hl.clone()))
            .collect()
    };
    // Plain text stays plain.
    assert_eq!(hl(&c, 0), [("one".to_owned(), None)]);
    c.command("editor.open", json!({ "path": root.join("src/lib.rs") }));
    let first = hl(&c, 0);
    assert_eq!(first[0], ("pub".into(), Some("syntax.keyword".into())));
    assert!(first.contains(&("f".into(), Some("syntax.function".into()))));
    // A new second line is sent; the first row is copied and keeps its colours.
    c.keys("<C-Home><End><CR>let");
    assert_eq!(hl(&c, 0), first);
    assert_eq!(hl(&c, 1)[0], ("let".into(), Some("syntax.keyword".into())));
}

#[test]
fn saving_under_a_new_language_recolours_every_row() {
    let root = workspace("save-as");
    let s = session_in(&root, &["notes.txt"]);
    let mut c = Client::attach(&s, 160, 30);
    c.keys("<C-Home>fn ");
    let plain = &c.view.as_ref().unwrap().lines[0].spans;
    assert!(plain.iter().all(|s| s.hl.is_none()), "{plain:?}");
    let r = c.command("buffer.save", json!({ "path": root.join("notes.rs") }));
    assert!(matches!(r, Reply::Ok(_)), "{r:?}");
    let line = &c.view.as_ref().unwrap().lines[0].spans;
    assert_eq!(line[0].hl.as_deref(), Some("syntax.keyword"), "{line:?}");
}
