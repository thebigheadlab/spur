//! The buffer: the sequencer that orders edits, plus change log, undo tree and anchors.

use std::collections::VecDeque;
use std::fmt;
use std::time::SystemTime;

use crate::anchor::{Anchor, AnchorId, AnchorTable};
use crate::change::{Bias, Change, ChangeSet, rebase_tagged};
use crate::history::{History, NewRevision, RedoEntry, UndoTarget};
use crate::selection::Selection;
use crate::snapshot::Snapshot;
use crate::storage::Text;
use crate::types::{AuthorId, GroupKey, UndoLane, UndoScope, Version};

/// Default number of committed change sets kept for rebasing and lazy anchors.
pub const DEFAULT_LOG_CAPACITY: usize = 10_000;

/// Default budget for inserted text held by the change log, in bytes. Whichever limit
/// is hit first drops the oldest entries, so whole-file rewrites can't grow it unbounded.
pub const DEFAULT_LOG_BYTES: usize = 64 << 20;

/// An edit submitted to a buffer.
#[derive(Clone, Debug)]
pub struct Transaction {
    /// Who made the edit.
    pub author: AuthorId,
    /// Version the changes were computed against.
    pub base: Version,
    /// The edit, in `base` coordinates.
    pub changes: ChangeSet,
    /// Selection before the edit, restored by undo.
    pub selection_before: Option<Selection>,
    /// Selection after the edit, restored by redo. Dropped if the transaction is rebased.
    pub selection_after: Option<Selection>,
    /// The author's undo lane (window); undo in one lane never takes another lane's
    /// edits.
    pub lane: Option<UndoLane>,
    /// Undo group; consecutive transactions with the same author, lane and group undo
    /// together.
    pub group: Option<GroupKey>,
}

impl Transaction {
    /// A transaction with no selections and no group.
    pub fn new(author: AuthorId, base: Version, changes: ChangeSet) -> Self {
        Self {
            author,
            base,
            changes,
            selection_before: None,
            selection_after: None,
            lane: None,
            group: None,
        }
    }

    /// Sets the selections before and after.
    #[must_use]
    pub fn with_selections(mut self, before: Selection, after: Selection) -> Self {
        self.selection_before = Some(before);
        self.selection_after = Some(after);
        self
    }

    /// Sets the undo lane.
    #[must_use]
    pub fn with_lane(mut self, lane: UndoLane) -> Self {
        self.lane = Some(lane);
        self
    }

    /// Sets the undo group.
    #[must_use]
    pub fn with_group(mut self, group: GroupKey) -> Self {
        self.group = Some(group);
        self
    }
}

/// A committed edit: the one stream frontends, extensions and the journal consume.
#[derive(Clone, Debug)]
pub struct Applied {
    /// Who made it (for undo and redo, the author who asked).
    pub author: AuthorId,
    /// The new version.
    pub version: Version,
    /// The edit as applied, in coordinates of `version - 1`.
    pub changes: ChangeSet,
    /// Selection to show after the edit, if known.
    pub selection: Option<Selection>,
    /// Undo group of the edit: the transaction's own group, or for an undo or redo the
    /// group of the edit it undoes or redoes. Lets a caller tell whose edit it was.
    pub group: Option<GroupKey>,
}

fn with_group(applied: Option<Applied>, group: Option<GroupKey>) -> Option<Applied> {
    applied.map(|a| Applied { group, ..a })
}

/// An incoming hunk that overlaps a committed one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// The incoming hunk, in the coordinates it was submitted in.
    pub incoming: Change,
    /// The committed hunk, in coordinates of `committed_version - 1`.
    pub committed: Change,
    /// The version that committed it.
    pub committed_version: Version,
}

/// Why a transaction was rejected. Nothing is applied on error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// The changes overlap edits committed since `base`.
    Stale {
        /// The overlapping hunks.
        conflicts: Vec<Conflict>,
    },
    /// `base` is older than the retained change log.
    TooOld {
        /// The transaction's base.
        base: Version,
        /// The oldest version that can still be rebased from.
        oldest: Version,
    },
    /// `base` is newer than the buffer.
    FutureVersion {
        /// The transaction's base.
        base: Version,
        /// The buffer's version.
        current: Version,
    },
    /// The change set's base length does not match the text at `base`.
    LengthMismatch {
        /// Length of the text at `base`.
        expected: usize,
        /// The change set's base length.
        actual: usize,
    },
    /// A hunk starts or ends inside a UTF-8 character.
    NotCharBoundary {
        /// The offending offset, in current coordinates.
        offset: usize,
    },
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stale { conflicts } => {
                write!(
                    f,
                    "edit conflicts with {} committed hunk(s)",
                    conflicts.len()
                )
            }
            Self::TooOld { base, oldest } => {
                write!(
                    f,
                    "base {base} is older than the oldest rebasable version {oldest}"
                )
            }
            Self::FutureVersion { base, current } => {
                write!(f, "base {base} is newer than the buffer ({current})")
            }
            Self::LengthMismatch { expected, actual } => {
                write!(f, "change set expects length {actual}, text has {expected}")
            }
            Self::NotCharBoundary { offset } => write!(f, "offset {offset} is not a char boundary"),
        }
    }
}

