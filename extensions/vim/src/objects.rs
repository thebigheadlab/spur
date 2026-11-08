//! Pure helpers for the grammar: find-char, bracket matching and text objects.

use spur_core::editor::motion::{
    Class, char_before, chars_after, class, line_end, line_of, line_start, line_text,
};
use spur_text::Snapshot;

/// Like `f` / `t` / `F` / `T`: the `n`th `target` on the current line. `till` stops one
/// char before it (after it when `backward`). `None` if not found.
pub fn find_char(
    s: &Snapshot,
    offset: usize,
    target: char,
    n: usize,
    till: bool,
    backward: bool,
) -> Option<usize> {
    let line = line_of(s, offset);
    let (start, end) = (line_start(s, line), line_end(s, line));
    let text = s.slice(start..end)?;
    let rel = offset - start;
    let mut hits: Vec<usize> = text
        .char_indices()
        .filter(|&(i, c)| c == target && if backward { i < rel } else { i > rel })
        .map(|(i, _)| i)
        .collect();
    if backward {
        hits.reverse();
    }
    let hit = *hits.get(n.max(1) - 1)?;
    let at = if !till {
        hit
    } else if backward {
        hit + text[hit..].chars().next()?.len_utf8()
    } else {
        text[..hit].char_indices().last().map_or(0, |(i, _)| i)
    };
    Some(start + at)
}

fn pair_of(c: char) -> Option<(char, char, bool)> {
    Some(match c {
        '(' => ('(', ')', true),
        ')' => ('(', ')', false),
        '[' => ('[', ']', true),
        ']' => ('[', ']', false),
        '{' => ('{', '}', true),
        '}' => ('{', '}', false),
        '<' => ('<', '>', true),
        '>' => ('<', '>', false),
        _ => return None,
    })
}

