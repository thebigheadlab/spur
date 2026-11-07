//! Change sets: sorted, non-overlapping hunks over one base text.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

/// Which side a position sticks to when an edit touches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bias {
    /// Stick to the text before the position: snap to the start of a replacement.
    Left,
    /// Stick to the text after the position: snap to the end of a replacement.
    Right,
}

/// One hunk: replace `range` (byte offsets in the base text) with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Byte range in the base text that is removed.
    pub range: Range<usize>,
    /// Text inserted at `range.start`.
    pub text: Arc<str>,
}

impl Change {
    /// Creates a hunk.
    pub fn new(range: Range<usize>, text: impl Into<Arc<str>>) -> Self {
        Self {
            range,
            text: text.into(),
        }
    }
}

/// Why a [`ChangeSet`] could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeError {
    /// A hunk ends before it starts, or past the end of the base text.
    OutOfBounds {
        /// The offending range.
        range: Range<usize>,
        /// Length of the base text.
        base_len: usize,
    },
    /// Two hunks overlap.
    Overlap {
        /// First hunk's range.
        first: Range<usize>,
        /// Second hunk's range.
        second: Range<usize>,
    },
}

impl fmt::Display for ChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { range, base_len } => {
                write!(
                    f,
                    "range {range:?} is out of bounds for base length {base_len}"
                )
            }
            Self::Overlap { first, second } => write!(f, "hunks {first:?} and {second:?} overlap"),
        }
    }
}

impl std::error::Error for ChangeError {}

/// A hunk of an incoming change set that overlaps a hunk of a committed one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HunkConflict {
    /// Index of the incoming hunk, in the order of the incoming change set.
    pub incoming: usize,
    /// The committed hunk it overlaps, in the committed change set's base coordinates.
    pub committed: Change,
}

/// An edit of one base text: sorted, non-overlapping hunks in byte offsets.
///
/// The form is canonical: touching hunks are merged, empty hunks dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSet {
    base_len: usize,
    hunks: Vec<Change>,
    /// `prefix[i]` = (bytes inserted, bytes removed) by hunks `0..i`.
    prefix: Vec<(usize, usize)>,
}

impl ChangeSet {
    /// Builds a change set. Hunks may be given in any order; touching hunks are merged
    /// (an insertion is placed before a replacement starting at the same offset).
    ///
    /// # Errors
    /// [`ChangeError`] if a hunk is out of bounds or two hunks overlap.
    pub fn new(
        base_len: usize,
        changes: impl IntoIterator<Item = Change>,
    ) -> Result<Self, ChangeError> {
        let mut hunks: Vec<Change> = changes.into_iter().collect();
        for h in &hunks {
            if h.range.start > h.range.end || h.range.end > base_len {
                return Err(ChangeError::OutOfBounds {
                    range: h.range.clone(),
                    base_len,
                });
            }
        }
        hunks.sort_by_key(|h| (h.range.start, h.range.end));
        for w in hunks.windows(2) {
            if w[0].range.end > w[1].range.start {
                return Err(ChangeError::Overlap {
                    first: w[0].range.clone(),
                    second: w[1].range.clone(),
                });
            }
        }
        Ok(Self::from_sorted(base_len, hunks))
    }

    /// Builds from hunks that are sorted and may touch but never overlap.
    fn from_sorted(base_len: usize, hunks: Vec<Change>) -> Self {
        let mut merged: Vec<Change> = Vec::with_capacity(hunks.len());
        for h in hunks {
            if h.range.is_empty() && h.text.is_empty() {
                continue;
            }
            match merged.last_mut() {
                Some(last) if last.range.end == h.range.start => {
                    last.range.end = h.range.end;
                    if !h.text.is_empty() {
                        let mut s = String::with_capacity(last.text.len() + h.text.len());
                        s.push_str(&last.text);
                        s.push_str(&h.text);
                        last.text = s.into();
                    }
                }
                _ => merged.push(h),
            }
        }
        let mut prefix = Vec::with_capacity(merged.len() + 1);
        let (mut ins, mut del) = (0, 0);
        prefix.push((0, 0));
        for h in &merged {
            ins += h.text.len();
            del += h.range.len();
            prefix.push((ins, del));
        }
        Self {
            base_len,
            hunks: merged,
            prefix,
        }
    }

    /// A change set that changes nothing.
    pub fn empty(base_len: usize) -> Self {
        Self::from_sorted(base_len, Vec::new())
    }

