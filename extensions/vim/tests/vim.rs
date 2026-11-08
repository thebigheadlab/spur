//! Vim preset scenarios. Fixture format: text with `|` before the char under the cursor;
//! an expectation without `|` checks the text only. Expected states follow Vim; known
//! deviations live in the preset file.

use std::time::Instant;

use spur_keymap::{Session, parse_keys};
use spur_text::{Buffer, SelRange, Selection};

fn session(fixture: &str) -> Session {
    let at = fixture.find('|').expect("fixture needs a | cursor");
    let text = fixture.replacen('|', "", 1);
    let mut s = spur_vim::session();
    let ed = &mut s.core.state;
    let doc = ed.add_doc(Buffer::new(&text), None);
    let view = ed.add_view(doc);
    ed.focus = view;
    let v = ed.views.get_mut(&view).unwrap();
    v.selection = Selection {
        ranges: vec![SelRange::cursor(at)],
        primary: 0,
    };
    s
}

fn state(s: &Session) -> (String, String) {
    let snap = s.core.state.snapshot();
    let text = snap.slice(0..snap.len()).unwrap();
    let head = s.core.state.primary().head;
    let mut with_cursor = text.clone();
    with_cursor.insert(head, '|');
    (text, with_cursor)
}

fn run(fixture: &str, keys: &str) -> Session {
    let mut s = session(fixture);
    let now = Instant::now();
    for k in parse_keys(keys).unwrap_or_else(|e| panic!("{keys}: {e}")) {
        s.key(k, now);
    }
    // Fire any binding still waiting for a longer one.
    s.tick(now + s.keymap.timeout);
    s
}

#[track_caller]
fn check(fixture: &str, keys: &str, expected: &str, mode: &str) {
    let s = run(fixture, keys);
    let (text, with_cursor) = state(&s);
    let got = if expected.contains('|') {
        with_cursor
    } else {
        text
    };
    assert_eq!(got, expected, "{fixture:?} + {keys:?}");
    assert_eq!(&*s.mode(), mode, "{fixture:?} + {keys:?} mode");
}

#[test]
fn motions() {
    check("|hello world", "w", "hello |world", "normal");
    check("hello |world", "b", "|hello world", "normal");
    check("|hello world", "e", "hell|o world", "normal");
    check("|hello world", "$", "hello worl|d", "normal");
    check("  hel|lo", "^", "  |hello", "normal");
    check("  hel|lo", "0", "|  hello", "normal");
    check("|abc", "l l l l", "ab|c", "normal");
    check("|a\nb\nc", "G", "a\nb\n|c", "normal");
    check("a\nb\n|c", "gg", "|a\nb\nc", "normal");
    check("|a\nb\nc", "2G", "a\n|b\nc", "normal");
    check("|a,b,c", "f,;", "a,b|,c", "normal");
    check("|a,b,c", "t,;", "a,|b,c", "normal");
    check("a,b,|c", "F,", "a,b|,c", "normal");
    check("|(a)", "%", "(a|)", "normal");
    check("|hello\nhi\nworld", "$jj", "hello\nhi\nworl|d", "normal");
}

#[test]
fn operators() {
    check("|hello world", "dw", "|world", "normal");
    check("hello |world foo", "dw", "hello |foo", "normal");
    check("hello |world", "dw", "hello| ", "normal");
    check("foo |bar\nbaz", "dw", "foo| \nbaz", "normal");
    check("|a b c d", "2dw", "|c d", "normal");
    check("|a b c d", "d2w", "|c d", "normal");
    check("|a b c d e f g", "2d2w", "|e f g", "normal");
    check("|hello world", "de", "| world", "normal");
    check("|hello world", "d$", "", "normal");
    check("|one\ntwo\nthree", "dd", "|two\nthree", "normal");
    check("|one\ntwo\nthree", "jdd", "one\n|three", "normal");
    check("|one\ntwo", "Gdd", "|one", "normal");
    check("|one\ntwo\nthree", "dj", "|three", "normal");
    check("|one\ntwo\nthree", "2dd", "|three", "normal");
    check("ab|c", "x", "a|b", "normal");
    check("|abc", "2x", "|c", "normal");
    check("a|bc", "X", "|bc", "normal");
    check("|a,b,c", "dt,", "|,b,c", "normal");
    check("|a,b,c", "df,", "|b,c", "normal");
    check("|a", ">>", "    |a", "normal");
    check("    |a", "<lt><lt>", "|a", "normal");
    check("|Hello", "gUw", "|HELLO", "normal");
    check("|Hello", "guu", "|hello", "normal");
    check("|abc", "rX", "|Xbc", "normal");
    check("|abc", "2rX", "X|Xc", "normal");
    check("|a\n  b", "J", "a| b", "normal");
}

#[test]
fn change_and_insert() {
    check("|foo bar", "cwxy<Esc>", "x|y bar", "normal");
    check("|foo bar", "ciwX<Esc>", "|X bar", "normal");
    check("|foo\nbar", "ccX<Esc>", "|X\nbar", "normal");
    check("|ab", "oX<Esc>", "ab\n|X", "normal");
    check("|ab", "OX<Esc>", "|X\nab", "normal");
    check("  |ab", "oX<Esc>", "  ab\n  |X", "normal");
    check("|ab", "AX<Esc>", "ab|X", "normal");
    check("a|b", "aX<Esc>", "ab|X", "normal");
    check("  |ab", "IX<Esc>", "  |Xab", "normal");
    check("|abc", "Cx<Esc>", "|x", "normal");
    check("|abc", "sX<Esc>", "|Xbc", "normal");
    check("|abc", "i", "|abc", "insert");
}

