//! Read-only views of a buffer at one version.

use std::ops::Range;

use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

use crate::storage::Text;
use crate::types::Version;

/// A position as line and column. Lines break at LF, CRLF and CR; `column` is in bytes
/// from the start of the line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    /// Zero-based line.
    pub line: usize,
    /// Zero-based byte column.
    pub column: usize,
}

/// The text of a buffer at one version. Cloning is O(1); `Send + Sync`, so background
/// work (highlighting, search, agents) can hold one while the buffer keeps changing.
///
/// Offsets are bytes. Methods return `None` for offsets past the end or not on a char
/// boundary instead of panicking.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub(crate) text: Text,
    pub(crate) version: Version,
}

impl Snapshot {
    /// The version this snapshot shows.
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

    /// True if `offset` is within the text and on a char boundary.
    pub fn is_boundary(&self, offset: usize) -> bool {
        self.text.is_char_boundary(offset)
    }

    fn check_range(&self, range: &Range<usize>) -> bool {
        range.start <= range.end && self.is_boundary(range.start) && self.is_boundary(range.end)
    }

    /// The text in `range`, copied.
    pub fn slice(&self, range: Range<usize>) -> Option<String> {
        self.check_range(&range).then(|| self.text.slice(range))
    }

    /// The text in `range` as borrowed chunks, without copying.
    pub fn chunks(&self, range: Range<usize>) -> Option<impl Iterator<Item = &str>> {
        self.check_range(&range).then(|| self.text.chunks(range))
    }

    /// Number of lines (line breaks + 1).
    pub fn line_count(&self) -> usize {
        self.text.len_lines()
    }

    /// Byte range of `line`, including its line ending.
    pub fn line_range(&self, line: usize) -> Option<Range<usize>> {
        let count = self.line_count();
        (line < count).then(|| {
            let start = self.text.line_to_byte(line);
            let end = if line + 1 < count {
                self.text.line_to_byte(line + 1)
            } else {
                self.len()
            };
            start..end
        })
    }

    /// Char index of a byte offset.
    pub fn byte_to_char(&self, offset: usize) -> Option<usize> {
        self.is_boundary(offset)
            .then(|| self.text.byte_to_char(offset))
    }

    /// Byte offset of a char index.
    pub fn char_to_byte(&self, ch: usize) -> Option<usize> {
        (ch <= self.text.len_chars()).then(|| self.text.char_to_byte(ch))
    }

    /// UTF-16 code unit index of a byte offset (for LSP).
    pub fn byte_to_utf16(&self, offset: usize) -> Option<usize> {
        self.is_boundary(offset)
            .then(|| self.text.byte_to_utf16(offset))
    }

    /// Byte offset of a UTF-16 code unit index. An index inside a surrogate pair maps to the
    /// start of its char.
    pub fn utf16_to_byte(&self, unit: usize) -> Option<usize> {
        (unit <= self.text.len_utf16()).then(|| self.text.utf16_to_byte(unit))
    }

    /// Line and byte column of an offset.
    pub fn byte_to_point(&self, offset: usize) -> Option<Point> {
        self.is_boundary(offset).then(|| {
            let line = self.text.byte_to_line(offset);
            Point {
                line,
                column: offset - self.text.line_to_byte(line),
            }
        })
    }

    /// Byte offset of a point. The column must not run past the line (including its ending).
    pub fn point_to_byte(&self, point: Point) -> Option<usize> {
        let range = self.line_range(point.line)?;
        let offset = range.start + point.column;
        (offset <= range.end && self.is_boundary(offset)).then_some(offset)
    }

    /// The next grapheme cluster boundary after `offset`, or `None` at the end.
    pub fn next_grapheme_boundary(&self, offset: usize) -> Option<usize> {
        if !self.is_boundary(offset) || offset == self.len() {
            return None;
        }
        let mut cursor = GraphemeCursor::new(offset, self.len(), true);
        let (mut chunk, mut start) = self.text.chunk_at(offset);
        loop {
            match cursor.next_boundary(chunk, start) {
                Ok(b) => return b,
                Err(GraphemeIncomplete::NextChunk) => {
                    let next = start + chunk.len();
                    (chunk, start) = self.text.chunk_at(next);
                }
                Err(GraphemeIncomplete::PreContext(n)) => {
                    let (ctx, ctx_start) = self.text.chunk_at(n - 1);
                    cursor.provide_context(&ctx[..n - ctx_start], ctx_start);
                }
                Err(_) => unreachable!("grapheme cursor: unexpected state"),
            }
        }
    }

    /// The previous grapheme cluster boundary before `offset`, or `None` at the start.
    pub fn prev_grapheme_boundary(&self, offset: usize) -> Option<usize> {
        if !self.is_boundary(offset) || offset == 0 {
            return None;
        }
        let mut cursor = GraphemeCursor::new(offset, self.len(), true);
        let (mut chunk, mut start) = self.text.chunk_at(offset - 1);
        loop {
            match cursor.prev_boundary(chunk, start) {
                Ok(b) => return b,
                Err(GraphemeIncomplete::PrevChunk) => {
                    (chunk, start) = self.text.chunk_at(start - 1);
                }
                Err(GraphemeIncomplete::PreContext(n)) => {
                    let (ctx, ctx_start) = self.text.chunk_at(n - 1);
                    cursor.provide_context(&ctx[..n - ctx_start], ctx_start);
                }
                Err(_) => unreachable!("grapheme cursor: unexpected state"),
            }
        }
    }
}
