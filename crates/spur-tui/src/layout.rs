//! Lays a UI tree out in cells: one pass per container along its main axis.

use spur_rpc::{Dim, Node, NodeKind};

use crate::clean;

/// A rectangle of cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left.
    pub x: u32,
    /// Top.
    pub y: u32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

impl Rect {
    /// True if the cell is inside.
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// A node and where it goes, in tree order (parents before children).
#[derive(Clone, Copy, Debug)]
pub struct Placed<'a> {
    /// The node.
    pub node: &'a Node,
    /// Its rectangle.
    pub rect: Rect,
}

/// Lays `root` out in `area`.
pub fn layout(root: &Node, area: Rect) -> Vec<Placed<'_>> {
    let mut out = Vec::new();
    place(root, area, &mut out);
    out
}

fn horizontal(n: &Node) -> bool {
    n.kind == NodeKind::Row
}

fn padding(n: &Node) -> (u32, u32) {
    n.padding.map_or((0, 0), |[v, h]| (v, h))
}

/// The content size of a node: (width, height).
fn content(n: &Node) -> (u32, u32) {
    match n.kind {
        NodeKind::Text => (n.text.as_deref().map_or(0, clean::width), 1),
        NodeKind::Icon => (1, 1),
        NodeKind::Spacer | NodeKind::Surface | NodeKind::List => (0, 0),
        NodeKind::Row | NodeKind::Column | NodeKind::Card | NodeKind::Unknown => {
            let (pv, ph) = padding(n);
            let sizes: Vec<(u32, u32)> = n.children.iter().map(fixed_or_content).collect();
            // Saturating: sizes come from the server and may be anything.
            let gaps = n
                .gap
                .saturating_mul(u32::try_from(sizes.len().saturating_sub(1)).unwrap_or(0));
            let sum = |it: &mut dyn Iterator<Item = u32>| it.fold(0u32, u32::saturating_add);
            let (w, h) = if horizontal(n) {
                (
                    sum(&mut sizes.iter().map(|s| s.0)).saturating_add(gaps),
                    sizes.iter().map(|s| s.1).max().unwrap_or(0),
                )
            } else {
                (
                    sizes.iter().map(|s| s.0).max().unwrap_or(0),
                    sum(&mut sizes.iter().map(|s| s.1)).saturating_add(gaps),
                )
            };
            (
                w.saturating_add(ph.saturating_mul(2)),
                h.saturating_add(pv.saturating_mul(2)),
            )
        }
    }
}

fn fixed_or_content(n: &Node) -> (u32, u32) {
    let (cw, ch) = content(n);
    let pick = |d: Dim, c: u32| match d {
        Dim::Units { n } => n,
        _ => c,
    };
    (pick(n.width, cw), pick(n.height, ch))
}

/// Sizes along the main axis: fixed and fit sizes first, in order, cut to the
/// budget; fills share what is left by weight, the remainder one cell at a time
/// from the first fill.
fn main_sizes(children: &[Node], row: bool, mut budget: u32) -> Vec<u32> {
    let dim = |c: &Node| if row { c.width } else { c.height };
    let mut sizes: Vec<Option<u32>> = children
        .iter()
        .map(|c| {
            let want = match dim(c) {
                Dim::Units { n } => n,
                Dim::Fit => {
                    let (w, h) = content(c);
                    if row { w } else { h }
                }
                Dim::Fill { .. } | Dim::Unknown => return None,
            };
            let s = want.min(budget);
            budget -= s;
            Some(s)
        })
        .collect();
    let weight = |c: &Node| match dim(c) {
        Dim::Fill { weight } => weight.max(1),
        _ => 1,
    };
    let total: u64 = children
        .iter()
        .zip(&sizes)
        .filter(|(_, s)| s.is_none())
        .map(|(c, _)| u64::from(weight(c)))
        .sum();
    let mut left = budget;
    for (c, s) in children.iter().zip(sizes.iter_mut()) {
        if s.is_none() {
            let share =
                u32::try_from(u64::from(budget) * u64::from(weight(c)) / total.max(1)).unwrap_or(0);
            *s = Some(share);
            left -= share;
        }
    }
    let fills = children
        .iter()
        .zip(sizes.iter_mut())
        .filter(|(c, _)| matches!(dim(c), Dim::Fill { .. } | Dim::Unknown));
    for (_, s) in fills {
        if left == 0 {
            break;
        }
        *s = s.map(|v| v + 1);
        left -= 1;
    }
    sizes.into_iter().map(|s| s.unwrap_or(0)).collect()
}

