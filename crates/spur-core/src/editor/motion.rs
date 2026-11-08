//! Cursor motions as pure functions of a snapshot. Offsets are bytes on char
//! boundaries; columns used for vertical motion ("goals") are in chars.

use spur_text::Snapshot;

/// `offset` clamped to the text and moved back onto a char boundary.
pub fn snap_char(s: &Snapshot, offset: usize) -> usize {
    let mut o = offset.min(s.len());
    while !s.is_boundary(o) {
        o -= 1;
    }
    o
}

/// `offset` moved back to the start of the grapheme cluster it falls in.
pub fn snap_grapheme(s: &Snapshot, offset: usize) -> usize {
    let o = snap_char(s, offset);
    match s.prev_grapheme_boundary(o) {
        Some(p) if s.next_grapheme_boundary(p) != Some(o) => p,
        _ => o,
    }
}

/// Line containing `offset`.
pub fn line_of(s: &Snapshot, offset: usize) -> usize {
    s.byte_to_point(snap_char(s, offset)).map_or(0, |p| p.line)
}

/// Bytes of line ending at the end of `range` (0, 1 for `\n` or `\r`, 2 for `\r\n`).
pub fn ending_len(s: &Snapshot, range: std::ops::Range<usize>) -> usize {
    // Line endings are ASCII, so look at the last two chars, not the last two bytes.
    let end_char = s.byte_to_char(range.end).unwrap_or(0);
    let tail_start = s
        .char_to_byte(end_char.saturating_sub(2))
        .unwrap_or(range.start)
        .max(range.start);
    let tail = s.slice(tail_start..range.end).unwrap_or_default();
    if tail.ends_with("\r\n") {
        2
    } else {
        usize::from(tail.ends_with('\n') || tail.ends_with('\r'))
    }
}

/// Start of `line`.
pub fn line_start(s: &Snapshot, line: usize) -> usize {
    s.line_range(line).map_or(s.len(), |r| r.start)
}

/// End of `line`'s content, before its line ending.
pub fn line_end(s: &Snapshot, line: usize) -> usize {
    let Some(r) = s.line_range(line) else {
        return s.len();
    };
    r.end - ending_len(s, r)
}

/// Text of `line` without its ending.
pub fn line_text(s: &Snapshot, line: usize) -> String {
    s.slice(line_start(s, line)..line_end(s, line))
        .unwrap_or_default()
}

/// Char column of `offset` within its line.
pub fn char_col(s: &Snapshot, offset: usize) -> usize {
    let offset = snap_char(s, offset);
    let start = line_start(s, line_of(s, offset));
    s.byte_to_char(offset)
        .unwrap_or(0)
        .saturating_sub(s.byte_to_char(start).unwrap_or(0))
}

/// Offset of char column `col` on `line`, clamped to the line's content.
pub fn offset_at_col(s: &Snapshot, line: usize, col: usize) -> usize {
    let start = line_start(s, line);
    let end = line_end(s, line);
    let c0 = s.byte_to_char(start).unwrap_or(0);
    let c_end = s.byte_to_char(end).unwrap_or(c0);
    let at = s.char_to_byte((c0 + col).min(c_end)).unwrap_or(end);
    // Goals count chars; never land inside a grapheme cluster.
    snap_grapheme(s, at)
}

/// One grapheme left. With `wrap = false`, stops at the line start.
pub fn left(s: &Snapshot, offset: usize, wrap: bool) -> usize {
    if !wrap && offset == line_start(s, line_of(s, offset)) {
        return offset;
    }
    s.prev_grapheme_boundary(offset).unwrap_or(0)
}

/// One grapheme right (a CRLF is one grapheme). With `wrap = false`, stops at the end of
/// the line's content.
pub fn right(s: &Snapshot, offset: usize, wrap: bool) -> usize {
    if !wrap && offset >= line_end(s, line_of(s, offset)) {
        return offset;
    }
    s.next_grapheme_boundary(offset).unwrap_or(s.len())
}

