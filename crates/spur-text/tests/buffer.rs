//! Scenario tests for the buffer API.

use spur_text::{
    AnchorError, AuthorId, Bias, Buffer, Change, ChangeSet, EditError, GroupKey, Point, SelRange,
    Selection, Snapshot, Transaction, UndoError, UndoLane, UndoScope, Version,
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
    let undone = buf.undo(&AuthorId::User.into()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "ab");
    assert_eq!(undone.selection, Some(Selection::cursor(1)));
    assert!(buf.undo(&AuthorId::User.into()).unwrap().is_none());
    let redone = buf.redo(&AuthorId::User.into()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "aXb");
    assert_eq!(redone.selection, Some(Selection::cursor(2)));
    assert!(buf.redo(&AuthorId::User.into()).unwrap().is_none());
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
    buf.undo(&AuthorId::User.into()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "line\nAGENT\n");
    // Nothing more of the user's to undo.
    assert!(buf.undo(&AuthorId::User.into()).unwrap().is_none());
    buf.redo(&AuthorId::User.into()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "user line\nAGENT\n");
    // The agent can undo its own edit independently.
    buf.undo(&agent().into()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "user line\n");
}

fn lane(n: u64) -> UndoScope {
    UndoScope::new(AuthorId::User, Some(UndoLane(n)))
}

fn insert_in(buf: &Buffer, n: u64, at: usize, s: &str) -> Transaction {
    insert(buf, AuthorId::User, buf.version(), at, s).with_lane(UndoLane(n))
}

#[test]
fn each_lane_undoes_only_its_own_edits() {
    let mut buf = Buffer::new("");
    buf.apply(insert_in(&buf, 1, 0, "one\n")).unwrap();
    buf.apply(insert_in(&buf, 2, 4, "two\n")).unwrap();
    // Lane 1 undoes its edit although lane 2's is newer.
    buf.undo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "two\n");
    assert!(buf.undo(&lane(1)).unwrap().is_none());
    // The author without a lane has nothing to undo.
    assert!(buf.undo(&AuthorId::User.into()).unwrap().is_none());
    buf.undo(&lane(2)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "");
    buf.redo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "one\n");
    buf.redo(&lane(2)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "one\ntwo\n");
}

#[test]
fn an_undo_in_one_lane_keeps_another_lanes_redo() {
    let mut buf = Buffer::new("|");
    buf.apply(insert_in(&buf, 1, 0, "a")).unwrap();
    buf.apply(insert_in(&buf, 2, 2, "b")).unwrap();
    // Lane 2 undoes its edit (the head), then lane 1 undoes its own, which is now the
    // head too. Moving the head off it would strand lane 2's redo.
    buf.undo(&lane(2)).unwrap().unwrap();
    buf.undo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "|");
    buf.redo(&lane(2)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "|b");
    buf.redo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "a|b");
}

#[test]
fn several_redos_survive_another_lanes_edit() {
    let mut buf = Buffer::new("|");
    buf.apply(insert_in(&buf, 2, 1, "x")).unwrap();
    buf.apply(insert_in(&buf, 2, 2, "y")).unwrap();
    buf.undo(&lane(2)).unwrap().unwrap();
    buf.undo(&lane(2)).unwrap().unwrap();
    // Lane 1 edits where lane 2's undone edits used to hang in the undo tree.
    buf.apply(insert_in(&buf, 1, 0, "a")).unwrap();
    buf.redo(&lane(2)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "a|x");
    buf.redo(&lane(2)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "a|xy");
    // And back: lane 2's undo still takes only its own edits.
    buf.undo(&lane(2)).unwrap().unwrap();
    buf.undo(&lane(2)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "a|");
}

#[test]
fn redos_land_in_place_when_another_lane_typed_at_the_same_spot() {
    let mut buf = Buffer::new("");
    buf.apply(insert_in(&buf, 2, 0, "xx")).unwrap();
    buf.apply(insert_in(&buf, 2, 1, "y")).unwrap();
    buf.undo(&lane(2)).unwrap().unwrap();
    buf.undo(&lane(2)).unwrap().unwrap();
    buf.apply(insert_in(&buf, 1, 0, "ab")).unwrap();
    buf.redo(&lane(2)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "abxx");
    buf.redo(&lane(2)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "abxyx");
}

#[test]
fn a_redone_delete_removes_the_same_text() {
    let mut buf = Buffer::new("");
    buf.apply(insert_in(&buf, 1, 0, "xyz")).unwrap();
    let del = Transaction::new(
        AuthorId::User,
        buf.version(),
        ChangeSet::delete(3, 1..2).unwrap(),
    )
    .with_lane(UndoLane(1));
    buf.apply(del).unwrap();
    buf.undo(&lane(1)).unwrap().unwrap();
    buf.undo(&lane(1)).unwrap().unwrap();
    buf.apply(insert_in(&buf, 2, 0, "B")).unwrap();
    buf.redo(&lane(1)).unwrap().unwrap();
    buf.redo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "Bxz");
}

