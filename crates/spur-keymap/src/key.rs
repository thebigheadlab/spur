//! Keys and Vim-style key notation: `gg`, `<C-w>v`, `<Space>f`, `<Esc>`, `f<char>`.

use std::fmt;

/// Modifier set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mods(u8);

impl Mods {
    /// No modifiers.
    pub const NONE: Self = Self(0);
    /// Control.
    pub const CTRL: Self = Self(1);
    /// Alt / Meta.
    pub const ALT: Self = Self(2);
    /// Shift (only kept for non-char keys; chars carry their case).
    pub const SHIFT: Self = Self(4);
    /// Super / Cmd.
    pub const SUPER: Self = Self(8);

    /// True if all of `other` are set.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True if no modifier is set.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Without `other`.
    #[must_use]
    pub fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl std::ops::BitOr for Mods {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// A key, without modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[allow(missing_docs)]
pub enum KeyCode {
    Char(char),
    Esc,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    /// Pattern only: matches any printable char without Ctrl/Alt and passes it to the
    /// command as the `char` arg (`f<char>`, `r<char>`).
    AnyChar,
}

/// A key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    /// The key.
    pub code: KeyCode,
    /// Modifiers.
    pub mods: Mods,
}

impl Key {
    /// A key with modifiers, normalized: letters carry their case, so Shift is dropped from
    /// them; Ctrl/Alt letters are lowercased unless Shift was given. Chars without case
    /// (space, digits, punctuation) keep Shift as a modifier.
    pub fn new(code: KeyCode, mods: Mods) -> Self {
        match code {
            KeyCode::Char(c) => {
                let shifted = mods.contains(Mods::SHIFT);
                // Only letters carry Shift as case; `<S-Space>` keeps it as a modifier.
                if shifted && !c.is_lowercase() && !c.is_uppercase() {
                    return Self {
                        code: KeyCode::Char(c),
                        mods,
                    };
                }
                let c = if !mods.without(Mods::SHIFT).is_empty() && !shifted {
                    c.to_ascii_lowercase()
                } else if shifted {
                    c.to_ascii_uppercase()
                } else {
                    c
                };
                Self {
                    code: KeyCode::Char(c),
                    mods: mods.without(Mods::SHIFT),
                }
            }
            _ => Self { code, mods },
        }
    }

    /// A plain char.
    pub fn char(c: char) -> Self {
        Self::new(KeyCode::Char(c), Mods::NONE)
    }

    /// A named key without modifiers.
    pub fn named(code: KeyCode) -> Self {
        Self::new(code, Mods::NONE)
    }

    /// The char this key types, if it types one.
    pub fn text(self) -> Option<char> {
        match self.code {
            KeyCode::Char(c)
                if !self.mods.contains(Mods::CTRL)
                    && !self.mods.contains(Mods::ALT)
                    && !self.mods.contains(Mods::SUPER)
                    && !c.is_control() =>
            {
                Some(c)
            }
            _ => None,
        }
    }
}

/// Key notation that failed to parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyParseError {
    /// Byte offset.
    pub offset: usize,
    /// What went wrong.
    pub message: String,
}

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for KeyParseError {}

fn named(name: &str) -> Option<KeyCode> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "esc" | "escape" => KeyCode::Esc,
        "cr" | "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "bs" | "backspace" => KeyCode::Backspace,
        "del" | "delete" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "space" => KeyCode::Char(' '),
        "lt" => KeyCode::Char('<'),
        "gt" => KeyCode::Char('>'),
        "bar" => KeyCode::Char('|'),
        "char" => KeyCode::AnyChar,
        _ => {
            let n: u8 = lower.strip_prefix('f')?.parse().ok()?;
            if !(1..=24).contains(&n) {
                return None;
            }
            KeyCode::F(n)
        }
    })
}

