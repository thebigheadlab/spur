//! Scenario tests for the buffer API.

use spur_text::{
    AnchorError, AuthorId, Bias, Buffer, Change, ChangeSet, EditError, GroupKey, Point, Selection,
    Snapshot, Transaction, UndoError, Version,
};

fn text(s: &Snapshot) -> String {
    s.slice(0..s.len()).unwrap()
}

fn agent() -> AuthorId {
    AuthorId::Agent("a1".into())
}

fn insert(buf: &Buffer, author: AuthorId, base: Version, at: usize, s: &str) -> Transaction {
    Transaction::new(author, base, ChangeSet::insert(buf.len(), at, s).unwrap())
}

#[test]
fn apply_bumps_version_and_snapshots_are_stable() {
    let mut buf = Buffer::new("hello");
    let before = buf.snapshot();
    let txn = insert(&buf, AuthorId::User, buf.version(), 5, " world");
    let applied = buf.apply(txn).unwrap();
    assert_eq!(applied.version, Version(1));
    assert_eq!(text(&buf.snapshot()), "hello world");
    assert_eq!(text(&before), "hello");
    assert_eq!(before.version(), Version(0));
}

#[test]
fn stale_edit_is_rebased() {
    let mut buf = Buffer::new("fn main() {}\n");
    let v0 = buf.version();
    // Agent computes an edit against v0 ...
    let agent_txn = Transaction::new(
        agent(),
        v0,
        ChangeSet::replace(13, 11..11, " body ").unwrap(),
    );
    // ... while the user types at the start.
    buf.apply(insert(&buf, AuthorId::User, v0, 0, "pub "))
        .unwrap();
    let applied = buf.apply(agent_txn).unwrap();
    assert_eq!(applied.changes.hunks()[0].range, 15..15);
    assert_eq!(text(&buf.snapshot()), "pub fn main() { body }\n");
}

#[test]
fn overlapping_stale_edit_is_rejected_whole() {
    let mut buf = Buffer::new("abcdefgh");
    let v0 = buf.version();
    buf.apply(Transaction::new(
        AuthorId::User,
        v0,
        ChangeSet::replace(8, 2..4, "XY").unwrap(),
    ))
    .unwrap();
    let stale = ChangeSet::new(8, [Change::new(0..1, "0"), Change::new(3..5, "")]).unwrap();
    let err = buf.apply(Transaction::new(agent(), v0, stale)).unwrap_err();
    let EditError::Stale { conflicts } = err else {
        panic!("{err:?}")
    };
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].incoming.range, 3..5);
    assert_eq!(conflicts[0].committed.range, 2..4);
    assert_eq!(conflicts[0].committed_version, Version(1));
    assert_eq!(text(&buf.snapshot()), "abXYefgh");
    assert_eq!(buf.version(), Version(1));
}

#[test]
fn too_old_and_future_bases() {
    let mut buf = Buffer::with_log_capacity("", 2);
    for i in 0..3 {
        let t = insert(&buf, AuthorId::User, buf.version(), i, "x");
        buf.apply(t).unwrap();
    }
    assert_eq!(buf.oldest_version(), Version(1));
    let old = Transaction::new(agent(), Version(0), ChangeSet::insert(0, 0, "y").unwrap());
    assert!(matches!(buf.apply(old), Err(EditError::TooOld { .. })));
    let future = Transaction::new(agent(), Version(9), ChangeSet::insert(3, 0, "y").unwrap());
    assert!(matches!(
        buf.apply(future),
        Err(EditError::FutureVersion { .. })
    ));
    let ok = Transaction::new(agent(), Version(1), ChangeSet::insert(1, 0, "y").unwrap());
    buf.apply(ok).unwrap();
    assert_eq!(text(&buf.snapshot()), "yxxx");
}

#[test]
fn rejects_offsets_inside_chars() {
    let mut buf = Buffer::new("é");
    let t = Transaction::new(
        AuthorId::User,
        buf.version(),
        ChangeSet::insert(2, 1, "x").unwrap(),
    );
    assert_eq!(
        buf.apply(t).unwrap_err(),
        EditError::NotCharBoundary { offset: 1 }
    );
    let t = Transaction::new(
        AuthorId::User,
        buf.version(),
        ChangeSet::insert(5, 1, "x").unwrap(),
    );
    assert!(matches!(
        buf.apply(t),
        Err(EditError::LengthMismatch { .. })
    ));
}

#[test]
fn undo_redo_single_author_restores_selections() {
    let mut buf = Buffer::new("ab");
    let t = insert(&buf, AuthorId::User, buf.version(), 1, "X")
        .with_selections(Selection::cursor(1), Selection::cursor(2));
    buf.apply(t).unwrap();
    let undone = buf.undo(&AuthorId::User).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "ab");
    assert_eq!(undone.selection, Some(Selection::cursor(1)));
    assert!(buf.undo(&AuthorId::User).unwrap().is_none());
    let redone = buf.redo(&AuthorId::User).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "aXb");
    assert_eq!(redone.selection, Some(Selection::cursor(2)));
    assert!(buf.redo(&AuthorId::User).unwrap().is_none());
    assert_eq!(buf.version(), Version(3));
}