impl std::error::Error for EditError {}

/// Why an undo or redo was refused. Nothing is applied on error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoError {
    /// Undoing would change text another author or lane edited later.
    Conflict {
        /// The overlapping hunks.
        conflicts: Vec<Conflict>,
    },
}

impl fmt::Display for UndoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict { conflicts } => write!(
                f,
                "undo or redo overlaps {} later edit(s) by another author or window",
                conflicts.len()
            ),
        }
    }
}

impl std::error::Error for UndoError {}

/// Why a lazy anchor could not be resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorError {
    /// The anchor's version is older than the retained change log.
    TooOld,
    /// The anchor's version is newer than the buffer.
    FutureVersion,
    /// The offset was past the end of the text at the anchor's version.
    OutOfBounds,
}

impl fmt::Display for AnchorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooOld => "anchor version is older than the change log",
            Self::FutureVersion => "anchor version is newer than the buffer",
            Self::OutOfBounds => "anchor offset is out of bounds",
        })
    }
}

impl std::error::Error for AnchorError {}

fn text_bytes(changes: &ChangeSet) -> usize {
    changes.hunks().iter().map(|h| h.text.len()).sum()
}

struct LogEntry {
    version: Version,
    changes: ChangeSet,
}

/// One text buffer and its sequencer. Owned by the core state thread; readers take
/// [`Snapshot`]s.
pub struct Buffer {
    text: Text,
    version: Version,
    log: VecDeque<LogEntry>,
    log_capacity: usize,
    log_byte_limit: usize,
    log_bytes: usize,
    history: History,
    anchors: AnchorTable,
}

impl Buffer {
    /// A buffer holding `text` at version 0.
    pub fn new(text: &str) -> Self {
        Self::with_log_capacity(text, DEFAULT_LOG_CAPACITY)
    }

    /// Like [`Buffer::new`], keeping `capacity` change sets for rebasing (at least 1).
    pub fn with_log_capacity(text: &str, capacity: usize) -> Self {
        Self::with_log_limits(text, capacity, DEFAULT_LOG_BYTES)
    }

    /// Like [`Buffer::new`], keeping at most `capacity` change sets (at least 1) and at
    /// most `byte_limit` bytes of inserted text in the change log, whichever comes first.
    /// The newest change set is always kept.
    pub fn with_log_limits(text: &str, capacity: usize, byte_limit: usize) -> Self {
        let text = Text::new(text);
        Self {
            history: History::new(text.len()),
            text,
            version: Version(0),
            log: VecDeque::new(),
            log_capacity: capacity.max(1),
            log_byte_limit: byte_limit,
            log_bytes: 0,
            anchors: AnchorTable::default(),
        }
    }

