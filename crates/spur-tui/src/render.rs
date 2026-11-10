//! Lays out and draws a UI tree and a surface frame into a [`Grid`]. Nothing here knows
//! what the editor looks like: sizes, text and token names all come from the server.

// Geometry reads best with x, y, w and h.
#![allow(clippy::many_single_char_names, clippy::similar_names)]

use std::collections::{BTreeMap, HashMap};

use spur_rpc::{
    Align, Anchor, Color, CursorKind, Dim, Node, NodeKind, Pos, RowKind, Selected, Span, Style,
    SurfaceFrame, Ui,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::grid::Grid;

/// Columns a tab advances to.
const TAB: usize = 4;

/// A rectangle in cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left.
    pub x: u16,
    /// Top.
    pub y: u16,
    /// Width.
    pub w: u16,
    /// Height.
    pub h: u16,
}

impl Rect {
    fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn right(self) -> u16 {
        self.x + self.w
    }
}

/// What a click or scroll at a place means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The text area of the editor surface.
    Surface,
    /// A list; `rows[i]` is the row index drawn on line `i` of the rectangle.
    List {
        /// Row index per screen line.
        rows: Vec<Option<u32>>,
    },
    /// Something that swallows the click (a float's background).
    Nothing,
}

/// A clickable area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Node id.
    pub node: String,
    /// Where it is.
    pub rect: Rect,
    /// What it is.
    pub target: Target,
}

/// A drawn frame.
#[derive(Clone, Debug)]
pub struct Frame {
    /// The cells.
    pub grid: Grid,
    /// Where the terminal cursor goes, if it shows.
    pub cursor: Option<(u16, u16)>,
    /// Clickable areas; later entries are on top.
    pub hits: Vec<Hit>,
    /// Rows and columns of the surface's text area, to report to the server.
    pub surface_size: Option<(u16, u16)>,
    /// Columns the surface is scrolled right.
    pub hscroll: usize,
}

/// State the frontend keeps between frames: how far each list is scrolled.
#[derive(Clone, Debug, Default)]
pub struct ViewState {
    scroll: HashMap<String, usize>,
}

struct Painter<'a> {
    grid: Grid,
    theme: &'a BTreeMap<String, Style>,
    surface: &'a SurfaceFrame,
    focus: &'a Selected,
    state: &'a mut ViewState,
    cursor: Option<(u16, u16)>,
    hits: Vec<Hit>,
    surface_size: Option<(u16, u16)>,
    hscroll: usize,
}

fn width(s: &str) -> u16 {
    u16::try_from(s.width()).unwrap_or(u16::MAX)
}

fn spans_width(spans: &[Span]) -> u16 {
    spans
        .iter()
        .map(|s| width(&s.text))
        .fold(0, u16::saturating_add)
}

/// Display column of char column `col` in `line` (tabs expand).
pub fn display_col(line: &str, col: usize) -> usize {
    let mut x = 0;
    for ch in line.chars().take(col) {
        x += if ch == '\t' {
            TAB - x % TAB
        } else {
            ch.width().unwrap_or(0)
        };
    }
    x
}

/// Char column of display column `x` in `line`; the inverse of [`display_col`].
pub fn char_col(line: &str, x: usize) -> usize {
    let mut at = 0;
    for (i, ch) in line.chars().enumerate() {
        let w = if ch == '\t' {
            TAB - at % TAB
        } else {
            ch.width().unwrap_or(0)
        };
        if at + w > x {
            return i;
        }
        at += w;
    }
    line.chars().count()
}

/// Text of the surface line drawn on screen row `row`.
fn line_text(f: &SurfaceFrame, row: usize) -> String {
    f.lines
        .get(row)
        .map(|l| l.iter().map(|s| &*s.text).collect())
        .unwrap_or_default()
}

fn measure_w(n: &Node) -> u16 {
    let inner = match &n.kind {
        NodeKind::Text { spans, .. } => spans_width(spans),
        NodeKind::Row { children } => children.iter().map(want_w).fold(0, u16::saturating_add),
        NodeKind::Column { children } => children.iter().map(want_w).max().unwrap_or(0),
        _ => 0,
    };
    inner.saturating_add(n.pad.left + n.pad.right)
}

