//! Spur's own default experience: one mode, the Ask card, find, grain and leaving.

use std::time::Instant;

use spur_core::editor::AskKind;
use spur_keymap::{Key, Session, parse_keys};
use spur_text::{Buffer, SelRange, Selection};

fn session(text: &str, at: usize) -> Session {
    let mut s = Session::new();
    let ed = &mut s.core.state;
    let doc = ed.add_doc(Buffer::new(text), None);
    let view = ed.add_view(doc);
    ed.focus = view;
    ed.views.get_mut(&view).unwrap().selection = Selection {
        ranges: vec![SelRange::cursor(at)],
        primary: 0,
    };
    s
}

/// Presses keys in Spur notation.
fn press(s: &mut Session, keys: &str) {
    let now = Instant::now();
    for k in parse_keys(keys).unwrap_or_else(|e| panic!("{keys}: {e}")) {
        s.key(k, now);
    }
}

/// Types text, one char at a time.
fn typ(s: &mut Session, text: &str) {
    let now = Instant::now();
    for c in text.chars() {
        s.key(Key::char(c), now);
    }
}

fn text(s: &Session) -> String {
    let snap = s.core.state.snapshot();
    snap.slice(0..snap.len()).unwrap()
}

fn selected(s: &Session) -> String {
    let r = s.core.state.primary();
    let snap = s.core.state.snapshot();
    snap.slice(r.anchor.min(r.head)..r.anchor.max(r.head))
        .unwrap()
}

#[test]
fn typing_types_in_the_one_mode() {
    let mut s = Session::new();
    assert_eq!(&*s.mode(), "edit");
    typ(&mut s, "hi");
    press(&mut s, "enter");
    typ(&mut s, "there");
    press(&mut s, "backspace left ctrl+z");
    // Moving closed the undo step, so undo removes the whole run of typing.
    assert_eq!(text(&s), "");
    assert_eq!(&*s.mode(), "edit");
}

#[test]
fn ask_runs_a_command_by_name() {
    let mut s = session("one two", 0);
    press(&mut s, "ctrl+space");
    assert_eq!(&*s.mode(), "ask");
    let ask = s.core.state.ask.clone().unwrap();
    assert_eq!(ask.kind, AskKind::Ask);
    assert!(!ask.items.is_empty(), "featured rows before typing");
    typ(&mut s, "select every");
    assert_eq!(
        s.core.state.ask.as_ref().unwrap().items[0].title,
        "Select everything"
    );
    press(&mut s, "enter");
    assert_eq!(&*s.mode(), "edit");
    assert!(s.core.state.ask.is_none());
    assert_eq!(selected(&s), "one two");
}

#[test]
fn ask_shows_key_hints() {
    let mut s = session("x", 0);
    press(&mut s, "ctrl+space");
    typ(&mut s, "save");
    let items = &s.core.state.ask.as_ref().unwrap().items;
    let save = items.iter().find(|i| i.title == "Save").unwrap();
    assert_eq!(save.hint, "ctrl+s");
}

#[test]
fn ask_number_goes_to_line() {
    let mut s = session("a\n  b\nc", 0);
    press(&mut s, "ctrl+space");
    typ(&mut s, "2");
    press(&mut s, "enter");
    assert_eq!(s.core.state.primary().head, 4);
}

#[test]
fn ask_text_offers_find() {
    let mut s = session("alpha beta", 0);
    press(&mut s, "ctrl+space");
    typ(&mut s, "beta");
    let items = s.core.state.ask.as_ref().unwrap().items.clone();
    let i = items
        .iter()
        .position(|i| i.title.starts_with("Find"))
        .unwrap();
    for _ in 0..i {
        press(&mut s, "down");
    }
    press(&mut s, "enter");
    // Find opened with the text and selected the match.
    assert_eq!(s.core.state.ask.as_ref().unwrap().kind, AskKind::Find);
    assert_eq!(selected(&s), "beta");
}

