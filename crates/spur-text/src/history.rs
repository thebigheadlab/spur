//! Undo tree. Revision 0 is the root (the loaded text); the path from the root to `head`
//! composes to the current text.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::change::ChangeSet;
use crate::selection::Selection;
use crate::types::{AuthorId, GroupKey, Version};

pub(crate) const ROOT: usize = 0;

pub(crate) struct Revision {
    pub(crate) parent: usize,
    pub(crate) last_child: Option<usize>,
    pub(crate) author: AuthorId,
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

/// One step an author can redo.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RedoEntry {
    /// Undone by moving the head to the parent; redo re-applies this child.
    TreeMove(usize),
    /// Undone by this revert revision; redo reverts it.
    Revert(usize),
}

/// What `undo(author)` should do.
pub(crate) enum UndoTarget {
    Nothing,
    /// The head is the author's: move to its parent.
    TreeMove(usize),
    /// An earlier revision of the author's, still in effect: revert it.
    Revert(usize),
}

pub(crate) struct History {
    pub(crate) revs: Vec<Revision>,
    pub(crate) head: usize,
    pub(crate) redo: HashMap<AuthorId, Vec<RedoEntry>>,
}

/// A new revision, before it is linked into the tree.
pub(crate) struct NewRevision {
    pub(crate) author: AuthorId,
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
            && head.author == new.author;
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

    /// Finds the author's latest revision on the head path that is still in effect.
    /// Undo revisions (an odd number of reverts deep) are skipped, so undoing again goes
    /// to the author's previous edit. Redo revisions (a revert of an undo) are targets
    /// themselves: undoing a redo reverts the redo, not the original edit, which would
    /// overlap the undo/redo pair still on the path.
    pub(crate) fn undo_target(&self, author: &AuthorId) -> UndoTarget {
        let mut reverted: Vec<usize> = Vec::new();
        let mut at = self.head;
        while at != ROOT {
            let rev = &self.revs[at];
            if !reverted.contains(&at) {
                if let Some(target) = rev.reverts {
                    reverted.push(target);
                }
                if rev.author == *author && !self.is_undo(at) {
                    return if at == self.head {
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

    pub(crate) fn push_redo(&mut self, author: &AuthorId, entry: RedoEntry) {
        self.redo.entry(author.clone()).or_default().push(entry);
    }

    pub(crate) fn pop_redo(&mut self, author: &AuthorId) -> Option<RedoEntry> {
        self.redo.get_mut(author)?.pop()
    }

    pub(crate) fn clear_redo(&mut self, author: &AuthorId) {
        if let Some(stack) = self.redo.get_mut(author) {
            stack.clear();
        }
    }
}