fn measure_h(n: &Node) -> u16 {
    let inner = match &n.kind {
        NodeKind::Text { .. } | NodeKind::Input { .. } => 1,
        NodeKind::Column { children } => children.iter().map(want_h).fold(0, u16::saturating_add),
        NodeKind::Row { children } => children.iter().map(want_h).max().unwrap_or(0),
        NodeKind::List { rows, .. } => u16::try_from(rows.len()).unwrap_or(u16::MAX),
        _ => 0,
    };
    inner.saturating_add(n.pad.top + n.pad.bottom)
}

/// Height a float's content asks for: like [`measure_h`], but a list that fills its
/// parent counts its lines, so a card can shrink to fit what it shows.
fn natural_h(n: &Node) -> u16 {
    let own = |c: &Node| match (&c.kind, c.height) {
        (NodeKind::List { rows, .. }, Dim::Fill(_)) => {
            let gaps = rows
                .iter()
                .skip(1)
                .filter(|r| r.kind == RowKind::Header)
                .count();
            u16::try_from(rows.len() + gaps).unwrap_or(u16::MAX).max(1)
        }
        (NodeKind::Column { .. } | NodeKind::Row { .. }, Dim::Fill(_)) => natural_h(c),
        _ => want_h(c),
    };
    let inner = match &n.kind {
        NodeKind::Column { children } => children.iter().map(own).fold(0, u16::saturating_add),
        NodeKind::Row { children } => children.iter().map(own).max().unwrap_or(0),
        _ => return measure_h(n),
    };
    inner.saturating_add(n.pad.top + n.pad.bottom)
}

fn want_w(n: &Node) -> u16 {
    match n.width {
        Dim::Cells(c) => c,
        Dim::Fit => measure_w(n),
        Dim::Fill(_) => 0,
    }
}

fn want_h(n: &Node) -> u16 {
    match n.height {
        Dim::Cells(c) => c,
        Dim::Fit => measure_h(n),
        Dim::Fill(_) => 0,
    }
}

/// Splits `total` among children along one axis: fixed sizes first, then the rest by
/// fill weight.
fn split(total: u16, dims: &[(Dim, u16)]) -> Vec<u16> {
    let fixed: u16 = dims
        .iter()
        .map(|(d, want)| if matches!(d, Dim::Fill(_)) { 0 } else { *want })
        .fold(0, u16::saturating_add);
    let weights: u32 = dims
        .iter()
        .map(|(d, _)| {
            if let Dim::Fill(w) = d {
                u32::from(*w.max(&1))
            } else {
                0
            }
        })
        .sum();
    let spare = u32::from(total.saturating_sub(fixed));
    let mut left = total;
    let mut spare_left = spare;
    let mut weights_left = weights;
    dims.iter()
        .map(|(d, want)| {
            let size = if let Dim::Fill(w) = d {
                let w = u32::from(*w.max(&1));
                // The last filler takes what rounding left over.
                let part = if weights_left == w {
                    spare_left
                } else {
                    spare * w / weights
                };
                spare_left -= part.min(spare_left);
                weights_left -= w;
                u16::try_from(part).unwrap_or(u16::MAX)
            } else {
                *want
            }
            .min(left);
            left -= size;
            size
        })
        .collect()
}