#[test]
fn undo_mine_keeps_other_authors_text() {
    let mut buf = Buffer::new("line\n");
    buf.apply(insert(&buf, AuthorId::User, buf.version(), 0, "user "))
        .unwrap();
    buf.apply(insert(&buf, agent(), buf.version(), 10, "AGENT\n"))
        .unwrap();
    assert_eq!(text(&buf.snapshot()), "user line\nAGENT\n");
    buf.undo(&AuthorId::User).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "line\nAGENT\n");
    // Nothing more of the user's to undo.
    assert!(buf.undo(&AuthorId::User).unwrap().is_none());
    buf.redo(&AuthorId::User).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "user line\nAGENT\n");
    // The agent can undo its own edit independently.
    buf.undo(&agent()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "user line\n");
}

#[test]
fn undo_refuses_when_another_author_edited_inside() {
    let mut buf = Buffer::new("");
    buf.apply(insert(&buf, AuthorId::User, buf.version(), 0, "hello"))
        .unwrap();
    buf.apply(insert(&buf, agent(), buf.version(), 2, "ZZ"))
        .unwrap();
    let err = buf.undo(&AuthorId::User).unwrap_err();
    let UndoError::Conflict { conflicts } = err;
    assert_eq!(conflicts[0].committed_version, Version(2));
    assert_eq!(text(&buf.snapshot()), "heZZllo");
}

#[test]
fn group_undoes_as_one_step() {
    let mut buf = Buffer::new("");
    let group = GroupKey {
        actor: AuthorId::User,
        turn: 7,
    };
    for (i, c) in ["a", "b", "c"].iter().enumerate() {
        let t = insert(&buf, AuthorId::User, buf.version(), i, c).with_group(group.clone());
        buf.apply(t).unwrap();
    }
    buf.apply(insert(&buf, AuthorId::User, buf.version(), 3, "d"))
        .unwrap();
    buf.undo(&AuthorId::User).unwrap();
    assert_eq!(text(&buf.snapshot()), "abc");
    buf.undo(&AuthorId::User).unwrap();
    assert_eq!(text(&buf.snapshot()), "");
}

#[test]
fn redo_after_others_edited_reapplies() {
    let mut buf = Buffer::new("1234");
    buf.apply(insert(&buf, AuthorId::User, buf.version(), 0, "u"))
        .unwrap();
    buf.undo(&AuthorId::User).unwrap();
    buf.apply(insert(&buf, agent(), buf.version(), 4, "A"))
        .unwrap();
    buf.redo(&AuthorId::User).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "u1234A");
}

#[test]
fn lazy_and_tracked_anchors_follow_edits() {
    let mut buf = Buffer::new("abc def");
    let lazy = buf.anchor(4, Bias::Left);
    let tracked = buf.track(4, Bias::Left);
    buf.apply(insert(&buf, AuthorId::User, buf.version(), 0, ">> "))
        .unwrap();
    buf.apply(Transaction::new(
        AuthorId::User,
        buf.version(),
        ChangeSet::delete(buf.len(), 3..5).unwrap(),
    ))
    .unwrap();
    assert_eq!(buf.resolve(lazy).unwrap(), 5);
    assert_eq!(buf.tracked(tracked), Some(5));
    assert!(buf.untrack(tracked));
    assert_eq!(buf.tracked(tracked), None);
    let reused = buf.track(0, Bias::Right);
    assert_eq!(buf.tracked(tracked), None);
    assert_eq!(buf.tracked(reused), Some(0));
    let far = spur_text::Anchor {
        version: Version(99),
        offset: 0,
        bias: Bias::Left,
    };
    assert_eq!(buf.resolve(far), Err(AnchorError::FutureVersion));
}

#[test]
fn snapshot_coordinates_and_line_endings() {
    let buf = Buffer::new("a\r\nb\rc\nd😀e\u{301}");
    let s = buf.snapshot();
    assert_eq!(s.line_count(), 4);
    assert_eq!(s.line_range(0), Some(0..3));
    assert_eq!(s.line_range(1), Some(3..5));
    assert_eq!(s.line_range(3), Some(7..s.len()));
    assert_eq!(s.byte_to_point(8), Some(Point { line: 3, column: 1 }));
    assert_eq!(s.point_to_byte(Point { line: 1, column: 1 }), Some(4));
    assert_eq!(s.byte_to_utf16(12), Some(10));
    assert_eq!(s.utf16_to_byte(10), Some(12));
    assert_eq!(s.byte_to_char(12), Some(9));
    assert_eq!(s.byte_to_point(9), None, "inside the emoji");
    // CRLF is one grapheme; e + combining accent is one grapheme.
    assert_eq!(s.next_grapheme_boundary(1), Some(3));
    assert_eq!(s.next_grapheme_boundary(12), Some(s.len()));
    assert_eq!(s.prev_grapheme_boundary(s.len()), Some(12));
    assert_eq!(s.next_grapheme_boundary(s.len()), None);
    // Line endings round-trip byte for byte.
    assert_eq!(text(&s), "a\r\nb\rc\nd😀e\u{301}");
}