#[test]
fn esc_closes_ask_and_backspace_edits() {
    let mut s = session("x", 0);
    press(&mut s, "ctrl+space");
    typ(&mut s, "ab");
    press(&mut s, "backspace");
    assert_eq!(s.core.state.ask.as_ref().unwrap().text, "a");
    press(&mut s, "esc");
    assert!(s.core.state.ask.is_none());
    assert_eq!(text(&s), "x");
}

#[test]
fn find_lights_up_and_steps() {
    let mut s = session("greet(a)\ngreet(b)\nGreet(c)", 0);
    press(&mut s, "ctrl+f");
    typ(&mut s, "greet");
    let ask = s.core.state.ask.clone().unwrap();
    assert_eq!(ask.note.as_deref(), Some("1 of 3"));
    assert_eq!(s.core.state.viewport(10).highlights.len(), 3);
    press(&mut s, "enter");
    assert_eq!(s.core.state.primary().anchor, 9);
    assert_eq!(
        s.core.state.ask.as_ref().unwrap().note.as_deref(),
        Some("2 of 3")
    );
    press(&mut s, "esc");
    // The match stays selected, and f3 goes on from there.
    assert_eq!(selected(&s), "greet");
    press(&mut s, "f3");
    assert_eq!(s.core.state.primary().anchor, 18);
    // Typing replaces the found text.
    typ(&mut s, "Hi");
    assert!(text(&s).ends_with("Hi(c)"));
}

#[test]
fn find_with_capitals_matches_case() {
    let mut s = session("greet Greet", 0);
    press(&mut s, "ctrl+f");
    typ(&mut s, "Gr");
    assert_eq!(s.core.state.primary().anchor, 6);
}

#[test]
fn empty_find_returns_to_where_it_started() {
    let mut s = session("abc abc", 5);
    press(&mut s, "ctrl+f");
    typ(&mut s, "a");
    assert_eq!(s.core.state.primary().anchor, 0);
    press(&mut s, "backspace esc");
    assert_eq!(s.core.state.primary().head, 5);
}

#[test]
fn grain_grows_and_shrinks() {
    let mut s = session(
        "fn main() {\n    let names = 1;\n    run(names);\n}\n\nnext",
        21,
    );
    press(&mut s, "alt+up");
    assert_eq!(selected(&s), "names");
    assert_eq!(
        s.core
            .state
            .viewport(10)
            .grain
            .map(spur_core::editor::Grain::name),
        Some("word")
    );
    press(&mut s, "alt+up");
    assert_eq!(selected(&s), "    let names = 1;");
    press(&mut s, "alt+up");
    assert_eq!(
        selected(&s),
        "fn main() {\n    let names = 1;\n    run(names);\n}"
    );
    press(&mut s, "alt+up");
    assert_eq!(selected(&s), text(&s));
    press(&mut s, "alt+down alt+down");
    assert_eq!(selected(&s), "    let names = 1;");
    // Moving starts over.
    press(&mut s, "right alt+up");
    assert!(selected(&s).len() < 20);
}

#[test]
fn leaving_with_unsaved_work_asks() {
    let mut s = session("x", 0);
    typ(&mut s, "y");
    press(&mut s, "ctrl+q");
    assert!(!s.should_quit());
    let ask = s.core.state.ask.clone().unwrap();
    assert_eq!(ask.kind, AskKind::Confirm);
    let titles: Vec<_> = ask.items.iter().map(|i| i.title.as_str()).collect();
    // No path yet: nothing to save to.
    assert_eq!(titles, ["Leave without saving", "Keep editing"]);
    // Typing does nothing on a question.
    typ(&mut s, "z");
    assert_eq!(text(&s), "yx");
    press(&mut s, "down enter");
    assert!(!s.should_quit());
    assert!(s.core.state.ask.is_none());
    press(&mut s, "ctrl+q enter");
    assert!(s.should_quit());
}

#[test]
fn leaving_saved_work_is_immediate() {
    let mut s = session("x", 0);
    press(&mut s, "ctrl+q");
    assert!(s.should_quit());
}