/// `lines` lines up (negative) or down, keeping char column `goal`.
pub fn vertical(s: &Snapshot, offset: usize, lines: isize, goal: usize) -> usize {
    let line = line_of(s, offset);
    let last = s.line_count().saturating_sub(1);
    let target = line.saturating_add_signed(lines).min(last);
    offset_at_col(s, target, goal)
}

/// First non-blank char of `line` (or its end if blank).
pub fn first_nonblank(s: &Snapshot, line: usize) -> usize {
    let start = line_start(s, line);
    let text = line_text(s, line);
    start
        + text
            .char_indices()
            .find(|(_, c)| !matches!(c, ' ' | '\t'))
            .map_or(text.len(), |(i, _)| i)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Newline,
    Space,
    Word,
    Punct,
}

fn class(c: char, big: bool) -> Class {
    match c {
        '\n' | '\r' => Class::Newline,
        c if c.is_whitespace() => Class::Space,
        _ if big => Class::Word,
        c if c.is_alphanumeric() || c == '_' => Class::Word,
        _ => Class::Punct,
    }
}

fn chars_after(s: &Snapshot, offset: usize) -> impl Iterator<Item = (usize, char)> + '_ {
    let mut at = offset;
    s.chunks(offset..s.len())
        .into_iter()
        .flatten()
        .flat_map(str::chars)
        .map(move |c| {
            let here = at;
            at += c.len_utf8();
            (here, c)
        })
}

fn char_before(s: &Snapshot, offset: usize) -> Option<(usize, char)> {
    let start = (offset.saturating_sub(4)..offset).find(|&i| {
        s.is_boundary(i) && s.slice(i..offset).is_some_and(|t| t.chars().count() == 1)
    })?;
    s.slice(start..offset)?.chars().next().map(|c| (start, c))
}

/// Start of the next word (Vim `w`; `big` = `W`). An empty line counts as a word.
pub fn word_next(s: &Snapshot, offset: usize, big: bool) -> usize {
    let mut it = chars_after(s, offset);
    let Some((_, first)) = it.next() else {
        return s.len();
    };
    let start_class = class(first, big);
    let mut newlines = usize::from(start_class == Class::Newline);
    let mut seen_space = start_class == Class::Space;
    let mut prev_cr = first == '\r';
    for (i, c) in it {
        let k = class(c, big);
        match k {
            Class::Newline => {
                if !(prev_cr && c == '\n') {
                    newlines += 1;
                }
                prev_cr = c == '\r';
                // Two line breaks with nothing between: stop on the empty line.
                if newlines >= 2 {
                    return i;
                }
            }
            Class::Space => {
                prev_cr = false;
                seen_space = true;
            }
            _ => {
                if k != start_class || seen_space || newlines > 0 {
                    return i;
                }
                prev_cr = false;
            }
        }
    }
    s.len()
}

/// Start of the previous word (Vim `b`).
pub fn word_prev(s: &Snapshot, offset: usize, big: bool) -> usize {
    let mut at = offset;
    // Skip whitespace (stopping at an empty line).
    let mut newlines = 0;
    while let Some((i, c)) = char_before(s, at) {
        match class(c, big) {
            Class::Newline => {
                // A CRLF is one line break.
                let start = if c == '\n' && char_before(s, i).is_some_and(|(_, p)| p == '\r') {
                    i - 1
                } else {
                    i
                };
                newlines += 1;
                if newlines >= 2 && at != offset {
                    return at;
                }
                at = start;
            }
            Class::Space => at = i,
            _ => break,
        }
    }
    let Some((_, c)) = char_before(s, at) else {
        return at;
    };
    let k = class(c, big);
    while let Some((i, c)) = char_before(s, at) {
        if class(c, big) != k {
            break;
        }
        at = i;
    }
    at
}

