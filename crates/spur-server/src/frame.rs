//! Building what a client is sent: the UI tree and surface frames.

use spur_core::editor::{Editor, ViewId};
use spur_rpc::{Cursor, CursorKind, Dim, Line, Node, NodeKind, Pos, Sel, Span, SurfaceView};

/// Id of the editor surface node.
pub const SURFACE: &str = "editor";
/// Longest line prefix sent, in chars.
pub const MAX_LINE: usize = 4000;
/// Most cursors and most selections sent per surface. With the row cap, this keeps a
/// surface under the frame limit even when every char needs a 6-byte JSON escape.
pub const MAX_MARKS: usize = 10_000;

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
/// cursors when they show the same document. Cost is O(rows × [`MAX_LINE`] + peers).
pub fn surface(ed: &Editor, view: ViewId, rows: usize, peers: &[(u64, ViewId)]) -> SurfaceView {
    let Some(vp) = ed.viewport_of(view, rows, MAX_LINE) else {
        return SurfaceView::default();
    };
    let v = &ed.views[&view];
    let doc = &ed.docs[&v.doc];
    let lines = vp
        .lines
        .into_iter()
        .zip(vp.clipped)
        .map(|(text, truncated)| Line {
            spans: vec![Span { text, hl: None }],
            truncated,
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
    cursors.truncate(MAX_MARKS);
    for &(peer, pv) in peers.iter().take(MAX_MARKS) {
        if ed.views.get(&pv).is_none_or(|p| p.doc != v.doc) {
            continue;
        }
        let Some(p) = ed.primary_point(pv) else {
            continue;
        };
        cursors.push(Cursor {
            line: n32(p.line),
            col: n32(p.column),
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
            .take(MAX_MARKS)
            .map(|(a, b)| Sel {
                start: pos(*a),
                end: pos(*b),
            })
            .collect(),
        // Absolute (the core stores it so). Clients shorten it for people.
        path: vp.path.as_deref().map(|p| p.display().to_string()),
        modified: vp.modified,
        mode: vp.mode.to_string(),
    }
}