impl Painter<'_> {
    fn token(&self, name: &str) -> Style {
        self.theme.get(name).copied().unwrap_or_default()
    }

    fn opt(&self, name: Option<&str>) -> Style {
        name.map(|n| self.token(n)).unwrap_or_default()
    }

    fn node(&mut self, n: &Node, rect: Rect) {
        if rect.w == 0 || rect.h == 0 {
            return;
        }
        if let Some(name) = &n.style {
            let s = self.token(name);
            self.grid.paint(rect.x, rect.y, rect.w, rect.h, s);
        }
        let inner = Rect {
            x: rect.x + n.pad.left.min(rect.w),
            y: rect.y + n.pad.top.min(rect.h),
            w: rect.w.saturating_sub(n.pad.left + n.pad.right),
            h: rect.h.saturating_sub(n.pad.top + n.pad.bottom),
        };
        if inner.w == 0 || inner.h == 0 {
            return;
        }
        match &n.kind {
            NodeKind::Row { children } => {
                let dims: Vec<_> = children.iter().map(|c| (c.width, want_w(c))).collect();
                let mut x = inner.x;
                for (c, w) in children.iter().zip(split(inner.w, &dims)) {
                    let h = match c.height {
                        Dim::Fill(_) => inner.h,
                        _ => want_h(c).min(inner.h),
                    };
                    self.node(
                        c,
                        Rect {
                            x,
                            y: inner.y,
                            w,
                            h,
                        },
                    );
                    x += w;
                }
            }
            NodeKind::Column { children } => {
                let dims: Vec<_> = children.iter().map(|c| (c.height, want_h(c))).collect();
                let mut y = inner.y;
                for (c, h) in children.iter().zip(split(inner.h, &dims)) {
                    let w = match c.width {
                        Dim::Fill(_) => inner.w,
                        _ => want_w(c).min(inner.w),
                    };
                    self.node(
                        c,
                        Rect {
                            x: inner.x,
                            y,
                            w,
                            h,
                        },
                    );
                    y += h;
                }
            }
            NodeKind::Text { spans, align } => {
                let total = spans_width(spans);
                let mut x = match align {
                    Align::Start => inner.x,
                    Align::End => inner.x + inner.w.saturating_sub(total),
                };
                for s in spans {
                    let style = self.opt(s.style.as_deref());
                    x = self.grid.put(x, inner.y, inner.right(), &s.text, style);
                }
            }
            NodeKind::Spacer => {}
            NodeKind::Surface => self.surface(&n.id, inner),
            NodeKind::List {
                rows,
                selected,
                focused,
            } => self.list(&n.id, inner, rows, *selected, *focused),
            NodeKind::Input {
                prompt,
                value,
                cursor,
                placeholder,
            } => {
                let p = self.token("input.prompt");
                let x = self.grid.put(inner.x, inner.y, inner.right(), prompt, p);
                if value.is_empty() {
                    let s = self.token("input.placeholder");
                    self.grid.put(x, inner.y, inner.right(), placeholder, s);
                } else {
                    self.grid
                        .put(x, inner.y, inner.right(), value, Style::default());
                }
                if *self.focus == Selected::Node(n.id.clone()) {
                    let before: String = value.chars().take(*cursor as usize).collect();
                    let cx = x
                        .saturating_add(width(&before))
                        .min(inner.right().saturating_sub(1));
                    self.cursor = Some((cx, inner.y));
                }
            }
        }
    }

    fn surface(&mut self, id: &str, rect: Rect) {
        let f = self.surface;
        let digits = f.line_count.max(1).to_string().len().max(3);
        let gutter = u16::try_from(digits).unwrap_or(3) + 2;
        if rect.w <= gutter {
            return;
        }
        let text = Rect {
            x: rect.x + gutter,
            w: rect.w - gutter,
            ..rect
        };
        self.surface_size = Some((text.h, text.w));
        self.hits.push(Hit {
            node: id.to_owned(),
            rect: text,
            target: Target::Surface,
        });
        let primary = f
            .cursors
            .iter()
            .find(|c| c.kind == CursorKind::Primary)
            .map(|c| c.pos);
        // Scroll right just enough to keep the primary cursor on screen.
        let cols = usize::from(text.w);
        let hscroll = primary
            .filter(|p| p.line >= f.top_line)
            .map(|p| {
                display_col(
                    &line_text(f, (p.line - f.top_line) as usize),
                    p.col as usize,
                )
            })
            .map_or(0, |x| (x + 1).saturating_sub(cols));
        self.hscroll = hscroll;

        for row in 0..text.h {
            let line_no = f.top_line + u32::from(row);
            let Some(spans) = f.lines.get(usize::from(row)) else {
                break;
            };
            let y = text.y + row;
            let current = primary.is_some_and(|p| p.line == line_no);
            if current {
                let s = self.token("line.current");
                self.grid.paint(rect.x, y, rect.w, 1, s);
            }
            let num = format!("{:>digits$}", line_no + 1);
            let s = self.token(if current { "gutter.current" } else { "gutter" });
            self.grid.put(rect.x, y, text.x, &num, s);

            // Draw from display column `hscroll`, expanding tabs.
            let mut at = 0usize;
            for span in spans {
                let style = self.opt(span.style.as_deref());
                for ch in span.text.chars() {
                    let (w, shown) = if ch == '\t' {
                        (TAB - at % TAB, ' ')
                    } else {
                        (ch.width().unwrap_or(0), ch)
                    };
                    if w > 0 && at >= hscroll && at + w - hscroll <= cols {
                        let x = text.x + u16::try_from(at - hscroll).unwrap_or(0);
                        let mut buf = [0u8; 4];
                        self.grid
                            .put(x, y, text.right(), shown.encode_utf8(&mut buf), style);
                    }
                    at += w;
                }
            }
        }
        self.selections(text, hscroll);
        self.cursors(text, hscroll);
    }

    fn selections(&mut self, text: Rect, hscroll: usize) {
        let f = self.surface;
        let cols = usize::from(text.w);
        let sel = self.token("selection");
        for (a, b) in &f.selections {
            for line in a.line.max(f.top_line)..=b.line {
                let Some(row) = u16::try_from(line - f.top_line)
                    .ok()
                    .filter(|r| *r < text.h)
                else {
                    break;
                };
                let lt = line_text(f, usize::from(row));
                let from = if line == a.line {
                    display_col(&lt, a.col as usize)
                } else {
                    0
                };
                // A selection that runs past the line end shows one cell for the newline.
                let to = if line == b.line {
                    display_col(&lt, b.col as usize)
                } else {
                    display_col(&lt, usize::MAX) + 1
                };
                let from = from.saturating_sub(hscroll).min(cols);
                let to = to.saturating_sub(hscroll).min(cols);
                if to > from {
                    let x = text.x + u16::try_from(from).unwrap_or(0);
                    let w = u16::try_from(to - from).unwrap_or(0);
                    self.grid.paint(x, text.y + row, w, 1, sel);
                }
            }
        }
    }

    fn cursors(&mut self, text: Rect, hscroll: usize) {
        let f = self.surface;
        let screen = |p: Pos| -> Option<(u16, u16)> {
            let row = p.line.checked_sub(f.top_line)?;
            if row >= u32::from(text.h) {
                return None;
            }
            let x =
                display_col(&line_text(f, row as usize), p.col as usize).checked_sub(hscroll)?;
            let x = u16::try_from(x).ok().filter(|x| *x < text.w)?;
            Some((text.x + x, text.y + u16::try_from(row).ok()?))
        };
        for c in &f.cursors {
            let Some((x, y)) = screen(c.pos) else {
                continue;
            };
            match c.kind {
                CursorKind::Primary => {
                    if *self.focus == Selected::Surface {
                        self.cursor = Some((x, y));
                    }
                }
                CursorKind::Secondary => {
                    let s = self.token("cursor.secondary");
                    self.grid.paint(x, y, 1, 1, s);
                }
                CursorKind::Peer => {
                    let s = self.token("cursor.peer");
                    self.grid.paint(x, y, 1, 1, s);
                }
            }
        }
    }

    fn list(
        &mut self,
        id: &str,
        rect: Rect,
        rows: &[spur_rpc::ListRow],
        selected: Option<u32>,
        focused: bool,
    ) {
        // Screen lines: a blank line separates a header from the rows above it.
        let mut lines: Vec<Option<usize>> = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            if r.kind == RowKind::Header && i > 0 {
                lines.push(None);
            }
            lines.push(Some(i));
        }
        let h = usize::from(rect.h);
        let selected = selected.map(|s| s as usize);
        let sel_line = selected.and_then(|s| lines.iter().position(|l| *l == Some(s)));
        let scroll = self.state.scroll.entry(id.to_owned()).or_default();
        if let Some(s) = sel_line {
            if s < *scroll {
                // Keep a header just above the selection in view.
                *scroll = s.saturating_sub(if s <= 2 { s } else { 0 });
            } else if s >= *scroll + h {
                *scroll = s + 1 - h;
            }
        }
        *scroll = (*scroll).min(lines.len().saturating_sub(h));
        let scroll = *scroll;

        let mut hit_rows = Vec::new();
        for (dy, line) in lines.iter().skip(scroll).take(h).enumerate() {
            let y = rect.y + u16::try_from(dy).unwrap_or(0);
            let Some(i) = *line else {
                hit_rows.push(None);
                continue;
            };
            let r = &rows[i];
            if r.kind == RowKind::Header {
                hit_rows.push(None);
                let s = self.token("list.header");
                let label: String = r.spans.iter().map(|s| &*s.text).collect();
                self.grid.put(rect.x + 2, y, rect.right(), &label, s);
                continue;
            }
            hit_rows.push(u32::try_from(i).ok());
            let is_sel = selected == Some(i);
            if is_sel {
                let s = self.token(if focused {
                    "list.selected"
                } else {
                    "list.selected.idle"
                });
                self.grid.paint(rect.x, y, rect.w, 1, s);
                if focused {
                    let s = self.token("accent");
                    self.grid.put(rect.x, y, rect.right(), "▌", s);
                }
            }
            let hint_w = width(&r.hint);
            let end = if hint_w > 0 {
                rect.right().saturating_sub(hint_w + 2)
            } else {
                rect.right().saturating_sub(1)
            };
            let mut x = rect.x + 2 + r.indent * 2;
            if !r.icon.is_empty() {
                let s = self.token("list.icon");
                x = self.grid.put(x, y, end, &r.icon, s);
                x = self.grid.put(x, y, end, " ", Style::default());
            }
            for span in &r.spans {
                let s = self.opt(span.style.as_deref());
                x = self.grid.put(x, y, end, &span.text, s);
            }
            if !r.detail.is_empty() && x + 2 + width(&r.detail) <= end {
                let s = self.token("list.detail");
                self.grid.put(x + 2, y, end, &r.detail, s);
            }
            if hint_w > 0 && end > x {
                let s = self.token("list.hint");
                self.grid.put(end + 1, y, rect.right(), &r.hint, s);
            }
        }
        self.hits.push(Hit {
            node: id.to_owned(),
            rect,
            target: Target::List { rows: hit_rows },
        });
    }
}

