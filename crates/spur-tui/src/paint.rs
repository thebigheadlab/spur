//! Drawing what the server sent into a grid: node styles, texts, icons, lists and
//! code surfaces.

use std::collections::BTreeMap;

use spur_rpc::{
    CursorKind, Dim, ItemKind, ListView, Node, NodeKind, Overlay, SurfaceSize, SurfaceView, tokens,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::clean::{Piece, cell};
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
    /// Trees drawn on top of `root`.
    pub overlays: Vec<Overlay>,
    /// Latest text per input node.
    pub inputs: BTreeMap<String, InputText>,
}

/// An input's text, cursor and selection (grapheme indices).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputText {
    /// Text.
    pub text: String,
    /// Cursor.
    pub cursor: u32,
    /// Other end of the selection.
    pub anchor: Option<u32>,
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

/// What was drawn under a hit, so a press resolves against what the person saw,
/// not against messages that arrived after the draw.
#[derive(Clone, Debug)]
enum HitKind {
    Surface {
        text_x: u32,
        top_line: u32,
        hscroll: u32,
        tab: u32,
        lines: Vec<String>,
    },
    List {
        items: Vec<String>,
    },
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
    pub fn target(&self, x: u32, y: u32) -> Option<Target> {
        let hit = self.hits.iter().rev().find(|h| h.rect.contains(x, y))?;
        let row = (y - hit.rect.y) as usize;
        Some(match &hit.kind {
            HitKind::Click => Target::Click(hit.node.clone()),
            HitKind::List { items } => Target::List {
                node: hit.node.clone(),
                item: items.get(row).cloned(),
            },
            HitKind::Surface {
                text_x,
                top_line,
                hscroll,
                tab,
                lines,
            } => {
                let dc = hscroll + x.saturating_sub(*text_x);
                let col = lines.get(row).map_or(0, |l| col_at(l, dc, *tab));
                Target::Surface {
                    node: hit.node.clone(),
                    line: top_line + u32::try_from(row).unwrap_or(u32::MAX),
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
            .filter(|h| matches!(h.kind, HitKind::Surface { .. } | HitKind::List { .. }))
            .find(|h| h.rect.contains(x, y))
            .map(|h| h.node.as_str())
    }
}

/// A list's hit, with the item ids drawn in its rows.
fn list_hit(screen: &Screen, node: &str, r: Rect) -> Hit {
    let items = screen.lists.get(node).map_or_else(Vec::new, |l| {
        l.items
            .iter()
            .take(r.h as usize)
            .map(|i| i.id.clone())
            .collect()
    });
    Hit {
        rect: r,
        node: node.to_owned(),
        kind: HitKind::List { items },
    }
}

/// A surface's hit, with the lines drawn in its rows.
fn surface_hit(screen: &Screen, node: &str, r: Rect, text_x: u32, tab: u32) -> Hit {
    let (top_line, hscroll, lines) = screen.surfaces.get(node).map_or((0, 0, Vec::new()), |s| {
        let lines = s
            .view
            .lines
            .iter()
            .take(r.h as usize)
            .map(line_text)
            .collect();
        (s.view.top_line, s.hscroll, lines)
    });
    Hit {
        rect: r,
        node: node.to_owned(),
        kind: HitKind::Surface {
            text_x,
            top_line,
            hscroll,
            tab,
            lines,
        },
    }
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
        match cell(c) {
            Some(p) => {
                let w = p.width;
                cells.push((dc, p));
                dc += w;
            }
            None => {
                if let Some((_, p)) = cells.last_mut() {
                    p.text.push(c);
                }
            }
        }
    }
    starts.push(dc);
    (starts, cells)
}

/// Draws the screen in a `w` x `h` grid: the tree, then each overlay over the
/// dimmed tree.
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
    let focus = screen.focus.clone().unwrap_or_default();
    paint_tree(&mut frame, screen, &root, Rect { x: 0, y: 0, w, h }, &focus);
    screen.root = Some(root);
    let overlays = std::mem::take(&mut screen.overlays);
    for o in &overlays {
        let target = screen.palette.get(tokens::OVERLAY_DIM).and_then(|s| s.bg);
        frame.grid.dim(o.dim, target);
        // Presses on the dimmed tree reach the server, which closes the overlay.
        let r = overlay_rect(o, screen, w, h);
        paint_tree(&mut frame, screen, &o.node, r, &focus);
    }
    screen.overlays = overlays;
    frame
}

/// Where an overlay goes: centred, `top` down, its fit height (lists count their
/// rows) capped at `max_height`; kept 2 cells from each side of the window.
fn overlay_rect(o: &Overlay, screen: &Screen, w: u32, h: u32) -> Rect {
    let width = o.width.min(w.saturating_sub(4)).max(1.min(w));
    let height = fit_h(&o.node, screen)
        .min(o.max_height)
        .min(h.saturating_sub(4))
        .max(1.min(h));
    let top = o.top.min(h.saturating_sub(2).saturating_sub(height));
    Rect {
        x: (w - width) / 2,
        y: top,
        w: width,
        h: height,
    }
}

/// The height a node wants, counting a list's rows.
fn fit_h(n: &Node, screen: &Screen) -> u32 {
    if let Dim::Units { n } = n.height {
        return n;
    }
    let (v, _) = n.padding.map_or((0, 0), |[v, h]| (v, h));
    let pad = v
        .saturating_add(u32::from(n.border.is_some()))
        .saturating_mul(2);
    match n.kind {
        NodeKind::Text | NodeKind::Icon | NodeKind::Input => 1,
        NodeKind::List => screen.lists.get(&n.id).map_or(0, |l| l.total),
        NodeKind::Spacer | NodeKind::Surface => 0,
        NodeKind::Row => n
            .children
            .iter()
            .map(|c| fit_h(c, screen))
            .max()
            .unwrap_or(0)
            .saturating_add(pad),
        NodeKind::Column | NodeKind::Card | NodeKind::Unknown => {
            let gaps = n
                .gap
                .saturating_mul(u32::try_from(n.children.len().saturating_sub(1)).unwrap_or(0));
            n.children
                .iter()
                .map(|c| fit_h(c, screen))
                .fold(0u32, u32::saturating_add)
                .saturating_add(gaps)
                .saturating_add(pad)
        }
    }
}

/// Lays out and draws one tree in `area`.
fn paint_tree(frame: &mut Frame, screen: &mut Screen, root: &Node, area: Rect, focus: &str) {
    let placed = layout(root, area);
    // Nodes inside the focused node, by id.
    let mut focused_ids = std::collections::HashSet::new();
    collect_focused(root, focus, false, &mut focused_ids);
    for p in &placed {
        let n = p.node;
        let r = p.rect;
        if let Some(s) = n.style.as_deref().and_then(|s| screen.palette.get(s)) {
            frame.grid.paint(r.x, r.y, r.w, r.h, s);
        }
        paint_border(&mut frame.grid, r, n, &screen.palette, screen.ascii);
        match n.kind {
            NodeKind::Text => paint_text_node(&mut frame.grid, r, n),
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
            NodeKind::Input => {
                let cursor = paint_input(
                    &mut frame.grid,
                    r,
                    n,
                    screen.inputs.get(&n.id),
                    &screen.palette,
                );
                if n.id == focus {
                    frame.cursor = cursor;
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
                frame.hits.push(list_hit(screen, &n.id, r));
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
                frame.hits.push(surface_hit(screen, &n.id, r, text_x, tab));
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
}

/// Draws an input: its text scrolled sideways to keep the cursor in view (or the
/// placeholder while empty), and the selection. Returns the cursor's cell.
fn paint_input(
    g: &mut Grid,
    r: Rect,
    n: &Node,
    t: Option<&InputText>,
    pal: &Palette,
) -> Option<(u32, u32)> {
    if r.w == 0 || r.h == 0 {
        return None;
    }
    let t = t.cloned().unwrap_or_default();
    if t.text.is_empty() {
        if let Some(p) = &n.text {
            let s = n.placeholder_style.as_deref().and_then(|s| pal.get(s));
            g.text(r.x, r.y, r.x + r.w, p, fg_only(s));
        }
        return Some((r.x, r.y));
    }
    let gs: Vec<&str> = t.text.graphemes(true).collect();
    let widths: Vec<u32> = gs.iter().map(|s| crate::clean::width(s)).collect();
    let cursor = (t.cursor as usize).min(gs.len());
    // First grapheme shown: enough that the cursor's cell fits.
    let mut first = 0;
    let room = r.w.saturating_sub(1);
    while first < cursor && widths[first..cursor].iter().sum::<u32>() > room {
        first += 1;
    }
    let sel = t
        .anchor
        .map(|a| (a.min(t.cursor) as usize, a.max(t.cursor) as usize));
    let sel_style = pal.get(tokens::SELECTION);
    let mut x = r.x;
    let mut cx = r.x;
    for (i, s) in gs.iter().enumerate().skip(first) {
        if i == cursor {
            cx = x;
        }
        if x >= r.x + r.w {
            break;
        }
        let style = match sel {
            Some((a, b)) if i >= a && i < b => sel_style,
            _ => None,
        };
        x = g.text(x, r.y, r.x + r.w, s, style);
    }
    if cursor >= gs.len() {
        cx = x;
    }
    Some((cx.min(r.x + r.w - 1), r.y))
}

/// Draws a text node, repeated to fill its width if asked; nothing when it has
/// no room (a zero-height row must not land on the row below).
fn paint_text_node(g: &mut Grid, r: Rect, n: &Node) {
    let Some(t) = &n.text else {
        return;
    };
    if r.w == 0 || r.h == 0 {
        return;
    }
    if n.repeat {
        let one = crate::clean::width(t).max(1);
        let copies = r.w.div_ceil(one) as usize;
        g.text(r.x, r.y, r.x + r.w, &t.repeat(copies), None);
    } else {
        g.text(r.x, r.y, r.x + r.w, t, None);
    }
}

/// The border's six glyphs, if they are six one-cell pieces.
fn border_glyphs(n: &Node, ascii: bool) -> Option<Vec<Piece>> {
    let g = if ascii {
        n.border_alt.as_ref().or(n.border.as_ref())
    } else {
        n.border.as_ref()
    }?;
    let pieces: Vec<Piece> = g.chars().map(cell).collect::<Option<_>>()?;
    (pieces.len() == 6 && pieces.iter().all(|p| p.width == 1)).then_some(pieces)
}

/// Draws a node's border on the edge cells of `r`, over its style.
fn paint_border(g: &mut Grid, r: Rect, n: &Node, pal: &Palette, ascii: bool) {
    if r.w < 2 || r.h < 2 {
        return;
    }
    let Some(p) = border_glyphs(n, ascii) else {
        return;
    };
    let style = fg_only(n.border_style.as_deref().and_then(|s| pal.get(s)));
    let (right, bottom) = (r.x + r.w - 1, r.y + r.h - 1);
    let mut put = |x: u32, y: u32, i: usize| {
        g.pieces(x, y, x + 1, std::slice::from_ref(&p[i]), style);
    };
    put(r.x, r.y, 0);
    put(right, r.y, 1);
    put(r.x, bottom, 2);
    put(right, bottom, 3);
    for x in r.x + 1..right {
        put(x, r.y, 4);
        put(x, bottom, 4);
    }
    for y in r.y + 1..bottom {
        put(r.x, y, 5);
        put(right, y, 5);
    }
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
        if item.current
            && let Some(s) = get(tokens::LIST_ROW_CURRENT)
        {
            g.paint(r.x, y, r.w, 1, s);
        }
        if l.selected == Some(index) && item.kind != ItemKind::Section {
            let name = if focused {
                tokens::LIST_ROW_SELECTED_FOCUSED
            } else {
                tokens::LIST_ROW_SELECTED
            };
            if let Some(s) = get(name) {
                g.paint(r.x, y, r.w, 1, s);
            }
        }
        let mut end = r.x + r.w;
        // The key hint sits at the right edge; the text stops a cell before it.
        if let Some(k) = item.key.as_deref().filter(|k| !k.is_empty()) {
            let kw = crate::clean::width(k);
            if kw + 2 < r.w {
                let kx = end - kw - 1;
                g.text(kx, y, end, k, fg_only(get(tokens::LIST_KEY)));
                end = kx.saturating_sub(1);
            }
        }
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
            ItemKind::Section => get(tokens::LIST_SECTION),
            _ => None,
        };
        x = matched(g, x, y, end, &item.text, &item.matches, fg_only(style), pal);
        if let Some(d) = item.detail.as_deref().filter(|d| !d.is_empty()) {
            x = g.text(x, y, end, " ", None);
            matched(
                g,
                x,
                y,
                end,
                d,
                &item.detail_matches,
                fg_only(get(tokens::LIST_DETAIL)),
                pal,
            );
        }
    }
}

/// Writes `text` with the graphemes in `ranges` in the match style. Returns the x
/// after it.
#[allow(clippy::too_many_arguments)]
fn matched(
    g: &mut Grid,
    x: u32,
    y: u32,
    end: u32,
    text: &str,
    ranges: &[[u32; 2]],
    style: Option<Style>,
    pal: &Palette,
) -> u32 {
    let after = g.text(x, y, end, text, style);
    if ranges.is_empty() {
        return after;
    }
    let m = fg_only(pal.get(tokens::LIST_MATCH));
    let mut cx = x;
    for (i, s) in text.graphemes(true).enumerate() {
        let w = crate::clean::width(s);
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        if cx >= end {
            break;
        }
        if let Some(st) = m
            && ranges.iter().any(|[a, b]| (*a..*b).contains(&i))
        {
            g.paint(cx, y, w.min(end - cx), 1, st);
        }
        cx += w;
    }
    after
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
type LineCells<'a> = (Vec<u32>, Vec<(u32, Piece)>, Vec<Option<&'a str>>);

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
    let lines: Vec<LineCells<'_>> = v
        .lines
        .iter()
        .take(r.h as usize)
        .map(|l| {
            let text = line_text(l);
            let (starts, cells) = columns(&text, tab);
            // The highlight name of each char.
            let hl: Vec<Option<&str>> = l
                .spans
                .iter()
                .flat_map(|sp| sp.text.chars().map(|_| sp.hl.as_deref()))
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
    hl: &[Option<&str>],
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
        let st = hl.get(ci).copied().flatten().and_then(|h| pal.get(h));
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

    fn text(id: &str, t: &str) -> Node {
        Node {
            text: Some(t.into()),
            width: spur_rpc::Dim::Fit,
            height: spur_rpc::Dim::Fit,
            ..Node::new(id, NodeKind::Text)
        }
    }

    fn draw(root: Node, w: u32, h: u32, ascii: bool) -> Vec<String> {
        let mut screen = Screen {
            root: Some(root),
            ascii,
            ..Screen::default()
        };
        paint(&mut screen, w, h).grid.lines()
    }

    fn boxed(children: Vec<Node>) -> Node {
        Node {
            border: Some("\u{256d}\u{256e}\u{2570}\u{256f}\u{2500}\u{2502}".into()),
            border_alt: Some("++++-|".into()),
            children,
            ..Node::new("card", NodeKind::Card)
        }
    }

    #[test]
    fn a_border_draws_around_the_node_and_insets_its_children() {
        let lines = draw(boxed(vec![text("t", "hi")]), 6, 3, false);
        assert_eq!(
            lines,
            [
                "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}",
                "\u{2502}hi  \u{2502}",
                "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}"
            ]
        );
        assert_eq!(
            draw(boxed(vec![text("t", "hi")]), 5, 3, true),
            ["+---+", "|hi |", "+---+"]
        );
    }

    #[test]
    fn a_bad_border_draws_nothing() {
        let mut n = boxed(vec![]);
        n.border = Some("+-".into());
        n.border_alt = None;
        assert_eq!(draw(n, 4, 2, false), ["", ""]);
        let mut wide = boxed(vec![]);
        wide.border = Some("\u{6771}\u{6771}\u{6771}\u{6771}\u{6771}\u{6771}".into());
        assert_eq!(draw(wide, 4, 2, false), ["", ""]);
    }

    #[test]
    fn repeat_fills_the_width() {
        let rule = Node {
            repeat: true,
            width: spur_rpc::Dim::FILL,
            ..text("rule", "-=")
        };
        let root = Node {
            children: vec![rule],
            ..Node::new("root", NodeKind::Column)
        };
        assert_eq!(draw(root, 5, 1, false)[0], "-=-=-");
    }

    #[test]
    fn the_current_row_takes_its_token() {
        let mut tokens = std::collections::BTreeMap::new();
        tokens.insert(
            tokens::LIST_ROW_CURRENT.to_owned(),
            spur_rpc::Token {
                bg: Some("#ff0000".into()),
                ..spur_rpc::Token::default()
            },
        );
        let list = Node::new("l", NodeKind::List);
        let mut screen = Screen {
            root: Some(list),
            palette: Palette::new(&tokens, true),
            ..Screen::default()
        };
        screen.lists.insert(
            "l".into(),
            ListView {
                items: vec![
                    spur_rpc::Item {
                        id: "a".into(),
                        text: "a".into(),
                        ..spur_rpc::Item::default()
                    },
                    spur_rpc::Item {
                        id: "b".into(),
                        text: "b".into(),
                        current: true,
                        ..spur_rpc::Item::default()
                    },
                ],
                ..ListView::default()
            },
        );
        let f = paint(&mut screen, 3, 2);
        assert_eq!(f.grid.cell(0, 0).bg, None);
        assert!(f.grid.cell(0, 1).bg.is_some());
    }

    #[test]
    fn text_with_no_height_draws_nothing() {
        let root = Node {
            children: vec![
                text("seen", "y"),
                Node {
                    height: spur_rpc::Dim::Units { n: 0 },
                    ..text("gone", "x")
                },
            ],
            ..Node::new("root", NodeKind::Column)
        };
        assert_eq!(draw(root, 2, 2, false), ["y", ""]);
    }
}
