//! Keys and key notation.
//!
//! Spur's notation is plain words: `ctrl+s`, `alt+up`, `shift+left`, `enter`, `f5`.
//! A sequence separates keys with spaces (`ctrl+k ctrl+s`), `space` and `plus` name
//! those keys, and `{char}` is a pattern for any typed char. Keys are shown this way.
//!
//! Preset extensions may also use bracket notation (`gg`, `<C-w>v`, `f<char>`), which
//! is parsed when a binding has no spaces, no `+` and is not a key name.

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

/// Parses key notation (Spur's words, or bracket notation) into a sequence.
///
/// # Errors
///
/// Returns [`KeyParseError`] for an unknown key or modifier, or an empty string.
pub fn parse_keys(s: &str) -> Result<Vec<Key>, KeyParseError> {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    let mod_prefix = MODIFIERS
        .iter()
        .any(|m| lower.starts_with(m) && lower[m.len()..].starts_with('+'));
    let words = mod_prefix
        || t.contains(char::is_whitespace)
        || (t.len() > 1 && t.contains('+') && !t.contains('<'))
        || (t.chars().count() > 1 && (word_key(t).is_some() || t == "{char}"));
    if words {
        parse_words(s)
    } else {
        parse_brackets(s)
    }
}

/// Modifier words.
const MODIFIERS: &[&str] = &[
    "ctrl", "control", "alt", "opt", "option", "shift", "cmd", "super", "win", "meta",
];

fn word_key(name: &str) -> Option<KeyCode> {
    match name.to_ascii_lowercase().as_str() {
        "plus" => Some(KeyCode::Char('+')),
        "{char}" => Some(KeyCode::AnyChar),
        "cr" | "bs" | "del" | "lt" | "gt" | "bar" | "char" => None,
        _ => named(name),
    }
}

fn parse_words(s: &str) -> Result<Vec<Key>, KeyParseError> {
    let mut out = Vec::new();
    let base = s.as_ptr() as usize;
    for word in s.split_whitespace() {
        let offset = word.as_ptr() as usize - base;
        let err = |message: String| KeyParseError { offset, message };
        // `ctrl++` is ctrl and the `+` key; a lone trailing `+` is missing its key.
        let (body, plus) = match word.strip_suffix("++") {
            Some(b) if !b.is_empty() => (format!("{b}+plus"), true),
            _ => (word.to_owned(), false),
        };
        if !plus && word.len() > 1 && word.ends_with('+') {
            return Err(err(format!("`{word}` is missing a key after `+`")));
        }
        let parts: Vec<&str> = body.split('+').collect();
        let (last, mod_parts) = match parts.split_last() {
            Some((l, rest)) if !l.is_empty() => (*l, rest),
            _ => return Err(err("empty key".into())),
        };
        let mut mods = Mods::NONE;
        for m in mod_parts {
            mods = mods
                | match m.to_ascii_lowercase().as_str() {
                    "ctrl" | "control" => Mods::CTRL,
                    "alt" | "opt" | "option" => Mods::ALT,
                    "shift" => Mods::SHIFT,
                    "cmd" | "super" | "win" | "meta" => Mods::SUPER,
                    other => return Err(err(format!("unknown modifier `{other}`"))),
                };
        }
        let code = if last.chars().count() == 1 {
            KeyCode::Char(last.chars().next().unwrap_or_default())
        } else {
            word_key(last).ok_or_else(|| err(format!("unknown key `{last}`")))?
        };
        out.push(Key::new(code, mods));
    }
    if out.is_empty() {
        return Err(KeyParseError {
            offset: 0,
            message: "empty key sequence".into(),
        });
    }
    Ok(out)
}