#[test]
fn text_objects() {
    check("f(|abc, d)", "di(", "f(|)", "normal");
    check("f(a, (|b), c)", "da(", "f(a, |, c)", "normal");
    check("say \"h|i\" ok", "ci\"X<Esc>", "say \"|X\" ok", "normal");
    check("say \"h|i\" ok", "da\"", "say | ok", "normal");
    check("|foo bar", "daw", "|bar", "normal");
    check("x {a |b} y", "diB", "x {|} y", "normal");
    check("|a\nb\n\nc", "dap", "|c", "normal");
}

#[test]
fn visual() {
    check("|hello world", "vex", "| world", "normal");
    check("|a\nb\nc", "Vjd", "|c", "normal");
    check("|hello world", "veU", "|HELLO world", "normal");
    check("|hello world", "vey", "|hello world", "normal");
    check("f(|abc)", "vi(d", "f(|)", "normal");
    check("|abc", "vl<Esc>", "a|bc", "normal");
    check("|abc", "v", "|abc", "visual");
}

#[test]
fn registers_and_paste() {
    check("|one\ntwo", "yyp", "one\n|one\ntwo", "normal");
    check("|a\nb", "ddp", "b\n|a", "normal");
    check("|foo bar", "yiwwP", "foo fo|obar", "normal");
    check("|foo bar", "yiw$p", "foo barfo|o", "normal");
    check("|a b", "\"ayiwyiww\"ap", "a ba", "normal");
    check("|a b", "\"ayiwwyiw\"aP", "a ab", "normal");
    check("|a b", "dwdw\"1p", "", "normal");
    check("|a b", "\"_dwP", "b", "normal");
}

#[test]
fn undo_redo() {
    check("|abc", "xxu", "|bc", "normal");
    check("|abc", "xxuu<C-r>", "|bc", "normal");
    check("|", "ihello<Esc>u", "|", "normal");
    check("|abc", "cwX<Esc>u", "|abc", "normal");
}

#[test]
fn dot_repeat() {
    check("|a b c", "dw.", "|c", "normal");
    check("|a b c d e", "dw3.", "|e", "normal");
    check("|foo bar baz", "cwX<Esc>w.", "X |X baz", "normal");
    check("|a\nb", "A;<Esc>j.", "a;\nb|;", "normal");
    check("|a\nb\nc", "ddj.", "b", "normal");
    check("|abc", "x..", "", "normal");
    // `.` after `.` keeps repeating the original change, and undo takes it back whole.
    check("|foo bar baz", "cwX<Esc>w.u", "X |bar baz", "normal");
}

#[test]
fn macros() {
    check("|a\nb\nc", "qaA!<Esc>jq@a", "a!\nb!\nc", "normal");
    check("|a\nb\nc", "qaA!<Esc>jq2@a", "a!\nb!\nc!", "normal");
    check("|a\nb\nc\nd", "qaA!<Esc>jq@a@@", "a!\nb!\nc!\nd", "normal");
    // Undo after a macro undoes everything the macro changed.
    check("|a\nb\nc", "qaA!<Esc>jq@au", "a!\nb\nc", "normal");
}

#[test]
fn search_and_cmdline() {
    check("|foo bar foo", "/foo<CR>", "foo bar |foo", "normal");
    check("|foo bar foo", "/foo<CR>n", "|foo bar foo", "normal");
    check("foo bar |foo", "?bar<CR>", "foo |bar foo", "normal");
    check("|a\nb", ":2<CR>", "a\n|b", "normal");
    check("|a", ":", "|a", "cmdline");
    check("|a", ":q<Esc>", "|a", "normal");
    let s = run("|a", ":q<CR>");
    assert!(s.should_quit());
    let s = run("|a", "x:q<CR>");
    assert!(!s.should_quit(), "unsaved changes block :q");
    assert!(s.core.state.message.as_ref().is_some_and(|m| m.error));
    let s = run("|a", "x:q!<CR>");
    assert!(s.should_quit());
    check("|a\nb", ":editor.move_down<CR>", "a\n|b", "normal");
}

#[test]
fn cancel() {
    check("|abc", "d<Esc>x", "|bc", "normal");
    check("|abc", "5<Esc>x", "|bc", "normal");
}

#[test]
fn preset_has_no_duplicate_bindings() {
    let s = spur_vim::session();
    let dups: Vec<_> = s
        .keymap
        .conflicts()
        .into_iter()
        .filter(|c| c.kind == spur_keymap::ConflictKind::Duplicate)
        .collect();
    assert!(dups.is_empty(), "{dups:?}");
}

#[test]
fn motions_without_a_target_do_nothing() {
    check("|abc def", "dfz", "|abc def", "normal");
    check("|abc def", "dtz", "|abc def", "normal");
    check("|abc def", "d%", "|abc def", "normal");
    // A failing motion drops the operator.
    check("|abc", "dnx", "|bc", "normal");
}

#[test]
fn paste_is_recorded_and_respects_the_command_line() {
    let mut s = session("|a\nb");
    let now = Instant::now();
    for k in parse_keys("i").unwrap() {
        s.key(k, now);
    }
    s.paste("P");
    for k in parse_keys("<Esc>j0.").unwrap() {
        s.key(k, now);
    }
    assert_eq!(state(&s).0, "Pa\nPb");
    for k in parse_keys(":").unwrap() {
        s.key(k, now);
    }
    s.paste("w");
    assert_eq!(state(&s).0, "Pa\nPb");
    assert_eq!(s.core.state.line_prompt.as_deref(), Some(":w"));
}
