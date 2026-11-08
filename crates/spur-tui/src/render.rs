//! Layout into a cell grid, and minimal diffs between grids.
//!
//! Spur's look: a quiet rail on top (file, unsaved dot, grain, position, a short-lived
//! message), the text below with no filler marks, and cards that open at the caret
//! (Ask, Find, key hints) instead of a command line at the bottom.

use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, MoveTo, SetCursorStyle, Show};
use crossterm::queue;
use crossterm::style::{
    Attribute, Color as CtColor, Print, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use spur_core::editor::{AskKind, Viewport};
use spur_keymap::Session;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Tab stop width.
pub const TAB: usize = 4;
/// Delay before the key hints card shows.
pub const WHICH_KEY_DELAY: Duration = Duration::from_millis(200);

/// A color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    /// The terminal's default.
    #[default]
    Default,
    /// An ANSI palette index.
    Ansi(u8),
    /// 24-bit color.
    Rgb(u8, u8, u8),
}

impl From<Color> for CtColor {
    fn from(c: Color) -> Self {
        match c {
            Color::Default => Self::Reset,
            Color::Ansi(n) => Self::AnsiValue(n),
            Color::Rgb(r, g, b) => Self::Rgb { r, g, b },
        }
    }
}

/// Cell style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// Foreground.
    pub fg: Color,
    /// Background.
    pub bg: Color,
    /// Bold.
    pub bold: bool,
    /// Reverse video.
    pub reverse: bool,
    /// Dim.
    pub dim: bool,
}

/// Spur's palette: ink, bone and copper. Agents get teal when they arrive.
mod theme {
    use super::Color;
    pub const INK: Color = Color::Rgb(0x15, 0x17, 0x1c);
    pub const INK_LINE: Color = Color::Rgb(0x1b, 0x1e, 0x25);
    pub const RAIL: Color = Color::Rgb(0x1f, 0x22, 0x2a);
    pub const CARD: Color = Color::Rgb(0x22, 0x25, 0x2e);
    pub const BONE: Color = Color::Rgb(0xdd, 0xd6, 0xc8);
    pub const MUTED: Color = Color::Rgb(0x6c, 0x70, 0x7c);
    pub const FAINT: Color = Color::Rgb(0x45, 0x49, 0x54);
    pub const COPPER: Color = Color::Rgb(0xd9, 0x8b, 0x48);
    pub const COPPER_DIM: Color = Color::Rgb(0x8a, 0x5a, 0x33);
    pub const SELECT: Color = Color::Rgb(0x4a, 0x35, 0x25);
    pub const MATCH: Color = Color::Rgb(0x23, 0x41, 0x3f);
    pub const ERROR: Color = Color::Rgb(0xe8, 0x6a, 0x5c);
}

const fn st(fg: Color, bg: Color, bold: bool) -> Style {
    Style {
        fg,
        bg,
        bold,
        reverse: false,
        dim: false,
    }
}

const TEXT: Style = st(theme::BONE, theme::INK, false);
const TEXT_LINE: Style = st(theme::BONE, theme::INK_LINE, false);
const GUTTER: Style = st(theme::FAINT, theme::INK, false);
const GUTTER_HERE: Style = st(theme::COPPER, theme::INK_LINE, false);
const SELECTION: Style = st(theme::BONE, theme::SELECT, false);
const MATCH: Style = st(theme::BONE, theme::MATCH, false);
const CURSOR: Style = st(theme::INK, theme::COPPER, false);
const RAIL: Style = st(theme::MUTED, theme::RAIL, false);
const RAIL_NAME: Style = st(theme::BONE, theme::RAIL, false);
const RAIL_MARK: Style = st(theme::COPPER, theme::RAIL, true);
const RAIL_ERROR: Style = st(theme::ERROR, theme::RAIL, true);
const CARD: Style = st(theme::BONE, theme::CARD, false);
const CARD_MUTED: Style = st(theme::MUTED, theme::CARD, false);
const CARD_EDGE: Style = st(theme::COPPER_DIM, theme::CARD, false);
const CARD_TITLE: Style = st(theme::COPPER, theme::CARD, true);
const CARD_PICK: Style = st(theme::INK, theme::COPPER, true);
const CARD_PICK_HINT: Style = st(theme::INK, theme::COPPER, false);
const WELCOME: Style = st(theme::MUTED, theme::INK, false);
const WELCOME_KEY: Style = st(theme::COPPER, theme::INK, false);

