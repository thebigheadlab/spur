//! Per-client UI state: the pack's state keys, focus, notices and list positions,
//! and the context keys that expose them to `when` clauses and templates.

use std::collections::BTreeMap;
use std::path::PathBuf;

use spur_core::editor::{Editor, ViewId};
use spur_core::{Context, ContextLookup, CtxValue, Scoped};
use spur_rpc::{ListView, Node, Overlay};

use super::field::Field;
use super::files::{Files, Tree};
use super::pack::{Pack, StateValue};
use super::palette::PaletteState;
use super::{EDITOR_REGION, FILES_SOURCE};
use crate::lang;

/// A `ui` message as last sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentUi {
    /// The tree.
    pub root: Node,
    /// The focused node.
    pub focus: String,
    /// The overlays.
    pub overlays: Vec<Overlay>,
}

impl SentUi {
    /// True if `id` is in the tree or an overlay.
    pub fn has(&self, id: &str) -> bool {
        crate::session::has_node(&self.root, id)
            || self
                .overlays
                .iter()
                .any(|o| crate::session::has_node(&o.node, id))
    }

    /// True if `id` is in an overlay.
    pub fn in_overlay(&self, id: &str) -> bool {
        self.overlays
            .iter()
            .any(|o| crate::session::has_node(&o.node, id))
    }
}

/// A client's UI state.
#[derive(Clone, Debug)]
pub struct UiState {
    /// Declared state keys.
    pub values: BTreeMap<String, StateValue>,
    /// Focused region name.
    pub focus: String,
    /// The quit prompt.
    pub prompt: Option<String>,
    /// The last failed command from a key or click.
    pub status: Option<String>,
    /// The quit prompt was showing when the current input arrived.
    pub quit_armed: bool,
    /// Files trees by list node id.
    pub trees: BTreeMap<String, Tree>,
    /// Rows each list node was given by the client's layout.
    pub list_rows: BTreeMap<String, u32>,
    /// The windows last sent, by list node; forgotten when the node is not shown.
    pub sent_lists: BTreeMap<String, ListView>,
    /// The theme generation last sent.
    pub sent_theme: Option<u64>,
    /// The tree, focus and overlays last sent.
    pub sent_ui: Option<SentUi>,
    /// The open overlay.
    pub overlay: Option<String>,
    /// The region focused before the overlay opened.
    pub prev_focus: Option<String>,
    /// The focused input, when one was chosen (else the first in the region).
    pub input: Option<String>,
    /// Input texts by node id.
    pub fields: BTreeMap<String, Field>,
    /// The input texts last sent.
    pub sent_fields: BTreeMap<String, Field>,
    /// The palette.
    pub palette: PaletteState,
    /// Documents this window made active, newest first (the current one too).
    pub recent: Vec<PathBuf>,
}

impl UiState {
    /// Defaults from the pack; the Files trees reveal `reveal` (a relative id).
    pub fn new(pack: &Pack, files: &mut Files, reveal: Option<&str>) -> Self {
        let trees = pack
            .nodes
            .iter()
            .filter(|(_, n)| n.source.as_deref() == Some(FILES_SOURCE))
            .map(|(id, _)| (id.clone(), Tree::revealing(files, reveal)))
            .collect();
        Self {
            values: pack.state.clone(),
            focus: EDITOR_REGION.into(),
            prompt: None,
            status: None,
            quit_armed: false,
            trees,
            list_rows: BTreeMap::new(),
            sent_lists: BTreeMap::new(),
            sent_theme: None,
            sent_ui: None,
            overlay: None,
            prev_focus: None,
            input: None,
            fields: BTreeMap::new(),
            sent_fields: BTreeMap::new(),
            palette: PaletteState::default(),
            recent: Vec::new(),
        }
    }

    /// Notes the document now active; keeps [`super::palette::MAX_RECENT`] + 1.
    pub fn visit(&mut self, path: Option<&std::path::Path>) {
        let Some(p) = path else {
            return;
        };
        if self.recent.first().is_some_and(|r| r == p) {
            return;
        }
        self.recent.retain(|r| r != p);
        self.recent.insert(0, p.to_owned());
        self.recent.truncate(super::palette::MAX_RECENT + 1);
    }

    /// The notice shown: prompt, else status, else the pack error.
    pub fn notice<'a>(&'a self, pack_error: Option<&'a str>) -> &'a str {
        self.prompt
            .as_deref()
            .or(self.status.as_deref())
            .or(pack_error)
            .unwrap_or("")
    }

