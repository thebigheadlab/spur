//! Property tests: change-set algebra, rebasing, undo and anchors against simple models.

use proptest::prelude::*;
use spur_text::{
    AuthorId, Bias, Buffer, Change, ChangeSet, Snapshot, Transaction, UndoLane, UndoScope, Version,
};

fn apply_str(text: &str, cs: &ChangeSet) -> String {
    let mut out = String::new();
    let mut pos = 0;
    for h in cs.hunks() {
        out.push_str(&text[pos..h.range.start]);
        out.push_str(&h.text);
        pos = h.range.end;
    }
    out.push_str(&text[pos..]);
    out
}

fn full(s: &Snapshot) -> String {
    s.slice(0..s.len()).unwrap()
}

/// Random change set over an ASCII text of `len` bytes, from raw (offset, len, text) picks.
fn changeset(len: usize, picks: &[(usize, usize, String)]) -> ChangeSet {
    let mut hunks: Vec<Change> = Vec::new();
    let mut cuts: Vec<(usize, usize, &String)> = picks
        .iter()
        .map(|(o, l, t)| {
            let start = o % (len + 1);
            (start, (start + l % 4).min(len), t)
        })
        .collect();
    cuts.sort_by_key(|c| (c.0, c.1));
    let mut last_end = None;
    for (start, end, t) in cuts {
        if last_end.is_some_and(|e| start <= e) {
            continue;
        }
        hunks.push(Change::new(start..end, t.as_str()));
        last_end = Some(end);
    }
    ChangeSet::new(len, hunks).unwrap()
}

fn picks() -> impl Strategy<Value = Vec<(usize, usize, String)>> {
    prop::collection::vec((0usize..64, 0usize..8, "[a-c\n]{0,3}"), 0..6)
}

/// Applies committed `a` then incoming `b` (same base) the way the rebase rule says:
/// at the same start offset, committed hunks go first.
fn merged(text: &str, a: &ChangeSet, b: &ChangeSet) -> String {
    let mut all: Vec<(usize, u8, &Change)> = a
        .hunks()
        .iter()
        .map(|h| (h.range.start, 0, h))
        .chain(b.hunks().iter().map(|h| (h.range.start, 1, h)))
        .collect();
    all.sort_by_key(|x| (x.0, x.1));
    let mut out = String::new();
    let mut pos = 0;
    for (_, _, h) in all {
        out.push_str(&text[pos.min(h.range.start)..h.range.start]);
        out.push_str(&h.text);
        pos = pos.max(h.range.end);
    }
    out.push_str(&text[pos..]);
    out
}

