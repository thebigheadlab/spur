//! Undo tree. Revision 0 is the root (the loaded text); the path from the root to `head`
//! composes to the current text.
//!
//! Each scope (an author, or one of their windows) has its own redo stack. Every entry
//! needs one revision to stay on the head path ([`RedoEntry::needs`]): a tree move needs
//! the undone child's parent. Two rules keep that true while several scopes undo and
//! edit in turn:
//!
//! - an undo moves the head off a revision only if no other scope's entry needs it;
//!   otherwise it reverts the revision (a new revision on top);
//! - when a redo cannot move the head back (someone edited since) it re-applies the
//!   child as a new revision, and the scope's entries that needed the old child are
//!   rewritten as [`RedoEntry::Over`]: re-apply that revision over a `prefix` leading
//!   from its own base text to the new revision's text.
//!
//! A scope's own entries below its last tree move need revisions that are off the path
//! until it redoes in order (last undone first), which a stack guarantees.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::change::ChangeSet;
use crate::selection::Selection;
use crate::types::{AuthorId, GroupKey, UndoLane, UndoScope, Version};

pub(crate) const ROOT: usize = 0;

pub(crate) struct Revision {
    pub(crate) parent: usize,
    pub(crate) last_child: Option<usize>,
    pub(crate) author: AuthorId,
    /// The author's undo lane (window) the revision was made in.
    pub(crate) lane: Option<UndoLane>,
    pub(crate) group: Option<GroupKey>,
    /// From the parent's text to this revision's text.
    pub(crate) changes: ChangeSet,
    /// From this revision's text back to the parent's.
    pub(crate) inverse: ChangeSet,
    pub(crate) selection_before: Option<Selection>,
    pub(crate) selection_after: Option<Selection>,
    /// The revision this one reverts (a selective undo or redo).
    pub(crate) reverts: Option<usize>,
    /// Buffer version that committed this revision (the latest one for a merged group).
    pub(crate) version: Version,
    /// Wall-clock time of the commit (the latest one for a merged group).
    pub(crate) time: SystemTime,
}

/// One step a scope can redo.
#[derive(Clone, Debug)]
pub(crate) enum RedoEntry {
    /// Undone by moving the head to the parent; redo re-applies this child.
    TreeMove(usize),
    /// Undone by this revert revision; redo reverts it.
    Revert(usize),
    /// A tree move whose parent left the head path when an earlier redo had to be
    /// rebased: `prefix` leads from the text before revision `rev` to the text at `base`,
    /// a revision on the head path. Redo re-applies `rev` over `prefix` and the path
    /// after `base`.
    Over {
        rev: usize,
        base: usize,
        prefix: ChangeSet,
    },
}

impl RedoEntry {
    /// The revision that must stay on the head path for this redo to apply.
    pub(crate) fn needs(&self, revs: &[Revision]) -> usize {
        match *self {
            Self::TreeMove(child) => revs[child].parent,
            Self::Revert(node) => node,
            Self::Over { base, .. } => base,
        }
    }
}

/// What `undo(scope)` should do.
pub(crate) enum UndoTarget {
    Nothing,
    /// The head is the scope's and no other scope's redo hangs off it: move to its parent.
    TreeMove(usize),
    /// A revision of the scope's still in effect: revert it (a new revision).
    Revert(usize),
}

pub(crate) struct History {
    pub(crate) revs: Vec<Revision>,
    pub(crate) head: usize,
    pub(crate) redo: HashMap<UndoScope, Vec<RedoEntry>>,
}

/// A new revision, before it is linked into the tree.
pub(crate) struct NewRevision {
    pub(crate) author: AuthorId,
    pub(crate) lane: Option<UndoLane>,
    pub(crate) group: Option<GroupKey>,
    pub(crate) changes: ChangeSet,
    pub(crate) inverse: ChangeSet,
    pub(crate) selection_before: Option<Selection>,
    pub(crate) selection_after: Option<Selection>,
    pub(crate) reverts: Option<usize>,
    pub(crate) version: Version,
    pub(crate) time: SystemTime,
}

impl History {
    pub(crate) fn new(len: usize) -> Self {
        Self {
            revs: vec![Revision {
                parent: ROOT,
                last_child: None,
                author: AuthorId::User,
                lane: None,
                group: None,
                changes: ChangeSet::empty(len),
                inverse: ChangeSet::empty(len),
                selection_before: None,
                selection_after: None,
                reverts: None,
                version: Version(0),
                time: SystemTime::now(),
            }],
            head: ROOT,
            redo: HashMap::new(),
        }
    }

