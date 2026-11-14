//! The text of an `input` node: a one-line field edited by graphemes.

use unicode_segmentation::UnicodeSegmentation;

/// Most bytes a field holds; longer text is cut.
pub const MAX_BYTES: usize = 4096;

/// A field's text, cursor and selection (grapheme indices).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Field {
    /// The text: one line.
    pub text: String,
    /// Cursor.
    pub cursor: usize,
    /// Other end of the selection.
    pub anchor: Option<usize>,
}

/// How far a move goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// One grapheme.
    Char,
    /// To the next word start (or end, backwards).
    Word,
    /// The start.
    Start,
    /// The end.
    End,
}

impl Field {
    fn len(&self) -> usize {
        self.text.graphemes(true).count()
    }

    /// Byte offset of grapheme `i`.
    fn byte(&self, i: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .nth(i)
            .map_or(self.text.len(), |(b, _)| b)
    }

    fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| (a.min(self.cursor), a.max(self.cursor)))
    }

    fn delete_range(&mut self, from: usize, to: usize) {
        let (b0, b1) = (self.byte(from), self.byte(to));
        self.text.replace_range(b0..b1, "");
        self.cursor = from;
        self.anchor = None;
    }

    /// Deletes the selection; true if there was one.
    fn delete_selection(&mut self) -> bool {
        if let Some((a, b)) = self.selection() {
            self.delete_range(a, b);
            true
        } else {
            self.anchor = None;
            false
        }
    }

    /// Types `text` at the cursor, replacing the selection. Line breaks and tabs
    /// become spaces, control chars are dropped, and the field stops at
    /// [`MAX_BYTES`].
    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        let clean: String = text
            .chars()
            .filter_map(|c| match c {
                '\n' | '\r' | '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect();
        let room = MAX_BYTES.saturating_sub(self.text.len());
        let mut cut = clean.len().min(room);
        while !clean.is_char_boundary(cut) {
            cut -= 1;
        }
        let clean = &clean[..cut];
        if clean.is_empty() {
            return;
        }
        let at = self.byte(self.cursor);
        let before = self.len();
        self.text.insert_str(at, clean);
        self.cursor += self.len() - before;
    }

    /// Deletes the selection, or the grapheme before the cursor.
    pub fn delete_backward(&mut self) {
        if !self.delete_selection() && self.cursor > 0 {
            self.delete_range(self.cursor - 1, self.cursor);
        }
    }

    /// Deletes the selection, or the grapheme after the cursor.
    pub fn delete_forward(&mut self) {
        if !self.delete_selection() && self.cursor < self.len() {
            self.delete_range(self.cursor, self.cursor + 1);
        }
    }

    /// Moves the cursor; `extend` keeps or starts a selection.
    pub fn move_by(&mut self, step: Step, forward: bool, extend: bool) {
        let len = self.len();
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else if let Some((a, b)) = self.selection() {
            // A plain move collapses the selection to its side.
            self.anchor = None;
            if step == Step::Char {
                self.cursor = if forward { b } else { a };
                return;
            }
        } else {
            self.anchor = None;
        }
        self.cursor = match step {
            Step::Char if forward => (self.cursor + 1).min(len),
            Step::Char => self.cursor.saturating_sub(1),
            Step::Start => 0,
            Step::End => len,
            Step::Word => self.word(forward),
        };
    }

    fn word(&self, forward: bool) -> usize {
        let g: Vec<bool> = self
            .text
            .graphemes(true)
            .map(|s| s.chars().any(char::is_alphanumeric))
            .collect();
        let mut i = self.cursor;
        if forward {
            while i < g.len() && g[i] {
                i += 1;
            }
            while i < g.len() && !g[i] {
                i += 1;
            }
        } else {
            while i > 0 && !g[i - 1] {
                i -= 1;
            }
            while i > 0 && g[i - 1] {
                i -= 1;
            }
        }
        i
    }

    /// Empties the field.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_moving_and_deleting() {
        let mut f = Field::default();
        f.insert("héllo wörld");
        assert_eq!(f.cursor, 11);
        f.move_by(Step::Word, false, false);
        assert_eq!(f.cursor, 6);
        f.move_by(Step::Char, true, true);
        f.move_by(Step::Char, true, true);
        f.insert("WO");
        assert_eq!(f.text, "héllo WOrld");
        f.move_by(Step::Start, false, false);
        f.delete_forward();
        f.move_by(Step::End, true, false);
        f.delete_backward();
        assert_eq!(f.text, "éllo WOrl");
        f.insert("a\nb\u{7}c");
        assert_eq!(f.text, "éllo WOrla bc");
        f.clear();
        f.insert(&"x".repeat(MAX_BYTES + 10));
        assert_eq!(f.text.len(), MAX_BYTES);
    }
}
