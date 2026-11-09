//! Keymap engine behaviour.

use std::time::{Duration, Instant};

use serde_json::json;
use spur_core::{Actor, CommandId, Context, Source};
use spur_keymap::{ConflictKind, Feed, Key, KeyCode, Keymap, Layer, Mods, Status, parse_keys};

const BASE: &str = r#"
[mode.normal]
counts = true
registers = true

[mode.insert]
fallback = "editor.insert_text"

[[bind]]
mode = "normal"
keys = "j"
command = "editor.move_down"

[[bind]]
mode = "normal"
keys = "g"
command = "test.g"

[[bind]]
mode = "normal"
keys = "gg"
command = "editor.move_doc_start"
label = "top"

[[bind]]
mode = "normal"
keys = "f<char>"
command = "test.find"
args = { before = false }

[[bind]]
mode = "normal insert"
keys = "<C-s>"
command = "buffer.save"

[[bind]]
mode = "normal"
keys = "o"
run = [{ command = "editor.move_line_end" }, { command = "editor.insert_newline" }, { command = "editor.set_mode", args = { mode = "insert" } }]

[[bind]]
mode = "normal"
keys = "<Space>w"
command = "test.write"
when = "buffer.modified"

[[bind]]
mode = "normal"
keys = "<Space>q"
command = "test.quit"

[[group]]
mode = "normal"
keys = "<Space>"
label = "more"

[[group]]
mode = "normal"
keys = "<C-w>"
label = "+window"
sticky = true

[[bind]]
mode = "normal"
keys = "<C-w>>"
command = "test.wider"

[[bind]]
mode = "insert"
keys = "<Esc>"
command = "editor.set_mode"
args = { mode = "normal" }
"#;

fn keymap() -> Keymap {
    let mut k = Keymap::new();
    k.push_layer(Layer::from_toml("base", Source::Core, BASE).unwrap());
    k
}

fn cx(modified: bool) -> Context {
    let mut c = Context::new();
    c.set("buffer.modified", modified).unwrap();
    c
}

fn t0() -> Instant {
    Instant::now()
}

/// Feeds keys; returns the command ids run (with count/register/chars) and last status.
fn typ(k: &mut Keymap, mode: &str, keys: &str, cx: &Context) -> (Vec<String>, Status) {
    let now = t0();
    let mut ran = Vec::new();
    let mut status = Status::Idle;
    for key in parse_keys(keys).unwrap() {
        let Feed { run, status: s } = k.feed(key, mode, cx, now);
        for d in run {
            let mut s = d
                .steps
                .iter()
                .map(|s| s.command.to_string())
                .collect::<Vec<_>>()
                .join("+");
            if let Some(c) = d.count {
                s = format!("{c}*{s}");
            }
            if let Some(r) = d.register {
                s = format!("\"{r}{s}");
            }
            if !d.chars.is_empty() {
                s = format!("{s}({})", d.chars.iter().collect::<String>());
            }
            ran.push(s);
        }
        status = s;
    }
    (ran, status)
}

#[test]
fn simple_counts_registers() {
    let mut k = keymap();
    let c = cx(false);
    assert_eq!(typ(&mut k, "normal", "j", &c).0, ["editor.move_down"]);
    assert_eq!(typ(&mut k, "normal", "12j", &c).0, ["12*editor.move_down"]);
    assert_eq!(
        typ(&mut k, "normal", "\"a3j", &c).0,
        ["\"a3*editor.move_down"]
    );
    // `0` alone is not a count.
    let (ran, status) = typ(&mut k, "normal", "0", &c);
    assert!(ran.is_empty());
    assert_eq!(status, Status::Unmatched(parse_keys("0").unwrap()));
    assert_eq!(typ(&mut k, "normal", "10j", &c).0, ["10*editor.move_down"]);
}

#[test]
fn ambiguous_prefix_waits_then_fires() {
    let mut k = keymap();
    let c = cx(false);
    assert_eq!(typ(&mut k, "normal", "gg", &c).0, ["editor.move_doc_start"]);

    let now = t0();
    let f = k.feed(Key::char('g'), "normal", &c, now);
    assert_eq!(f.status, Status::Pending);
    assert!(f.run.is_empty());
    let deadline = k.deadline().unwrap();
    assert!(
        k.tick(deadline.checked_sub(Duration::from_millis(1)).unwrap())
            .is_none()
    );
    let d = k.tick(deadline).unwrap();
    assert_eq!(d.steps[0].command.as_str(), "test.g");
    assert!(!k.is_pending());

    // A non-extending key fires the shorter binding first, then runs normally.
    assert_eq!(
        typ(&mut k, "normal", "gj", &c).0,
        ["test.g", "editor.move_down"]
    );
}