/// How long a message stays on the rail.
pub const MESSAGE_FOR: Duration = Duration::from_secs(4);

/// One terminal cell. A wide grapheme fills its first cell; the next is a continuation
/// (`sym` empty).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Grapheme shown, or empty for the right half of a wide one.
    pub sym: String,
    /// Style.
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            sym: " ".into(),
            style: Style::default(),
        }
    }
}

/// A screen of cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    /// Columns.
    pub width: u16,
    /// Rows.
    pub height: u16,
    /// Row-major cells.
    pub cells: Vec<Cell>,
}

impl Grid {
    /// A blank grid.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cells: vec![Cell::default(); usize::from(width) * usize::from(height)],
        }
    }

    /// A grid filled with `style`.
    pub fn filled(width: u16, height: u16, style: Style) -> Self {
        let mut g = Self::new(width, height);
        for c in &mut g.cells {
            c.style = style;
        }
        g
    }

    fn idx(&self, x: u16, y: u16) -> Option<usize> {
        (x < self.width && y < self.height)
            .then(|| usize::from(y) * usize::from(self.width) + usize::from(x))
    }

    /// Writes `text` at (x, y), clipped to the row; returns the column after it.
    pub fn put(&mut self, x: u16, y: u16, text: &str, style: Style) -> u16 {
        let mut x = x;
        for g in text.graphemes(true) {
            let w = u16::try_from(g.width().max(1)).unwrap_or(1);
            if x + w > self.width {
                break;
            }
            self.set(x, y, g, style, w);
            x += w;
        }
        x
    }

    /// Sets one grapheme of display width `w` at (x, y).
    pub fn set(&mut self, x: u16, y: u16, g: &str, style: Style, w: u16) {
        if let Some(i) = self.idx(x, y) {
            self.cells[i] = Cell {
                sym: g.to_owned(),
                style,
            };
            for k in 1..w {
                if let Some(j) = self.idx(x + k, y) {
                    self.cells[j] = Cell {
                        sym: String::new(),
                        style,
                    };
                }
            }
        }
    }

    /// Fills a row span with a style.
    pub fn fill(&mut self, y: u16, from: u16, to: u16, style: Style) {
        for x in from..to.min(self.width) {
            if let Some(i) = self.idx(x, y) {
                self.cells[i] = Cell {
                    sym: " ".into(),
                    style,
                };
            }
        }
    }

    /// Row `y` as text (for tests).
    pub fn row(&self, y: u16) -> String {
        (0..self.width)
            .filter_map(|x| self.idx(x, y))
            .map(|i| self.cells[i].sym.as_str())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }
}

/// A frame: cells plus where the terminal cursor goes and its shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Cells.
    pub grid: Grid,
    /// Terminal cursor (column, row).
    pub cursor: Option<(u16, u16)>,
    /// `block`, `bar` or `underline`.
    pub shape: String,
}

/// Frontend-only state that survives between frames.
#[derive(Debug, Default)]
pub struct UiState {
    /// First display column shown (horizontal scroll).
    pub left_col: usize,
    /// When the keymap started waiting for more keys.
    pub pending_since: Option<Instant>,
    /// The message on the rail and when it first showed.
    pub message: Option<(String, Instant)>,
}

/// Display layout of one line: for each grapheme, (char index, display column, width,
/// text shown).
fn layout(line: &str) -> Vec<(usize, usize, usize, String)> {
    let mut out = Vec::new();
    let (mut ch, mut col) = (0, 0);
    for g in line.graphemes(true) {
        let (w, shown) = if g == "\t" {
            let w = TAB - col % TAB;
            (w, " ".repeat(w))
        } else if g.chars().all(char::is_control) {
            (
                2,
                format!(
                    "^{}",
                    char::from(
                        b'@' + u8::try_from(g.chars().next().map_or(0, |c| c as u32 & 0x1f))
                            .unwrap_or(0)
                    )
                ),
            )
        } else if g.width() == 0 {
            // Invisible chars (BOM, zero-width space) get a visible dot of known width.
            (1, "\u{b7}".to_owned())
        } else {
            (g.width(), g.to_owned())
        };
        out.push((ch, col, w, shown));
        ch += g.chars().count();
        col += w;
    }
    out
}