    /// Keeps values that still fit a new pack; resets the rest and adds new keys.
    pub fn migrate(&mut self, pack: &Pack, files: &mut Files, scope: &mut Context) {
        let old = std::mem::take(&mut self.values);
        for k in old.keys() {
            if !pack.state.contains_key(k) {
                scope.remove(k);
            }
        }
        for (k, default) in &pack.state {
            let keep = old.get(k).filter(|v| {
                v.same_type(default)
                    && match (v, pack.state_values.get(k)) {
                        (StateValue::Str(s), Some(list)) => list.contains(s),
                        _ => true,
                    }
            });
            self.values
                .insert(k.clone(), keep.unwrap_or(default).clone());
        }
        if self
            .overlay
            .as_ref()
            .is_some_and(|o| !pack.overlays.contains_key(o))
        {
            self.overlay = None;
            if let Some(p) = self.prev_focus.take() {
                self.focus = p;
            }
        }
        if !pack.regions.contains_key(&self.focus) {
            self.focus = EDITOR_REGION.into();
        }
        if self.input.as_ref().is_some_and(|i| {
            pack.nodes
                .get(i)
                .is_none_or(|n| n.kind != spur_rpc::NodeKind::Input)
        }) {
            self.input = None;
        }
        self.fields.retain(|id, _| pack.nodes.contains_key(id));
        self.sent_fields.clear();
        let mut trees = std::mem::take(&mut self.trees);
        for (id, n) in &pack.nodes {
            if n.source.as_deref() == Some(FILES_SOURCE) {
                let t = trees
                    .remove(id)
                    .or_else(|| trees.values().next().cloned())
                    .unwrap_or_else(|| Tree::revealing(files, None));
                self.trees.insert(id.clone(), t);
            }
        }
        self.list_rows.retain(|id, _| pack.nodes.contains_key(id));
        self.sent_lists.clear();
        self.sent_ui = None;
    }
}

/// Everything the scope needs besides the UI state.
pub struct ScopeInput<'a> {
    /// The editor.
    pub ed: &'a Editor,
    /// The client's active view.
    pub view: ViewId,
    /// Client size.
    pub cols: u32,
    /// Client size.
    pub rows: u32,
    /// The pack.
    pub pack: &'a Pack,
    /// Workspace folder name.
    pub workspace: &'a str,
    /// The pack error notice, if any.
    pub pack_error: Option<&'a str>,
    /// The core's global context.
    pub global: &'a Context,
}

/// Writes `ui.*`, `client.*`, `doc.*`, `workspace.name` and then `cond.*` into the
/// client's scope (the view keys are written by the caller first).
pub fn refresh(scope: &mut Context, ui: &UiState, x: &ScopeInput<'_>) {
    // Keys were checked as names at load, so `set` cannot fail.
    for (k, v) in &ui.values {
        let _ = match v {
            StateValue::Str(s) => scope.set(k, s.as_str()),
            StateValue::Bool(b) => scope.set(k, *b),
            StateValue::Int(i) => scope.set(k, *i),
        };
    }
    let _ = scope.set("ui.focus", ui.focus.as_str());
    let _ = scope.set("ui.notice", ui.notice(x.pack_error));
    let _ = scope.set("ui.overlay", ui.overlay.as_deref().unwrap_or(""));
    let input = focused_input(x.pack, ui, &Scoped(&[x.global, scope])).unwrap_or_default();
    let _ = scope.set("ui.input", input.as_str());
    let _ = scope.set("palette.scope", ui.palette.scope.as_str());
    let label = if ui.palette.scope.is_empty() {
        String::new()
    } else {
        x.pack
            .message(&format!("palette.scope.{}", ui.palette.scope), &[])
    };
    let _ = scope.set("palette.label", label.as_str());
    let _ = scope.set("client.cols", i64::from(x.cols));
    let _ = scope.set("client.rows", i64::from(x.rows));
    let path =
        x.ed.views
            .get(&x.view)
            .and_then(|v| x.ed.docs.get(&v.doc))
            .and_then(|d| d.path.clone());
    let name = path.as_deref().and_then(|p| p.file_name()).map_or_else(
        || x.pack.message("doc.untitled", &[]),
        |n| n.to_string_lossy().into_owned(),
    );
    let _ = scope.set("doc.name", name.as_str());
    let lang = lang::of_path(path.as_deref());
    let label = x.pack.languages.get(lang).map_or(lang, String::as_str);
    let _ = scope.set("doc.language", label);
    let _ = scope.set("workspace.name", x.workspace);
    // Conditions read no other condition, so their order does not matter.
    let values: Vec<(String, bool)> = {
        let look = Scoped(&[x.global, scope]);
        x.pack
            .conditions
            .iter()
            .map(|(n, w)| (format!("cond.{n}"), w.eval(&look)))
            .collect()
    };
    for (k, v) in values {
        let _ = scope.set(&k, v);
    }
}