#[test]
fn unambiguous_fires_immediately() {
    let mut k = keymap();
    let f = k.feed(Key::char('j'), "normal", &cx(false), t0());
    assert_eq!(f.run.len(), 1);
    assert_eq!(k.deadline(), None);
}

#[test]
fn any_char_capture_and_args() {
    let mut k = keymap();
    let now = t0();
    let c = cx(false);
    k.feed(Key::char('f'), "normal", &c, now);
    let f = k.feed(Key::char('x'), "normal", &c, now);
    let invs = f.run[0].invocations(&Actor::User);
    assert_eq!(*invs[0].args, json!({"before": false, "char": "x"}));
    // A ctrl key is not a char.
    k.feed(Key::char('f'), "normal", &c, now);
    let f = k.feed(Key::new(KeyCode::Char('x'), Mods::CTRL), "normal", &c, now);
    assert!(matches!(f.status, Status::Unmatched(_)));
}

#[test]
fn run_sequence_count_on_first_step() {
    let mut k = keymap();
    let now = t0();
    let c = cx(false);
    k.feed(Key::char('3'), "normal", &c, now);
    let f = k.feed(Key::char('o'), "normal", &c, now);
    let invs = f.run[0].invocations(&Actor::User);
    assert_eq!(invs.len(), 3);
    assert_eq!(invs[0].count, Some(3));
    assert_eq!(invs[1].count, None);
    assert_eq!(*invs[2].args, json!({"mode": "insert"}));
}

#[test]
fn when_filters_and_describe() {
    let mut k = keymap();
    assert!(typ(&mut k, "normal", "<Space>w", &cx(false)).0.is_empty());
    assert_eq!(
        typ(&mut k, "normal", "<Space>w", &cx(true)).0,
        ["test.write"]
    );
    let d = k.describe_key("normal", &parse_keys("<Space>w").unwrap(), &cx(false));
    assert!(d.chosen.is_none());
    assert_eq!(
        d.rejected[0].reason.as_deref(),
        Some("when fails: buffer.modified (buffer.modified is false)")
    );
    let d = k.describe_key("normal", &parse_keys("<Space>").unwrap(), &cx(false));
    assert!(d.is_prefix);
}

#[test]
fn which_key_menu() {
    let mut k = keymap();
    let c = cx(true);
    k.feed(Key::char(' '), "normal", &c, t0());
    let menu = k.menu("normal", &c);
    let rows: Vec<_> = menu
        .iter()
        .map(|m| format!("{}={}", m.key, m.label))
        .collect();
    assert_eq!(rows, ["q=test.quit", "w=test.write"]);
    k.reset();
    let root = k.menu("normal", &c);
    assert!(
        root.iter().take_while(|m| m.group).count() >= 3,
        "groups first: {root:?}"
    );
    assert!(
        root.iter()
            .any(|m| m.key == "g" && m.group && m.label == "test.g")
    );
    assert!(root.iter().any(|m| m.key == "space" && m.label == "more"));
}

#[test]
fn esc_cancels_pending() {
    let mut k = keymap();
    let c = cx(false);
    let (_, s) = typ(&mut k, "normal", "5<Space><Esc>", &c);
    assert_eq!(s, Status::Cancelled);
    assert!(!k.is_pending());
    // Esc bound in insert mode runs its binding.
    assert_eq!(typ(&mut k, "insert", "<Esc>", &c).0, ["editor.set_mode"]);
}

#[test]
fn sticky_group_repeats_until_esc() {
    let mut k = keymap();
    let c = cx(false);
    let (ran, s) = typ(&mut k, "normal", "<C-w>>>>", &c);
    assert_eq!(ran, ["test.wider"; 3]);
    assert_eq!(s, Status::Pending);
    assert_eq!(k.pending_keys(), "ctrl+w");
    let (_, s) = typ(&mut k, "normal", "<Esc>", &c);
    assert_eq!(s, Status::Cancelled);
    assert_eq!(typ(&mut k, "normal", "j", &c).0, ["editor.move_down"]);
}

#[test]
fn fallback_self_insert() {
    let mut k = keymap();
    let now = t0();
    let f = k.feed(Key::char('é'), "insert", &cx(false), now);
    let invs = f.run[0].invocations(&Actor::User);
    assert!(
        matches!(&invs[0].command, spur_core::Target::Id(id) if id.as_str() == "editor.insert_text")
    );
    assert_eq!(*invs[0].args, json!({"text": "é"}));
    // Bound keys still win.
    assert_eq!(
        typ(&mut k, "insert", "<C-s>", &cx(false)).0,
        ["buffer.save"]
    );
}

