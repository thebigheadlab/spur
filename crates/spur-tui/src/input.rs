//! Terminal key events to Spur keys.

use crossterm::event::{KeyCode as Ct, KeyEvent, KeyEventKind, KeyModifiers};
use spur_keymap::{Key, KeyCode, Mods};

/// Maps a terminal key event. Releases and unknown keys map to `None`.
pub fn map_key(ev: &KeyEvent) -> Option<Key> {
    if ev.kind == KeyEventKind::Release {
        return None;
    }
    let mut mods = Mods::NONE;
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        mods = mods | Mods::CTRL;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        mods = mods | Mods::ALT;
    }
    if ev.modifiers.contains(KeyModifiers::SUPER) || ev.modifiers.contains(KeyModifiers::META) {
        mods = mods | Mods::SUPER;
    }
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    let code = match ev.code {
        // Chars already carry their case; Shift only matters with Ctrl/Alt.
        Ct::Char(c) => {
            if shift && !mods.is_empty() {
                mods = mods | Mods::SHIFT;
            }
            KeyCode::Char(c)
        }
        Ct::BackTab => {
            mods = mods | Mods::SHIFT;
            KeyCode::Tab
        }
        other => {
            if shift {
                mods = mods | Mods::SHIFT;
            }
            match other {
                Ct::Esc => KeyCode::Esc,
                Ct::Enter => KeyCode::Enter,
                Ct::Tab => KeyCode::Tab,
                Ct::Backspace => KeyCode::Backspace,
                Ct::Delete => KeyCode::Delete,
                Ct::Insert => KeyCode::Insert,
                Ct::Up => KeyCode::Up,
                Ct::Down => KeyCode::Down,
                Ct::Left => KeyCode::Left,
                Ct::Right => KeyCode::Right,
                Ct::Home => KeyCode::Home,
                Ct::End => KeyCode::End,
                Ct::PageUp => KeyCode::PageUp,
                Ct::PageDown => KeyCode::PageDown,
                Ct::F(n) => KeyCode::F(n),
                _ => return None,
            }
        }
    };
    Some(Key::new(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_keymap::parse_keys;

    fn ev(code: Ct, mods: KeyModifiers) -> Option<String> {
        map_key(&KeyEvent::new(code, mods)).map(|k| k.to_string())
    }

    #[test]
    fn mapping() {
        assert_eq!(ev(Ct::Char('a'), KeyModifiers::NONE).unwrap(), "a");
        assert_eq!(ev(Ct::Char('A'), KeyModifiers::SHIFT).unwrap(), "A");
        assert_eq!(ev(Ct::Char('w'), KeyModifiers::CONTROL).unwrap(), "ctrl+w");
        assert_eq!(
            ev(Ct::Char('W'), KeyModifiers::CONTROL | KeyModifiers::SHIFT).unwrap(),
            "ctrl+shift+w"
        );
        assert_eq!(ev(Ct::BackTab, KeyModifiers::SHIFT).unwrap(), "shift+tab");
        assert_eq!(ev(Ct::Left, KeyModifiers::SHIFT).unwrap(), "shift+left");
        assert_eq!(ev(Ct::Char(' '), KeyModifiers::NONE).unwrap(), "space");
        assert_eq!(
            map_key(&KeyEvent::new(Ct::Esc, KeyModifiers::NONE)),
            parse_keys("esc").unwrap().first().copied()
        );
        assert!(ev(Ct::CapsLock, KeyModifiers::NONE).is_none());
    }
}