#[test]
fn clipboard_round_trip() {
    let mut s = session("word", 0);
    press(&mut s, "ctrl+a ctrl+c end ctrl+v");
    assert_eq!(text(&s), "wordword");
    press(&mut s, "ctrl+a ctrl+x");
    assert_eq!(text(&s), "");
}

#[test]
fn default_keys_have_no_duplicates() {
    let s = Session::new();
    let dups: Vec<_> = s
        .keymap
        .conflicts()
        .into_iter()
        .filter(|c| c.kind == spur_keymap::ConflictKind::Duplicate)
        .collect();
    assert!(dups.is_empty(), "{dups:?}");
}

#[test]
fn unbound_alt_key_is_esc_then_key() {
    let mut s = session("abc", 0);
    press(&mut s, "ctrl+f");
    typ(&mut s, "b");
    // `esc` and `x` arriving together read as alt+x on most terminals.
    press(&mut s, "alt+x");
    assert!(s.core.state.ask.is_none());
    assert_eq!(text(&s), "axc");
}

#[test]
fn alt_key_on_the_ask_card_stays_on_the_card() {
    let mut s = session("one", 0);
    press(&mut s, "ctrl+space");
    typ(&mut s, "sa");
    press(&mut s, "alt+v");
    assert_eq!(&*s.mode(), "ask");
    assert_eq!(text(&s), "one");
}

#[test]
fn grow_merges_cursors_in_one_word() {
    let mut s = session("hello world", 1);
    let id = s.core.state.focus;
    s.core.state.views.get_mut(&id).unwrap().selection = Selection {
        ranges: vec![SelRange::cursor(1), SelRange::cursor(3)],
        primary: 0,
    };
    press(&mut s, "alt+up");
    assert_eq!(s.core.state.view().selection.ranges.len(), 1);
}

#[test]
fn grow_from_a_blank_line_skips_to_the_file() {
    let mut s = session("aaa\nbbb\n\nccc\nddd", 8);
    press(&mut s, "alt+up");
    assert_eq!(selected(&s), text(&s));
}

#[test]
fn ctrl_end_goes_to_the_very_end() {
    let mut s = session("one\n  two three", 0);
    press(&mut s, "ctrl+end");
    assert_eq!(s.core.state.primary().head, 15);
    press(&mut s, "ctrl+shift+home");
    assert_eq!(selected(&s), text(&s));
}

#[test]
fn find_without_matches_goes_back() {
    let mut s = session("abc abd", 7);
    press(&mut s, "ctrl+f");
    typ(&mut s, "abz");
    assert_eq!(s.core.state.primary().head, 7);
    assert_eq!(selected(&s), "");
    press(&mut s, "esc");
    assert_eq!(s.core.state.primary().head, 7);
    assert!(s.core.state.find_text.is_none());
}

#[test]
fn save_and_leave_saves_every_file() {
    let dir = std::env::temp_dir().join(format!("spur-leave-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
    std::fs::write(&a, "aaa").unwrap();
    std::fs::write(&b, "bbb").unwrap();
    let mut s = Session::new();
    s.command("editor.open", serde_json::json!({ "path": b }))
        .unwrap();
    typ(&mut s, "X");
    s.command("editor.open", serde_json::json!({ "path": a }))
        .unwrap();
    press(&mut s, "ctrl+q");
    let titles: Vec<_> = s
        .core
        .state
        .ask
        .as_ref()
        .unwrap()
        .items
        .iter()
        .map(|i| i.title.clone())
        .collect();
    assert_eq!(titles[0], "Save and leave");
    press(&mut s, "enter");
    assert!(s.should_quit());
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "Xbbb");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn paste_goes_where_typing_goes() {
    let mut s = session("x", 0);
    s.paste("ab\ncd");
    assert_eq!(text(&s), "ab\ncdx");
    press(&mut s, "ctrl+space");
    s.paste("save\nmore");
    assert_eq!(s.core.state.ask.as_ref().unwrap().text, "save");
    assert_eq!(text(&s), "ab\ncdx");
}
