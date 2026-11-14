//! Text from the server made safe for the terminal and split into cells.
//!
//! Controls (C0, DEL, C1) and bidi controls become the replacement char in a cell
//! of their own, so file names and file contents cannot send escape sequences or
//! reorder the line. Other zero-width chars (combining accents, joiners) join the
//! previous cell; at the start of a text they are dropped. Wide chars take two
//! cells.

use unicode_width::UnicodeWidthChar;

/// True for chars that must never reach the terminal as they are.
pub fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
        )
}

/// One cell's worth of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    /// The text drawn in the cell: one char plus any zero-width chars after it.
    pub text: String,
    /// Cells taken: 1 or 2.
    pub width: u32,
}

/// Splits `s` into cells.
pub fn pieces(s: &str) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::with_capacity(s.len());
    for c in s.chars() {
        if is_unsafe(c) {
            out.push(Piece {
                text: char::REPLACEMENT_CHARACTER.to_string(),
                width: 1,
            });
            continue;
        }
        match c.width() {
            Some(0) | None => {
                if let Some(last) = out.last_mut() {
                    last.text.push(c);
                }
            }
            Some(w) => out.push(Piece {
                text: c.to_string(),
                width: if w >= 2 { 2 } else { 1 },
            }),
        }
    }
    out
}

/// Cells `s` takes once cleaned.
pub fn width(s: &str) -> u32 {
    pieces(s).iter().map(|p| p.width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<String> {
        pieces(s).into_iter().map(|p| p.text).collect()
    }

    #[test]
    fn controls_and_escapes_become_replacement_chars() {
        let r = char::REPLACEMENT_CHARACTER.to_string();
        assert_eq!(texts("a\x1bb"), ["a", &r, "b"]);
        assert_eq!(texts("\x07\x7f\u{9b}"), [&r, &r, &r]);
        // An OSC 52 clipboard write cannot get through.
        let osc = "\x1b]52;c;aGk=\x07";
        assert!(texts(osc).iter().all(|t| !t.contains('\x1b') && !t.contains('\x07')));
        // Bidi controls.
        assert_eq!(texts("a\u{202E}b\u{2066}"), ["a", &r, "b", &r]);
    }

    #[test]
    fn zero_width_chars_join_the_previous_cell() {
        // An NFD name, as macOS writes it: e + combining acute.
        assert_eq!(texts("cafe\u{301}"), ["c", "a", "f", "e\u{301}"]);
        assert_eq!(width("cafe\u{301}"), 4);
        // At the start there is nothing to join.
        assert_eq!(texts("\u{301}x"), ["x"]);
    }

    #[test]
    fn wide_chars_take_two_cells() {
        let p = pieces("a\u{6771}");
        assert_eq!(p[1].width, 2);
        assert_eq!(width("a\u{6771}"), 3);
    }
}
