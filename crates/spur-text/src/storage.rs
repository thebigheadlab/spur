//! Private rope wrapper. The only module that names the rope crate, so it can be swapped.

use std::ops::Range;

use ropey::{LineType, Rope};

const LINES: LineType = LineType::LF_CR;

/// Immutable-by-clone text storage. Cloning is O(1).
#[derive(Clone, Debug, Default)]
pub(crate) struct Text(Rope);

impl Text {
    pub(crate) fn new(text: &str) -> Self {
        Self(Rope::from_str(text))
    }
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
    pub(crate) fn is_char_boundary(&self, byte: usize) -> bool {
        byte <= self.0.len() && self.0.is_char_boundary(byte)
    }
    pub(crate) fn replace(&mut self, range: Range<usize>, text: &str) {
        if !range.is_empty() {
            self.0.remove(range.clone());
        }
        if !text.is_empty() {
            self.0.insert(range.start, text);
        }
    }
    pub(crate) fn slice(&self, range: Range<usize>) -> String {
        self.0.slice(range).to_string()
    }
    pub(crate) fn chunks(&self, range: Range<usize>) -> impl Iterator<Item = &str> {
        self.0.slice(range).chunks()
    }
    /// The chunk containing `byte` and the chunk's start offset.
    pub(crate) fn chunk_at(&self, byte: usize) -> (&str, usize) {
        self.0.chunk(byte)
    }
    pub(crate) fn byte_to_char(&self, byte: usize) -> usize {
        self.0.byte_to_char_idx(byte)
    }
    pub(crate) fn char_to_byte(&self, ch: usize) -> usize {
        self.0.char_to_byte_idx(ch)
    }
    pub(crate) fn len_chars(&self) -> usize {
        self.0.len_chars()
    }
    pub(crate) fn byte_to_utf16(&self, byte: usize) -> usize {
        self.0.byte_to_utf16_idx(byte)
    }
    pub(crate) fn utf16_to_byte(&self, unit: usize) -> usize {
        self.0.utf16_to_byte_idx(unit)
    }
    pub(crate) fn len_utf16(&self) -> usize {
        self.0.len_utf16()
    }
    pub(crate) fn byte_to_line(&self, byte: usize) -> usize {
        self.0.byte_to_line_idx(byte, LINES)
    }
    pub(crate) fn line_to_byte(&self, line: usize) -> usize {
        self.0.line_to_byte_idx(line, LINES)
    }
    pub(crate) fn len_lines(&self) -> usize {
        self.0.len_lines(LINES)
    }
}
