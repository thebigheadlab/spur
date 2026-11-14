//! Terminal key events as `spur-keymap` notation.
//!
//! Letters with ctrl or alt are lower-case with an explicit `S-` for shift
//! (ctrl+shift+e is written C-S-e in angle brackets); without ctrl or alt, shift
//! folds into the char. The client produces notation; it binds nothing.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// The notation of a key event, or `None` for releases and keys with no notation.
pub fn notation(ev: &KeyEvent) -> Option<String> {
    if ev.kind == KeyEventKind::Release {
        return None;
    }
    key(ev.code, ev.modifiers)
}

/// The notation of a key and its modifiers.
pub fn key(code: KeyCode, mods: KeyModifiers) -> Option<String> {
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let shift = mods.contains(KeyModifiers::SHIFT);
    let sup = mods.contains(KeyModifiers::SUPER);
    // Keys with hyper or meta (kitty protocol only) have no notation.
    if mods.intersects(KeyModifiers::HYPER | KeyModifiers::META) {
        return None;
    }
    let (name, shift) = match code {
        KeyCode::Char(c) => {
            // Windows reports AltGr as ctrl+alt: there only letters and digits are
            // chords.
            if cfg!(windows) && ctrl && alt && !sup && !c.is_ascii_alphanumeric() {
                return Some(plain(c));
            }
            if !ctrl && !alt && !sup {
                return Some(if c == ' ' && shift {
                    "<S-Space>".into()
                } else {
                    plain(c)
                });
            }
            if c.is_alphabetic() {
                // A char whose lower case is several chars (U+0130) stays as is,
                // as the keymap reads it.
                let mut lower = c.to_lowercase();
                match (lower.next(), lower.next()) {
                    (Some(l), None) => (l.to_string(), shift || c.is_uppercase()),
                    _ => (c.to_string(), shift),
                }
            } else {
                let name = match c {
                    ' ' => "Space".to_owned(),
                    '<' => "lt".to_owned(),
                    '>' => "gt".to_owned(),
                    '|' => "bar".to_owned(),
                    c => c.to_string(),
                };
                (name, shift && c == ' ')
            }
        }
        KeyCode::BackTab => ("Tab".into(), true),
        KeyCode::Enter => ("CR".into(), shift),
        KeyCode::Backspace => ("BS".into(), shift),
        KeyCode::Delete => ("Del".into(), shift),
        KeyCode::Tab => ("Tab".into(), shift),
        KeyCode::Esc => ("Esc".into(), shift),
        KeyCode::Home => ("Home".into(), shift),
        KeyCode::End => ("End".into(), shift),
        KeyCode::PageUp => ("PageUp".into(), shift),
        KeyCode::PageDown => ("PageDown".into(), shift),
        KeyCode::Insert => ("Insert".into(), shift),
        KeyCode::Up => ("Up".into(), shift),
        KeyCode::Down => ("Down".into(), shift),
        KeyCode::Left => ("Left".into(), shift),
        KeyCode::Right => ("Right".into(), shift),
        KeyCode::F(n) if (1..=24).contains(&n) => (format!("F{n}"), shift),
        _ => return None,
    };
    let mut out = String::from("<");
    if ctrl {
        out.push_str("C-");
    }
    if alt {
        out.push_str("A-");
    }
    if shift {
        out.push_str("S-");
    }
    if sup {
        out.push_str("D-");
    }
    out.push_str(&name);
    out.push('>');
    Some(out)
}

/// A typed char.
fn plain(c: char) -> String {
    match c {
        '<' => "<lt>".into(),
        ' ' => "<Space>".into(),
        c => c.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, mods: KeyModifiers) -> String {
        key(code, mods).unwrap()
    }

    const C: KeyModifiers = KeyModifiers::CONTROL;
    const A: KeyModifiers = KeyModifiers::ALT;
    const S: KeyModifiers = KeyModifiers::SHIFT;
    const N: KeyModifiers = KeyModifiers::NONE;

    #[test]
    fn notation_matches_the_keymap() {
        let cases = [
            (k(KeyCode::Char('a'), N), "a"),
            (k(KeyCode::Char('A'), S), "A"),
            (k(KeyCode::Char('<'), N), "<lt>"),
            (k(KeyCode::Char(' '), N), "<Space>"),
            (k(KeyCode::Char(' '), S), "<S-Space>"),
            (k(KeyCode::Char('!'), S), "!"),
            (k(KeyCode::Char('e'), C | S), "<C-S-e>"),
            (k(KeyCode::Char('E'), C | S), "<C-S-e>"),
            (k(KeyCode::Char('b'), C | A), "<C-A-b>"),
            (k(KeyCode::Char('1'), A), "<A-1>"),
            // AltGr: Windows only; elsewhere ctrl+alt is a chord.
            (
                k(KeyCode::Char('@'), C | A),
                if cfg!(windows) { "@" } else { "<C-A-@>" },
            ),
            (
                k(KeyCode::Char('\u{e9}'), C | A),
                if cfg!(windows) {
                    "\u{e9}"
                } else {
                    "<C-A-\u{e9}>"
                },
            ),
            (k(KeyCode::Char('s'), KeyModifiers::SUPER), "<D-s>"),
            (k(KeyCode::Char('\u{130}'), C), "<C-\u{130}>"),
            (k(KeyCode::Char('<'), C), "<C-lt>"),
            (k(KeyCode::Char('|'), A), "<A-bar>"),
            (k(KeyCode::BackTab, S), "<S-Tab>"),
            (k(KeyCode::Enter, N), "<CR>"),
            (k(KeyCode::Backspace, N), "<BS>"),
            (k(KeyCode::Delete, N), "<Del>"),
            (k(KeyCode::Insert, N), "<Insert>"),
            (k(KeyCode::F(24), N), "<F24>"),
            (k(KeyCode::Home, C), "<C-Home>"),
            (k(KeyCode::PageDown, N), "<PageDown>"),
        ];
        for (got, want) in cases {
            assert_eq!(got, want);
            // The keymap reads it back as one key that prints the same.
            let keys = spur_keymap::parse_keys(&got).unwrap();
            assert_eq!(keys.len(), 1, "{got}");
            assert_eq!(spur_keymap::format_keys(&keys), got);
        }
        assert_eq!(key(KeyCode::F(25), N), None);
        assert_eq!(key(KeyCode::Char('s'), KeyModifiers::HYPER), None);
        assert_eq!(key(KeyCode::Char('s'), KeyModifiers::META), None);
        assert_eq!(key(KeyCode::CapsLock, N), None);
    }

    #[test]
    fn releases_are_ignored() {
        let mut ev = KeyEvent::new(KeyCode::Char('a'), N);
        ev.kind = KeyEventKind::Release;
        assert_eq!(notation(&ev), None);
        ev.kind = KeyEventKind::Repeat;
        assert_eq!(notation(&ev).as_deref(), Some("a"));
    }
}
