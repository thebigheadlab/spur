//! Drawing what the server sent into a grid: node styles, texts, icons, lists and
//! code surfaces.

use std::collections::BTreeMap;

use spur_rpc::{CursorKind, ItemKind, ListView, Node, NodeKind, SurfaceSize, SurfaceView, tokens};
use unicode_width::UnicodeWidthChar;

use crate::clean::{Piece, is_unsafe};
use crate::color::{Palette, Style};
use crate::grid::Grid;
use crate::layout::{Rect, layout};

/// Columns kept between the primary cursor and the side of a surface.
const H_MARGIN: u32 = 4;

/// A surface as last received, and its horizontal scroll.
#[derive(Clone, Debug, Default)]
pub struct SurfaceState {
    /// The full view.
    pub view: SurfaceView,
    /// First display column shown.
    pub hscroll: u32,
}

/// Everything the client keeps from the server.
#[derive(Debug, Default)]
pub struct Screen {
    /// Styles of the current theme.
    pub palette: Palette,
    /// The tree.
    pub root: Option<Node>,
    /// The focused node.
    pub focus: Option<String>,
    /// Latest window per list node.
    pub lists: BTreeMap<String, ListView>,
    /// Latest view per surface node.
    pub surfaces: BTreeMap<String, SurfaceState>,
    /// Draw icons with their ASCII fallback.
    pub ascii: bool,
}

/// What a press at a cell lands on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A document position on a surface.
    Surface {
        /// Node.
        node: String,
        /// Line.
        line: u32,
        /// Char column.
        col: u32,
    },
    /// A list, and the item under the pointer if any.
    List {
        /// Node.
        node: String,
        /// Item id.
        item: Option<String>,
    },
    /// A clickable node.
    Click(String),
}

#[derive(Clone, Debug)]
struct Hit {
    rect: Rect,
    node: String,
    kind: HitKind,
}

#[derive(Clone, Debug)]
enum HitKind {
    Surface { text_x: u32 },
    List,
    Click,
}

/// One drawn frame.
#[derive(Debug)]
pub struct Frame {
    /// The cells.
    pub grid: Grid,
    /// Where the terminal cursor goes, if shown.
    pub cursor: Option<(u32, u32)>,
    /// Sizes of the surface and list nodes, for `layout`.
    pub sizes: Vec<SurfaceSize>,
    hits: Vec<Hit>,
}

impl Frame {
    /// The deepest surface, list or clickable node under `(x, y)`.
    pub fn target(&self, screen: &Screen, x: u32, y: u32) -> Option<Target> {
        let hit = self.hits.iter().rev().find(|h| h.rect.contains(x, y))?;
        let row = y - hit.rect.y;
        Some(match hit.kind {
            HitKind::Click => Target::Click(hit.node.clone()),
            HitKind::List => {
                let item = screen
                    .lists
                    .get(&hit.node)
                    .and_then(|l| l.items.get(row as usize))
                    .map(|i| i.id.clone());
                Target::List {
                    node: hit.node.clone(),
                    item,
                }
            }
            HitKind::Surface { text_x } => {
                let s = screen.surfaces.get(&hit.node)?;
                let line = s.view.top_line + row;
                let dc = s.hscroll + x.saturating_sub(text_x);
                let tab = tab_width_of(screen, &hit.node);
                let col = s
                    .view
                    .lines
                    .get(row as usize)
                    .map_or(0, |l| col_at(&line_text(l), dc, tab));
                Target::Surface {
                    node: hit.node.clone(),
                    line,
                    col,
                }
            }
        })
    }

    /// The scroll target under `(x, y)`: a surface or list node.
    pub fn scroll_target(&self, x: u32, y: u32) -> Option<&str> {
        self.hits
            .iter()
            .rev()
            .filter(|h| matches!(h.kind, HitKind::Surface { .. } | HitKind::List))
            .find(|h| h.rect.contains(x, y))
            .map(|h| h.node.as_str())
    }
}

fn tab_width_of(screen: &Screen, node: &str) -> u32 {
    fn find<'a>(n: &'a Node, id: &str) -> Option<&'a Node> {
        if n.id == id {
            return Some(n);
        }
        n.children.iter().find_map(|c| find(c, id))
    }
    screen
        .root
        .as_ref()
        .and_then(|r| find(r, node))
        .map_or(4, |n| if n.tab_width == 0 { 4 } else { n.tab_width })
}

fn line_text(l: &spur_rpc::Line) -> String {
    l.spans.iter().map(|s| s.text.as_str()).collect()
}