    /// Inserts `text` at `at`.
    ///
    /// # Errors
    /// [`ChangeError::OutOfBounds`] if `at > base_len`.
    pub fn insert(base_len: usize, at: usize, text: &str) -> Result<Self, ChangeError> {
        Self::new(base_len, [Change::new(at..at, text)])
    }

    /// Deletes `range`.
    ///
    /// # Errors
    /// [`ChangeError::OutOfBounds`] if the range is invalid.
    pub fn delete(base_len: usize, range: Range<usize>) -> Result<Self, ChangeError> {
        Self::new(base_len, [Change::new(range, "")])
    }

    /// Replaces `range` with `text`.
    ///
    /// # Errors
    /// [`ChangeError::OutOfBounds`] if the range is invalid.
    pub fn replace(base_len: usize, range: Range<usize>, text: &str) -> Result<Self, ChangeError> {
        Self::new(base_len, [Change::new(range, text)])
    }

    /// Length of the text this change set applies to.
    pub fn base_len(&self) -> usize {
        self.base_len
    }

    /// Length of the text after applying.
    pub fn new_len(&self) -> usize {
        let (ins, del) = self.prefix[self.hunks.len()];
        self.base_len + ins - del
    }

    /// The hunks, sorted by offset.
    pub fn hunks(&self) -> &[Change] {
        &self.hunks
    }

    /// True if applying changes nothing.
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// Start of hunk `i` in the output text.
    fn new_start(&self, i: usize) -> usize {
        let (ins, del) = self.prefix[i];
        self.hunks[i].range.start + ins - del
    }

    /// Maps a byte offset in the base text to the output text.
    ///
    /// Positions before a hunk are unchanged; positions after it shift by its length delta;
    /// positions touching or inside a replaced range snap to the start ([`Bias::Left`]) or
    /// the end ([`Bias::Right`]) of the replacement.
    pub fn map_offset(&self, offset: usize, bias: Bias) -> usize {
        let i = self.hunks.partition_point(|h| h.range.end < offset);
        match self.hunks.get(i) {
            Some(h) if h.range.start <= offset => {
                let start = self.new_start(i);
                match bias {
                    Bias::Left => start,
                    Bias::Right => start + h.text.len(),
                }
            }
            _ => {
                let (ins, del) = self.prefix[i];
                offset + ins - del
            }
        }
    }

