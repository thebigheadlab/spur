//! Pack actions (named steps with a title, keys and a condition) and how key hints
//! print.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use spur_core::{CommandId, ContextLookup, When};
use spur_keymap::{
    Action as KeyAction, Binding, Key, KeyCode, Keymap, Layer, Mods, Step, legacy_can_send,
};

/// The command every action key runs: `ui.action.run {id}`.
pub const RUN_ACTION: &str = "ui.action.run";

/// A pack action: `[actions.<id>]`.
#[derive(Clone, Debug)]
pub struct Action {
    /// Shown in the palette; untitled actions are key-only.
    pub title: Option<String>,
    /// What runs when no case holds.
    pub steps: Arc<[Step]>,
    /// Ordered cases: the first whose `when` holds runs instead of `steps`.
    pub cases: Vec<Case>,
    /// When it applies at all (keys, clicks and the palette).
    pub when: Option<When>,
    /// Key modes its keys are bound in.
    pub modes: Vec<String>,
    /// Its keys, in order; the first that applies is its hint.
    pub keys: Vec<ActionKey>,
}

/// One case of an action: `[[actions.<id>.case]]`.
#[derive(Clone, Debug)]
pub struct Case {
    /// When it runs.
    pub when: When,
    /// What runs.
    pub steps: Arc<[Step]>,
}

/// One key of an action.
#[derive(Clone, Debug)]
pub struct ActionKey {
    /// Parsed keys.
    pub keys: Vec<Key>,
    /// Extra condition for this key.
    pub when: Option<When>,
}

/// The one step that runs action `id`.
///
/// # Panics
///
/// Never: the command id is valid.
pub fn run_step(id: &str) -> Step {
    Step {
        command: CommandId::new(RUN_ACTION).expect("a valid command id"),
        args: Arc::new(json!({ "id": id })),
    }
}

/// The action that `steps` run, if they are exactly one `ui.action.run`.
pub fn run_target(steps: &[Step]) -> Option<&str> {
    match steps {
        [s] if s.command.as_str() == RUN_ACTION => s.args.get("id")?.as_str(),
        _ => None,
    }
}

impl Action {
    /// What runs now: `None` if the action does not apply, else its first case
    /// that holds, else its default steps.
    pub fn steps_for(&self, look: &impl ContextLookup) -> Option<Arc<[Step]>> {
        if self.when.as_ref().is_some_and(|w| !w.eval(look)) {
            return None;
        }
        Some(
            self.cases
                .iter()
                .find(|c| c.when.eval(look))
                .map_or_else(|| self.steps.clone(), |c| c.steps.clone()),
        )
    }

    /// The label its bindings carry: its title, else its id.
    fn label(&self, id: &str) -> Arc<str> {
        self.title.as_deref().unwrap_or(id).into()
    }

    /// The bindings its keys make, one per mode and key, each running
    /// `ui.action.run {id}`. The binding's `when` is the action's and the key's
    /// together, so a key falls through when the action does not apply.
    ///
    /// # Errors
    ///
    /// The two conditions do not combine into one (too deeply nested).
    pub fn bindings(&self, id: &str) -> Result<Vec<Binding>, String> {
        let steps: Arc<[Step]> = Arc::new([run_step(id)]);
        let mut out = Vec::new();
        for k in &self.keys {
            let when = When::both(self.when.as_ref(), k.when.as_ref())
                .map_err(|e| format!("its when and its key's when: {e}"))?;
            for m in &self.modes {
                out.push(Binding {
                    mode: m.as_str().into(),
                    keys: k.keys.clone(),
                    action: KeyAction::Run(steps.clone()),
                    when: when.clone(),
                    label: Some(self.label(id)),
                });
            }
        }
        Ok(out)
    }

    /// Its first key that, pressed now in `mode`, runs this action (the binding
    /// chosen for it is `ui.action.run` of this action, from any layer) and can
    /// arrive from the client (without full keys, every key of the sequence
    /// must be one a legacy terminal sends).
    fn live_key(
        &self,
        id: &str,
        keymap: &Keymap,
        mode: &str,
        look: &impl ContextLookup,
        full_keys: bool,
    ) -> Option<&ActionKey> {
        self.keys.iter().find(|k| {
            (full_keys || k.keys.iter().all(legacy_can_send))
                && matches!(
                    keymap.resolve_key(mode, &k.keys, look),
                    Some((_, KeyAction::Run(s))) if run_target(s) == Some(id)
                )
        })
    }

    /// The hint for this action in `mode`: its live key, printed.
    pub fn hint(
        &self,
        id: &str,
        keymap: &Keymap,
        mode: &str,
        look: &impl ContextLookup,
        display: &KeyDisplay,
        full_keys: bool,
    ) -> Option<String> {
        self.live_key(id, keymap, mode, look, full_keys)
            .map(|k| display.format(&k.keys))
    }
}