/// The char column at display column `dc`.
fn col_at(text: &str, dc: u32, tab: u32) -> u32 {
    let (starts, _) = columns(text, tab);
    let n = starts.len().saturating_sub(1);
    let i = (0..n).find(|&i| starts[i + 1] > dc).unwrap_or(n);
    u32::try_from(i).unwrap_or(u32::MAX)
}

/// The display column where each char starts, plus the end; and the cells.
fn columns(text: &str, tab: u32) -> (Vec<u32>, Vec<(u32, Piece)>) {
    let tab = tab.max(1);
    let mut starts = Vec::with_capacity(text.len() + 1);
    let mut cells: Vec<(u32, Piece)> = Vec::new();
    let mut dc = 0;
    for c in text.chars() {
        starts.push(dc);
        if c == '\t' {
            let next = (dc / tab + 1) * tab;
            while dc < next {
                cells.push((
                    dc,
                    Piece {
                        text: " ".into(),
                        width: 1,
                    },
                ));
                dc += 1;
            }
            continue;
        }
        if is_unsafe(c) {
            cells.push((
                dc,
                Piece {
                    text: char::REPLACEMENT_CHARACTER.to_string(),
                    width: 1,
                },
            ));
            dc += 1;
            continue;
        }
        match c.width() {
            Some(0) | None => {
                if let Some((_, p)) = cells.last_mut() {
                    p.text.push(c);
                }
            }
            Some(w) => {
                let w = if w >= 2 { 2 } else { 1 };
                cells.push((
                    dc,
                    Piece {
                        text: c.to_string(),
                        width: w,
                    },
                ));
                dc += w;
            }
        }
    }
    starts.push(dc);
    (starts, cells)
}

/// Draws the screen in a `w` x `h` grid.
pub fn paint(screen: &mut Screen, w: u32, h: u32) -> Frame {
    let mut frame = Frame {
        grid: Grid::new(w, h),
        cursor: None,
        sizes: Vec::new(),
        hits: Vec::new(),
    };
    let Some(root) = screen.root.take() else {
        return frame;
    };
    let placed = layout(&root, Rect { x: 0, y: 0, w, h });
    let focus = screen.focus.clone().unwrap_or_default();
    // Nodes inside the focused node, by id.
    let mut focused_ids = std::collections::HashSet::new();
    collect_focused(&root, &focus, false, &mut focused_ids);
    for p in &placed {
        let n = p.node;
        let r = p.rect;
        if let Some(s) = n.style.as_deref().and_then(|s| screen.palette.get(s)) {
            frame.grid.paint(r.x, r.y, r.w, r.h, s);
        }
        match n.kind {
            NodeKind::Text => {
                if let Some(t) = &n.text {
                    frame.grid.text(r.x, r.y, r.x + r.w, t, None);
                }
            }
            NodeKind::Icon => {
                let t = if screen.ascii {
                    n.alt.as_ref().or(n.text.as_ref())
                } else {
                    n.text.as_ref()
                };
                if let Some(t) = t
                    && r.w > 0
                    && r.h > 0
                {
                    frame
                        .grid
                        .text(r.x + (r.w - 1) / 2, r.y + (r.h - 1) / 2, r.x + r.w, t, None);
                }
            }
            NodeKind::List => {
                let focused = focused_ids.contains(n.id.as_str());
                if let Some(l) = screen.lists.get(&n.id) {
                    paint_list(
                        &mut frame.grid,
                        r,
                        l,
                        &screen.palette,
                        focused,
                        screen.ascii,
                    );
                }
                frame.sizes.push(SurfaceSize {
                    node: n.id.clone(),
                    rows: r.h,
                    cols: r.w,
                });
                frame.hits.push(Hit {
                    rect: r,
                    node: n.id.clone(),
                    kind: HitKind::List,
                });
            }
            NodeKind::Surface => {
                let focused = n.id == focus;
                let tab = if n.tab_width == 0 { 4 } else { n.tab_width };
                let (text_x, text_w, cursor) = if let Some(s) = screen.surfaces.get_mut(&n.id) {
                    paint_surface(&mut frame.grid, r, n, s, &screen.palette, tab)
                } else {
                    let gw = gutter_width(n, 1, r.w);
                    (r.x + gw, r.w - gw, None)
                };
                if focused {
                    frame.cursor = cursor;
                }
                frame.sizes.push(SurfaceSize {
                    node: n.id.clone(),
                    rows: r.h,
                    cols: text_w,
                });
                frame.hits.push(Hit {
                    rect: r,
                    node: n.id.clone(),
                    kind: HitKind::Surface { text_x },
                });
            }
            _ => {}
        }
        if n.clickable && !matches!(n.kind, NodeKind::List | NodeKind::Surface) {
            frame.hits.push(Hit {
                rect: r,
                node: n.id.clone(),
                kind: HitKind::Click,
            });
        }
    }
    drop(placed);
    screen.root = Some(root);
    frame
}

