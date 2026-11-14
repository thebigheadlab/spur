//! Pack actions (named steps with a title, keys and a condition) and how key hints
//! print.

use std::collections::BTreeMap;
use std::sync::Arc;

use spur_core::{ContextLookup, When};
use spur_keymap::{Action as KeyAction, Binding, Key, KeyCode, Keymap, Mods, Step};

/// A pack action: `[actions.<id>]`.
#[derive(Clone, Debug)]
pub struct Action {
    /// Shown in the palette; untitled actions are key-only.
    pub title: Option<String>,
    /// What runs.
    pub steps: Arc<[Step]>,
    /// When it applies (keys and the palette).
    pub when: Option<When>,
    /// Key modes its keys are bound in.
    pub modes: Vec<String>,
    /// Its keys, in order; the first that applies is its hint.
    pub keys: Vec<ActionKey>,
}

/// One key of an action.
#[derive(Clone, Debug)]
pub struct ActionKey {
    /// Parsed keys.
    pub keys: Vec<Key>,
    /// Extra condition for this key.
    pub when: Option<When>,
}

impl Action {
    /// The bindings its keys make, one per mode and key. The binding's `when` is the
    /// action's and the key's together.
    ///
    /// # Errors
    ///
    /// The two conditions do not combine into one (too deeply nested).
    pub fn bindings(&self) -> Result<Vec<Binding>, String> {
        let mut out = Vec::new();
        for k in &self.keys {
            let when = match (&self.when, &k.when) {
                (Some(a), Some(b)) => Some(
                    When::parse(&format!("({a}) && ({b})"))
                        .map_err(|e| format!("its when and its key's when: {e}"))?,
                ),
                (Some(a), None) => Some(a.clone()),
                (None, b) => b.clone(),
            };
            for m in &self.modes {
                out.push(Binding {
                    mode: m.as_str().into(),
                    keys: k.keys.clone(),
                    action: KeyAction::Run(self.steps.clone()),
                    when: when.clone(),
                    label: self.title.as_deref().map(Into::into),
                });
            }
        }
        Ok(out)
    }

    /// Its first key that runs this action in `mode` now and is not masked by
    /// another layer (the pack's own special cases for the key count as the
    /// action), with the steps a press of it runs.
    fn live_key(
        &self,
        keymap: &Keymap,
        layer: &str,
        mode: &str,
        look: &impl ContextLookup,
    ) -> Option<(&ActionKey, Arc<[Step]>)> {
        self.keys.iter().find_map(|k| {
            let (l, a) = keymap.resolve_key(mode, &k.keys, look)?;
            match a {
                KeyAction::Run(s)
                    if Arc::ptr_eq(s, &self.steps) || **s == *self.steps || l == layer =>
                {
                    Some((k, s.clone()))
                }
                KeyAction::Run(_) | KeyAction::Unbind => None,
            }
        })
    }

    /// The hint for this action in `mode`: its live key, printed.
    pub fn hint(
        &self,
        keymap: &Keymap,
        layer: &str,
        mode: &str,
        look: &impl ContextLookup,
        display: &KeyDisplay,
    ) -> Option<String> {
        self.live_key(keymap, layer, mode, look)
            .map(|(k, _)| display.format(&k.keys))
    }

    /// The steps a press of its hinted key runs now in `mode` (what the hint
    /// promises), or its own steps if no key is live.
    pub fn press(
        &self,
        keymap: &Keymap,
        layer: &str,
        mode: &str,
        look: &impl ContextLookup,
    ) -> Arc<[Step]> {
        self.live_key(keymap, layer, mode, look)
            .map_or_else(|| self.steps.clone(), |(_, s)| s)
    }
}

/// How hints print parsed keys: `[keys.display]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyDisplay {
    /// Before a key with Ctrl.
    pub ctrl: String,
    /// Before a key with Shift.
    pub shift: String,
    /// Before a key with Alt.
    pub alt: String,
    /// Before a key with Super.
    pub super_: String,
    /// Key names (`Space`, `CR`, `Up`, or a char) to the text shown.
    pub names: BTreeMap<String, String>,
}

impl Default for KeyDisplay {
    fn default() -> Self {
        Self {
            ctrl: "ctrl+".into(),
            shift: "shift+".into(),
            alt: "alt+".into(),
            super_: "super+".into(),
            names: BTreeMap::new(),
        }
    }
}

impl KeyDisplay {
    /// A key sequence as text: keys separated by a space.
    pub fn format(&self, keys: &[Key]) -> String {
        keys.iter()
            .map(|k| self.one(*k))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn one(&self, k: Key) -> String {
        let mut shift = k.mods.contains(Mods::SHIFT);
        // Letters carry Shift as their case.
        let code = match k.code {
            KeyCode::Char(c) if c.is_uppercase() && !k.mods.without(Mods::SHIFT).is_empty() => {
                shift = true;
                KeyCode::Char(c.to_ascii_lowercase())
            }
            c => c,
        };
        let plain = Key {
            code,
            mods: Mods::NONE,
        }
        .to_string();
        let name = plain
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
            .unwrap_or(&plain);
        let shown = self
            .names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_lowercase());
        let mut out = String::new();
        if k.mods.contains(Mods::CTRL) {
            out.push_str(&self.ctrl);
        }
        if k.mods.contains(Mods::ALT) {
            out.push_str(&self.alt);
        }
        if k.mods.contains(Mods::SUPER) {
            out.push_str(&self.super_);
        }
        if shift {
            out.push_str(&self.shift);
        }
        out.push_str(&shown);
        out
    }
}

#[cfg(test)]
mod tests {
    use spur_keymap::parse_keys;

    use super::*;

    #[test]
    fn keys_print_with_the_display_table() {
        let mut d = KeyDisplay::default();
        d.names.insert("Space".into(), "space".into());
        d.names.insert("CR".into(), "enter".into());
        let f = |s: &str| d.format(&parse_keys(s).unwrap());
        assert_eq!(f("<C-s>"), "ctrl+s");
        assert_eq!(f("<C-k>e"), "ctrl+k e");
        assert_eq!(f("<C-S-e>"), "ctrl+shift+e");
        assert_eq!(f("<C-Space>"), "ctrl+space");
        assert_eq!(f("<A-1>"), "alt+1");
        assert_eq!(f("<CR>"), "enter");
        assert_eq!(f("<Up>"), "up");
        assert_eq!(f("<C-A-b>"), "ctrl+alt+b");
    }
}