    /// The inverse change set, given the removed text for each hunk (in hunk order).
    pub(crate) fn invert_with(&self, mut removed: impl FnMut(Range<usize>) -> String) -> Self {
        let hunks = self
            .hunks
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let start = self.new_start(i);
                Change::new(start..start + h.text.len(), removed(h.range.clone()))
            })
            .collect();
        Self::from_sorted(self.new_len(), hunks)
    }

    /// Composes `self` then `next` (whose base is `self`'s output) into one change set.
    ///
    /// # Panics
    /// If `next.base_len() != self.new_len()`.
    #[must_use]
    pub fn compose(&self, next: &ChangeSet) -> ChangeSet {
        assert_eq!(next.base_len, self.new_len(), "compose: length mismatch");
        let mut first = self.ops().into_iter();
        let mut second = next.ops().into_iter();
        let mut out = OpBuilder::default();
        let mut ha = first.next();
        let mut hb = second.next();
        loop {
            match (ha.take(), hb.take()) {
                (None, None) => break,
                (Some(Op::Delete(n)), y) => {
                    out.delete(n);
                    ha = first.next();
                    hb = y;
                }
                (x, Some(Op::Insert(s))) => {
                    out.insert(&s);
                    ha = x;
                    hb = second.next();
                }
                (Some(Op::Retain(i)), Some(Op::Retain(j))) => {
                    let n = i.min(j);
                    out.retain(n);
                    ha = if i > n {
                        Some(Op::Retain(i - n))
                    } else {
                        first.next()
                    };
                    hb = if j > n {
                        Some(Op::Retain(j - n))
                    } else {
                        second.next()
                    };
                }
                (Some(Op::Retain(i)), Some(Op::Delete(j))) => {
                    let n = i.min(j);
                    out.delete(n);
                    ha = if i > n {
                        Some(Op::Retain(i - n))
                    } else {
                        first.next()
                    };
                    hb = if j > n {
                        Some(Op::Delete(j - n))
                    } else {
                        second.next()
                    };
                }
                (Some(Op::Insert(s)), Some(Op::Retain(j))) => {
                    let n = s.len().min(j);
                    out.insert(&s[..n]);
                    ha = if s.len() > n {
                        Some(Op::Insert(s[n..].into()))
                    } else {
                        first.next()
                    };
                    hb = if j > n {
                        Some(Op::Retain(j - n))
                    } else {
                        second.next()
                    };
                }
                (Some(Op::Insert(s)), Some(Op::Delete(j))) => {
                    let n = s.len().min(j);
                    ha = if s.len() > n {
                        Some(Op::Insert(s[n..].into()))
                    } else {
                        first.next()
                    };
                    hb = if j > n {
                        Some(Op::Delete(j - n))
                    } else {
                        second.next()
                    };
                }
                (Some(Op::Retain(_) | Op::Insert(_)), None) | (None, Some(_)) => {
                    unreachable!("compose: lengths checked above")
                }
            }
        }
        out.finish(self.base_len)
    }

    fn ops(&self) -> Vec<Op> {
        let mut ops = Vec::with_capacity(self.hunks.len() * 3 + 1);
        let mut pos = 0;
        for h in &self.hunks {
            if h.range.start > pos {
                ops.push(Op::Retain(h.range.start - pos));
            }
            if !h.range.is_empty() {
                ops.push(Op::Delete(h.range.len()));
            }
            if !h.text.is_empty() {
                ops.push(Op::Insert(h.text.to_string()));
            }
            pos = h.range.end;
        }
        if self.base_len > pos {
            ops.push(Op::Retain(self.base_len - pos));
        }
        ops
    }

    /// Transforms `self` so it applies after `over`; both must share a base text.
    ///
    /// # Errors
    /// The conflicting hunks if any hunk of `self` overlaps a hunk of `over`.
    ///
    /// # Panics
    /// If the two change sets have different base lengths.
    pub fn rebase(&self, over: &ChangeSet) -> Result<ChangeSet, Vec<HunkConflict>> {
        let tagged: Vec<(usize, Change)> = self.hunks.iter().cloned().enumerate().collect();
        let mapped = rebase_tagged(&tagged, self.base_len, over)?;
        Ok(Self::from_sorted(
            over.new_len(),
            mapped.into_iter().map(|(_, c)| c).collect(),
        ))
    }
}

/// Rebases hunks tagged with their original index. Keeps the tags so conflicts can be
/// reported against the caller's original hunks across several rebases.
pub(crate) fn rebase_tagged(
    hunks: &[(usize, Change)],
    base_len: usize,
    over: &ChangeSet,
) -> Result<Vec<(usize, Change)>, Vec<HunkConflict>> {
    assert_eq!(base_len, over.base_len, "rebase: base length mismatch");
    let mut conflicts = Vec::new();
    let mut out = Vec::with_capacity(hunks.len());
    let mut j = 0;
    for (tag, h) in hunks {
        // Skip committed hunks entirely before `h` (not touching).
        while j < over.hunks.len() && over.hunks[j].range.end < h.range.start {
            j += 1;
        }
        let mut k = j;
        while k < over.hunks.len() && over.hunks[k].range.start <= h.range.end {
            let c = &over.hunks[k];
            if overlaps(&h.range, &c.range) {
                conflicts.push(HunkConflict {
                    incoming: *tag,
                    committed: c.clone(),
                });
            }
            k += 1;
        }
        let start = over.map_offset(h.range.start, Bias::Right);
        let end = if h.range.is_empty() {
            start
        } else {
            over.map_offset(h.range.end, Bias::Left).max(start)
        };
        out.push((*tag, Change::new(start..end, h.text.clone())));
    }
    if conflicts.is_empty() {
        Ok(out)
    } else {
        Err(conflicts)
    }
}

/// Two hunks conflict if their ranges share at least one byte, or one inserts strictly
/// inside the other's replaced range. Touching ranges and same-offset inserts don't.
fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    let shared = a.start.max(b.start) < a.end.min(b.end);
    let a_inside_b = a.is_empty() && b.start < a.start && a.start < b.end;
    let b_inside_a = b.is_empty() && a.start < b.start && b.start < a.end;
    shared || a_inside_b || b_inside_a
}

enum Op {
    Retain(usize),
    Delete(usize),
    Insert(String),
}

#[derive(Default)]
struct OpBuilder {
    pos: usize,
    hunks: Vec<Change>,
    cur: Option<(Range<usize>, String)>,
}