    /// Records a commit as a child of the head, or merges it into the head when it
    /// continues the head's undo group. Returns the revision index.
    pub(crate) fn push(&mut self, new: NewRevision) -> usize {
        let is_last = self.head == self.revs.len() - 1;
        let head = &mut self.revs[self.head];
        let continues_group = self.head != ROOT
            && is_last
            && head.last_child.is_none()
            && head.reverts.is_none()
            && new.reverts.is_none()
            && new.group.is_some()
            && head.group == new.group
            && head.author == new.author
            && head.lane == new.lane;
        if continues_group {
            head.changes = head.changes.compose(&new.changes);
            head.inverse = new.inverse.compose(&head.inverse);
            head.selection_after = new.selection_after;
            head.version = new.version;
            head.time = new.time;
            return self.head;
        }
        let index = self.revs.len();
        self.revs.push(Revision {
            parent: self.head,
            last_child: None,
            author: new.author,
            lane: new.lane,
            group: new.group,
            changes: new.changes,
            inverse: new.inverse,
            selection_before: new.selection_before,
            selection_after: new.selection_after,
            reverts: new.reverts,
            version: new.version,
            time: new.time,
        });
        self.revs[self.head].last_child = Some(index);
        self.head = index;
        index
    }

    /// Finds the scope's latest revision on the head path that is still in effect.
    /// Undo revisions (an odd number of reverts deep) are skipped, so undoing again goes
    /// to the scope's previous edit. Redo revisions (a revert of an undo) are targets
    /// themselves: undoing a redo reverts the redo, not the original edit, which would
    /// overlap the undo/redo pair still on the path.
    pub(crate) fn undo_target(&self, scope: &UndoScope) -> UndoTarget {
        let mut reverted: Vec<usize> = Vec::new();
        let mut at = self.head;
        while at != ROOT {
            let rev = &self.revs[at];
            if !reverted.contains(&at) {
                if let Some(target) = rev.reverts {
                    reverted.push(target);
                }
                if self.in_scope(at, scope) && !self.is_undo(at) {
                    return if at == self.head && !self.redo_hangs_off(at, scope) {
                        UndoTarget::TreeMove(at)
                    } else {
                        UndoTarget::Revert(at)
                    };
                }
            }
            at = rev.parent;
        }
        UndoTarget::Nothing
    }

    fn in_scope(&self, rev: usize, scope: &UndoScope) -> bool {
        let r = &self.revs[rev];
        r.author == scope.author && r.lane == scope.lane
    }

    /// True if another scope could redo a child of `rev` by a tree move. Moving the head
    /// off `rev` would take that child's parent off the head path and lose the redo, so
    /// undoing `rev` must revert it instead.
    fn redo_hangs_off(&self, rev: usize, scope: &UndoScope) -> bool {
        self.redo.iter().any(|(owner, stack)| {
            owner != scope && stack.iter().any(|e| e.needs(&self.revs) == rev)
        })
    }

    /// True if `rev` undoes something: it ends an odd-length chain of reverts.
    fn is_undo(&self, rev: usize) -> bool {
        let mut depth = 0;
        let mut at = rev;
        while let Some(target) = self.revs[at].reverts {
            depth += 1;
            at = target;
        }
        depth % 2 == 1
    }

    /// Revisions after `from` up to and including the head, oldest first, or `None` if
    /// `from` is not on the head path.
    pub(crate) fn path_after(&self, from: usize) -> Option<Vec<usize>> {
        let mut path = Vec::new();
        let mut at = self.head;
        while at != from {
            if at == ROOT {
                return None;
            }
            path.push(at);
            at = self.revs[at].parent;
        }
        path.reverse();
        Some(path)
    }

    pub(crate) fn push_redo(&mut self, scope: &UndoScope, entry: RedoEntry) {
        self.redo.entry(scope.clone()).or_default().push(entry);
    }

    pub(crate) fn pop_redo(&mut self, scope: &UndoScope) -> Option<RedoEntry> {
        let stack = self.redo.get_mut(scope)?;
        let entry = stack.pop();
        if stack.is_empty() {
            self.redo.remove(scope);
        }
        entry
    }

    pub(crate) fn clear_redo(&mut self, scope: &UndoScope) {
        self.redo.remove(scope);
    }
}