fn place<'a>(n: &'a Node, r: Rect, out: &mut Vec<Placed<'a>>) {
    out.push(Placed { node: n, rect: r });
    if n.children.is_empty() {
        return;
    }
    let (pv, ph) = padding(n);
    let inner = Rect {
        x: r.x + ph.min(r.w),
        y: r.y + pv.min(r.h),
        w: r.w.saturating_sub(ph.saturating_mul(2)),
        h: r.h.saturating_sub(pv.saturating_mul(2)),
    };
    let row = horizontal(n);
    let (main, cross) = if row {
        (inner.w, inner.h)
    } else {
        (inner.h, inner.w)
    };
    let count = u32::try_from(n.children.len()).unwrap_or(u32::MAX);
    let gaps = n.gap.saturating_mul(count - 1);
    let sizes = main_sizes(&n.children, row, main.saturating_sub(gaps));
    let mut at = if row { inner.x } else { inner.y };
    for (i, (c, size)) in n.children.iter().zip(sizes).enumerate() {
        let d = if row { c.height } else { c.width };
        let along = match d {
            Dim::Units { n } => n.min(cross),
            Dim::Fit => {
                let (w, h) = content(c);
                (if row { h } else { w }).min(cross)
            }
            Dim::Fill { .. } | Dim::Unknown => cross,
        };
        let rect = if row {
            Rect {
                x: at,
                y: inner.y,
                w: size,
                h: along,
            }
        } else {
            Rect {
                x: inner.x,
                y: at,
                w: along,
                h: size,
            }
        };
        place(c, rect, out);
        at = at.saturating_add(size);
        if i + 1 < n.children.len() {
            at = at.saturating_add(n.gap);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, kind: NodeKind, w: Dim, h: Dim, children: Vec<Node>) -> Node {
        Node {
            width: w,
            height: h,
            children,
            ..Node::new(id, kind)
        }
    }

    fn rect_of(p: &[Placed<'_>], id: &str) -> Rect {
        p.iter().find(|p| p.node.id == id).unwrap().rect
    }

    #[test]
    fn fixed_then_fill_by_weight() {
        let root = node(
            "r",
            NodeKind::Row,
            Dim::FILL,
            Dim::FILL,
            vec![
                node(
                    "a",
                    NodeKind::Spacer,
                    Dim::Units { n: 10 },
                    Dim::FILL,
                    vec![],
                ),
                node(
                    "b",
                    NodeKind::Spacer,
                    Dim::Fill { weight: 1 },
                    Dim::FILL,
                    vec![],
                ),
                node(
                    "c",
                    NodeKind::Spacer,
                    Dim::Fill { weight: 2 },
                    Dim::Units { n: 3 },
                    vec![],
                ),
            ],
        );
        let p = layout(
            &root,
            Rect {
                x: 0,
                y: 0,
                w: 41,
                h: 5,
            },
        );
        assert_eq!(
            rect_of(&p, "a"),
            Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 5
            }
        );
        // 31 left: 10 and 20, the remainder to the first fill.
        assert_eq!(
            rect_of(&p, "b"),
            Rect {
                x: 10,
                y: 0,
                w: 11,
                h: 5
            }
        );
        assert_eq!(
            rect_of(&p, "c"),
            Rect {
                x: 21,
                y: 0,
                w: 20,
                h: 3
            }
        );
    }

    #[test]
    fn padding_gap_and_fit_text() {
        let mut t = node("t", NodeKind::Text, Dim::Fit, Dim::Fit, vec![]);
        t.text = Some("hello".into());
        let mut col = node(
            "c",
            NodeKind::Column,
            Dim::FILL,
            Dim::FILL,
            vec![
                t,
                node("s", NodeKind::Surface, Dim::FILL, Dim::FILL, vec![]),
            ],
        );
        col.padding = Some([1, 2]);
        col.gap = 1;
        let p = layout(
            &col,
            Rect {
                x: 0,
                y: 0,
                w: 20,
                h: 10,
            },
        );
        assert_eq!(
            rect_of(&p, "t"),
            Rect {
                x: 2,
                y: 1,
                w: 5,
                h: 1
            }
        );
        assert_eq!(
            rect_of(&p, "s"),
            Rect {
                x: 2,
                y: 3,
                w: 16,
                h: 6
            }
        );
    }

    #[test]
    fn fixed_sizes_that_do_not_fit_cut_later_children() {
        let root = node(
            "r",
            NodeKind::Row,
            Dim::FILL,
            Dim::FILL,
            vec![
                node(
                    "a",
                    NodeKind::Spacer,
                    Dim::Units { n: 8 },
                    Dim::FILL,
                    vec![],
                ),
                node("f", NodeKind::Spacer, Dim::FILL, Dim::FILL, vec![]),
                node(
                    "b",
                    NodeKind::Spacer,
                    Dim::Units { n: 8 },
                    Dim::FILL,
                    vec![],
                ),
            ],
        );
        let p = layout(
            &root,
            Rect {
                x: 0,
                y: 0,
                w: 12,
                h: 1,
            },
        );
        assert_eq!(rect_of(&p, "a").w, 8);
        assert_eq!(rect_of(&p, "f").w, 0);
        assert_eq!(rect_of(&p, "b").w, 4);
    }
}