/// A context value as template text.
pub fn text_of(v: &CtxValue) -> String {
    match v {
        CtxValue::Bool(b) => b.to_string(),
        CtxValue::Int(i) => i.to_string(),
        CtxValue::Str(s) => s.to_string(),
        CtxValue::List(l) => l.join(", "),
    }
}

/// The first list node under `node` (itself included) in tree order, among the
/// nodes whose `when` holds. `node` is taken as shown.
pub fn first_list(pack: &Pack, node: &str, look: &impl ContextLookup) -> Option<String> {
    first_of(pack, node, look, spur_rpc::NodeKind::List)
}

/// True if `node` and every node above it pass their `when`, and the tree it is in
/// is shown (the root, or an open overlay).
pub fn shown(pack: &Pack, node: &str, look: &impl ContextLookup) -> bool {
    let mut id = node;
    loop {
        let Some(spec) = pack.nodes.get(id) else {
            return false;
        };
        if spec.when.as_ref().is_some_and(|w| !w.eval(look)) {
            return false;
        }
        match pack.parent.get(id) {
            Some(p) => id = p,
            None => break,
        }
    }
    id == pack.root
        || pack.overlays.iter().any(|(oid, o)| {
            o.node == id
                && matches!(look.get("ui.overlay"), Some(CtxValue::Str(s)) if &**s == oid.as_str())
        })
}

/// The input that has focus: the chosen one if it is in the focused region and
/// shown, else the first shown input of the region.
pub fn focused_input(pack: &Pack, ui: &UiState, look: &impl ContextLookup) -> Option<String> {
    let region = pack.regions.get(&ui.focus)?;
    if let Some(i) = &ui.input
        && pack.within(i, &region.node)
        && shown(pack, i, look)
    {
        return Some(i.clone());
    }
    first_of(pack, &region.node, look, spur_rpc::NodeKind::Input)
}

/// The first node of `kind` under `node` (itself included) in tree order, among
/// the nodes whose `when` holds. `node` is taken as shown.
pub fn first_of(
    pack: &Pack,
    node: &str,
    look: &impl ContextLookup,
    kind: spur_rpc::NodeKind,
) -> Option<String> {
    let spec = pack.nodes.get(node)?;
    if spec.kind == kind {
        return Some(node.to_owned());
    }
    spec.children.iter().find_map(|c| {
        let shown = pack
            .nodes
            .get(c)
            .is_some_and(|s| s.when.as_ref().is_none_or(|w| w.eval(look)));
        if shown {
            first_of(pack, c, look, kind)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::ux::pack::{PackSource, default_commands, read_source};

    /// `ux_fresh` with extra `[state]` lines and `[state_values]` lines.
    fn pack(state: &str, values: &str) -> Pack {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/ux_fresh");
        let mut files = read_source(&PackSource::Dir(dir)).unwrap();
        let toml = &mut files.iter_mut().find(|(p, _)| p == "pack.toml").unwrap().1;
        *toml = toml
            .replacen("[state]", &format!("[state]\n{state}"), 1)
            .replacen("[state_values]", &format!("[state_values]\n{values}"), 1);
        let ids = default_commands();
        Pack::load(&files, &|id| ids.contains(id)).unwrap_or_else(|e| panic!("{e:#?}"))
    }

    #[test]
    fn migrate_keeps_values_that_still_fit() {
        let old = pack(
            "\"ui.extra\" = true\n\"ui.mode\" = \"a\"\n\"ui.gone\" = 1",
            "\"ui.mode\" = [\"a\", \"b\"]",
        );
        let new = pack(
            "\"ui.extra\" = \"on\"\n\"ui.mode\" = \"a\"",
            "\"ui.mode\" = [\"a\", \"c\"]",
        );
        let mut files = Files::new(std::env::temp_dir());
        let mut ui = UiState::new(&old, &mut files, None);
        ui.values
            .insert("ui.sidebar_open".into(), StateValue::Bool(false));
        ui.values.insert("ui.extra".into(), StateValue::Bool(false));
        ui.values
            .insert("ui.mode".into(), StateValue::Str("b".into()));
        ui.focus = "nowhere".into();
        let mut scope = Context::default();
        scope.set("ui.gone", 1).unwrap();
        ui.migrate(&new, &mut files, &mut scope);
        // Same key, same type: kept.
        assert_eq!(ui.values["ui.sidebar_open"], StateValue::Bool(false));
        // The type changed: the new default.
        assert_eq!(ui.values["ui.extra"], StateValue::Str("on".into()));
        // No longer an allowed value: the new default.
        assert_eq!(ui.values["ui.mode"], StateValue::Str("a".into()));
        // Gone from the pack: gone from the values and the scope.
        assert!(!ui.values.contains_key("ui.gone"));
        assert!(scope.get("ui.gone").is_none());
        assert_eq!(ui.focus, EDITOR_REGION);
        assert_eq!(ui.trees.len(), 1);
    }
}
