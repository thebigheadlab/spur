//! Turns a pack's node definitions into the tree one client sees. Bindings read context
//! keys and server state only, so nothing here can call into an extension.

use spur_core::{CommandId, When};
use spur_rpc::{Float, ListRow, Node, NodeKind, Selected, Span, Ui};

use crate::pack::{KindDef, Pack};

/// What a list node shows.
pub(crate) struct ListData {
    pub rows: Vec<ListRow>,
    pub selected: Option<u32>,
}

/// The client state a tree is resolved against.
pub(crate) trait Env {
    /// A context key as text.
    fn get(&self, key: &str) -> Option<String>;
    /// Evaluates a condition.
    fn test(&self, when: &When) -> bool;
    /// The key that runs `command` right now, formatted for people.
    fn key_hint(&self, command: &CommandId) -> Option<String>;
    /// Rows of a list source.
    fn list(&self, source: &str) -> ListData;
    /// Value of an input source.
    fn input(&self, source: &str) -> String;
}

struct Resolver<'a> {
    pack: &'a Pack,
    env: &'a dyn Env,
    /// Source that has keyboard focus, if not the editor.
    focused: Option<&'a str>,
    focus_input: Option<String>,
    focus_list: Option<String>,
}

impl Resolver<'_> {
    fn node(&mut self, name: &str) -> Option<Node> {
        let def = self.pack.nodes.get(name)?;
        if let Some(w) = &def.when
            && !self.env.test(w)
        {
            return None;
        }
        let kind = match &def.kind {
            KindDef::Row(children) => NodeKind::Row {
                children: children.iter().filter_map(|c| self.node(c)).collect(),
            },
            KindDef::Column(children) => NodeKind::Column {
                children: children.iter().filter_map(|c| self.node(c)).collect(),
            },
            KindDef::Spacer => NodeKind::Spacer,
            KindDef::Surface => NodeKind::Surface,
            KindDef::Text { spans, align } => {
                let mut out = Vec::new();
                for s in spans {
                    if s.when.as_ref().is_some_and(|w| !self.env.test(w)) {
                        continue;
                    }
                    if let Some(cmd) = &s.key {
                        let Some(hint) = self.env.key_hint(cmd) else {
                            continue;
                        };
                        out.push(Span::new(hint, "kbd"));
                    }
                    let text = s.text.render(|k| self.env.get(k));
                    if !text.is_empty() {
                        out.push(Span {
                            text,
                            style: s.style.clone(),
                        });
                    }
                }
                NodeKind::Text {
                    spans: out,
                    align: *align,
                }
            }
            KindDef::List { source } => {
                let focused = self.focused == Some(source.as_str());
                if focused && self.focus_list.is_none() {
                    self.focus_list = Some(name.to_owned());
                }
                let data = self.env.list(source);
                NodeKind::List {
                    rows: data.rows,
                    selected: data.selected,
                    focused,
                }
            }
            KindDef::Input {
                source,
                prompt,
                placeholder,
            } => {
                if self.focused == Some(source.as_str()) && self.focus_input.is_none() {
                    self.focus_input = Some(name.to_owned());
                }
                let value = self.env.input(source);
                NodeKind::Input {
                    prompt: prompt.clone(),
                    cursor: u32::try_from(value.chars().count()).unwrap_or(u32::MAX),
                    value,
                    placeholder: placeholder.clone(),
                }
            }
        };
        Some(Node {
            id: name.to_owned(),
            style: def.style.clone(),
            width: def.width,
            height: def.height,
            pad: def.pad,
            kind,
        })
    }
}

/// Resolves the pack for one client. `focused` is the list/input source that has
/// keyboard focus, or `None` for the editor.
pub(crate) fn resolve(pack: &Pack, env: &dyn Env, focused: Option<&str>) -> Ui {
    let mut r = Resolver {
        pack,
        env,
        focused,
        focus_input: None,
        focus_list: None,
    };
    let root = r.node(&pack.root).unwrap_or(Node {
        id: pack.root.clone(),
        style: None,
        width: spur_rpc::Dim::Fill(1),
        height: spur_rpc::Dim::Fill(1),
        pad: spur_rpc::Pad::default(),
        kind: NodeKind::Spacer,
    });
    let floats = pack
        .floats
        .iter()
        .filter(|f| f.when.as_ref().is_none_or(|w| env.test(w)))
        .filter_map(|f| {
            Some(Float {
                node: r.node(&f.node)?,
                anchor: f.anchor,
                width: f.width,
                height: f.height,
                scrim: f.scrim,
            })
        })
        .collect();
    let focus = r
        .focus_input
        .or(r.focus_list)
        .map_or(Selected::Surface, Selected::Node);
    Ui {
        root,
        floats,
        focus,
    }
}
