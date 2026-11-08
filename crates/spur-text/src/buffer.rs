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
use crate::types::{AuthorId, GroupKey, Version};

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
    /// Undo group; consecutive transactions with the same author and group undo together.
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
    /// Undoing would change text another author edited later.
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
                "undo overlaps {} later edit(s) by other authors",
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
            });
        }
        let inverse = self.commit(&changes);
        self.history.clear_redo(&txn.author);
        self.history.push(NewRevision {
            author: txn.author.clone(),
            group: txn.group,
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
        })
    }

    /// Undoes the author's latest edit that is still in effect. If it is the newest
    /// revision this moves back in the undo tree; otherwise its inverse is rebased over
    /// later edits and committed as a new revision. Returns `None` if there is nothing
    /// to undo.
    ///
    /// # Errors
    /// [`UndoError::Conflict`] if another author changed the same text later.
    pub fn undo(&mut self, author: &AuthorId) -> Result<Option<Applied>, UndoError> {
        match self.history.undo_target(author) {
            UndoTarget::Nothing => Ok(None),
            UndoTarget::TreeMove(rev) => {
                let inverse = self.history.revs[rev].inverse.clone();
                let selection = self.history.revs[rev].selection_before.clone();
                self.commit(&inverse);
                self.history.head = self.history.revs[rev].parent;
                self.history.push_redo(author, RedoEntry::TreeMove(rev));
                Ok(Some(self.applied(author, inverse, selection)))
            }
            UndoTarget::Revert(rev) => {
                let applied = self.revert(author, rev)?;
                let node = self.history.head;
                self.history.push_redo(author, RedoEntry::Revert(node));
                Ok(applied)
            }
        }
    }

    /// Redoes the author's latest undo. Returns `None` if there is nothing to redo, or
    /// the undone edit is no longer on the current undo-tree branch.
    ///
    /// # Errors
    /// [`UndoError::Conflict`] if another author changed the same text since; the redo
    /// stays available.
    pub fn redo(&mut self, author: &AuthorId) -> Result<Option<Applied>, UndoError> {
        let Some(entry) = self.history.pop_redo(author) else {
            return Ok(None);
        };
        let result = match entry {
            RedoEntry::TreeMove(child) => self.redo_child(author, child),
            RedoEntry::Revert(node) => self.revert(author, node),
        };
        if result.is_err() {
            self.history.push_redo(author, entry);
        }
        result
    }

    fn redo_child(
        &mut self,
        author: &AuthorId,
        child: usize,
    ) -> Result<Option<Applied>, UndoError> {
        let parent = self.history.revs[child].parent;
        if self.history.head == parent {
            let changes = self.history.revs[child].changes.clone();
            let selection = self.history.revs[child].selection_after.clone();
            self.commit(&changes);
            self.history.head = child;
            return Ok(Some(self.applied(author, changes, selection)));
        }
        // Others edited since the undo: re-apply the child over their edits.
        let Some(path) = self.history.path_after(parent) else {
            return Ok(None);
        };
        let rev = &self.history.revs[child];
        let (changes, selection) = (rev.changes.clone(), rev.selection_after.clone());
        self.commit_over(author, &changes, selection, &path, None)
    }

    /// Reverts revision `rev` (on the head path) by rebasing its inverse over later edits.
    fn revert(&mut self, author: &AuthorId, rev: usize) -> Result<Option<Applied>, UndoError> {
        let Some(path) = self.history.path_after(rev) else {
            return Ok(None);
        };
        let r = &self.history.revs[rev];
        // `selection_before` is in the coordinates before `rev`; move it past `rev` so it
        // can follow the later edits, then past the rebased inverse (in `commit_over`).
        let selection = r.selection_before.as_ref().map(|s| s.map(&r.changes));
        let inverse = r.inverse.clone();
        self.commit_over(author, &inverse, selection, &path, Some(rev))
    }

    /// Rebases `changes` over the revisions in `path`, then commits it as a new revision.
    fn commit_over(
        &mut self,
        author: &AuthorId,
        changes: &ChangeSet,
        selection: Option<Selection>,
        path: &[usize],
        reverts: Option<usize>,
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
            selection = selection.map(|s| s.map(over));
        }
        let changes = ChangeSet::new(len, tagged.into_iter().map(|(_, c)| c))
            .expect("rebased hunks stay sorted and in bounds");
        if reverts.is_some() {
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
            author: author.clone(),
            group: None,
            changes: changes.clone(),
            inverse,
            selection_before: None,
            selection_after: selection.clone(),
            reverts,
            version: self.version,
            time: SystemTime::now(),
        });
        Ok(Some(self.applied(author, changes, selection)))
    }

    fn applied(
        &self,
        author: &AuthorId,
        changes: ChangeSet,
        selection: Option<Selection>,
    ) -> Applied {
        Applied {
            author: author.clone(),
            version: self.version,
            changes,
            selection,
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