fn collect_focused<'a>(
    n: &'a Node,
    focus: &str,
    inside: bool,
    out: &mut std::collections::HashSet<&'a str>,
) {
    let inside = inside || n.id == focus;
    if inside {
        out.insert(n.id.as_str());
    }
    for c in &n.children {
        collect_focused(c, focus, inside, out);
    }
}

fn paint_list(g: &mut Grid, r: Rect, l: &ListView, pal: &Palette, focused: bool, ascii: bool) {
    let get = |name: &str| pal.get(name);
    for (i, item) in l.items.iter().enumerate().take(r.h as usize) {
        let y = r.y + u32::try_from(i).unwrap_or(u32::MAX);
        let index = l.offset + u32::try_from(i).unwrap_or(u32::MAX);
        if let Some(s) = get(tokens::LIST_ROW) {
            g.paint(r.x, y, r.w, 1, s);
        }
        if l.selected == Some(index) {
            let name = if focused {
                tokens::LIST_ROW_SELECTED_FOCUSED
            } else {
                tokens::LIST_ROW_SELECTED
            };
            if let Some(s) = get(name) {
                g.paint(r.x, y, r.w, 1, s);
            }
        }
        let end = r.x + r.w;
        let mut x = r.x + (2 * item.depth).min(r.w);
        let glyph = if ascii {
            item.alt.as_ref().or(item.glyph.as_ref())
        } else {
            item.glyph.as_ref()
        };
        if let Some(gl) = glyph {
            x = g.text(x, y, end, gl, fg_only(get(tokens::LIST_ICON)));
            x = g.text(x, y, end, " ", None);
        }
        let style = match item.kind {
            ItemKind::Dir => get(tokens::LIST_DIR),
            ItemKind::More => get(tokens::LIST_MORE),
            ItemKind::Error => get(tokens::LIST_ERROR),
            _ => None,
        };
        g.text(x, y, end, &item.text, fg_only(style));
    }
}

/// A style's text colour and attributes, keeping the row's background.
fn fg_only(s: Option<Style>) -> Option<Style> {
    s.map(|s| Style { bg: None, ..s })
}

fn gutter_width(n: &Node, line_count: u32, w: u32) -> u32 {
    if !n.gutter {
        return 0;
    }
    let digits = line_count.max(1).ilog10() + 1;
    (digits.max(3) + 1).min(w)
}

/// A surface line: char start columns, cells, and each char's highlight name.
type LineCells = (Vec<u32>, Vec<(u32, Piece)>, Vec<Option<String>>);

/// Draws a surface; returns the text area's x and width, and the primary cursor.
fn paint_surface(
    g: &mut Grid,
    r: Rect,
    n: &Node,
    s: &mut SurfaceState,
    pal: &Palette,
    tab: u32,
) -> (u32, u32, Option<(u32, u32)>) {
    let v = &s.view;
    let gw = gutter_width(n, v.line_count, r.w);
    let text_x = r.x + gw;
    let tw = r.w - gw;
    let primary = v.cursors.iter().find(|c| c.kind == CursorKind::Primary);
    let row_of = |line: u32| line.checked_sub(v.top_line).filter(|row| *row < r.h);
    let lines: Vec<LineCells> = v
        .lines
        .iter()
        .take(r.h as usize)
        .map(|l| {
            let text = line_text(l);
            let (starts, cells) = columns(&text, tab);
            // The highlight name of each char.
            let hl: Vec<Option<String>> = l
                .spans
                .iter()
                .flat_map(|sp| sp.text.chars().map(|_| sp.hl.clone()))
                .collect();
            (starts, cells, hl)
        })
        .collect();
    // Keep the primary cursor in view.
    let mut cursor_dc = None;
    if let Some(p) = primary
        && let Some(row) = row_of(p.line)
    {
        let dc = lines.get(row as usize).map_or(0, |(st, _, _)| {
            let i = (p.col as usize).min(st.len().saturating_sub(1));
            st[i] + u32::try_from((p.col as usize).saturating_sub(i)).unwrap_or(0)
        });
        let margin = H_MARGIN.min(tw / 4);
        if dc < s.hscroll + margin {
            s.hscroll = dc.saturating_sub(margin);
        } else if tw > 0 && dc + margin >= s.hscroll + tw {
            s.hscroll = dc + margin + 1 - tw;
        }
        cursor_dc = Some((row, dc));
    }
    let hs = s.hscroll;
    let v = &s.view;
    let style = |name: &str| pal.get(name);
    for row in 0..r.h {
        let y = r.y + row;
        let Some((starts, cells, hl)) = lines.get(row as usize) else {
            break;
        };
        let line = v.top_line + row;
        let current = primary.is_some_and(|p| p.line == line);
        if gw > 0 {
            let num = format!("{:>w$} ", line + 1, w = (gw - 1) as usize);
            let name = if current {
                tokens::GUTTER_CURRENT
            } else {
                tokens::GUTTER
            };
            g.text(r.x, y, text_x, &num, style(name));
        }
        if current
            && n.current_line
            && let Some(st) = style(tokens::LINE_CURRENT)
        {
            g.paint(text_x, y, tw, 1, st);
        }
        let area = Row { text_x, tw, hs, y };
        paint_text(g, area, starts, cells, hl, pal);
        paint_marks(g, area, line, starts, v, pal);
    }
    let cursor = cursor_dc
        .and_then(|(row, dc)| (dc >= hs && dc < hs + tw).then(|| (text_x + dc - hs, r.y + row)));
    (text_x, tw, cursor)
}

