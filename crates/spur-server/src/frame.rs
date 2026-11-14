//! Building what a client is sent: the UI tree and surface frames.

use spur_core::editor::{Editor, ViewId};
use spur_rpc::{Cursor, CursorKind, Line, Node, NodeKind, Pos, Rows, Sel, Span, SurfaceView};

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
        children: vec![Node::new(SURFACE, NodeKind::Surface)],
        ..Node::new("root", NodeKind::Column)
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
        rows: Vec::new(),
        seq: 0,
        base: None,
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

/// What to send for `new` when the client last got `last`: only the rows that changed,
/// with the rest copied from `last` (see [`SurfaceView::rows`]). A full surface when
/// there is no `last`, the document changed, or no row could be copied.
///
/// Each row is compared with the previous frame's row at the scroll offset, at the
/// same position, and at the offset of lines inserted or deleted above, so typing,
/// scrolling and adding or removing a line resend only what changed. Cost is O(rows)
/// line comparisons. The caller numbers the result (`seq`, `base`).
pub fn delta(last: Option<&SurfaceView>, new: &SurfaceView) -> SurfaceView {
    let mut wire = SurfaceView {
        lines: Vec::new(),
        ..new.clone_without_lines()
    };
    let Some(last) = last.filter(|l| l.doc == new.doc) else {
        wire.lines.clone_from(&new.lines);
        return wire;
    };
    let scroll = i64::from(new.top_line) - i64::from(last.top_line);
    let grown = i64::from(new.line_count) - i64::from(last.line_count);
    let mut runs: Vec<Rows> = Vec::new();
    let mut copied = false;
    for (i, line) in new.lines.iter().enumerate() {
        let i = i64::from(n32(i));
        // Continuing the current copy run first keeps runs long where lines repeat.
        let next = match runs.last() {
            Some(Rows {
                from: Some(f),
                count,
            }) => i64::from(*f) + i64::from(*count),
            _ => -1,
        };
        let found = [next, i + scroll, i, i + scroll - grown]
            .into_iter()
            .filter_map(|j| usize::try_from(j).ok())
            .find(|&j| last.lines.get(j) == Some(line));
        match (found, runs.last_mut()) {
            (
                Some(j),
                Some(Rows {
                    from: Some(f),
                    count,
                }),
            ) if *f as usize + *count as usize == j => {
                *count += 1;
            }
            (Some(j), _) => {
                runs.push(Rows {
                    from: Some(n32(j)),
                    count: 1,
                });
                copied = true;
            }
            (None, Some(Rows { from: None, count })) => {
                *count += 1;
                wire.lines.push(line.clone());
            }
            (None, _) => {
                runs.push(Rows {
                    from: None,
                    count: 1,
                });
                wire.lines.push(line.clone());
            }
        }
    }
    if copied {
        wire.rows = runs;
    } else {
        wire.lines.clone_from(&new.lines);
    }
    wire
}

trait WithoutLines {
    fn clone_without_lines(&self) -> SurfaceView;
}

impl WithoutLines for SurfaceView {
    fn clone_without_lines(&self) -> SurfaceView {
        SurfaceView {
            doc: self.doc,
            version: self.version,
            top_line: self.top_line,
            line_count: self.line_count,
            lines: Vec::new(),
            rows: Vec::new(),
            seq: 0,
            base: None,
            cursors: self.cursors.clone(),
            selections: self.selections.clone(),
            path: self.path.clone(),
            modified: self.modified,
            mode: self.mode.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(top: u32, count: u32, lines: &[&str]) -> SurfaceView {
        SurfaceView {
            doc: 1,
            top_line: top,
            line_count: count,
            lines: lines
                .iter()
                .map(|t| Line {
                    spans: vec![Span {
                        text: (*t).into(),
                        hl: None,
                    }],
                    truncated: false,
                })
                .collect(),
            ..SurfaceView::default()
        }
    }

    /// Sends `new` after `old` and checks what the client rebuilds; returns how many
    /// lines went over the wire.
    fn sent(old: &SurfaceView, new: &SurfaceView) -> usize {
        let mut wire = delta(Some(old), new);
        if !wire.rows.is_empty() {
            wire.base = Some(old.seq);
        }
        let n = wire.lines.len();
        wire.resolve(Some(old)).unwrap();
        assert_eq!(&wire, new);
        n
    }

    #[test]
    fn only_changed_rows_are_sent() {
        let old = view(10, 100, &["a", "b", "c", "d"]);
        // Typing on one line.
        assert_eq!(sent(&old, &view(10, 100, &["a", "bx", "c", "d"])), 1);
        // Scrolling down one line.
        assert_eq!(sent(&old, &view(11, 100, &["b", "c", "d", "e"])), 1);
        // Scrolling up one line.
        assert_eq!(sent(&old, &view(9, 100, &["z", "a", "b", "c"])), 1);
        // A line inserted, and one deleted, above the bottom.
        assert_eq!(sent(&old, &view(10, 101, &["a", "new", "b", "c"])), 1);
        assert_eq!(sent(&old, &view(10, 99, &["a", "c", "d", "e"])), 1);
        // Nothing in common: a full frame, without runs.
        let all = delta(Some(&old), &view(50, 100, &["p", "q", "r", "s"]));
        assert!(all.rows.is_empty());
        assert_eq!(all.lines.len(), 4);
        // Another document, or no previous frame: full.
        let mut other = old.clone();
        other.doc = 2;
        assert!(delta(Some(&old), &other).rows.is_empty());
        assert_eq!(delta(None, &old), old);
    }

    #[test]
    fn repeated_lines_keep_runs_long() {
        let old = view(0, 9, &["", "", "", "", "x"]);
        let wire = delta(Some(&old), &view(0, 9, &["", "", "", "", "y"]));
        assert_eq!(wire.rows.len(), 2, "{:?}", wire.rows);
    }

    #[test]
    fn any_change_rebuilds_exactly() {
        // A small deterministic generator: frames of 0..12 rows over 4 distinct texts,
        // so repeats, scrolls and shifts all happen.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let texts = ["", "a", "b", "long line"];
        let mut old = view(0, 20, &[]);
        for _ in 0..5000 {
            let rows = usize::try_from(next(12)).unwrap();
            let lines: Vec<&str> = (0..rows)
                .map(|_| texts[usize::try_from(next(4)).unwrap()])
                .collect();
            let new = view(n32(usize::try_from(next(8)).unwrap()), 20, &lines);
            sent(&old, &new);
            old = new;
        }
    }
}