impl OpBuilder {
    fn flush(&mut self) {
        if let Some((range, text)) = self.cur.take() {
            self.hunks.push(Change::new(range, text));
        }
    }
    fn retain(&mut self, n: usize) {
        if n > 0 {
            self.flush();
            self.pos += n;
        }
    }
    fn delete(&mut self, n: usize) {
        let pos = self.pos;
        let cur = self.cur.get_or_insert_with(|| (pos..pos, String::new()));
        cur.0.end += n;
        self.pos += n;
    }
    fn insert(&mut self, s: &str) {
        let pos = self.pos;
        self.cur
            .get_or_insert_with(|| (pos..pos, String::new()))
            .1
            .push_str(s);
    }
    fn finish(mut self, base_len: usize) -> ChangeSet {
        self.flush();
        ChangeSet::from_sorted(base_len, self.hunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, cs: &ChangeSet) -> String {
        let mut out = String::new();
        let mut pos = 0;
        for h in cs.hunks() {
            out.push_str(&text[pos..h.range.start]);
            out.push_str(&h.text);
            pos = h.range.end;
        }
        out.push_str(&text[pos..]);
        out
    }

    #[test]
    fn canonical_merges_touching_hunks() {
        let cs = ChangeSet::new(
            10,
            [
                Change::new(4..6, "b"),
                Change::new(2..4, "a"),
                Change::new(6..6, "c"),
            ],
        )
        .unwrap();
        assert_eq!(cs.hunks(), &[Change::new(2..6, "abc")]);
    }

    #[test]
    fn rejects_overlap_and_bounds() {
        assert!(ChangeSet::new(10, [Change::new(2..5, ""), Change::new(4..6, "")]).is_err());
        assert!(ChangeSet::new(3, [Change::new(2..5, "")]).is_err());
    }

    #[test]
    fn map_offset_bias() {
        let cs = ChangeSet::replace(10, 2..4, "xyz").unwrap();
        assert_eq!(cs.map_offset(1, Bias::Right), 1);
        assert_eq!(cs.map_offset(2, Bias::Left), 2);
        assert_eq!(cs.map_offset(2, Bias::Right), 5);
        assert_eq!(cs.map_offset(3, Bias::Left), 2);
        assert_eq!(cs.map_offset(4, Bias::Right), 5);
        assert_eq!(cs.map_offset(5, Bias::Left), 6);
        let ins = ChangeSet::insert(10, 3, "ab").unwrap();
        assert_eq!(ins.map_offset(3, Bias::Left), 3);
        assert_eq!(ins.map_offset(3, Bias::Right), 5);
    }

    #[test]
    fn compose_matches_sequential() {
        let t = "hello world";
        let a =
            ChangeSet::new(11, [Change::new(0..5, "HELLO"), Change::new(6..6, "big ")]).unwrap();
        let b = ChangeSet::new(a.new_len(), [Change::new(3..12, "p")]).unwrap();
        let ab = a.compose(&b);
        assert_eq!(apply(t, &ab), apply(&apply(t, &a), &b));
    }

    #[test]
    fn rebase_orders_same_offset_inserts_committed_first() {
        let t = "abc";
        let committed = ChangeSet::insert(3, 1, "X").unwrap();
        let incoming = ChangeSet::insert(3, 1, "Y").unwrap();
        let r = incoming.rebase(&committed).unwrap();
        assert_eq!(apply(&apply(t, &committed), &r), "aXYbc");
    }

    #[test]
    fn rebase_conflicts_on_overlap() {
        let committed = ChangeSet::replace(10, 2..5, "").unwrap();
        let incoming = ChangeSet::replace(10, 4..6, "z").unwrap();
        let err = incoming.rebase(&committed).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].committed.range, 2..5);
        // Insert strictly inside a replaced range conflicts; at its edge it does not.
        assert!(
            ChangeSet::insert(10, 3, "q")
                .unwrap()
                .rebase(&committed)
                .is_err()
        );
        assert!(
            ChangeSet::insert(10, 5, "q")
                .unwrap()
                .rebase(&committed)
                .is_ok()
        );
    }

    #[test]
    fn invert_round_trips() {
        let t = "hello world";
        let cs = ChangeSet::new(11, [Change::new(0..1, "J"), Change::new(5..11, "!")]).unwrap();
        let out = apply(t, &cs);
        let inv = cs.invert_with(|r| t[r].to_string());
        assert_eq!(apply(&out, &inv), t);
    }
}