/// Where one surface row is drawn.
#[derive(Clone, Copy)]
struct Row {
    text_x: u32,
    tw: u32,
    hs: u32,
    y: u32,
}

impl Row {
    /// Paints display columns `[x0, x1)`.
    fn span(self, g: &mut Grid, x0: u32, x1: u32, s: Option<Style>) {
        let Some(s) = s else {
            return;
        };
        let a = x0.max(self.hs);
        let b = x1.min(self.hs + self.tw);
        if b > a {
            g.paint(
                self.text_x + a - self.hs,
                self.y,
                b - a,
                1,
                Style { attrs: 0, ..s },
            );
        }
    }
}

/// A line's text, each char in its highlight colour.
fn paint_text(
    g: &mut Grid,
    at: Row,
    starts: &[u32],
    cells: &[(u32, Piece)],
    hl: &[Option<String>],
    pal: &Palette,
) {
    let mut ci = 0;
    for (dc, piece) in cells {
        while ci + 1 < starts.len() && starts[ci + 1] <= *dc {
            ci += 1;
        }
        if *dc < at.hs || *dc + piece.width > at.hs + at.tw {
            continue;
        }
        let st = hl.get(ci).cloned().flatten().and_then(|h| pal.get(&h));
        g.pieces(
            at.text_x + dc - at.hs,
            at.y,
            at.text_x + at.tw,
            std::slice::from_ref(piece),
            st.map(|s| Style { bg: None, ..s }),
        );
    }
}

/// Selections and the other cursors on one line; the line end counts as a cell.
fn paint_marks(g: &mut Grid, at: Row, line: u32, starts: &[u32], v: &SurfaceView, pal: &Palette) {
    let last = starts.len() - 1;
    let cell_of = |col: u32| {
        let i = (col as usize).min(last);
        starts[i] + u32::try_from((col as usize) - i).unwrap_or(0)
    };
    let len = u32::try_from(last).unwrap_or(0);
    for sel in &v.selections {
        if sel.start.line > line || sel.end.line < line {
            continue;
        }
        let a = if sel.start.line == line {
            sel.start.col
        } else {
            0
        };
        let b = if sel.end.line == line {
            sel.end.col
        } else {
            len + 1
        };
        at.span(g, cell_of(a), cell_of(b.max(a)), pal.get(tokens::SELECTION));
    }
    for c in v.cursors.iter().filter(|c| c.line == line) {
        let name = match c.kind {
            CursorKind::Secondary => tokens::CURSOR_SECONDARY,
            CursorKind::Peer => tokens::CURSOR_PEER,
            _ => continue,
        };
        let x0 = cell_of(c.col);
        at.span(g, x0, x0 + 1, pal.get(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_expand_tabs_and_count_wide_chars() {
        let (starts, _) = columns("a\tb\u{6771}c", 4);
        assert_eq!(starts, [0, 1, 4, 5, 7, 8]);
        assert_eq!(col_at("a\tb", 2, 4), 1, "inside the tab");
        assert_eq!(col_at("a\tb", 4, 4), 2);
        assert_eq!(col_at("ab", 9, 4), 2, "past the end");
    }
}
