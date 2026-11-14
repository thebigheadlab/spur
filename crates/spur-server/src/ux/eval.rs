//! Turning a pack into one client's UI tree and the theme into a token map.

use std::collections::BTreeMap;

use spur_core::{ContextLookup, CtxValue};
use spur_rpc::{Node, NodeKind, Overlay, Token, tokens};

use super::pack::{Pack, Theme};
use super::state::text_of;

/// One client's evaluated tree.
#[derive(Debug)]
pub struct Built {
    /// The tree.
    pub root: Node,
    /// List node ids shown, in tree order (overlays included).
    pub lists: Vec<String>,
    /// Input node ids shown.
    pub inputs: Vec<String>,
    /// The open overlay, evaluated.
    pub overlays: Vec<Overlay>,
}

/// Resolves `{key:<action>}` for a node: (action id, node id) to the hint.
pub type Hints<'h> = &'h dyn Fn(&str, &str) -> Option<String>;

struct Walk<'a, L> {
    pack: &'a Pack,
    look: &'a L,
    hints: Hints<'a>,
    lists: Vec<String>,
    inputs: Vec<String>,
}

/// Evaluates the pack for a client: `when`, active styles, templates and icons;
/// and the open overlay's tree.
pub fn tree(pack: &Pack, look: &impl ContextLookup, hints: Hints<'_>) -> Built {
    let mut w = Walk {
        pack,
        look,
        hints,
        lists: Vec::new(),
        inputs: Vec::new(),
    };
    let root = w.node(&pack.root).unwrap_or_else(|| {
        // The root's `when` was false: show an empty root rather than nothing.
        Node::new(pack.root.clone(), NodeKind::Column)
    });
    let mut overlays = Vec::new();
    if let Some(CtxValue::Str(open)) = look.get("ui.overlay")
        && let Some(o) = pack.overlays.get(&**open)
        && let Some(node) = w.node(&o.node)
    {
        overlays.push(Overlay {
            node,
            top: o.top,
            width: o.width,
            max_height: o.max_height,
            dim: o.dim,
        });
    }
    Built {
        root,
        lists: w.lists,
        inputs: w.inputs,
        overlays,
    }
}

impl<L: ContextLookup> Walk<'_, L> {
    fn node(&mut self, id: &str) -> Option<Node> {
        let (pack, look) = (self.pack, self.look);
        let spec = pack.nodes.get(id)?;
        if spec.when.as_ref().is_some_and(|w| !w.eval(look)) {
            return None;
        }
        let mut n = Node::new(id, spec.kind);
        n.width = spec.width;
        n.height = spec.height;
        let active = spec.active.as_ref().is_some_and(|w| w.eval(look));
        n.style = if active && spec.style_active.is_some() {
            spec.style_active.clone()
        } else {
            spec.style.clone()
        };
        n.border_style = if active && spec.border_style_active.is_some() {
            spec.border_style_active.clone()
        } else {
            spec.border_style.clone()
        };
        if let Some(b) = spec.border.as_deref().and_then(|b| pack.borders.get(b)) {
            if pack.ascii_icons {
                n.border = Some(b.ascii.clone());
            } else {
                n.border = Some(b.glyphs.clone());
                n.border_alt = Some(b.ascii.clone());
            }
        }
        n.repeat = spec.repeat;
        n.padding = spec.padding;
        n.gap = spec.gap;
        n.clickable = !spec.on_click.is_empty() || pack.regions.values().any(|r| r.node == id);
        if let Some(t) = &spec.text {
            let hints = self.hints;
            n.text = Some(t.render(|k| match k.strip_prefix("key:") {
                Some(a) => hints(a, id),
                None => look.get(k).map(text_of),
            }));
        }
        n.placeholder_style.clone_from(&spec.placeholder_style);
        if let Some(icon) = spec.icon.as_deref().and_then(|i| pack.icons.get(i)) {
            if pack.ascii_icons {
                n.text = Some(icon.ascii.clone());
            } else {
                n.text = Some(icon.glyph.clone());
                n.alt = Some(icon.ascii.clone());
            }
        }
        match spec.kind {
            NodeKind::Surface => {
                n.gutter = spec.gutter;
                n.current_line = spec.current_line;
                n.tab_width = pack.tab_width;
            }
            NodeKind::List => self.lists.push(id.to_owned()),
            NodeKind::Input => self.inputs.push(id.to_owned()),
            _ => {}
        }
        for c in &spec.children {
            if let Some(child) = self.node(c) {
                n.children.push(child);
            }
        }
        Some(n)
    }
}

/// The token map a client is sent: every defined token, plus each fixed name
/// resolved through its dotted chain.
pub fn theme_tokens(theme: &Theme) -> BTreeMap<String, Token> {
    let mut map = theme.tokens.clone();
    for name in tokens::ALL {
        if let Some(t) = theme.resolve(name) {
            map.entry(name.to_owned()).or_insert_with(|| t.clone());
        }
    }
    map
}