    /// The current version.
    pub fn version(&self) -> Version {
        self.version
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// True if the text is empty.
    pub fn is_empty(&self) -> bool {
        self.text.len() == 0
    }

    /// An O(1) snapshot of the current text.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            version: self.version,
        }
    }

    /// The oldest version a transaction or lazy anchor can still refer to.
    pub fn oldest_version(&self) -> Version {
        self.log
            .front()
            .map_or(self.version, |e| Version(e.version.0 - 1))
    }

    /// Commits a transaction, rebasing it first if `base` is older than the buffer.
    ///
    /// # Errors
    /// See [`EditError`]. Nothing is applied on error.
    pub fn apply(&mut self, txn: Transaction) -> Result<Applied, EditError> {
        let rebased = txn.base != self.version;
        let changes = self.rebase_to_current(txn.base, &txn.changes)?;
        self.check_boundaries(&changes)?;
        // The after-selection is relative to the text the author expected, which a rebase
        // changes; the before-selection maps through the edits since `base`.
        let selection_after = if rebased { None } else { txn.selection_after };
        let selection_before = txn.selection_before.map(|s| {
            self.log_since(txn.base)
                .fold(s, |s, entry| s.map(&entry.changes))
        });
        if changes.is_empty() {
            return Ok(Applied {
                author: txn.author,
                version: self.version,
                changes,
                selection: selection_after,
                group: txn.group,
            });
        }
        let inverse = self.commit(&changes);
        self.history
            .clear_redo(&UndoScope::new(txn.author.clone(), txn.lane));
        self.history.push(NewRevision {
            author: txn.author.clone(),
            lane: txn.lane,
            group: txn.group.clone(),
            changes: changes.clone(),
            inverse,
            selection_before,
            selection_after: selection_after.clone(),
            reverts: None,
            version: self.version,
            time: SystemTime::now(),
        });
        Ok(Applied {
            author: txn.author,
            version: self.version,
            changes,
            selection: selection_after,
            group: txn.group,
        })
    }

    /// Undoes the scope's latest edit that is still in effect: the author's, made in the
    /// same lane. If it is the newest revision this moves back in the undo tree;
    /// otherwise (or if another scope could still redo a newer edit) its inverse is
    /// rebased over later edits and committed as a new revision. Returns `None` if there
    /// is nothing to undo.
    ///
    /// # Errors
    /// [`UndoError::Conflict`] if another author or lane changed the same text later.
    pub fn undo(&mut self, scope: &UndoScope) -> Result<Option<Applied>, UndoError> {
        let author = &scope.author;
        match self.history.undo_target(scope) {
            UndoTarget::Nothing => Ok(None),
            UndoTarget::TreeMove(rev) => {
                let inverse = self.history.revs[rev].inverse.clone();
                let selection = self.history.revs[rev].selection_before.clone();
                let group = self.source_group(rev);
                self.commit(&inverse);
                self.history.head = self.history.revs[rev].parent;
                self.history.push_redo(scope, RedoEntry::TreeMove(rev));
                Ok(Some(self.applied(author, inverse, selection, group)))
            }
            UndoTarget::Revert(rev) => {
                let applied = self.revert(scope, rev)?;
                let node = self.history.head;
                self.history.push_redo(scope, RedoEntry::Revert(node));
                Ok(applied)
            }
        }
    }

    /// Drops the scope's redo steps, for a window that closed. Its edits stay in the
    /// history, where no other scope undoes them. Besides the memory, a pending redo
    /// keeps other scopes' undos of the revision it needs from moving the head (they
    /// revert instead), so a closed window's should go.
    pub fn forget_redo(&mut self, scope: &UndoScope) {
        self.history.clear_redo(scope);
    }

    /// Redoes the scope's latest undo. Returns `None` if there is nothing to redo, or
    /// the undone edit is no longer on the current undo-tree branch.
    ///
    /// # Errors
    /// [`UndoError::Conflict`] if another author or lane changed the same text since;
    /// the redo stays available.
    pub fn redo(&mut self, scope: &UndoScope) -> Result<Option<Applied>, UndoError> {
        let Some(entry) = self.history.pop_redo(scope) else {
            return Ok(None);
        };
        let result = match &entry {
            &RedoEntry::TreeMove(child) => self.redo_child(scope, child),
            &RedoEntry::Revert(node) => self.revert(scope, node),
            RedoEntry::Over { rev, base, prefix } => {
                self.redo_over(scope, *rev, *base, Some(prefix))
            }
        };
        if result.is_err() {
            self.history.push_redo(scope, entry);
        }
        result
    }

    fn redo_child(
        &mut self,
        scope: &UndoScope,
        child: usize,
    ) -> Result<Option<Applied>, UndoError> {
        let parent = self.history.revs[child].parent;
        if self.history.head == parent {
            let changes = self.history.revs[child].changes.clone();
            let selection = self.history.revs[child].selection_after.clone();
            let group = self.source_group(child);
            self.commit(&changes);
            self.history.head = child;
            return Ok(Some(self.applied(&scope.author, changes, selection, group)));
        }
        // Others edited since the undo: re-apply the child over their edits.
        self.redo_over(scope, child, parent, None)
    }

    /// Re-applies revision `child` as a new revision: its changes rebased over `prefix`
    /// (from the text before `child` to the text at `base`) and the path after `base`.
    /// The scope's redo steps that hung off `child` are rewritten to hang off the new
    /// revision, so redoing several undone steps after others edited keeps working.
    fn redo_over(
        &mut self,
        scope: &UndoScope,
        child: usize,
        base: usize,
        prefix: Option<&ChangeSet>,
    ) -> Result<Option<Applied>, UndoError> {
        let Some(path) = self.history.path_after(base) else {
            return Ok(None);
        };
        let path = self.history.without_cancelled(path);
        let rev = &self.history.revs[child];
        let mut changes = rev.changes.clone();
        if let Some(prefix) = prefix {
            changes = changes
                .rebase(prefix)
                .map_err(|hunks| UndoError::Conflict {
                    conflicts: hunks
                        .into_iter()
                        .map(|h| Conflict {
                            incoming: rev.changes.hunks()[h.incoming].clone(),
                            committed: h.committed,
                            committed_version: self.history.revs[base].version,
                        })
                        .collect(),
                })?;
        }
        // Edits from the text before `child` to the current text.
        let lead: Vec<ChangeSet> = prefix
            .into_iter()
            .cloned()
            .chain(path.iter().map(|&r| self.history.revs[r].changes.clone()))
            .collect();
        let after = self.after_redo(child, &lead);
        let selection = after
            .as_ref()
            .and_then(|a| Some(self.history.revs[child].selection_after.as_ref()?.map(a)));
        let group = self.source_group(child);
        let applied = self.commit_over(scope, &changes, selection, &path, None, true)?;
        if let Some(after) = after {
            self.rehang_redo(scope, child, &after);
        }
        Ok(with_group(applied, group))
    }

    /// The edits from the text after `child` to the text after re-applying `child` over
    /// `lead` (which starts at the text before `child`), or `None` if they overlap.
    fn after_redo(&self, child: usize, lead: &[ChangeSet]) -> Option<ChangeSet> {
        let mut ch = self.history.revs[child].changes.clone();
        let mut after = ChangeSet::empty(ch.new_len());
        for cs in lead {
            // `commit_over` puts `child` after `cs` where both change one offset, so
            // map `cs` as if it came first.
            after = after.compose(&cs.rebase_first(&ch).ok()?);
            ch = ch.rebase(cs).ok()?;
        }
        Some(after)
    }

    /// Rewrites the scope's redo steps that need revision `child` on the head path to
    /// need the head instead, which re-applied `child`; `after` leads from the text
    /// after `child` to the head's text.
    fn rehang_redo(&mut self, scope: &UndoScope, child: usize, after: &ChangeSet) {
        let head = self.history.head;
        let Some(stack) = self.history.redo.get(scope) else {
            return;
        };
        let revs = &self.history.revs;
        let rehung: Vec<RedoEntry> = stack
            .iter()
            .map(|e| match e {
                &RedoEntry::TreeMove(rev) if revs[rev].parent == child => RedoEntry::Over {
                    rev,
                    base: head,
                    prefix: after.clone(),
                },
                RedoEntry::Over { rev, base, prefix } if *base == child => RedoEntry::Over {
                    rev: *rev,
                    base: head,
                    prefix: prefix.compose(after),
                },
                e => e.clone(),
            })
            .collect();
        self.history.redo.insert(scope.clone(), rehung);
    }

    /// Reverts revision `rev` (on the head path) by rebasing its inverse over later edits.
    fn revert(&mut self, scope: &UndoScope, rev: usize) -> Result<Option<Applied>, UndoError> {
        let Some(path) = self.history.path_after(rev) else {
            return Ok(None);
        };
        let path = self.history.without_cancelled(path);
        let group = self.source_group(rev);
        let r = &self.history.revs[rev];
        let inverse = r.inverse.clone();
        // `selection_before` is in the coordinates before `rev`. The result is that text
        // plus the later edits, so map it through each later edit rebased over the
        // (progressively rebased) inverse. If that does not rebase cleanly, the inverse
        // overlaps a later edit and the commit below reports the conflict.
        let mut premapped = r.selection_before.clone();
        let mut inv = inverse.clone();
        for &later in &path {
            let over = &self.history.revs[later].changes;
            let (Ok(over_pre), Ok(inv_next)) = (over.rebase_first(&inv), inv.rebase(over)) else {
                let selection = r.selection_before.as_ref().map(|s| s.map(&r.changes));
                return self
                    .commit_over(scope, &inverse, selection, &path, Some(rev), false)
                    .map(|a| with_group(a, group));
            };
            premapped = premapped.map(|s| s.map(&over_pre));
            inv = inv_next;
        }
        let applied = self.commit_over(scope, &inverse, premapped, &path, Some(rev), true)?;
        // Undoing this revert (a redo, or an undo of a redo) restores the selection
        // `rev` left, carried over the later edits.
        let r = &self.history.revs[rev];
        let restore = r.selection_after.as_ref().map(|s| {
            path.iter()
                .fold(s.clone(), |s, &p| s.map(&self.history.revs[p].changes))
        });
        let head = self.history.head;
        self.history.revs[head].selection_before = restore;
        Ok(with_group(applied, group))
    }

    /// The undo group of revision `rev`, or for a revision that reverts another, the
    /// group of the reverted one.
    fn source_group(&self, rev: usize) -> Option<GroupKey> {
        let r = &self.history.revs[rev];
        r.group
            .clone()
            .or_else(|| r.reverts.and_then(|o| self.history.revs[o].group.clone()))
    }

    /// Rebases `changes` over the revisions in `path`, then commits it as a new revision.
    fn commit_over(
        &mut self,
        scope: &UndoScope,
        changes: &ChangeSet,
        selection: Option<Selection>,
        path: &[usize],
        reverts: Option<usize>,
        premapped: bool,
    ) -> Result<Option<Applied>, UndoError> {
        let mut selection = selection;
        let mut tagged: Vec<(usize, Change)> =
            changes.hunks().iter().cloned().enumerate().collect();
        let mut len = changes.base_len();
        for &later in path {
            let later = &self.history.revs[later];
            let over = &later.changes;
            tagged = rebase_tagged(&tagged, len, over).map_err(|hunks| UndoError::Conflict {
                conflicts: hunks
                    .into_iter()
                    .map(|h| Conflict {
                        incoming: changes.hunks()[h.incoming].clone(),
                        committed: h.committed,
                        committed_version: later.version,
                    })
                    .collect(),
            })?;
            len = over.new_len();
            if !premapped {
                selection = selection.map(|s| s.map(over));
            }
        }
        let changes = ChangeSet::new(len, tagged.into_iter().map(|(_, c)| c))
            .expect("rebased hunks stay sorted and in bounds");
        if reverts.is_some() && !premapped {
            selection = selection.map(|s| s.map(&changes));
        }
        let inverse = self.commit(&changes);
        // Never hand back an offset outside the text or inside a char.
        if let Some(sel) = &mut selection {
            for r in &mut sel.ranges {
                r.anchor = self.floor_boundary(r.anchor);
                r.head = self.floor_boundary(r.head);
            }
        }
        self.history.push(NewRevision {
            author: scope.author.clone(),
            lane: scope.lane,
            group: None,
            changes: changes.clone(),
            inverse,
            selection_before: None,
            selection_after: selection.clone(),
            reverts,
            version: self.version,
            time: SystemTime::now(),
        });
        Ok(Some(self.applied(&scope.author, changes, selection, None)))
    }

    fn applied(
        &self,
        author: &AuthorId,
        changes: ChangeSet,
        selection: Option<Selection>,
        group: Option<GroupKey>,
    ) -> Applied {
        Applied {
            author: author.clone(),
            version: self.version,
            changes,
            selection,
            group,
        }
    }

    fn rebase_to_current(
        &self,
        base: Version,
        changes: &ChangeSet,
    ) -> Result<ChangeSet, EditError> {
        if base > self.version {
            return Err(EditError::FutureVersion {
                base,
                current: self.version,
            });
        }
        if base == self.version {
            if changes.base_len() != self.len() {
                return Err(EditError::LengthMismatch {
                    expected: self.len(),
                    actual: changes.base_len(),
                });
            }
            return Ok(changes.clone());
        }
        let oldest = self.oldest_version();
        if base < oldest {
            return Err(EditError::TooOld { base, oldest });
        }
        let mut entries = self.log_since(base).peekable();
        if let Some(first) = entries.peek()
            && first.changes.base_len() != changes.base_len()
        {
            return Err(EditError::LengthMismatch {
                expected: first.changes.base_len(),
                actual: changes.base_len(),
            });
        }
        let mut tagged: Vec<(usize, Change)> =
            changes.hunks().iter().cloned().enumerate().collect();
        let mut len = changes.base_len();
        for entry in entries {
            tagged =
                rebase_tagged(&tagged, len, &entry.changes).map_err(|hunks| EditError::Stale {
                    conflicts: hunks
                        .into_iter()
                        .map(|h| Conflict {
                            incoming: changes.hunks()[h.incoming].clone(),
                            committed: h.committed,
                            committed_version: entry.version,
                        })
                        .collect(),
                })?;
            len = entry.changes.new_len();
        }
        Ok(ChangeSet::new(len, tagged.into_iter().map(|(_, c)| c))
            .expect("rebased hunks stay sorted and in bounds"))
    }

    /// Log entries committed after `base`, oldest first. `base` must be within
    /// `oldest_version()..=version()`.
    fn log_since(&self, base: Version) -> impl Iterator<Item = &LogEntry> {
        let skip = usize::try_from(base.0 - self.oldest_version().0).unwrap_or(usize::MAX);
        self.log.iter().skip(skip)
    }

    /// Wall-clock time of the newest revision on the current undo path, or `None` if the
    /// buffer is at its loaded state. Basis for "earlier by duration" and undo to last save.
    pub fn last_edit_time(&self) -> Option<SystemTime> {
        (self.history.head != crate::history::ROOT)
            .then(|| self.history.revs[self.history.head].time)
    }

    fn check_boundaries(&self, changes: &ChangeSet) -> Result<(), EditError> {
        for h in changes.hunks() {
            for offset in [h.range.start, h.range.end] {
                if !self.text.is_char_boundary(offset) {
                    return Err(EditError::NotCharBoundary { offset });
                }
            }
        }
        Ok(())
    }

    /// Applies validated changes to the text, bumps the version, logs them and moves
    /// tracked anchors. Returns the inverse.
    /// `offset` clamped to the text and moved back onto a char boundary.
    fn floor_boundary(&self, offset: usize) -> usize {
        let mut o = offset.min(self.len());
        while !self.text.is_char_boundary(o) {
            o -= 1;
        }
        o
    }

    fn commit(&mut self, changes: &ChangeSet) -> ChangeSet {
        debug_assert_eq!(changes.base_len(), self.text.len());
        let inverse = changes.invert_with(|r| self.text.slice(r));
        for h in changes.hunks().iter().rev() {
            self.text.replace(h.range.clone(), &h.text);
        }
        self.version = Version(self.version.0 + 1);
        self.log_bytes += text_bytes(changes);
        while !self.log.is_empty()
            && (self.log.len() >= self.log_capacity || self.log_bytes > self.log_byte_limit)
        {
            let dropped = self.log.pop_front().expect("checked non-empty");
            self.log_bytes -= text_bytes(&dropped.changes);
        }
        self.log.push_back(LogEntry {
            version: self.version,
            changes: changes.clone(),
        });
        self.anchors.map(changes);
        inverse
    }

    /// A lazy anchor at the current version.
    pub fn anchor(&self, offset: usize, bias: Bias) -> Anchor {
        Anchor {
            version: self.version,
            offset,
            bias,
        }
    }

    /// Resolves a lazy anchor to a current byte offset.
    ///
    /// # Errors
    /// [`AnchorError`] if the anchor's version is outside the change log, or its offset
    /// was out of bounds.
    pub fn resolve(&self, anchor: Anchor) -> Result<usize, AnchorError> {
        if anchor.version > self.version {
            return Err(AnchorError::FutureVersion);
        }
        let oldest = self.oldest_version();
        if anchor.version < oldest {
            return Err(AnchorError::TooOld);
        }
        let mut entries = self.log_since(anchor.version).peekable();
        let len_then = entries.peek().map_or(self.len(), |e| e.changes.base_len());
        if anchor.offset > len_then {
            return Err(AnchorError::OutOfBounds);
        }
        Ok(entries.fold(anchor.offset, |offset, e| {
            e.changes.map_offset(offset, anchor.bias)
        }))
    }

    /// Tracks a position that is updated on every commit. Resolving is O(1).
    pub fn track(&mut self, offset: usize, bias: Bias) -> AnchorId {
        self.anchors.insert(offset.min(self.len()), bias)
    }

    /// The current offset of a tracked anchor, or `None` if it was untracked.
    pub fn tracked(&self, id: AnchorId) -> Option<usize> {
        self.anchors.get(id)
    }

    /// Stops tracking an anchor. Returns false if it was not tracked.
    pub fn untrack(&mut self, id: AnchorId) -> bool {
        self.anchors.remove(id)
    }

    /// Number of tracked anchors.
    pub fn tracked_len(&self) -> usize {
        self.anchors.len()
    }
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Buffer")
            .field("version", &self.version)
            .field("len", &self.len())
            .field("revisions", &self.history.revs.len())
            .field("tracked", &self.anchors.len())
            .finish_non_exhaustive()
    }
}
