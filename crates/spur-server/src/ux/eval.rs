//! Turning a pack into one client's UI tree and the theme into a token map.

use std::collections::BTreeMap;

use spur_core::ContextLookup;
use spur_rpc::{Node, NodeKind, Token, tokens};

use super::pack::{Pack, Theme};
use super::state::text_of;

/// One client's evaluated tree.
#[derive(Debug)]
pub struct Built {
    /// The tree.
    pub root: Node,
    /// List node ids shown, in tree order.
    pub lists: Vec<String>,
}

/// Evaluates the pack for a client: `when`, active styles, templates and icons.
pub fn tree(pack: &Pack, look: &impl ContextLookup) -> Built {
    let mut lists = Vec::new();
    let root = node(pack, &pack.root, look, &mut lists).unwrap_or_else(|| {
        // The root's `when` was false: show an empty root rather than nothing.
        Node::new(pack.root.clone(), NodeKind::Column)
    });
    Built { root, lists }
}

fn node(pack: &Pack, id: &str, look: &impl ContextLookup, lists: &mut Vec<String>) -> Option<Node> {
    let spec = pack.nodes.get(id)?;
    if spec.when.as_ref().is_some_and(|w| !w.eval(look)) {
        return None;
    }
    let mut n = Node::new(id, spec.kind);
    n.width = spec.width;
    n.height = spec.height;
    let active = spec.active.as_ref().is_some_and(|w| w.eval(look));
    n.style = if active {
        spec.style_active.clone()
    } else {
        spec.style.clone()
    };
    n.padding = spec.padding;
    n.gap = spec.gap;
    n.clickable = !spec.on_click.is_empty() || pack.regions.values().any(|r| r.node == id);
    if let Some(t) = &spec.text {
        n.text = Some(t.render(|k| look.get(k).map(text_of)));
    }
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
        NodeKind::List => lists.push(id.to_owned()),
        _ => {}
    }
    for c in &spec.children {
        if let Some(child) = node(pack, c, look, lists) {
            n.children.push(child);
        }
    }
    Some(n)
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