proptest! {
    #[test]
    fn compose_equals_sequential(base in "[a-z\n]{0,40}", p1 in picks(), p2 in picks()) {
        let a = changeset(base.len(), &p1);
        let mid = apply_str(&base, &a);
        let b = changeset(mid.len(), &p2);
        prop_assert_eq!(apply_str(&base, &a.compose(&b)), apply_str(&mid, &b));
    }

    #[test]
    fn rebase_equals_merged_application(base in "[a-z\n]{0,40}", p1 in picks(), p2 in picks()) {
        let a = changeset(base.len(), &p1);
        let b = changeset(base.len(), &p2);
        if let Ok(b2) = b.rebase(&a) {
            prop_assert_eq!(apply_str(&apply_str(&base, &a), &b2), merged(&base, &a, &b));
        }
    }

    #[test]
    fn buffer_matches_string_model(base in "[a-zé😀\r\n]{0,30}", edits in prop::collection::vec(picks(), 0..12)) {
        let mut buf = Buffer::new(&base);
        let mut model = base.clone();
        for p in &edits {
            // Snap random offsets to char boundaries of the model.
            let cs = changeset(model.len(), p);
            let ok = cs.hunks().iter().all(|h| model.is_char_boundary(h.range.start) && model.is_char_boundary(h.range.end));
            let res = buf.apply(Transaction::new(AuthorId::User, buf.version(), cs.clone()));
            prop_assert_eq!(res.is_ok(), ok);
            if ok { model = apply_str(&model, &cs); }
        }
        let snap = buf.snapshot();
        prop_assert_eq!(full(&snap), model.clone());
        let lines = model.replace("\r\n", "\n").matches(['\r', '\n']).count() + 1;
        prop_assert_eq!(snap.line_count(), lines);
        for (i, _) in model.char_indices() {
            prop_assert_eq!(snap.byte_to_utf16(i), Some(model[..i].encode_utf16().count()));
            prop_assert_eq!(snap.byte_to_char(i), Some(model[..i].chars().count()));
        }
    }

    #[test]
    fn undo_all_then_redo_all_round_trips(base in "[a-z\n]{0,20}", edits in prop::collection::vec(picks(), 1..10)) {
        let mut buf = Buffer::new(&base);
        let mut states = vec![base.clone()];
        for p in &edits {
            let cs = changeset(buf.len(), p);
            if cs.is_empty() { continue; }
            buf.apply(Transaction::new(AuthorId::User, buf.version(), cs)).unwrap();
            states.push(full(&buf.snapshot()));
        }
        for expected in states.iter().rev().skip(1) {
            buf.undo(&AuthorId::User.into()).unwrap().unwrap();
            prop_assert_eq!(&full(&buf.snapshot()), expected);
        }
        prop_assert!(buf.undo(&AuthorId::User.into()).unwrap().is_none());
        for expected in states.iter().skip(1) {
            buf.redo(&AuthorId::User.into()).unwrap().unwrap();
            prop_assert_eq!(&full(&buf.snapshot()), expected);
        }
    }

    /// User types lowercase, the agent uppercase; both only insert. Undoing the user's
    /// edits never changes the agent's text, and undoing all of them leaves only it.
    #[test]
    fn undo_mine_never_touches_others(ops in prop::collection::vec((any::<bool>(), 0usize..64, "[a-c]{1,3}"), 1..16)) {
        let mut buf = Buffer::new("");
        for (is_user, at, s) in &ops {
            let at = at % (buf.len() + 1);
            let (author, s) = if *is_user { (AuthorId::User, s.clone()) } else { (AuthorId::Agent("x".into()), s.to_uppercase()) };
            buf.apply(Transaction::new(author, buf.version(), ChangeSet::insert(buf.len(), at, &s).unwrap())).unwrap();
        }
        let upper = |t: String| t.chars().filter(char::is_ascii_uppercase).collect::<String>();
        let agent_text = upper(full(&buf.snapshot()));
        loop {
            match buf.undo(&AuthorId::User.into()) {
                Ok(Some(_)) => prop_assert_eq!(upper(full(&buf.snapshot())), agent_text.clone()),
                Ok(None) => {
                    prop_assert_eq!(full(&buf.snapshot()), agent_text.clone());
                    break;
                }
                Err(_) => break, // the agent inserted inside the user's text: refused, nothing changed
            }
        }
    }

    #[test]
    fn lazy_and_tracked_anchors_agree(base in "[a-z]{1,20}", edits in prop::collection::vec(picks(), 0..10), at in 0usize..20, right in any::<bool>()) {
        let mut buf = Buffer::new(&base);
        let at = at % (base.len() + 1);
        let bias = if right { Bias::Right } else { Bias::Left };
        let lazy = buf.anchor(at, bias);
        let id = buf.track(at, bias);
        for p in &edits {
            let cs = changeset(buf.len(), p);
            buf.apply(Transaction::new(AuthorId::User, buf.version(), cs)).unwrap();
        }
        prop_assert_eq!(buf.resolve(lazy).ok(), buf.tracked(id));
        prop_assert!(buf.version() >= Version(0));
    }

    /// Random edits, undos and redos from two authors (inserts only, disjoint alphabets).
    /// Whatever succeeds never changes the other author's text, and an undo directly
    /// followed by a redo (or the reverse) round-trips without a conflict.
    #[test]
    fn multi_author_undo_redo(ops in prop::collection::vec((0u8..6, 0usize..64, "[a-c]{1,3}"), 1..40)) {
        let agent = AuthorId::Agent("x".into());
        let mut buf = Buffer::new("");
        let only = |t: &str, upper: bool| t.chars().filter(|c| c.is_ascii_uppercase() == upper).collect::<String>();
        for (op, at, s) in &ops {
            let before = full(&buf.snapshot());
            let (author, other_upper) = if op % 2 == 0 { (AuthorId::User, true) } else { (agent.clone(), false) };
            match op / 2 {
                0 => {
                    let at = at % (buf.len() + 1);
                    let s = if op % 2 == 0 { s.clone() } else { s.to_uppercase() };
                    buf.apply(Transaction::new(author, buf.version(), ChangeSet::insert(buf.len(), at, &s).unwrap())).unwrap();
                }
                kind => {
                    let first = if kind == 1 { buf.undo(&author.clone().into()) } else { buf.redo(&author.clone().into()) };
                    if let Ok(Some(_)) = first {
                            let mid = full(&buf.snapshot());
                            prop_assert_eq!(only(&mid, other_upper), only(&before, other_upper));
                            // Undo/redo (or redo/undo) twice: each step must succeed.
                            for _ in 0..2 {
                                let back = if kind == 1 { buf.redo(&author.clone().into()) } else { buf.undo(&author.clone().into()) };
                                prop_assert!(matches!(back, Ok(Some(_))), "round trip failed: {:?}", back);
                                prop_assert_eq!(full(&buf.snapshot()), before.clone());
                                let again = if kind == 1 { buf.undo(&author.clone().into()) } else { buf.redo(&author.clone().into()) };
                                prop_assert!(matches!(again, Ok(Some(_))), "second step failed: {:?}", again);
                                prop_assert_eq!(full(&buf.snapshot()), mid.clone());
                            }
                            let back = if kind == 1 { buf.redo(&author.clone().into()) } else { buf.undo(&author.clone().into()) };
                            prop_assert!(matches!(back, Ok(Some(_))), "final step failed: {:?}", back);
                            prop_assert_eq!(full(&buf.snapshot()), before);
                    } else {
                        prop_assert_eq!(full(&buf.snapshot()), before);
                    }
                }
            }
        }
    }

    /// Random edits, undos and redos from three scopes: two lanes of the user and an
    /// agent, each typing its own letters. An undo or redo never changes another
    /// scope's letters, a refused one changes nothing, and a scope with an undo to redo
    /// can always redo it (or gets a conflict), whatever the others undid meanwhile.
    #[test]
    fn lanes_undo_and_redo_independently(ops in prop::collection::vec((0u8..9, 0usize..64, "[a-c]{1,3}"), 1..50)) {
        let scopes = [
            UndoScope::new(AuthorId::User, Some(UndoLane(1))),
            UndoScope::new(AuthorId::User, Some(UndoLane(2))),
            UndoScope::from(AuthorId::Agent("x".into())),
        ];
        // Scope k types "abc" shifted by 3k: abc, def, ghi.
        let letters = |s: &str, k: usize| -> String {
            s.chars().map(|c| ["abc", "def", "ghi"][k].chars().nth(c as usize - 'a' as usize).unwrap()).collect()
        };
        let owner = |c: char| (c as usize - 'a' as usize) / 3;
        let others = |t: &str, k: usize| t.chars().filter(|&c| owner(c) != k).collect::<String>();
        let mut pending = [0usize; 3];
        let mut buf = Buffer::new("");
        for (op, at, s) in &ops {
            let (k, kind) = (usize::from(op % 3), op / 3);
            let scope = &scopes[k];
            let before = full(&buf.snapshot());
            if kind == 0 {
                let at = at % (buf.len() + 1);
                let mut t = Transaction::new(scope.author.clone(), buf.version(), ChangeSet::insert(buf.len(), at, &letters(s, k)).unwrap());
                if let Some(l) = scope.lane {
                    t = t.with_lane(l);
                }
                buf.apply(t).unwrap();
                pending[k] = 0;
                continue;
            }
            let r = if kind == 1 { buf.undo(scope) } else { buf.redo(scope) };
            match r {
                Ok(Some(_)) => {
                    prop_assert_eq!(others(&full(&buf.snapshot()), k), others(&before, k));
                    if kind == 1 { pending[k] += 1 } else { pending[k] -= 1 }
                }
                Ok(None) => {
                    prop_assert!(kind == 1 || pending[k] == 0, "redo lost for scope {}", k);
                    prop_assert_eq!(full(&buf.snapshot()), before);
                }
                Err(_) => prop_assert_eq!(full(&buf.snapshot()), before),
            }
        }
    }
}
