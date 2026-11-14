//! Per-client UI state: the pack's state keys, focus, notices and list positions,
//! and the context keys that expose them to `when` clauses and templates.

use std::collections::BTreeMap;

use spur_core::editor::{Editor, ViewId};
use spur_core::{Context, ContextLookup, CtxValue, Scoped};
use spur_rpc::{ListView, Node};

use super::files::{Files, Tree};
use super::pack::{Pack, StateValue};
use crate::lang;

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
    /// List nodes in the tree last built, in tree order.
    pub shown_lists: Vec<String>,
    /// The windows last sent, by list node; forgotten when the node is not shown.
    pub sent_lists: BTreeMap<String, ListView>,
    /// The theme generation last sent.
    pub sent_theme: Option<u64>,
    /// The tree and focus last sent.
    pub sent_ui: Option<(Node, String)>,
}

impl UiState {
    /// Defaults from the pack; the Files trees reveal `reveal` (a relative id).
    pub fn new(pack: &Pack, files: &mut Files, reveal: Option<&str>) -> Self {
        let trees = pack
            .nodes
            .iter()
            .filter(|(_, n)| n.source.as_deref() == Some("workspace.files"))
            .map(|(id, _)| (id.clone(), Tree::revealing(files, reveal)))
            .collect();
        Self {
            values: pack.state.clone(),
            focus: "editor".into(),
            prompt: None,
            status: None,
            quit_armed: false,
            trees,
            list_rows: BTreeMap::new(),
            shown_lists: Vec::new(),
            sent_lists: BTreeMap::new(),
            sent_theme: None,
            sent_ui: None,
        }
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
    #[allow(dead_code)] // used by pack reload (next phase)
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
        if !pack.regions.contains_key(&self.focus) {
            self.focus = "editor".into();
        }
        let mut trees = std::mem::take(&mut self.trees);
        for (id, n) in &pack.nodes {
            if n.source.as_deref() == Some("workspace.files") {
                let t = trees
                    .remove(id)
                    .or_else(|| trees.values().next().cloned())
                    .unwrap_or_else(|| Tree::revealing(files, None));
                self.trees.insert(id.clone(), t);
            }
        }
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

/// True if `node` and every node above it pass their `when`.
pub fn shown(pack: &Pack, node: &str, look: &impl ContextLookup) -> bool {
    let mut n = Some(node);
    while let Some(id) = n {
        let Some(spec) = pack.nodes.get(id) else {
            return false;
        };
        if spec.when.as_ref().is_some_and(|w| !w.eval(look)) {
            return false;
        }
        n = pack.parent.get(id).map(String::as_str);
    }
    true
}