#[test]
fn layers_override_and_unbind() {
    let mut k = keymap();
    let user = r#"
[[bind]]
mode = "normal"
keys = "j"
command = "user.down"

[[bind]]
mode = "normal"
keys = "<C-s>"
unbind = true
"#;
    let src = Source::Config {
        file: "keys.toml".into(),
        line: 1,
    };
    k.push_layer(Layer::from_toml("user", src, user).unwrap());
    let c = cx(false);
    assert_eq!(typ(&mut k, "normal", "j", &c).0, ["user.down"]);
    assert!(typ(&mut k, "normal", "<C-s>", &c).0.is_empty());
    assert_eq!(typ(&mut k, "insert", "<C-s>", &c).0, ["buffer.save"]);
    let d = k.describe_key("normal", &parse_keys("j").unwrap(), &c);
    assert_eq!(&*d.chosen.unwrap().layer, "user");
    assert_eq!(
        d.rejected[0].reason.as_deref(),
        Some("a higher binding applies")
    );
    let kinds: Vec<_> = k
        .conflicts()
        .iter()
        .map(|c| (c.kind, c.keys.clone()))
        .collect();
    assert!(kinds.contains(&(ConflictKind::LayerMask, "j".into())));
    assert!(kinds.contains(&(ConflictKind::PrefixShadow, "g".into())));
    assert_eq!(
        k.bindings_of(&CommandId::new("editor.move_down").unwrap())
            .len(),
        1
    );
    assert!(k.remove_layer("user"));
    assert_eq!(typ(&mut k, "normal", "j", &c).0, ["editor.move_down"]);
}

#[test]
fn bad_layers_are_reported() {
    for (toml, needle) in [
        (
            "[[bind]]\nmode='n'\nkeys='<nope>'\ncommand='a.b'",
            "unknown key",
        ),
        ("[[bind]]\nmode='n'\nkeys='x'\ncommand='bad'", "namespace"),
        (
            "[[bind]]\nmode='n'\nkeys='x'\ncommand='a.b'\nwhen='(('",
            "when",
        ),
        ("[[bind]]\nmode='n'\nkeys='x'", "exactly one"),
        (
            "[[bind]]\nmode='n'\nkeys='x'\ncommand='a.b'\nbogus=1",
            "unknown field",
        ),
    ] {
        let e = Layer::from_toml("t", Source::Core, toml).unwrap_err();
        assert!(e.to_string().contains(needle), "{e}");
    }
}

#[test]
fn duplicate_in_one_layer_reported() {
    let mut k = Keymap::new();
    let toml =
        "[[bind]]\nmode='n'\nkeys='x'\ncommand='a.b'\n[[bind]]\nmode='n'\nkeys='x'\ncommand='a.c'";
    k.push_layer(Layer::from_toml("t", Source::Core, toml).unwrap());
    assert_eq!(k.conflicts()[0].kind, ConflictKind::Duplicate);
}

/// Feeds keys in `mode` of a keymap built from `toml`; returns what ran, as
/// `command` or `command:text` for typed text, then the last status.
fn feed_all(k: &mut Keymap, mode: &str, keys: &str, now: Instant) -> (Vec<String>, Status) {
    let c = cx(false);
    let mut ran = Vec::new();
    let mut status = Status::Idle;
    for key in parse_keys(keys).unwrap() {
        let f = k.feed(key, mode, &c, now);
        for d in f.run {
            ran.push(describe(&d));
        }
        status = f.status;
    }
    (ran, status)
}

fn describe(d: &spur_keymap::Dispatch) -> String {
    let cmd = d.steps[0].command.to_string();
    match d.steps[0].args.get("text").and_then(|t| t.as_str()) {
        Some(t) => format!("{cmd}:{t}"),
        None => cmd,
    }
}

fn km(toml: &str) -> Keymap {
    let mut k = Keymap::new();
    k.push_layer(Layer::from_toml("t", Source::Core, toml).unwrap());
    k
}

const TYPING: &str = r#"
[mode.insert]
fallback = "editor.insert_text"

[[bind]]
mode = "insert"
keys = "jk"
command = "x.leave"

[[bind]]
mode = "insert"
keys = "a"
command = "x.a"
when = "buffer.modified"

[[bind]]
mode = "insert"
keys = "<Esc>"
command = "x.esc"
"#;

