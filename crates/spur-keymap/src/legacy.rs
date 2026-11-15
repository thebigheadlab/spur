//! Which keys a terminal without a full key protocol can send.

use crate::key::{Key, KeyCode, Mods};

/// True if a legacy terminal (xterm-style bytes, read as crossterm reads them,
/// without the kitty keyboard protocol) can send `key` so that it arrives as
/// itself. Key hints skip keys that cannot arrive.
///
/// Rules, over normalized keys (letters carry Shift as their case):
/// - Super never arrives; a char with Shift arrives unshifted or as the shifted
///   char.
/// - Ctrl arrives on a–z (not i, m: Tab and Enter; not h, j on purpose: some
///   terminals send Backspace as ctrl+h, and ctrl+j is a line feed in pastes), on
///   space, `/` and 4 to 6. Ctrl on a capital (ctrl+shift) arrives without Shift.
/// - Enter, Tab, Backspace and Esc arrive without modifiers only (shift+Tab
///   excepted). Alt on Enter, Tab and Backspace is excluded on purpose:
///   terminals differ.
/// - alt+`[` and alt+`O` start escape sequences; alt+Esc reads as Esc.
/// - Named keys (arrows, Home, End, page up, page down, Insert, Delete, F1 to F12)
///   arrive with any of Shift, Ctrl and Alt, except F3, which arrives only
///   without them.
pub fn legacy_can_send(key: &Key) -> bool {
    let m = key.mods;
    if m.contains(Mods::SUPER) {
        return false;
    }
    match key.code {
        KeyCode::Char(c) => {
            if m.contains(Mods::SHIFT) {
                return false;
            }
            if m.contains(Mods::CTRL)
                && !(c.is_ascii_lowercase() && !matches!(c, 'i' | 'm' | 'h' | 'j')
                    || matches!(c, ' ' | '/' | '4' | '5' | '6'))
            {
                return false;
            }
            !(m.contains(Mods::ALT) && matches!(c, '[' | 'O'))
        }
        KeyCode::Tab => m.is_empty() || m == Mods::SHIFT,
        // A modified F3 (`ESC [ 1 ; m R`) reads as a cursor position report.
        KeyCode::Enter | KeyCode::Backspace | KeyCode::Esc | KeyCode::F(3) => m.is_empty(),
        KeyCode::F(n) => n <= 12,
        KeyCode::Delete
        | KeyCode::Insert
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::AnyChar => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_keys;

    #[test]
    fn examples_per_rule() {
        let can = |s: &str| legacy_can_send(&parse_keys(s).unwrap()[0]);
        for k in [
            "a",
            "E",
            "<C-e>",
            "<C-Space>",
            "<C-/>",
            "<C-4>",
            "<A-1>",
            "<C-A-b>",
            "<A-E>",
            "<S-Tab>",
            "<C-S-Up>",
            "<A-Home>",
            "<F12>",
            "<CR>",
        ] {
            assert!(can(k), "{k} arrives");
        }
        for k in [
            "<C-S-e>",
            "<C-i>",
            "<C-m>",
            "<C-h>",
            "<C-j>",
            "<C-1>",
            "<C-@>",
            "<C-]>",
            "<C-_>",
            "<C-,>",
            "<S-Space>",
            "<S-1>",
            "<C-CR>",
            "<A-CR>",
            "<A-Esc>",
            "<A-[>",
            "<A-S-o>",
            "<D-s>",
            "<F13>",
            "<C-Tab>",
        ] {
            assert!(!can(k), "{k} does not arrive");
        }
    }
}