/// Last char of the current or next word (Vim `e`).
pub fn word_end(s: &Snapshot, offset: usize, big: bool) -> usize {
    let mut it = chars_after(s, offset).skip(1).peekable();
    // Skip whitespace.
    while it
        .peek()
        .is_some_and(|&(_, c)| matches!(class(c, big), Class::Space | Class::Newline))
    {
        it.next();
    }
    let Some((mut last, c)) = it.next() else {
        return offset;
    };
    let k = class(c, big);
    for (i, c) in it {
        if class(c, big) != k {
            break;
        }
        last = i;
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_text::Buffer;

    fn snap(t: &str) -> Snapshot {
        Buffer::new(t).snapshot()
    }

    #[test]
    fn lines() {
        let s = snap("ab\r\ncd\n\nx");
        assert_eq!(line_end(&s, 0), 2);
        assert_eq!(line_start(&s, 1), 4);
        assert_eq!(line_end(&s, 1), 6);
        assert_eq!(line_end(&s, 2), 7);
        assert_eq!(line_text(&s, 3), "x");
        assert_eq!(first_nonblank(&snap("  \tfoo"), 0), 3);
        // A multibyte last char must not hide the line ending.
        let m = snap("é\nx");
        assert_eq!(line_end(&m, 0), 2);
        assert_eq!(line_end(&snap("aé\r\nx"), 0), 3);
        assert_eq!(line_end(&snap("é"), 0), 2);
        // Offsets inside a char are snapped, not mapped to line 0 or underflowed.
        let t = snap("a\né");
        assert_eq!(line_of(&t, 3), 1);
        assert_eq!(char_col(&t, 3), 0);
    }

    #[test]
    fn vertical_skips_into_grapheme() {
        // Column 1 on the second line is inside "e" + combining acute.
        let s = snap("ab\ne\u{301}x");
        assert_eq!(vertical(&s, 1, 1, 1), 3);
        assert_eq!(snap_grapheme(&s, 4), 3);
        assert_eq!(snap_grapheme(&s, 6), 6);
    }

    #[test]
    fn horizontal() {
        let s = snap("ab\r\ncd");
        assert_eq!(right(&s, 2, true), 4, "CRLF is one step");
        assert_eq!(right(&s, 2, false), 2);
        assert_eq!(left(&s, 4, false), 4);
        assert_eq!(left(&s, 4, true), 2);
        let e = snap("e\u{301}x");
        assert_eq!(right(&e, 0, true), 3, "combining mark stays with its base");
    }

    #[test]
    fn vertical_keeps_goal() {
        let s = snap("hello\nhi\nworld");
        let at = vertical(&s, 4, 1, 4);
        assert_eq!(at, 8, "clamped to end of 'hi'");
        assert_eq!(vertical(&s, at, 1, 4), 9 + 4);
        assert_eq!(vertical(&s, 0, -5, 0), 0);
        assert_eq!(vertical(&s, 0, 99, 2), 11);
        assert_eq!(char_col(&snap("äb"), 2), 1);
    }

    #[test]
    fn word_prev_crlf_empty_line() {
        let s = snap("ab\r\n\r\ncd");
        assert_eq!(word_prev(&s, 6, false), 4);
        assert_eq!(word_prev(&s, 4, false), 0);
    }

    #[test]
    fn words() {
        let s = snap("foo.bar  baz\n\nqux");
        assert_eq!(word_next(&s, 0, false), 3);
        assert_eq!(word_next(&s, 3, false), 4);
        assert_eq!(word_next(&s, 4, false), 9);
        assert_eq!(word_next(&s, 0, true), 9);
        assert_eq!(word_next(&s, 9, false), 13, "empty line is a word");
        assert_eq!(word_next(&s, 13, false), 14);
        assert_eq!(word_prev(&s, 14, false), 13);
        assert_eq!(word_prev(&s, 13, false), 9);
        assert_eq!(word_prev(&s, 9, false), 4);
        assert_eq!(word_prev(&s, 9, true), 0);
        assert_eq!(word_prev(&s, 2, false), 0);
        assert_eq!(word_end(&s, 0, false), 2);
        assert_eq!(word_end(&s, 2, false), 3);
        assert_eq!(word_end(&s, 2, true), 6);
        assert_eq!(word_end(&s, 6, false), 11);
        assert_eq!(word_next(&snap("ab"), 0, false), 2);
    }
}
