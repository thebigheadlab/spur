//! Terminal key events as `spur-keymap` notation.
//!
//! Letters with ctrl or alt are lower-case with an explicit `S-` for shift
//! (ctrl+shift+e is written C-S-e in angle brackets); without ctrl or alt, shift
//! folds into the char. The client produces notation; it binds nothing.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// The notation of a key event, or `None` for releases and keys with no notation.
/// `full_keys`: the terminal reports keys in full (the kitty keyboard protocol,
/// or Windows).
pub fn notation(ev: &KeyEvent, full_keys: bool) -> Option<String> {
    if ev.kind == KeyEventKind::Release {
        return None;
    }
    key(ev.code, ev.modifiers, full_keys)
}

/// The notation of a key and its modifiers. Without full keys, ctrl+7 is what
/// ctrl+/ sends (0x1F), so it reads as ctrl+/.
pub fn key(code: KeyCode, mods: KeyModifiers, full_keys: bool) -> Option<String> {
    let code = match code {
        KeyCode::Char('7')
            if !full_keys && !cfg!(windows) && mods.contains(KeyModifiers::CONTROL) =>
        {
            KeyCode::Char('/')
        }
        c => c,
    };
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
        key(code, mods, true).unwrap()
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
        assert_eq!(key(KeyCode::F(25), N, true), None);
        assert_eq!(key(KeyCode::Char('s'), KeyModifiers::HYPER, true), None);
        assert_eq!(key(KeyCode::Char('s'), KeyModifiers::META, true), None);
        assert_eq!(key(KeyCode::CapsLock, N, true), None);
        // ctrl+/ arrives as ctrl+7 without full keys.
        assert_eq!(k(KeyCode::Char('7'), C), "<C-7>");
        if !cfg!(windows) {
            assert_eq!(key(KeyCode::Char('7'), C, false).as_deref(), Some("<C-/>"));
        }
    }

    /// The bytes a legacy xterm sends for `key` (US layout).
    fn xterm(key: spur_keymap::Key) -> Vec<u8> {
        use spur_keymap::{KeyCode as K, Mods};
        let m = key.mods;
        let (ctrl, alt, shift) = (
            m.contains(Mods::CTRL),
            m.contains(Mods::ALT),
            m.contains(Mods::SHIFT),
        );
        let param = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
        let csi = |n: &str, end: char| {
            let mut v = b"\x1b[".to_vec();
            if param > 1 {
                v.extend(format!("{n};{param}{end}").bytes());
            } else if end == '~' {
                v.extend(format!("{n}~").bytes());
            } else {
                v.push(end as u8);
            }
            v
        };
        let mut out = Vec::new();
        let named = |c: char| csi("1", c);
        let body = match key.code {
            K::Char(c) => {
                let b = if ctrl {
                    match c {
                        ' ' | '@' | '2' => vec![0],
                        'a'..='z' | 'A'..='Z' => vec![c.to_ascii_lowercase() as u8 & 0x1f],
                        '[' | '3' => vec![0x1b],
                        '\\' | '4' => vec![0x1c],
                        ']' | '5' => vec![0x1d],
                        '^' | '6' => vec![0x1e],
                        '_' | '7' | '/' => vec![0x1f],
                        '8' | '?' => vec![0x7f],
                        c => c.to_string().into_bytes(),
                    }
                } else {
                    c.to_string().into_bytes()
                };
                if alt {
                    out.push(0x1b);
                }
                b
            }
            K::Enter | K::Tab | K::Backspace | K::Esc => {
                if alt {
                    out.push(0x1b);
                }
                match key.code {
                    K::Enter => vec![b'\r'],
                    K::Tab if shift => b"\x1b[Z".to_vec(),
                    K::Tab => vec![b'\t'],
                    K::Backspace if ctrl => vec![0x08],
                    K::Backspace => vec![0x7f],
                    _ => vec![0x1b],
                }
            }
            K::Up => named('A'),
            K::Down => named('B'),
            K::Right => named('C'),
            K::Left => named('D'),
            K::Home => named('H'),
            K::End => named('F'),
            K::Insert => csi("2", '~'),
            K::Delete => csi("3", '~'),
            K::PageUp => csi("5", '~'),
            K::PageDown => csi("6", '~'),
            K::F(n @ 1..=4) if param == 1 => vec![0x1b, b'O', b'P' + n - 1],
            K::F(n @ 1..=4) => named((b'P' + n - 1) as char),
            K::F(n) => {
                let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n) - 5];
                csi(&code.to_string(), '~')
            }
            K::AnyChar => unreachable!(),
        };
        out.extend(body);
        out
    }

    /// The key event crossterm 0.29 reports for `bytes` in raw mode: a copy of
    /// its private `parse_event` (src/event/sys/unix/parse.rs) for the inputs
    /// above. Check it again when crossterm is bumped.
    fn crossterm(bytes: &[u8]) -> Option<(KeyCode, KeyModifiers)> {
        let (&b, rest) = bytes.split_first()?;
        match b {
            0x1b if rest.is_empty() => Some((KeyCode::Esc, N)),
            0x1b => match rest[0] {
                b'O' => {
                    let f = *rest.get(1)?;
                    (b'P'..=b'S')
                        .contains(&f)
                        .then(|| (KeyCode::F(1 + f - b'P'), N))
                }
                b'[' => csi(&rest[1..]),
                0x1b => Some((KeyCode::Esc, N)),
                _ => crossterm(rest).map(|(c, m)| (c, m | A)),
            },
            b'\r' => Some((KeyCode::Enter, N)),
            b'\t' => Some((KeyCode::Tab, N)),
            0x7f => Some((KeyCode::Backspace, N)),
            0x01..=0x1a => Some((KeyCode::Char((b - 1 + b'a') as char), C)),
            0x1c..=0x1f => Some((KeyCode::Char((b - 0x1c + b'4') as char), C)),
            0 => Some((KeyCode::Char(' '), C)),
            _ => {
                let c = std::str::from_utf8(bytes).ok()?.chars().next()?;
                Some((KeyCode::Char(c), if c.is_uppercase() { S } else { N }))
            }
        }
    }

    /// CSI sequences (after `ESC [`): `Z`, `A`..`H` with an optional `1;m`, and
    /// `n~` / `n;m~`.
    fn csi(s: &[u8]) -> Option<(KeyCode, KeyModifiers)> {
        let s = std::str::from_utf8(s).ok()?;
        if s == "Z" {
            return Some((KeyCode::BackTab, S));
        }
        let end = s.chars().last()?;
        let body = &s[..s.len() - 1];
        let mut parts = body.split(';');
        let n = parts.next().unwrap_or("");
        let m: u8 = parts.next().map_or(Some(1), |m| m.parse().ok())?;
        let bits = m - 1;
        let mut mods = N;
        if bits & 1 != 0 {
            mods |= S;
        }
        if bits & 2 != 0 {
            mods |= A;
        }
        if bits & 4 != 0 {
            mods |= C;
        }
        let code = match (end, n) {
            ('A', _) => KeyCode::Up,
            ('B', _) => KeyCode::Down,
            ('C', _) => KeyCode::Right,
            ('D', _) => KeyCode::Left,
            ('H', _) => KeyCode::Home,
            ('F', _) => KeyCode::End,
            ('P'..='S', _) => KeyCode::F(end as u8 - b'P' + 1),
            ('~', "2") => KeyCode::Insert,
            ('~', "3") => KeyCode::Delete,
            ('~', "5") => KeyCode::PageUp,
            ('~', "6") => KeyCode::PageDown,
            ('~', n) => {
                let n: u8 = n.parse().ok()?;
                let i = [15, 17, 18, 19, 20, 21, 23, 24]
                    .iter()
                    .position(|&x| x == n)?;
                KeyCode::F(5 + u8::try_from(i).ok()?)
            }
            _ => return None,
        };
        Some((code, mods))
    }

    /// For every key with every mix of ctrl, alt and shift: the hints' rule
    /// (`legacy_can_send`) says a key arrives exactly when the bytes a legacy
    /// terminal sends for it come back as that key, except keys left out on
    /// purpose.
    #[test]
    fn legacy_hints_name_keys_that_arrive() {
        use spur_keymap::{Key, KeyCode as K, Mods, legacy_can_send, parse_keys};
        let mut all = std::collections::BTreeSet::new();
        let codes = (0x20u8..0x7f)
            .map(|b| K::Char(b as char))
            .chain([
                K::Enter,
                K::Tab,
                K::Backspace,
                K::Esc,
                K::Up,
                K::Down,
                K::Left,
                K::Right,
                K::Home,
                K::End,
                K::Insert,
                K::Delete,
                K::PageUp,
                K::PageDown,
            ])
            .chain((1..=12).map(K::F));
        for code in codes {
            for i in 0..8u8 {
                let mut m = Mods::NONE;
                for (bit, md) in [(1, Mods::CTRL), (2, Mods::ALT), (4, Mods::SHIFT)] {
                    if i & bit != 0 {
                        m = m | md;
                    }
                }
                all.insert(Key::new(code, m));
            }
        }
        // Arrive as themselves, but hints leave them out (see legacy_can_send):
        // ctrl+h and ctrl+j (with or without alt), alt on Enter, Tab, Backspace.
        let on_purpose = |k: &Key| match k.code {
            K::Char('h' | 'j') => k.mods.contains(Mods::CTRL),
            K::Enter | K::Tab | K::Backspace => k.mods == Mods::ALT,
            _ => false,
        };
        for k in all {
            let got = crossterm(&xterm(k))
                .and_then(|(c, m)| key(c, m, false))
                .and_then(|n| parse_keys(&n).ok())
                .filter(|v| v.len() == 1)
                .map(|v| v[0]);
            let arrives = got == Some(k);
            if on_purpose(&k) {
                assert!(arrives && !legacy_can_send(&k), "{k}");
                continue;
            }
            assert_eq!(
                legacy_can_send(&k),
                arrives,
                "{k}: arrives as {:?}",
                got.map(|g| g.to_string())
            );
        }
    }

    #[test]
    fn releases_are_ignored() {
        let mut ev = KeyEvent::new(KeyCode::Char('a'), N);
        ev.kind = KeyEventKind::Release;
        assert_eq!(notation(&ev, true), None);
        ev.kind = KeyEventKind::Repeat;
        assert_eq!(notation(&ev, true).as_deref(), Some("a"));
    }
}