/// Parses bracket notation (`gg`, `<C-w>v`), used by preset extensions.
///
/// # Errors
///
/// Returns [`KeyParseError`] for an unknown `<name>` or an empty string.
pub fn parse_brackets(s: &str) -> Result<Vec<Key>, KeyParseError> {
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
            KeyCode::Char(' ') => "space".to_owned(),
            KeyCode::Char('+') => "plus".to_owned(),
            KeyCode::Char(c) => {
                if !mods.is_empty() && c.is_ascii_uppercase() {
                    mods = mods | Mods::SHIFT;
                    c.to_ascii_lowercase().to_string()
                } else {
                    c.to_string()
                }
            }
            KeyCode::Esc => "esc".into(),
            KeyCode::Enter => "enter".into(),
            KeyCode::Tab => "tab".into(),
            KeyCode::Backspace => "backspace".into(),
            KeyCode::Delete => "delete".into(),
            KeyCode::Insert => "insert".into(),
            KeyCode::Up => "up".into(),
            KeyCode::Down => "down".into(),
            KeyCode::Left => "left".into(),
            KeyCode::Right => "right".into(),
            KeyCode::Home => "home".into(),
            KeyCode::End => "end".into(),
            KeyCode::PageUp => "pageup".into(),
            KeyCode::PageDown => "pagedown".into(),
            KeyCode::F(n) => format!("f{n}"),
            KeyCode::AnyChar => "{char}".into(),
        };
        for (m, p) in [
            (Mods::CTRL, "ctrl+"),
            (Mods::ALT, "alt+"),
            (Mods::SHIFT, "shift+"),
            (Mods::SUPER, "cmd+"),
        ] {
            if mods.contains(m) {
                f.write_str(p)?;
            }
        }
        f.write_str(&name)
    }
}

/// Formats a key sequence in notation.
pub fn format_keys(keys: &[Key]) -> String {
    keys.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format() {
        for (src, canon) in [
            ("ctrl+s", "ctrl+s"),
            ("Ctrl+Shift+W", "ctrl+shift+w"),
            ("alt+up", "alt+up"),
            ("ctrl+k ctrl+s", "ctrl+k ctrl+s"),
            ("space", "space"),
            ("ctrl+space", "ctrl+space"),
            ("enter", "enter"),
            ("shift+tab", "shift+tab"),
            ("f12", "f12"),
            ("g g", "g g"),
            ("ctrl++", "ctrl+plus"),
            ("a {char}", "a {char}"),
            ("A", "A"),
            ("shift+space", "shift+space"),
            ("ctrl+shift+space", "ctrl+shift+space"),
        ] {
            let keys = parse_keys(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert_eq!(format_keys(&keys), canon, "{src}");
            assert_eq!(parse_keys(canon).unwrap(), keys, "{canon} round trip");
        }
        assert!(parse_keys("").is_err());
        assert!(parse_keys("hyper+a").is_err());
        assert!(parse_keys("ctrl+nope").is_err());
        assert!(parse_keys("g+").is_err());
        assert!(parse_keys("ctrl+").is_err());
        let lt = Key::new(KeyCode::Char('<'), Mods::CTRL);
        assert_eq!(parse_keys(&lt.to_string()).unwrap(), vec![lt]);
    }

    #[test]
    fn brackets() {
        for (src, canon) in [
            ("gg", "g g"),
            ("<C-w>v", "ctrl+w v"),
            ("<c-W>", "ctrl+w"),
            ("<C-S-w>", "ctrl+shift+w"),
            ("<space>f", "space f"),
            ("<esc>", "esc"),
            ("<Enter>", "enter"),
            ("<S-Tab>", "shift+tab"),
            ("<A-x>", "alt+x"),
            ("<lt>", "<"),
            ("f<char>", "f {char}"),
            ("<F12>", "f12"),
            ("<S-a>", "A"),
            ("<", "<"),
            ("<C-gt>", "ctrl+>"),
            ("<A-bar>", "alt+|"),
            ("<S-Space>", "shift+space"),
            ("<C-S-Space>", "ctrl+shift+space"),
        ] {
            let keys = parse_keys(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert_eq!(format_keys(&keys), canon, "{src}");
            assert_eq!(parse_keys(canon).unwrap(), keys, "{canon} round trip");
        }
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
