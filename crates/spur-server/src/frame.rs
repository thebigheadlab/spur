//! Building what a client is sent: the UI tree and surface frames.

use std::path::Path;

use spur_core::editor::{Editor, ViewId, motion};
use spur_rpc::{Cursor, CursorKind, Dim, Line, Node, NodeKind, Pos, Sel, Span, SurfaceView};

/// Id of the editor surface node.
pub const SURFACE: &str = "editor";
/// Longest line prefix sent, in chars.
pub const MAX_LINE: usize = 4000;

/// The slice 1 tree: one editor surface filling the client's area. UX packs replace it
/// from slice 2.
pub fn bare_ui() -> Node {
    Node {
        id: "root".into(),
        kind: NodeKind::Column,
        width: Dim::FILL,
        height: Dim::FILL,
        children: vec![Node {
            id: SURFACE.into(),
            kind: NodeKind::Surface,
            width: Dim::FILL,
            height: Dim::FILL,
            children: vec![],
        }],
    }
}

/// Saturating conversion for wire numbers.
pub fn n32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// What `view` shows in `rows` lines, with `peers` (client id, view) drawn as peer
/// cursors when they show the same document. Cost is O(rows + peers).
pub fn surface(ed: &Editor, view: ViewId, rows: usize, peers: &[(u64, ViewId)]) -> SurfaceView {
    let Some(vp) = ed.viewport_of(view, rows) else {
        return SurfaceView::default();
    };
    let v = &ed.views[&view];
    let doc = &ed.docs[&v.doc];
    let lines = vp
        .lines
        .into_iter()
        .map(|text| {
            let truncated = text.chars().count() > MAX_LINE;
            let text = if truncated {
                text.chars().take(MAX_LINE).collect()
            } else {
                text
            };
            Line {
                spans: vec![Span { text, hl: None }],
                truncated,
            }
        })
        .collect();
    let mut cursors: Vec<Cursor> = vp
        .cursors
        .iter()
        .map(|(p, primary)| Cursor {
            line: n32(p.line),
            col: n32(p.column),
            kind: if *primary {
                CursorKind::Primary
            } else {
                CursorKind::Secondary
            },
            peer: None,
        })
        .collect();
    let snap = doc.buffer.snapshot();
    for &(peer, pv) in peers {
        let Some(p) = ed.views.get(&pv).filter(|p| p.doc == v.doc) else {
            continue;
        };
        let head = p.selection.ranges[p.selection.primary].head;
        cursors.push(Cursor {
            line: n32(motion::line_of(&snap, head)),
            col: n32(motion::char_col(&snap, head)),
            kind: CursorKind::Peer,
            peer: Some(peer),
        });
    }
    let pos = |p: spur_text::Point| Pos {
        line: n32(p.line),
        col: n32(p.column),
    };
    SurfaceView {
        doc: v.doc.0,
        version: doc.buffer.version().0,
        top_line: n32(vp.top_line),
        line_count: n32(vp.line_count),
        lines,
        cursors,
        selections: vp
            .selections
            .iter()
            .map(|(a, b)| Sel {
                start: pos(*a),
                end: pos(*b),
            })
            .collect(),
        path: vp.path.as_deref().map(display_path),
        modified: vp.modified,
        mode: vp.mode.to_string(),
    }
}

/// A path for people: relative to the current directory when inside it.
fn display_path(p: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| p.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| p.to_path_buf())
        .display()
        .to_string()
}
