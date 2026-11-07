//! Text engine: rope storage, change sets with rebasing, anchors, per-author undo.
//!
//! One [`Buffer`] per open file is owned by the core state thread, which orders every edit.
//! Edits arrive as [`Transaction`]s against a [`Version`]; stale ones are rebased over
//! edits committed since, or rejected with the overlapping hunks. Readers use O(1)
//! [`Snapshot`]s. Design: `docs/adr/0003-text-model-and-buffer-api.md`.

mod anchor;
mod buffer;
mod change;
mod history;
mod selection;
mod snapshot;
mod storage;
mod types;

pub use anchor::{Anchor, AnchorId};
pub use buffer::{
    AnchorError, Applied, Buffer, Conflict, DEFAULT_LOG_BYTES, DEFAULT_LOG_CAPACITY, EditError,
    Transaction, UndoError,
};
pub use change::{Bias, Change, ChangeError, ChangeSet, HunkConflict};
pub use selection::{SelRange, Selection};
pub use snapshot::{Point, Snapshot};
pub use types::{AuthorId, GroupKey, Version};

/// Name of this crate, used in diagnostics.
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(super::NAME, "spur-text");
    }

    #[test]
    fn snapshot_is_send_sync() {
        fn check<T: Send + Sync + Clone>() {}
        check::<super::Snapshot>();
    }
}
