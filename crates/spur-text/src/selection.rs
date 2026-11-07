//! Selections stored with transactions so undo and redo restore cursors.

use crate::change::{Bias, ChangeSet};

/// One selection range. `head` is the cursor end; `anchor == head` is a plain cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SelRange {
    /// Fixed end, in bytes.
    pub anchor: usize,
    /// Moving end (the cursor), in bytes.
    pub head: usize,
}

impl SelRange {
    /// A cursor at `offset`.
    pub fn cursor(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }
}

/// A set of selection ranges with one primary range.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Selection {
    /// The ranges, in no particular order.
    pub ranges: Vec<SelRange>,
    /// Index of the primary range in `ranges`.
    pub primary: usize,
}

impl Selection {
    /// A single cursor.
    pub fn cursor(offset: usize) -> Self {
        Self {
            ranges: vec![SelRange::cursor(offset)],
            primary: 0,
        }
    }

    /// Maps every endpoint through `changes` with [`Bias::Right`], so text inserted at a
    /// cursor by someone else lands before it.
    #[must_use]
    pub fn map(&self, changes: &ChangeSet) -> Self {
        Self {
            ranges: self
                .ranges
                .iter()
                .map(|r| SelRange {
                    anchor: changes.map_offset(r.anchor, Bias::Right),
                    head: changes.map_offset(r.head, Bias::Right),
                })
                .collect(),
            primary: self.primary,
        }
    }
}
