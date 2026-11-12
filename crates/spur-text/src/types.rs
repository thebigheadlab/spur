//! Small shared types: versions, authors, undo groups.

use std::fmt;
use std::sync::Arc;

/// Monotonic per-buffer version. The loaded text is version 0; every commit adds one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64);

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// Who made an edit. Mirrors the core actor (user, extension, agent, RPC client) plus
/// `External` for reloads from disk and formatters.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AuthorId {
    /// The local user.
    User,
    /// An extension, by id.
    Extension(Arc<str>),
    /// An agent, by id.
    Agent(Arc<str>),
    /// An RPC client, by id.
    Rpc(Arc<str>),
    /// An external source such as a disk reload or a formatter.
    External(Arc<str>),
}

/// One undo history within an author: a window, so each window undoes only its own
/// edits. The number is the caller's (a view id, say).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UndoLane(pub u64);

/// Whose undo history: an author, and optionally one lane of theirs. Undo and redo take
/// only revisions made in the same scope.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UndoScope {
    /// The author.
    pub author: AuthorId,
    /// The lane, or `None` for the author's edits made without one.
    pub lane: Option<UndoLane>,
}

impl UndoScope {
    /// The scope of `author` in `lane`.
    pub fn new(author: AuthorId, lane: Option<UndoLane>) -> Self {
        Self { author, lane }
    }
}

impl From<AuthorId> for UndoScope {
    /// The author's edits made without a lane. Edits made in a lane are not in this
    /// scope: undoing it finds nothing of theirs.
    fn from(author: AuthorId) -> Self {
        Self { author, lane: None }
    }
}

/// Consecutive transactions with the same author, lane and key form one undo step.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GroupKey {
    /// Actor that owns the group.
    pub actor: AuthorId,
    /// Turn id, e.g. one insert-mode session or one agent turn.
    pub turn: u64,
}
