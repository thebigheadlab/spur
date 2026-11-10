//! Terminal events to protocol messages.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Key notation for a key press, e.g. `a`, `<C-s>`, `<S-Left>`. `None` for key releases
/// and keys the protocol has no name for.
pub fn notation(key: &KeyEvent) -> Option<String> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let name = match key.code {
        KeyCode::Char(c) => {
            // Letters and symbols already carry Shift in the char itself.
            if c != ' ' {
                shift = false;
            }
            match c {
                ' ' => "Space".to_owned(),
                '<' => "lt".to_owned(),
                '>' => "gt".to_owned(),
                c if c.is_control() => return None,
                c => {
                    if !ctrl && !alt {
                        return Some(c.to_string());
                    }
                    c.to_string()
                }
            }
        }
        KeyCode::Enter => "CR".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => {
            shift = true;
            "Tab".into()
        }
        KeyCode::Backspace => "BS".into(),
        KeyCode::Delete => "Del".into(),
        KeyCode::Insert => "Insert".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::F(n) if (1..=24).contains(&n) => format!("F{n}"),
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
    out.push_str(&name);
    out.push('>');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> Option<String> {
        notation(&KeyEvent::new(code, mods))
    }

    #[test]
    fn keys_become_notation() {
        let none = KeyModifiers::NONE;
        assert_eq!(key(KeyCode::Char('a'), none).as_deref(), Some("a"));
        assert_eq!(
            key(KeyCode::Char('A'), KeyModifiers::SHIFT).as_deref(),
            Some("A")
        );
        assert_eq!(key(KeyCode::Char('<'), none).as_deref(), Some("<lt>"));
        assert_eq!(key(KeyCode::Char(' '), none).as_deref(), Some("<Space>"));
        assert_eq!(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL).as_deref(),
            Some("<C-Space>")
        );
        assert_eq!(
            key(KeyCode::Char('s'), KeyModifiers::CONTROL).as_deref(),
            Some("<C-s>")
        );
        assert_eq!(
            key(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT).as_deref(),
            Some("<C-S-Left>")
        );
        assert_eq!(
            key(KeyCode::BackTab, KeyModifiers::SHIFT).as_deref(),
            Some("<S-Tab>")
        );
        assert_eq!(key(KeyCode::F(2), none).as_deref(), Some("<F2>"));
        assert_eq!(key(KeyCode::Char('é'), none).as_deref(), Some("é"));
        assert_eq!(key(KeyCode::Null, none), None);
        let release = KeyEvent::new_with_kind(KeyCode::Char('a'), none, KeyEventKind::Release);
        assert_eq!(notation(&release), None);
    }
}