#[test]
fn a_redo_after_others_edited_restores_the_cursor() {
    let mut buf = Buffer::new("mnop");
    let t = insert_in(&buf, 1, 0, "ab").with_selections(Selection::cursor(0), Selection::cursor(2));
    buf.apply(t).unwrap();
    buf.undo(&lane(1)).unwrap().unwrap();
    buf.apply(insert_in(&buf, 2, 1, "Q")).unwrap();
    let redone = buf.redo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "abmQnop");
    assert_eq!(redone.selection, Some(Selection::cursor(2)));
}

#[test]
fn a_redo_of_a_reverting_undo_restores_the_cursor() {
    let mut buf = Buffer::new("mn");
    let t = insert_in(&buf, 1, 0, "ab").with_selections(Selection::cursor(0), Selection::cursor(2));
    buf.apply(t).unwrap();
    // Lane 2's edit is newer, so lane 1's undo reverts instead of moving the head.
    buf.apply(insert_in(&buf, 2, 4, "Z")).unwrap();
    let undone = buf.undo(&lane(1)).unwrap().unwrap();
    assert_eq!(undone.selection, Some(Selection::cursor(0)));
    let redone = buf.redo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "abmnZ");
    assert_eq!(redone.selection, Some(Selection::cursor(2)));
}

#[test]
fn a_redo_skips_an_undo_and_redo_pair_in_between() {
    let mut buf = Buffer::new("");
    buf.apply(insert_in(&buf, 1, 0, "xyz")).unwrap();
    let del = Transaction::new(
        AuthorId::User,
        buf.version(),
        ChangeSet::delete(3, 1..2).unwrap(),
    )
    .with_lane(UndoLane(1));
    buf.apply(del).unwrap();
    buf.undo(&lane(1)).unwrap().unwrap();
    buf.apply(insert_in(&buf, 2, 3, "Q")).unwrap();
    // Undo then redo of the insert, by revert: a pair that cancels out.
    buf.undo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "Q");
    buf.redo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "xyzQ");
    buf.redo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "xzQ");
}

#[test]
fn a_new_edit_clears_only_its_own_lanes_redo() {
    let mut buf = Buffer::new("");
    buf.apply(insert_in(&buf, 1, 0, "a")).unwrap();
    buf.apply(insert_in(&buf, 2, 1, "b")).unwrap();
    buf.undo(&lane(1)).unwrap().unwrap();
    buf.undo(&lane(2)).unwrap().unwrap();
    buf.apply(insert_in(&buf, 1, 0, "x")).unwrap();
    assert!(buf.redo(&lane(1)).unwrap().is_none());
    buf.redo(&lane(2)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "xb");
    // A closed lane's redo can be dropped.
    buf.undo(&lane(2)).unwrap().unwrap();
    buf.forget_redo(&lane(2));
    assert!(buf.redo(&lane(2)).unwrap().is_none());
}

#[test]
fn a_group_never_spans_lanes() {
    let mut buf = Buffer::new("");
    let group = GroupKey {
        actor: AuthorId::User,
        turn: 1,
    };
    for (n, s) in [(1, "a"), (2, "b"), (1, "c")] {
        let t = insert_in(&buf, n, buf.len(), s).with_group(group.clone());
        buf.apply(t).unwrap();
    }
    buf.undo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "ab");
    buf.undo(&lane(1)).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "b");
}

#[test]
fn undo_refuses_when_another_lane_edited_inside() {
    let mut buf = Buffer::new("");
    buf.apply(insert_in(&buf, 1, 0, "hello")).unwrap();
    buf.apply(insert_in(&buf, 2, 2, "ZZ")).unwrap();
    let UndoError::Conflict { conflicts } = buf.undo(&lane(1)).unwrap_err();
    assert_eq!(conflicts[0].committed_version, Version(2));
    assert_eq!(text(&buf.snapshot()), "heZZllo");
}

#[test]
fn undo_refuses_when_another_author_edited_inside() {
    let mut buf = Buffer::new("");
    buf.apply(insert(&buf, AuthorId::User, buf.version(), 0, "hello"))
        .unwrap();
    buf.apply(insert(&buf, agent(), buf.version(), 2, "ZZ"))
        .unwrap();
    let err = buf.undo(&AuthorId::User.into()).unwrap_err();
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
    buf.undo(&AuthorId::User.into()).unwrap();
    assert_eq!(text(&buf.snapshot()), "abc");
    buf.undo(&AuthorId::User.into()).unwrap();
    assert_eq!(text(&buf.snapshot()), "");
}