#[test]
fn typing_mode_never_loses_keys() {
    let now = t0();
    let mut k = km(TYPING);
    // A prefix that leads nowhere is typed, then the next key is handled afresh.
    assert_eq!(
        feed_all(&mut k, "insert", "jx", now).0,
        ["editor.insert_text:j", "editor.insert_text:x"]
    );
    assert_eq!(
        feed_all(&mut k, "insert", "j<Esc>", now).0,
        ["editor.insert_text:j", "x.esc"]
    );
    // A pending prefix is typed when its timeout passes.
    let (ran, status) = feed_all(&mut k, "insert", "j", now);
    assert!(ran.is_empty());
    assert_eq!(status, Status::Pending);
    let due = k.deadline().expect("typing prefix has a deadline");
    assert_eq!(describe(&k.tick(due).unwrap()), "editor.insert_text:j");
    // A key whose binding's `when` fails types itself.
    assert_eq!(
        feed_all(&mut k, "insert", "a", now).0,
        ["editor.insert_text:a"]
    );
    assert_eq!(feed_all(&mut k, "insert", "jk", now).0, ["x.leave"]);
}

#[test]
fn dead_exact_child_does_not_hide_char_capture() {
    let mut k = km(r#"
[[bind]]
mode = "n"
keys = "f<char>"
command = "x.find"

[[bind]]
mode = "n"
keys = "fa"
command = "x.fa"
when = "buffer.modified"
"#);
    let c = cx(false);
    let now = t0();
    k.feed(parse_keys("f").unwrap()[0], "n", &c, now);
    let f = k.feed(parse_keys("a").unwrap()[0], "n", &c, now);
    assert_eq!(f.run[0].steps[0].command.as_str(), "x.find");
    assert_eq!(f.run[0].chars, ['a']);
}

#[test]
fn unbind_leaves_no_phantom_prefix() {
    let mut k = km(r#"
[[bind]]
mode = "n"
keys = "g"
command = "x.g"

[[bind]]
mode = "n"
keys = "gg"
command = "x.gg"

[[bind]]
mode = "n"
keys = "x"
command = "x.x"
"#);
    k.push_layer(
        Layer::from_toml(
            "user",
            Source::Core,
            r#"
[[bind]]
mode = "n"
keys = "gg"
unbind = true

[[bind]]
mode = "n"
keys = "xy"
unbind = true
"#,
        )
        .unwrap(),
    );
    let now = t0();
    assert_eq!(feed_all(&mut k, "n", "g", now).0, ["x.g"]);
    assert_eq!(feed_all(&mut k, "n", "x", now).0, ["x.x"]);
    assert!(k.deadline().is_none());
    assert!(
        !k.conflicts()
            .iter()
            .any(|c| c.kind == spur_keymap::ConflictKind::PrefixShadow)
    );
}

#[test]
fn esc_after_ambiguous_prefix_cancels() {
    let mut k = km(r#"
[[bind]]
mode = "n"
keys = "g"
command = "x.g"

[[bind]]
mode = "n"
keys = "gg"
command = "x.gg"
"#);
    let (ran, status) = feed_all(&mut k, "n", "g<Esc>", t0());
    assert!(ran.is_empty(), "{ran:?}");
    assert_eq!(status, Status::Cancelled);
}

#[test]
fn higher_layer_can_turn_options_off() {
    let mut k = km("[mode.n]\ncounts = true\n");
    k.push_layer(
        Layer::from_toml(
            "user",
            Source::Core,
            "[mode.n]\ncounts = false\n\n[[bind]]\nmode = \"n\"\nkeys = \"3\"\ncommand = \"x.three\"\n",
        )
        .unwrap(),
    );
    assert_eq!(feed_all(&mut k, "n", "3", t0()).0, ["x.three"]);
}

#[test]
fn sticky_group_ends_when_the_mode_changes() {
    let mut k = km(r#"
[mode.i]
fallback = "editor.insert_text"

[[group]]
mode = "n"
keys = "<C-w>"
label = "+w"
sticky = true

[[bind]]
mode = "n"
keys = "<C-w>i"
command = "x.to_insert"
"#);
    let now = t0();
    assert_eq!(feed_all(&mut k, "n", "<C-w>i", now).0, ["x.to_insert"]);
    assert_eq!(feed_all(&mut k, "i", "a", now).0, ["editor.insert_text:a"]);
}

#[test]
fn layer_validation() {
    for bad in [
        "[[bind]]\nmode = \"\"\nkeys = \"a\"\ncommand = \"x.a\"\n",
        "[[bind]]\nmode = \"n\"\nkeys = \"a\"\nrun = [{ command = \"x.a\" }]\nargs = { y = 1 }\n",
        "[[bind]]\nmode = \"n\"\nkeys = \"a\"\nunbind = true\nargs = { y = 1 }\n",
        "[[bind]]\nmode = \"n\"\nkeys = \"<C-char>\"\ncommand = \"x.a\"\n",
        "[[group]]\nmode = \" \"\nkeys = \"a\"\nlabel = \"g\"\n",
    ] {
        assert!(Layer::from_toml("t", Source::Core, bad).is_err(), "{bad}");
    }
}