/// Gives every binding of `layer` that runs a known action (`ui.action.run {id}`)
/// the action's `when` beside its own and the action's label, so it falls through
/// when the action does not apply.
///
/// # Errors
///
/// A combined condition nests too deeply: the binding's keys and the error.
pub fn wrap_bindings<'a>(
    layer: &mut Layer,
    action: impl Fn(&str) -> Option<&'a Action>,
) -> Result<(), String> {
    for b in &mut layer.bindings {
        let KeyAction::Run(steps) = &b.action else {
            continue;
        };
        let Some(id) = run_target(steps) else {
            continue;
        };
        let Some(a) = action(id) else {
            continue;
        };
        b.when = When::both(a.when.as_ref(), b.when.as_ref()).map_err(|e| {
            format!(
                "{}: the action's when and its own: {e}",
                spur_keymap::format_keys(&b.keys)
            )
        })?;
        if b.label.is_none() {
            b.label = Some(a.label(id));
        }
    }
    Ok(())
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

    fn action(when: Option<&str>, keys: &[&str]) -> Action {
        Action {
            title: Some("Show".into()),
            steps: Arc::new([]),
            cases: vec![Case {
                when: When::parse("ui.focus == agent").unwrap(),
                steps: Arc::new([run_step("x")]),
            }],
            when: when.map(|w| When::parse(w).unwrap()),
            modes: vec!["insert".into()],
            keys: keys
                .iter()
                .map(|k| ActionKey {
                    keys: parse_keys(k).unwrap(),
                    when: None,
                })
                .collect(),
        }
    }

    fn layer(name: &str, toml: &str) -> Layer {
        Layer::from_toml(name, spur_core::Source::Core, toml).unwrap()
    }

    #[test]
    fn the_first_case_that_holds_runs() {
        let a = action(Some("ui.on"), &[]);
        let mut cx = spur_core::Context::new();
        assert!(a.steps_for(&cx).is_none(), "its when is false");
        cx.set("ui.on", true).unwrap();
        assert!(a.steps_for(&cx).unwrap().is_empty(), "the default steps");
        cx.set("ui.focus", "agent").unwrap();
        assert_eq!(run_target(&a.steps_for(&cx).unwrap()), Some("x"));
    }

    #[test]
    fn hints_name_only_keys_that_run_the_action() {
        let a = action(None, &["<C-k>a", "<C-a>"]);
        let mut pack = Layer::new("pack", spur_core::Source::Core);
        pack.bindings = a.bindings("show").unwrap();
        // A higher layer takes ctrl+k a for something else: the hint skips it.
        let over = layer(
            "user",
            "[[bind]]\nmode = \"insert\"\nkeys = \"<C-k>a\"\ncommand = \"editor.undo\"\n",
        );
        let mut km = Keymap::new();
        km.push_layer(pack);
        km.push_layer(over);
        let cx = spur_core::Context::new();
        let d = KeyDisplay::default();
        assert_eq!(
            a.hint("show", &km, "insert", &cx, &d, true).as_deref(),
            Some("ctrl+a")
        );
        assert_eq!(a.hint("other", &km, "insert", &cx, &d, true), None);
    }

    #[test]
    fn a_binding_to_an_action_falls_through_when_the_action_does_not_apply() {
        let a = action(Some("ui.on"), &[]);
        let mut low = layer(
            "low",
            "[[bind]]\nmode = \"insert\"\nkeys = \"<C-a>\"\ncommand = \"editor.undo\"\n",
        );
        let mut high = layer(
            "high",
            "[[bind]]\nmode = \"insert\"\nkeys = \"<C-a>\"\ncommand = \"ui.action.run\"\nargs = { id = \"show\" }\n",
        );
        wrap_bindings(&mut low, |_| Some(&a)).unwrap();
        wrap_bindings(&mut high, |id| (id == "show").then_some(&a)).unwrap();
        assert_eq!(high.bindings[0].label.as_deref(), Some("Show"));
        let mut km = Keymap::new();
        km.push_layer(low);
        km.push_layer(high);
        let keys = parse_keys("<C-a>").unwrap();
        let mut cx = spur_core::Context::new();
        let run = |cx: &spur_core::Context| match km.resolve_key("insert", &keys, cx) {
            Some((_, KeyAction::Run(s))) => s[0].command.to_string(),
            _ => String::new(),
        };
        assert_eq!(run(&cx), "editor.undo");
        cx.set("ui.on", true).unwrap();
        assert_eq!(run(&cx), RUN_ACTION);
    }

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