#[test]
fn redo_after_others_edited_reapplies() {
    let mut buf = Buffer::new("1234");
    buf.apply(insert(&buf, AuthorId::User, buf.version(), 0, "u"))
        .unwrap();
    buf.undo(&AuthorId::User.into()).unwrap();
    buf.apply(insert(&buf, agent(), buf.version(), 4, "A"))
        .unwrap();
    buf.redo(&AuthorId::User.into()).unwrap().unwrap();
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

#[test]
fn undo_after_redo_with_other_authors_edit_between() {
    // Regression: the second undo used to revert the original edit over the
    // undo/redo pair and conflict with the user's own undo.
    let mut buf = Buffer::new("");
    buf.apply(insert(&buf, AuthorId::User, buf.version(), 0, "foo"))
        .unwrap();
    buf.apply(insert(&buf, agent(), buf.version(), 3, " X"))
        .unwrap();
    buf.undo(&AuthorId::User.into()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), " X");
    buf.redo(&AuthorId::User.into()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "foo X");
    buf.undo(&AuthorId::User.into()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), " X");
    buf.redo(&AuthorId::User.into()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "foo X");
    // After another agent edit the redo is no longer the head: still undoable.
    buf.apply(insert(&buf, agent(), buf.version(), 0, "Y"))
        .unwrap();
    buf.undo(&AuthorId::User.into()).unwrap().unwrap();
    assert_eq!(text(&buf.snapshot()), "Y X");
}

#[test]
fn undo_conflict_reports_original_coordinates() {
    let mut buf = Buffer::new("");
    buf.apply(insert(&buf, AuthorId::User, buf.version(), 0, "hello"))
        .unwrap();
    buf.apply(insert(&buf, agent(), buf.version(), 0, ">> "))
        .unwrap();
    buf.apply(insert(&buf, agent(), buf.version(), 5, "ZZ"))
        .unwrap();
    let UndoError::Conflict { conflicts } = buf.undo(&AuthorId::User.into()).unwrap_err();
    // The undo deletes 0..5 of the text right after the user's edit, not 3..8.
    assert_eq!(conflicts[0].incoming.range, 0..5);
}

#[test]
fn log_byte_limit_is_configurable() {
    let mut buf = Buffer::with_log_limits("", 100, 4);
    for i in 0..3 {
        buf.apply(insert(&buf, AuthorId::User, buf.version(), i, "ab"))
            .unwrap();
    }
    // 2 + 2 bytes fit; the third insert pushes the first out.
    assert_eq!(buf.oldest_version(), Version(1));
    assert!(buf.last_edit_time().is_some());
    assert!(Buffer::new("x").last_edit_time().is_none());
}

#[test]
fn rebased_transaction_keeps_mapped_selection_before() {
    let mut buf = Buffer::new("abc");
    let v0 = buf.version();
    let agent_txn = Transaction::new(agent(), v0, ChangeSet::insert(3, 3, "!").unwrap())
        .with_selections(Selection::cursor(3), Selection::cursor(4));
    buf.apply(insert(&buf, AuthorId::User, v0, 0, "__"))
        .unwrap();
    let applied = buf.apply(agent_txn).unwrap();
    assert_eq!(applied.selection, None);
    let undone = buf.undo(&agent().into()).unwrap().unwrap();
    assert_eq!(undone.selection, Some(Selection::cursor(5)));
    assert_eq!(text(&buf.snapshot()), "__abc");
}

#[test]
fn undo_selection_follows_the_reverted_edit_and_later_ones() {
    let mut buf = Buffer::new("zz\nhello world");
    let del = Transaction::new(
        AuthorId::User,
        buf.version(),
        ChangeSet::new(buf.len(), [Change::new(3..9, "")]).unwrap(),
    )
    .with_selections(
        Selection {
            ranges: vec![SelRange { anchor: 3, head: 9 }],
            primary: 0,
        },
        Selection::cursor(3),
    );
    buf.apply(del).unwrap();
    buf.apply(insert(&buf, agent(), buf.version(), buf.len(), "éééé"))
        .unwrap();
    let undone = buf.undo(&AuthorId::User.into()).unwrap().unwrap();
    let snap = buf.snapshot();
    assert_eq!(text(&snap), "zz\nhello worldéééé");
    // The selection the user had before deleting comes back whole.
    let sel = undone.selection.unwrap();
    assert_eq!((sel.ranges[0].anchor, sel.ranges[0].head), (3, 9));
    // The next edit at the restored cursor works.
    buf.apply(insert(&buf, AuthorId::User, buf.version(), 3, "!"))
        .unwrap();
}