/// Parses key notation into a sequence.
///
/// # Errors
///
/// Returns [`KeyParseError`] for an unknown `<name>`, an unclosed `<`, or an empty string.
pub fn parse_keys(s: &str) -> Result<Vec<Key>, KeyParseError> {
    let err = |offset, message: &str| KeyParseError {
        offset,
        message: message.into(),
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let c = rest.chars().next().unwrap_or_default();
        if c == '<'
            && let Some(close) = rest.find('>')
            && close > 1
        {
            let inner = &rest[1..close];
            let mut mods = Mods::NONE;
            let mut name = inner;
            while name.len() > 2 && name.as_bytes()[1] == b'-' {
                mods = mods
                    | match name.as_bytes()[0].to_ascii_uppercase() {
                        b'C' => Mods::CTRL,
                        b'A' | b'M' => Mods::ALT,
                        b'S' => Mods::SHIFT,
                        b'D' => Mods::SUPER,
                        _ => return Err(err(i, "unknown modifier")),
                    };
                name = &name[2..];
            }
            let code = if name.chars().count() == 1 {
                KeyCode::Char(name.chars().next().unwrap_or_default())
            } else {
                named(name).ok_or_else(|| err(i, &format!("unknown key <{inner}>")))?
            };
            if code == KeyCode::AnyChar && !mods.is_empty() {
                return Err(err(i, "<char> takes no modifiers"));
            }
            out.push(Key::new(code, mods));
            i += close + 1;
            continue;
        }
        out.push(Key::char(c));
        i += c.len_utf8();
    }
    if out.is_empty() {
        return Err(err(0, "empty key sequence"));
    }
    Ok(out)
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut mods = self.mods;
        let name = match self.code {
            KeyCode::Char(' ') => Some("Space".to_owned()),
            KeyCode::Char('<') => Some("lt".to_owned()),
            // With a modifier these would close or split the `<...>` notation.
            KeyCode::Char('>') if !self.mods.is_empty() => Some("gt".to_owned()),
            KeyCode::Char('|') if !self.mods.is_empty() => Some("bar".to_owned()),
            KeyCode::Char(c) => {
                if self.mods.is_empty() {
                    return write!(f, "{c}");
                }
                if c.is_ascii_uppercase() {
                    mods = mods | Mods::SHIFT;
                }
                Some(c.to_ascii_lowercase().to_string())
            }
            KeyCode::Esc => Some("Esc".into()),
            KeyCode::Enter => Some("CR".into()),
            KeyCode::Tab => Some("Tab".into()),
            KeyCode::Backspace => Some("BS".into()),
            KeyCode::Delete => Some("Del".into()),
            KeyCode::Insert => Some("Insert".into()),
            KeyCode::Up => Some("Up".into()),
            KeyCode::Down => Some("Down".into()),
            KeyCode::Left => Some("Left".into()),
            KeyCode::Right => Some("Right".into()),
            KeyCode::Home => Some("Home".into()),
            KeyCode::End => Some("End".into()),
            KeyCode::PageUp => Some("PageUp".into()),
            KeyCode::PageDown => Some("PageDown".into()),
            KeyCode::F(n) => Some(format!("F{n}")),
            KeyCode::AnyChar => Some("char".into()),
        };
        let name = name.unwrap_or_default();
        f.write_str("<")?;
        for (m, p) in [
            (Mods::CTRL, "C-"),
            (Mods::ALT, "A-"),
            (Mods::SHIFT, "S-"),
            (Mods::SUPER, "D-"),
        ] {
            if mods.contains(m) {
                f.write_str(p)?;
            }
        }
        write!(f, "{name}>")
    }
}

/// Formats a key sequence in notation.
pub fn format_keys(keys: &[Key]) -> String {
    keys.iter().map(ToString::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format() {
        for (src, canon) in [
            ("gg", "gg"),
            ("<C-w>v", "<C-w>v"),
            ("<c-W>", "<C-w>"),
            ("<C-S-w>", "<C-S-w>"),
            ("<space>f", "<Space>f"),
            ("<esc>", "<Esc>"),
            ("<Enter>", "<CR>"),
            ("<S-Tab>", "<S-Tab>"),
            ("<A-x>", "<A-x>"),
            ("<lt>", "<lt>"),
            ("f<char>", "f<char>"),
            ("<F12>", "<F12>"),
            ("<S-a>", "A"),
            ("<", "<lt>"),
            ("<>", "<lt>>"),
            ("<C-gt>", "<C-gt>"),
            ("<A-bar>", "<A-bar>"),
            ("<S-Space>", "<S-Space>"),
            ("<C-S-Space>", "<C-S-Space>"),
        ] {
            let keys = parse_keys(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert_eq!(format_keys(&keys), canon, "{src}");
            assert_eq!(parse_keys(canon).unwrap(), keys, "{canon} round trip");
        }
        assert!(parse_keys("").is_err());
        assert!(parse_keys("<nope>").is_err());
        assert!(parse_keys("<X-a>").is_err());
        assert!(parse_keys("<F99>").is_err());
        assert!(parse_keys("<C-char>").is_err());
        assert_ne!(
            parse_keys("<S-Space>").unwrap(),
            parse_keys("<Space>").unwrap()
        );
    }

    #[test]
    fn text() {
        assert_eq!(Key::char('a').text(), Some('a'));
        assert_eq!(Key::new(KeyCode::Char('a'), Mods::CTRL).text(), None);
        assert_eq!(Key::named(KeyCode::Enter).text(), None);
        assert_eq!(Key::new(KeyCode::Char('a'), Mods::SHIFT), Key::char('A'));
    }
}