/// Display column of char index `ch` in a laid-out line.
fn col_of(lay: &[(usize, usize, usize, String)], ch: usize) -> usize {
    lay.iter()
        .find(|g| g.0 >= ch)
        .map_or_else(|| lay.last().map_or(0, |g| g.1 + g.2), |g| g.1)
}

/// The rail: brand, file, unsaved dot on the left; message, grain and position on the
/// right.
fn rail(grid: &mut Grid, s: &Session, vp: &Viewport, message: Option<(&str, bool)>) {
    let width = grid.width;
    grid.fill(0, 0, width, RAIL);
    let mut x = grid.put(1, 0, "\u{25c6} ", RAIL_MARK);
    let name = vp
        .path
        .as_ref()
        .map_or_else(|| "new file".to_owned(), |p| p.display().to_string());
    x = grid.put(
        x,
        0,
        &name,
        if vp.path.is_some() { RAIL_NAME } else { RAIL },
    );
    if vp.modified {
        grid.put(x + 1, 0, "\u{25cf}", RAIL_MARK);
    }

    let (line, col) = vp
        .cursors
        .iter()
        .find(|c| c.1)
        .map_or((0, 0), |c| (c.0.line + 1, c.0.column + 1));
    let mut right: Vec<(String, Style)> = Vec::new();
    if let Some((text, error)) = message {
        right.push((text.to_owned(), if error { RAIL_ERROR } else { RAIL_MARK }));
    }
    let pending = s.keymap.pending_keys();
    if !pending.is_empty() {
        right.push((pending, RAIL_NAME));
    }
    if let Some(g) = vp.grain {
        right.push((g.name().to_owned(), RAIL_MARK));
    }
    if vp.mode.as_ref() != spur_core::editor::EDIT_MODE
        && vp.mode.as_ref() != spur_core::editor::ask::ASK_MODE
    {
        // A preset's mode, named as the preset names it.
        right.push((vp.mode.to_string(), RAIL_NAME));
    }
    right.push((format!("{line}\u{b7}{col}"), RAIL));
    let total: usize = right.iter().map(|(t, _)| t.width() + 3).sum();
    let mut rx = width.saturating_sub(u16::try_from(total).unwrap_or(u16::MAX));
    // Never cover the file name.
    rx = rx.max(x + 3);
    for (i, (t, style)) in right.iter().enumerate() {
        if i > 0 {
            rx = grid.put(rx, 0, " \u{b7} ", RAIL);
        } else {
            rx = grid.put(rx, 0, "  ", RAIL);
        }
        rx = grid.put(rx, 0, t, *style);
    }
}

/// Draws a card with a rounded edge and a title at (x, y). Returns the inner area.
#[allow(clippy::many_single_char_names)]
fn card_frame(grid: &mut Grid, x: u16, y: u16, w: u16, h: u16, title: &str) {
    if w < 4 || h < 2 {
        return;
    }
    let right = x + w - 1;
    let bottom = y + h - 1;
    for row in y..=bottom {
        grid.fill(row, x, x + w, CARD);
        if row != y && row != bottom {
            grid.put(x, row, "\u{2502}", CARD_EDGE);
            grid.put(right, row, "\u{2502}", CARD_EDGE);
        }
    }
    let line = "\u{2500}".repeat(usize::from(w - 2));
    grid.put(x, y, "\u{256d}", CARD_EDGE);
    grid.put(x + 1, y, &line, CARD_EDGE);
    grid.put(right, y, "\u{256e}", CARD_EDGE);
    grid.put(x, bottom, "\u{2570}", CARD_EDGE);
    grid.put(x + 1, bottom, &line, CARD_EDGE);
    grid.put(right, bottom, "\u{256f}", CARD_EDGE);
    if !title.is_empty() {
        let t = format!(" {title} ");
        let max = usize::from(w.saturating_sub(4));
        let t: String = t.graphemes(true).take(max).collect();
        grid.put(x + 2, y, &t, CARD_TITLE);
    }
}