/// Vim `%`: from the first bracket at or after the cursor on this line, its match.
pub fn match_bracket(s: &Snapshot, offset: usize) -> Option<usize> {
    let line_end = line_end(s, line_of(s, offset));
    let (at, c) = chars_after(s, offset)
        .take_while(|&(i, _)| i < line_end)
        .find(|&(_, c)| matches!(c, '(' | ')' | '[' | ']' | '{' | '}'))?;
    let (open, close, forward) = pair_of(c)?;
    if forward {
        let mut depth = 0usize;
        for (i, ch) in chars_after(s, at) {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
        None
    } else {
        enclosing_open(s, at, open, close)
    }
}

/// The unmatched `open` before `offset`.
fn enclosing_open(s: &Snapshot, offset: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = offset;
    while let Some((i, c)) = char_before(s, at) {
        if c == close {
            depth += 1;
        } else if c == open {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
        at = i;
    }
    None
}

/// The unmatched `close` at or after `offset`.
fn enclosing_close(s: &Snapshot, offset: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in chars_after(s, offset) {
        if c == open {
            depth += 1;
        } else if c == close {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

/// A text object: the byte range and whether it is whole lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextObject {
    /// Range in bytes.
    pub range: std::ops::Range<usize>,
    /// Whole lines.
    pub linewise: bool,
}

/// Text object `kind` around `offset`: `w`, `W`, quotes (`"`, `'`, backtick), brackets
/// (`(`, `)`, `b`, `[`, `]`, `{`, `}`, `B`, `<`, `>`) or `p` (paragraph).
pub fn text_object(s: &Snapshot, offset: usize, kind: char, inner: bool) -> Option<TextObject> {
    let charwise = |range| {
        Some(TextObject {
            range,
            linewise: false,
        })
    };
    match kind {
        'w' | 'W' => charwise(word_object(s, offset, kind == 'W', inner)),
        '"' | '\'' | '`' => charwise(quote_object(s, offset, kind, inner)?),
        'p' => Some(paragraph_object(s, offset, inner)),
        _ => {
            let k = match kind {
                'b' => '(',
                'B' => '{',
                k => k,
            };
            let (open, close, _) = pair_of(k)?;
            let at_open = chars_after(s, offset)
                .next()
                .is_some_and(|(_, c)| c == open);
            let start = if at_open {
                offset
            } else {
                enclosing_open(s, offset, open, close)?
            };
            let end = enclosing_close(s, start + open.len_utf8(), open, close)?;
            if inner {
                charwise(start + open.len_utf8()..end)
            } else {
                charwise(start..end + close.len_utf8())
            }
        }
    }
}

#[allow(clippy::many_single_char_names)]
fn word_object(s: &Snapshot, offset: usize, big: bool, inner: bool) -> std::ops::Range<usize> {
    let line = line_of(s, offset);
    let (ls, le) = (line_start(s, line), line_end(s, line));
    let Some((_, here)) = chars_after(s, offset).next().filter(|&(i, _)| i < le) else {
        return offset..offset;
    };
    let k = class(here, big);
    let run = |from: usize, k: Class| -> std::ops::Range<usize> {
        let mut start = from;
        while let Some((i, c)) = char_before(s, start) {
            if i < ls || class(c, big) != k {
                break;
            }
            start = i;
        }
        let end = chars_after(s, from)
            .find(|&(i, c)| i >= le || class(c, big) != k)
            .map_or(le, |(i, _)| i);
        start..end
    };
    let r = run(offset, k);
    if inner {
        return r;
    }
    if k == Class::Space {
        // `aw` on whitespace: the space plus the following word.
        return match chars_after(s, r.end).next() {
            Some((i, c)) if i < le => r.start..run(i, class(c, big)).end,
            _ => r,
        };
    }
    match chars_after(s, r.end).next() {
        Some((i, c)) if i < le && class(c, big) == Class::Space => {
            r.start..run(i, Class::Space).end
        }
        _ => {
            // No trailing space: take leading space instead.
            match char_before(s, r.start) {
                Some((i, c)) if i >= ls && class(c, big) == Class::Space => {
                    run(i, Class::Space).start..r.end
                }
                _ => r,
            }
        }
    }
}

fn quote_object(
    s: &Snapshot,
    offset: usize,
    q: char,
    inner: bool,
) -> Option<std::ops::Range<usize>> {
    let line = line_of(s, offset);
    let start = line_start(s, line);
    let text = line_text(s, line);
    let rel = offset - start;
    let mut quotes = Vec::new();
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if c == q && !escaped {
            quotes.push(i);
        }
        escaped = c == '\\' && !escaped;
    }
    let (a, b) = quotes
        .chunks_exact(2)
        .map(|p| (p[0], p[1]))
        .find(|&(a, b)| rel <= b && (rel >= a || quotes.iter().all(|&x| x >= rel || x < a)))?;
    let ql = q.len_utf8();
    Some(if inner {
        start + a + ql..start + b
    } else {
        start + a..start + b + ql
    })
}

fn paragraph_object(s: &Snapshot, offset: usize, inner: bool) -> TextObject {
    let blank = |l: usize| line_text(s, l).trim().is_empty();
    let line = line_of(s, offset);
    let last = s.line_count() - 1;
    let kind = blank(line);
    let mut a = line;
    while a > 0 && blank(a - 1) == kind {
        a -= 1;
    }
    let mut b = line;
    while b < last && blank(b + 1) == kind {
        b += 1;
    }
    if !inner {
        while b < last && blank(b + 1) != kind {
            b += 1;
        }
    }
    let end = s.line_range(b).map_or(s.len(), |r| r.end);
    TextObject {
        range: line_start(s, a)..end,
        linewise: true,
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;
    use spur_text::Buffer;

    fn snap(t: &str) -> Snapshot {
        Buffer::new(t).snapshot()
    }

    #[test]
    fn find() {
        let s = snap("a,b,c,d");
        assert_eq!(find_char(&s, 0, ',', 1, false, false), Some(1));
        assert_eq!(find_char(&s, 0, ',', 2, false, false), Some(3));
        assert_eq!(find_char(&s, 0, ',', 1, true, false), Some(0));
        assert_eq!(find_char(&s, 0, ',', 2, true, false), Some(2));
        assert_eq!(find_char(&s, 2, ',', 1, true, false), Some(2));
        assert_eq!(find_char(&s, 6, ',', 1, false, true), Some(5));
        assert_eq!(find_char(&s, 6, ',', 1, true, true), Some(6));
        assert_eq!(find_char(&s, 0, 'z', 1, false, false), None);
    }

    #[test]
    fn brackets() {
        let s = snap("f(a, (b), c)");
        assert_eq!(match_bracket(&s, 0), Some(11));
        assert_eq!(match_bracket(&s, 11), Some(1));
        let o = text_object(&s, 3, '(', true).unwrap();
        assert_eq!(s.slice(o.range).unwrap(), "a, (b), c");
        let o = text_object(&s, 6, 'b', false).unwrap();
        assert_eq!(s.slice(o.range).unwrap(), "(b)");
        assert!(text_object(&snap("abc"), 1, '(', true).is_none());
    }

    #[test]
    fn words_and_quotes() {
        let s = snap("say \"hi there\" now");
        let w = |o, inner| {
            s.slice(text_object(&s, o, 'w', inner).unwrap().range)
                .unwrap()
        };
        assert_eq!(w(1, true), "say");
        assert_eq!(w(1, false), "say ");
        assert_eq!(w(16, false), " now");
        let q = |o, inner| {
            s.slice(text_object(&s, o, '"', inner).unwrap().range)
                .unwrap()
        };
        assert_eq!(q(7, true), "hi there");
        assert_eq!(q(7, false), "\"hi there\"");
        assert_eq!(q(0, true), "hi there", "first pair after the cursor");
    }

    #[test]
    fn paragraphs() {
        let s = snap("a\nb\n\nc\n");
        let o = text_object(&s, 0, 'p', true).unwrap();
        assert_eq!(s.slice(o.range.clone()).unwrap(), "a\nb\n");
        assert!(o.linewise);
        let o = text_object(&s, 0, 'p', false).unwrap();
        assert_eq!(s.slice(o.range).unwrap(), "a\nb\n\n");
    }
}
