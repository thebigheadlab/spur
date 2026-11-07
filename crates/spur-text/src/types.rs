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

/// Consecutive transactions with the same author and key form one undo step.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GroupKey {
    /// Actor that owns the group.
    pub actor: AuthorId,
    /// Turn id, e.g. one insert-mode session or one agent turn.
    pub turn: u64,
}