/// Fits `text` into `w` columns, keeping the end (what was typed last).
fn tail_fit(text: &str, w: usize) -> String {
    if text.width() <= w {
        return text.to_owned();
    }
    let mut out: Vec<&str> = Vec::new();
    let mut used = 1;
    for g in text.graphemes(true).rev() {
        if used + g.width() > w {
            break;
        }
        used += g.width();
        out.push(g);
    }
    out.reverse();
    format!("\u{2026}{}", out.concat())
}

/// Places a card of `h` rows near the caret: below it if it fits, else above.
fn place(
    caret: (u16, u16),
    w: u16,
    h: u16,
    width: u16,
    top: u16,
    bottom: u16,
) -> Option<(u16, u16)> {
    let (cx, cy) = caret;
    if w > width || h > bottom.saturating_sub(top) {
        return None;
    }
    let x = cx.saturating_sub(2).min(width - w);
    let y = if cy + 1 + h <= bottom {
        cy + 1
    } else if cy >= top + h {
        cy - h
    } else {
        bottom - h
    };
    Some((x, y))
}

/// Lays out the session into a `width` x `height` frame.
#[allow(clippy::too_many_lines)]
pub fn render(s: &mut Session, width: u16, height: u16, ui: &mut UiState, now: Instant) -> Frame {
    let mut grid = Grid::filled(width, height, TEXT);
    let top_row: u16 = 1;
    let rows = usize::from(height.saturating_sub(top_row)).max(1);
    s.core.state.scroll_to_cursor(rows, 3);
    let vp = s.core.state.viewport(rows);
    let digits = vp.line_count.to_string().len().max(2);
    let gutter = digits + 2;
    let text_w = usize::from(width).saturating_sub(gutter + 1).max(1);
    let primary = vp.cursors.iter().find(|c| c.1).map(|c| c.0);

    // Keep the primary cursor's column visible.
    if let Some(p) = primary
        && let Some(line) = vp.lines.get(p.line.wrapping_sub(vp.top_line))
    {
        let lay = layout(line);
        let c = col_of(&lay, p.column);
        let cw = lay.iter().find(|g| g.0 >= p.column).map_or(1, |g| g.2);
        if c < ui.left_col {
            ui.left_col = c;
        } else if c + cw > ui.left_col + text_w {
            ui.left_col = c + cw - text_w.min(c + cw);
        }
    }
    let left = ui.left_col;
    let x0 = u16::try_from(gutter).unwrap_or(u16::MAX);
    let mut cursor = None;
    let here = primary.map(|p| p.line);
    let in_range = |line_no: usize, ch: usize, ranges: &[(spur_text::Point, spur_text::Point)]| {
        ranges
            .iter()
            .any(|(a, b)| (line_no, ch) >= (a.line, a.column) && (line_no, ch) < (b.line, b.column))
    };

    for row in 0..rows {
        let y = top_row + u16::try_from(row).unwrap_or(u16::MAX);
        let line_no = vp.top_line + row;
        let Some(text) = vp.lines.get(row) else {
            continue;
        };
        let is_here = here == Some(line_no) && vp.selections.is_empty();
        let base = if is_here { TEXT_LINE } else { TEXT };
        if is_here {
            grid.fill(y, 0, width, TEXT_LINE);
        }
        let num = format!("{:>w$}  ", line_no + 1, w = digits);
        grid.put(0, y, &num, if is_here { GUTTER_HERE } else { GUTTER });
        let lay = layout(text);
        let line_selected_to_end = vp
            .selections
            .iter()
            .any(|(a, b)| a.line <= line_no && line_no < b.line);
        for (ch, col, w, shown) in &lay {
            if *col < left || col + w > left + text_w {
                continue;
            }
            let x = x0 + u16::try_from(col - left).unwrap_or(0);
            let style = if in_range(line_no, *ch, &vp.selections) {
                SELECTION
            } else if in_range(line_no, *ch, &vp.highlights) {
                MATCH
            } else {
                base
            };
            if shown.width() > 1 && *w > 1 && shown.graphemes(true).count() > 1 {
                grid.put(x, y, shown, style);
            } else {
                grid.set(x, y, shown, style, u16::try_from(*w).unwrap_or(1));
            }
        }
        if line_selected_to_end {
            let end = lay.last().map_or(0, |g| g.1 + g.2);
            if end >= left && end < left + text_w {
                let x = x0 + u16::try_from(end - left).unwrap_or(0);
                grid.fill(y, x, x + 1, SELECTION);
            }
        }
        for (p, is_primary) in &vp.cursors {
            if p.line != line_no {
                continue;
            }
            let c = col_of(&lay, p.column);
            if c < left || c >= left + text_w {
                continue;
            }
            let x = x0 + u16::try_from(c - left).unwrap_or(0);
            if *is_primary {
                cursor = Some((x, y));
            } else if let Some(i) = grid.idx(x, y) {
                grid.cells[i].style = CURSOR;
            }
        }
    }

    // A new, empty file: say how to begin, once, quietly.
    let doc_empty = vp.line_count <= 1 && vp.lines.first().is_none_or(String::is_empty);
    if doc_empty && vp.path.is_none() && !vp.modified && vp.ask.is_none() && height >= 8 {
        let lines: [(&str, &str); 3] = [
            ("", "Start typing."),
            ("ctrl+space", "ask for anything"),
            ("ctrl+q", "leave"),
        ];
        let mid = height / 2 - 1;
        for (i, (key, what)) in lines.iter().enumerate() {
            let y = mid + u16::try_from(i).unwrap_or(0);
            let w = u16::try_from(key.width() + what.width() + 2).unwrap_or(0);
            let x = width.saturating_sub(w) / 2;
            let x = if key.is_empty() {
                x
            } else {
                grid.put(x, y, key, WELCOME_KEY) + 2
            };
            grid.put(x, y, what, WELCOME);
        }
    }

    // Message: shown on the rail for a few seconds.
    let message = if let Some(m) = &vp.message {
        let since = match &ui.message {
            Some((t, at)) if *t == m.text => *at,
            _ => now,
        };
        ui.message = Some((m.text.clone(), since));
        (now.duration_since(since) < MESSAGE_FOR).then_some((m.text.as_str(), m.error))
    } else {
        ui.message = None;
        None
    };
    rail(&mut grid, s, &vp, message);

    let caret = cursor.unwrap_or((x0, top_row));
    // The Ask card, at the caret.
    if let Some(a) = &vp.ask {
        let w = width.saturating_sub(2).clamp(20.min(width), 60);
        let inner = usize::from(w.saturating_sub(4));
        let input = a.kind != AskKind::Confirm;
        let rows_n = u16::try_from(a.items.len()).unwrap_or(0) + u16::from(input);
        let h = rows_n + 2;
        if let Some((x, y)) = place(caret, w, h, width, top_row, height) {
            card_frame(&mut grid, x, y, w, h, &a.title);
            let mut ry = y + 1;
            if input {
                let note = a.note.clone().unwrap_or_default();
                let note_w = note.width();
                let field = inner.saturating_sub(note_w + 3);
                let typed = tail_fit(&a.text, field);
                let after = grid.put(x + 2, ry, "\u{203a} ", CARD_TITLE);
                let end = grid.put(after, ry, &typed, CARD);
                if a.text.is_empty() {
                    let hint = match a.kind {
                        AskKind::Find => "text to find",
                        _ => "a command, a line number, or text to find",
                    };
                    let hint: String = hint.graphemes(true).take(field).collect();
                    grid.put(after + 1, ry, &hint, CARD_MUTED);
                }
                if !note.is_empty() {
                    let nx = (x + w).saturating_sub(2 + u16::try_from(note_w).unwrap_or(0));
                    grid.put(nx, ry, &note, CARD_MUTED);
                }
                cursor = Some((end, ry));
                ry += 1;
            } else {
                cursor = None;
            }
            for (i, it) in a.items.iter().enumerate() {
                let picked = i == a.selected;
                let (st, hs) = if picked {
                    (CARD_PICK, CARD_PICK_HINT)
                } else {
                    (CARD, CARD_MUTED)
                };
                grid.fill(ry, x + 1, x + w - 1, st);
                let hint_w = it.hint.width();
                let title: String = it
                    .title
                    .graphemes(true)
                    .take(inner.saturating_sub(hint_w + 2))
                    .collect();
                grid.put(x + 2, ry, &title, st);
                if !it.hint.is_empty() {
                    let hx = (x + w).saturating_sub(2 + u16::try_from(hint_w).unwrap_or(0));
                    grid.put(hx, ry, &it.hint, hs);
                }
                ry += 1;
            }
        }
    } else if let Some(c) = &vp.line_prompt {
        // A preset's own one-line prompt, drawn as a small card at the caret.
        let w = width.saturating_sub(2).clamp(20.min(width), 60);
        if let Some((x, y)) = place(caret, w, 3, width, top_row, height) {
            card_frame(&mut grid, x, y, w, 3, "");
            let typed = tail_fit(c, usize::from(w.saturating_sub(4)));
            let end = grid.put(x + 2, y + 1, &typed, CARD);
            cursor = Some((end, y + 1));
        }
    }

    // Key hints: after a pause in the middle of a key sequence.
    if s.keymap.is_pending() {
        let since = *ui.pending_since.get_or_insert(now);
        if now.duration_since(since) >= WHICH_KEY_DELAY {
            let mode = s.mode();
            let items = s.keymap.menu(&mode, s.core.context());
            let max_rows = usize::from(height.saturating_sub(top_row + 2)).min(12);
            if !items.is_empty() && max_rows > 0 {
                let shown: Vec<(String, String)> = items
                    .iter()
                    .take(max_rows)
                    .map(|m| {
                        let label = if m.group {
                            format!("{} \u{2026}", m.label)
                        } else {
                            m.label.clone()
                        };
                        (m.key.clone(), label)
                    })
                    .collect();
                let kw = shown.iter().map(|t| t.0.width()).max().unwrap_or(1);
                let lw = shown.iter().map(|t| t.1.width()).max().unwrap_or(1);
                let w = u16::try_from(kw + lw + 6)
                    .unwrap_or(u16::MAX)
                    .min(width.saturating_sub(2));
                let h = u16::try_from(shown.len()).unwrap_or(0) + 2;
                if let Some((x, y)) = place(caret, w, h, width, top_row, height) {
                    card_frame(&mut grid, x, y, w, h, &s.keymap.pending_keys());
                    for (i, (k, l)) in shown.iter().enumerate() {
                        let ry = y + 1 + u16::try_from(i).unwrap_or(0);
                        grid.put(x + 2, ry, k, CARD_TITLE);
                        let lx = x + 2 + u16::try_from(kw + 2).unwrap_or(0);
                        grid.put(lx, ry, l, CARD);
                    }
                }
            }
        }
    } else {
        ui.pending_since = None;
    }

    Frame {
        grid,
        cursor,
        shape: s.cursor_shape().to_owned(),
    }
}