/// Draws one frame.
pub fn render(
    ui: &Ui,
    surface: &SurfaceFrame,
    theme: &BTreeMap<String, Style>,
    size: (u16, u16),
    state: &mut ViewState,
) -> Frame {
    let (w, h) = size;
    let mut p = Painter {
        grid: Grid::new(w, h),
        theme,
        surface,
        focus: &ui.focus,
        state,
        cursor: None,
        hits: Vec::new(),
        surface_size: None,
        hscroll: 0,
    };
    let screen = Rect { x: 0, y: 0, w, h };
    p.node(&ui.root, screen);
    for f in &ui.floats {
        let fw = f.width.min(w.saturating_sub(4)).max(1.min(w));
        let fh = f
            .height
            .min(natural_h(&f.node))
            .min(h.saturating_sub(2))
            .max(1.min(h));
        let (x, y) = match f.anchor {
            Anchor::Center => ((w - fw) / 2, (h - fh) / 2),
            Anchor::Top => ((w - fw) / 2, (h / 8).max(2).min(h - fh)),
            Anchor::TopRight => (w.saturating_sub(fw + 2), 2.min(h - fh)),
            Anchor::BottomRight => (w.saturating_sub(fw + 2), h.saturating_sub(fh + 2)),
        };
        let rect = Rect { x, y, w: fw, h: fh };
        if f.scrim {
            let base = theme.get("base").copied().unwrap_or_default();
            p.grid.dim(
                base.fg.unwrap_or(Color(200, 200, 200)),
                base.bg.unwrap_or(Color(0, 0, 0)),
            );
            // A modal float takes all clicks.
            p.hits.clear();
            p.cursor = None;
            p.hits.push(Hit {
                node: f.node.id.clone(),
                rect: screen,
                target: Target::Nothing,
            });
        } else {
            p.hits.push(Hit {
                node: f.node.id.clone(),
                rect,
                target: Target::Nothing,
            });
        }
        // A float starts from a clean slate: nothing underneath shows through.
        let base = theme.get("base").copied().unwrap_or_default();
        p.grid.fill(
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            Style {
                fg: base.fg,
                bg: base.bg,
                ..Style::default()
            },
        );
        p.node(&f.node, rect);
    }
    Frame {
        grid: p.grid,
        cursor: p.cursor,
        hits: p.hits,
        surface_size: p.surface_size,
        hscroll: p.hscroll,
    }
}

impl Frame {
    /// The topmost clickable area at a screen position.
    pub fn hit(&self, x: u16, y: u16) -> Option<&Hit> {
        self.hits.iter().rev().find(|h| h.rect.contains(x, y))
    }
}
