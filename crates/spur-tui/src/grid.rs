//! A grid of cells, and the bytes that turn one grid into the next on screen.

use std::fmt::Write as _;

use crate::clean::{Piece, pieces};
use crate::color::{Color, Style, attr};

/// One terminal cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// What is drawn; empty for the second half of a wide char.
    pub text: String,
    /// Text colour.
    pub fg: Option<Color>,
    /// Background.
    pub bg: Option<Color>,
    /// [`attr`] bits.
    pub attrs: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: " ".into(),
            fg: None,
            bg: None,
            attrs: 0,
        }
    }
}

impl Cell {
    fn style(&self) -> (Option<Color>, Option<Color>, u8) {
        (self.fg, self.bg, self.attrs)
    }
}

/// A screen of cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
    cells: Vec<Cell>,
}

impl Grid {
    /// A blank grid.
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            cells: vec![Cell::default(); (w as usize) * (h as usize)],
        }
    }

    fn at(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.w as usize) + x as usize
    }

    /// The cell at `(x, y)`.
    pub fn cell(&self, x: u32, y: u32) -> &Cell {
        &self.cells[self.at(x, y)]
    }

    /// Paints `style` over a rectangle: set colours replace, unset ones stay;
    /// attributes are replaced.
    pub fn paint(&mut self, x: u32, y: u32, w: u32, h: u32, style: Style) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                let i = self.at(xx, yy);
                let c = &mut self.cells[i];
                if style.fg.is_some() {
                    c.fg = style.fg;
                }
                if style.bg.is_some() {
                    c.bg = style.bg;
                }
                c.attrs = style.attrs;
            }
        }
    }

    /// Writes cleaned text from `(x, y)`, stopping before `max_x`; each cell gets
    /// `style` painted over it. Returns the x after the text.
    pub fn text(&mut self, x: u32, y: u32, max_x: u32, text: &str, style: Option<Style>) -> u32 {
        self.pieces(x, y, max_x, &pieces(text), style)
    }

    /// Writes cells; a wide char that does not fit becomes a space.
    pub fn pieces(
        &mut self,
        mut x: u32,
        y: u32,
        max_x: u32,
        pieces: &[Piece],
        style: Option<Style>,
    ) -> u32 {
        let max_x = max_x.min(self.w);
        if y >= self.h {
            return x;
        }
        for p in pieces {
            if x >= max_x {
                break;
            }
            let fits = x + p.width <= max_x;
            let i = self.at(x, y);
            self.cells[i].text = if fits { p.text.clone() } else { " ".into() };
            if let Some(s) = style {
                self.paint(x, y, 1, 1, s);
            }
            if fits && p.width == 2 {
                let j = self.at(x + 1, y);
                let lead = self.cells[i].clone();
                self.cells[j] = Cell {
                    text: String::new(),
                    ..lead
                };
                x += 2;
            } else {
                x += 1;
            }
        }
        x
    }

    /// The grid as plain text, one line per row (tests and snapshots).
    pub fn lines(&self) -> Vec<String> {
        (0..self.h)
            .map(|y| {
                (0..self.w)
                    .map(|x| self.cell(x, y).text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }
}

fn sgr(out: &mut String, (fg, bg, attrs): (Option<Color>, Option<Color>, u8)) {
    out.push_str("\x1b[0");
    for (bit, code) in [
        (attr::BOLD, 1),
        (attr::DIM, 2),
        (attr::ITALIC, 3),
        (attr::UNDERLINE, 4),
    ] {
        if attrs & bit != 0 {
            let _ = write!(out, ";{code}");
        }
    }
    for (c, base) in [(fg, 38), (bg, 48)] {
        match c {
            Some(Color::Rgb(r, g, b)) => {
                let _ = write!(out, ";{base};2;{r};{g};{b}");
            }
            Some(Color::Index(i)) => {
                let _ = write!(out, ";{base};5;{i}");
            }
            None => {}
        }
    }
    out.push('m');
}

/// The bytes that turn `prev` (what the screen shows, or nothing) into `next`, then
/// place the cursor (`None` hides it). Only changed cells are written, inside
/// synchronised-output markers.
pub fn diff(prev: Option<&Grid>, next: &Grid, cursor: Option<(u32, u32)>) -> String {
    let mut out = String::from("\x1b[?2026h");
    let full = prev.is_none_or(|p| p.w != next.w || p.h != next.h);
    if full {
        out.push_str("\x1b[0m\x1b[2J");
    }
    let mut pos: Option<(u32, u32)> = None;
    let mut style = None;
    for y in 0..next.h {
        let mut x = 0;
        while x < next.w {
            let c = next.cell(x, y);
            let changed = |x: u32| full || prev.is_some_and(|p| p.cell(x, y) != next.cell(x, y));
            // A changed second half rewrites its wide char.
            let lead_changed = changed(x) || (x + 1 < next.w && next.cell(x + 1, y).text.is_empty() && changed(x + 1));
            if c.text.is_empty() || !lead_changed {
                x += 1;
                continue;
            }
            if pos != Some((x, y)) {
                let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
            }
            if style != Some(c.style()) {
                sgr(&mut out, c.style());
                style = Some(c.style());
            }
            out.push_str(&c.text);
            let w = if x + 1 < next.w && next.cell(x + 1, y).text.is_empty() {
                2
            } else {
                1
            };
            x += w;
            pos = (x < next.w).then_some((x, y));
        }
    }
    out.push_str("\x1b[0m");
    match cursor {
        Some((x, y)) => {
            let _ = write!(out, "\x1b[{};{}H\x1b[?25h", y + 1, x + 1);
        }
        None => out.push_str("\x1b[?25l"),
    }
    out.push_str("\x1b[?2026l");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_clips_and_wide_chars_do_not_split() {
        let mut g = Grid::new(5, 1);
        let end = g.text(0, 0, 5, "ab\u{6771}cd", None);
        assert_eq!(end, 5);
        assert_eq!(g.lines(), ["ab\u{6771}c"]);
        let mut g = Grid::new(3, 1);
        g.text(0, 0, 3, "ab\u{6771}", None);
        assert_eq!(g.lines(), ["ab"], "no room for the second half");
    }

    #[test]
    fn only_changed_cells_are_written() {
        let mut a = Grid::new(80, 24);
        a.text(0, 0, 80, "hello", None);
        let first = diff(None, &a, None);
        assert!(first.contains("hello"));
        let mut b = a.clone();
        b.text(0, 3, 80, "x", None);
        let d = diff(Some(&a), &b, Some((1, 3)));
        assert!(!d.contains("hello"));
        assert!(d.contains("\x1b[4;1Hx"));
        assert!(d.len() < 60, "{d:?}");
        assert!(d.starts_with("\x1b[?2026h") && d.ends_with("\x1b[?2026l"));
        assert!(d.contains("\x1b[4;2H\x1b[?25h"));
        let none = diff(Some(&b), &b, None);
        assert!(!none.contains('x'));
    }

    #[test]
    fn styles_are_written_once_per_run() {
        let mut a = Grid::new(10, 1);
        let s = Style {
            fg: Some(Color::Index(196)),
            bg: Some(Color::Rgb(1, 2, 3)),
            attrs: attr::BOLD,
        };
        a.paint(0, 0, 10, 1, s);
        a.text(0, 0, 10, "abc", None);
        let d = diff(None, &a, None);
        assert_eq!(d.matches("38;5;196").count(), 1, "{d:?}");
        assert!(d.contains("\x1b[0;1;38;5;196;48;2;1;2;3m"));
    }
}