fn apply_style(out: &mut Vec<u8>, st: Style) -> std::io::Result<()> {
    queue!(out, SetAttribute(Attribute::Reset))?;
    if st.bold {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    if st.dim {
        queue!(out, SetAttribute(Attribute::Dim))?;
    }
    if st.reverse {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    queue!(
        out,
        SetForegroundColor(st.fg.into()),
        SetBackgroundColor(st.bg.into())
    )
}

/// Bytes that turn `prev` into `next` on the terminal: only changed cells, SGR only on
/// style changes, cursor moves only where cells are not contiguous. `prev = None` (first
/// frame or resize) redraws everything.
///
/// # Errors
///
/// Only if writing to the buffer fails, which a `Vec` never does.
pub fn diff(prev: Option<&Frame>, next: &Frame, out: &mut Vec<u8>) -> std::io::Result<()> {
    let full =
        prev.is_none_or(|p| p.grid.width != next.grid.width || p.grid.height != next.grid.height);
    let mut style: Option<Style> = None;
    let mut at: Option<(u16, u16)> = None;
    let g = &next.grid;
    for y in 0..g.height {
        for x in 0..g.width {
            let i = usize::from(y) * usize::from(g.width) + usize::from(x);
            let cell = &g.cells[i];
            if cell.sym.is_empty() {
                continue;
            }
            if !full && prev.is_some_and(|p| p.grid.cells[i] == *cell) {
                // A wide cell's continuation may have changed even if this one did not.
                let next_changed = g.cells.get(i + 1).is_some_and(|n| {
                    n.sym.is_empty() && prev.is_some_and(|p| p.grid.cells.get(i + 1) != Some(n))
                });
                if !next_changed {
                    continue;
                }
            }
            if at != Some((x, y)) {
                queue!(out, MoveTo(x, y))?;
            }
            if style != Some(cell.style) {
                apply_style(out, cell.style)?;
                style = Some(cell.style);
            }
            queue!(out, Print(&cell.sym))?;
            let w = u16::try_from(cell.sym.width().max(1)).unwrap_or(1);
            at = Some((x + w, y));
        }
    }
    if style.is_some() {
        queue!(out, SetAttribute(Attribute::Reset))?;
    }
    if prev.is_none_or(|p| p.shape != next.shape) {
        let shape = match next.shape.as_str() {
            "block" => SetCursorStyle::SteadyBlock,
            "underline" => SetCursorStyle::SteadyUnderScore,
            _ => SetCursorStyle::SteadyBar,
        };
        queue!(out, shape)?;
    }
    // The terminal guard starts with the cursor hidden.
    let was_shown = prev.is_some_and(|p| p.cursor.is_some());
    match next.cursor {
        Some((x, y)) => {
            queue!(out, MoveTo(x, y))?;
            if !was_shown || full {
                queue!(out, Show)?;
            }
        }
        None if was_shown => queue!(out, Hide)?,
        None => {}
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_text::{Buffer, SelRange, Selection};

    fn session(text: &str) -> Session {
        let mut s = Session::new();
        let ed = &mut s.core.state;
        let doc = ed.add_doc(Buffer::new(text), None);
        ed.focus = ed.add_view(doc);
        s
    }

    #[test]
    fn layout_tabs_and_wide() {
        let l = layout("\tab");
        assert_eq!(l[0].1, 0);
        assert_eq!(l[0].2, 4);
        assert_eq!(l[1].1, 4);
        let w = layout("日本x");
        assert_eq!((w[1].1, w[2].1), (2, 4));
        assert_eq!(col_of(&w, 2), 4);
        assert_eq!(col_of(&w, 9), 5);
    }

    #[test]
    fn frame_rail_rows_and_cursor() {
        let mut s = session("hello\n\tworld");
        let id = s.core.state.focus;
        s.core.state.views.get_mut(&id).unwrap().selection = Selection {
            ranges: vec![SelRange::cursor(8)],
            primary: 0,
        };
        let mut ui = UiState::default();
        let f = render(&mut s, 30, 6, &mut ui, Instant::now());
        assert!(f.grid.row(0).contains("new file"), "{}", f.grid.row(0));
        assert!(f.grid.row(0).ends_with("2\u{b7}3"), "{}", f.grid.row(0));
        assert_eq!(f.grid.row(1), " 1  hello");
        assert_eq!(f.grid.row(2), " 2      world");
        // No filler marks after the end.
        assert_eq!(f.grid.row(3), "");
        // Cursor on 'o' in "\tworld": gutter 4, tab = 4 cols, then 'w'.
        assert_eq!(f.cursor, Some((4 + 4 + 1, 2)));
        assert_eq!(f.shape, "bar");
        // No mode name anywhere.
        assert!(!(0..6).any(|y| f.grid.row(y).to_lowercase().contains("edit")));
    }

    #[test]
    fn unsaved_dot_and_message() {
        let mut s = session("abc");
        let mut ui = UiState::default();
        let now = Instant::now();
        s.key(spur_keymap::Key::char('x'), now);
        let f = render(&mut s, 40, 5, &mut ui, now);
        assert!(
            f.grid.row(0).contains("new file \u{25cf}"),
            "{}",
            f.grid.row(0)
        );
        s.key(spur_keymap::parse_keys("ctrl+s").unwrap()[0], now);
        let f = render(&mut s, 40, 5, &mut ui, now);
        assert!(
            f.grid.row(0).contains("no file name") || f.grid.row(0).contains("path"),
            "{}",
            f.grid.row(0)
        );
        // Messages fade.
        let f = render(&mut s, 40, 5, &mut ui, now + MESSAGE_FOR);
        assert!(!f.grid.row(0).contains("path") && !f.grid.row(0).contains("file name"));
    }

    #[test]
    fn welcome_on_a_new_empty_file() {
        let mut s = Session::new();
        let mut ui = UiState::default();
        let f = render(&mut s, 50, 12, &mut ui, Instant::now());
        assert!((0..12).any(|y| f.grid.row(y).contains("Start typing.")));
        s.key(spur_keymap::Key::char('a'), Instant::now());
        let f = render(&mut s, 50, 12, &mut ui, Instant::now());
        assert!(!(0..12).any(|y| f.grid.row(y).contains("Start typing.")));
    }

    #[test]
    fn ask_card_opens_at_the_caret() {
        let mut s = session("one\ntwo\nthree");
        let mut ui = UiState::default();
        let now = Instant::now();
        s.key(spur_keymap::parse_keys("ctrl+space").unwrap()[0], now);
        let f = render(&mut s, 60, 16, &mut ui, now);
        // Below line 1 (row 1): the card's top edge is on row 2.
        assert!(f.grid.row(2).contains("ask"), "{}", f.grid.row(2));
        assert!(f.grid.row(3).contains('\u{203a}'), "{}", f.grid.row(3));
        assert!((4..16).any(|y| f.grid.row(y).contains("Save")));
        assert_eq!(f.cursor.map(|c| c.1), Some(3));
        for c in "sav".chars() {
            s.key(spur_keymap::Key::char(c), now);
        }
        let f = render(&mut s, 60, 16, &mut ui, now);
        assert!(
            f.grid.row(4).contains("Save") && f.grid.row(4).contains("ctrl+s"),
            "{}",
            f.grid.row(4)
        );
    }

    #[test]
    fn horizontal_scroll_follows_cursor() {
        let long: String = "x".repeat(100);
        let mut s = session(&long);
        let id = s.core.state.focus;
        s.core.state.views.get_mut(&id).unwrap().selection = Selection::cursor(90);
        let mut ui = UiState::default();
        let f = render(&mut s, 20, 4, &mut ui, Instant::now());
        let (x, _) = f.cursor.unwrap();
        assert!(x < 20);
        assert!(ui.left_col > 0);
    }

    #[test]
    fn diff_writes_only_changes() {
        let mut s = session("abc");
        let mut ui = UiState::default();
        let now = Instant::now();
        let a = render(&mut s, 20, 4, &mut ui, now);
        let mut full = Vec::new();
        diff(None, &a, &mut full).unwrap();
        let mut none = Vec::new();
        diff(Some(&a), &a, &mut none).unwrap();
        assert!(
            none.len() < full.len() / 4,
            "{} vs {}",
            none.len(),
            full.len()
        );
        assert!(!String::from_utf8_lossy(&none).contains("abc"));
        // The first frame shows the cursor the terminal guard hid; later ones leave it.
        assert!(String::from_utf8_lossy(&full).contains("\x1b[?25h"));
        assert!(!String::from_utf8_lossy(&none).contains("\x1b[?25h"));
        s.key(spur_keymap::Key::char('x'), now);
        let b = render(&mut s, 20, 4, &mut ui, now);
        let mut some = Vec::new();
        diff(Some(&a), &b, &mut some).unwrap();
        let text = String::from_utf8_lossy(&some);
        assert!(text.contains("bc") || text.contains('b'), "{text:?}");
    }

    #[test]
    fn key_hints_after_delay() {
        let mut s = session("abc");
        s.load_toml(
            "test",
            spur_core::Source::Core,
            r#"
            [[group]]
            mode = "edit"
            keys = "ctrl+k"
            label = "more"
            [[bind]]
            mode = "edit"
            keys = "ctrl+k t"
            command = "editor.move_doc_start"
            label = "to the top"
            "#,
        )
        .unwrap();
        let mut ui = UiState::default();
        let now = Instant::now();
        s.key(spur_keymap::parse_keys("ctrl+k").unwrap()[0], now);
        let f = render(&mut s, 60, 14, &mut ui, now);
        assert!(!(0..14).any(|y| f.grid.row(y).contains("to the top")));
        let f = render(&mut s, 60, 14, &mut ui, now + WHICH_KEY_DELAY);
        assert!(
            (0..14).any(|y| f.grid.row(y).contains("to the top")),
            "hints missing"
        );
    }

    #[test]
    fn invisible_chars_keep_columns() {
        let lay = layout("\u{feff}ab");
        assert_eq!(lay[0].3, "\u{b7}");
        assert_eq!((lay[1].1, lay[2].1), (1, 2));
    }
}
