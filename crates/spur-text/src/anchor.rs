//! Anchors: positions that follow edits.

use crate::change::{Bias, ChangeSet};
use crate::types::Version;

/// A position at a version. Resolves lazily through the change log since `version`.
/// Cheap to create; use for a few positions. For large sets use tracked anchors
/// ([`crate::Buffer::track`]), which resolve in O(1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Anchor {
    /// Version the offset refers to.
    pub version: Version,
    /// Byte offset at `version`.
    pub offset: usize,
    /// Which side the anchor sticks to.
    pub bias: Bias,
}

/// Handle to a tracked anchor. Carries a generation, so a removed id never resolves to a
/// reused slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AnchorId {
    index: usize,
    generation: u64,
}

struct Slot {
    generation: u64,
    entry: Option<(usize, Bias)>,
}

/// Tracked anchors, updated eagerly on every commit.
#[derive(Default)]
pub(crate) struct AnchorTable {
    slots: Vec<Slot>,
    free: Vec<usize>,
    live: usize,
}

impl AnchorTable {
    pub(crate) fn insert(&mut self, offset: usize, bias: Bias) -> AnchorId {
        self.live += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index];
            slot.generation += 1;
            slot.entry = Some((offset, bias));
            AnchorId {
                index,
                generation: slot.generation,
            }
        } else {
            self.slots.push(Slot {
                generation: 0,
                entry: Some((offset, bias)),
            });
            AnchorId {
                index: self.slots.len() - 1,
                generation: 0,
            }
        }
    }

    pub(crate) fn get(&self, id: AnchorId) -> Option<usize> {
        let slot = self.slots.get(id.index)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.entry.map(|(offset, _)| offset)
    }

    pub(crate) fn remove(&mut self, id: AnchorId) -> bool {
        match self.slots.get_mut(id.index) {
            Some(slot) if slot.generation == id.generation && slot.entry.is_some() => {
                slot.entry = None;
                self.free.push(id.index);
                self.live -= 1;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.live
    }

    pub(crate) fn map(&mut self, changes: &ChangeSet) {
        if self.live == 0 || changes.is_empty() {
            return;
        }
        for slot in &mut self.slots {
            if let Some((offset, bias)) = &mut slot.entry {
                *offset = changes.map_offset(*offset, *bias);
            }
        }
    }
}
