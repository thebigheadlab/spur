//! A grid of styled terminal cells and the escape codes that put it on screen.

// Geometry and colour maths read best with x, y, r, g and b.
#![allow(clippy::many_single_char_names, clippy::similar_names)]

use std::io::{self, Write};

use spur_rpc::{Color, Style};
use unicode_width::UnicodeWidthChar;

/// One terminal cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The char; `'\0'` marks the right half of a wide char.
    pub ch: char,
    /// How it is drawn.
    pub style: Style,
}

const BLANK: Cell = Cell {
    ch: ' ',
    style: Style {
        fg: None,
        bg: None,
        bold: false,
        italic: false,
        underline: false,
    },
};

/// Lays `over` on top of `base`: set fields win, flags add up.
pub fn merge(base: Style, over: Style) -> Style {
    Style {
        fg: over.fg.or(base.fg),
        bg: over.bg.or(base.bg),
        bold: base.bold || over.bold,
        italic: base.italic || over.italic,
        underline: base.underline || over.underline,
    }
}

/// A screen of cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    /// Columns.
    pub width: u16,
    /// Rows.
    pub height: u16,
    cells: Vec<Cell>,
}

impl Grid {
    /// A blank grid.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cells: vec![BLANK; usize::from(width) * usize::from(height)],
        }
    }

    fn index(&self, x: u16, y: u16) -> Option<usize> {
        (x < self.width && y < self.height)
            .then(|| usize::from(y) * usize::from(self.width) + usize::from(x))
    }

    /// The cell at a position.
    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        self.index(x, y).map(|i| &self.cells[i])
    }

    /// Applies `style` over the cells of a rectangle, keeping their text.
    pub fn paint(&mut self, x: u16, y: u16, w: u16, h: u16, style: Style) {
        for yy in y..y.saturating_add(h) {
            for xx in x..x.saturating_add(w) {
                if let Some(i) = self.index(xx, yy) {
                    self.cells[i].style = merge(self.cells[i].style, style);
                }
            }
        }
    }

    /// Resets a rectangle to blank cells with exactly `style`, hiding what was there.
    pub fn fill(&mut self, x: u16, y: u16, w: u16, h: u16, style: Style) {
        for yy in y..y.saturating_add(h) {
            for xx in x..x.saturating_add(w) {
                if let Some(i) = self.index(xx, yy) {
                    self.cells[i] = Cell { ch: ' ', style };
                }
            }
        }
    }

    /// Writes text starting at a position, not past `max_x` (exclusive). The style is laid
    /// over what is there, so an unset background shows through. Returns the next column.
    pub fn put(&mut self, x: u16, y: u16, max_x: u16, text: &str, style: Style) -> u16 {
        let mut x = x;
        let max_x = max_x.min(self.width);
        for ch in text.chars() {
            let w = ch.width().unwrap_or(0);
            // Zero-width chars (combining marks, joiners) are not drawn in v0.
            if w == 0 {
                continue;
            }
            let w = u16::try_from(w).unwrap_or(1);
            if x.saturating_add(w) > max_x {
                break;
            }
            if let Some(i) = self.index(x, y) {
                let style = merge(self.cells[i].style, style);
                self.cells[i] = Cell { ch, style };
                if w == 2 {
                    self.cells[i + 1] = Cell { ch: '\0', style };
                }
            }
            x += w;
        }
        x
    }

    /// Darkens every cell, for the area behind a modal float.
    pub fn dim(&mut self, default_fg: Color, default_bg: Color) {
        let mix = |c: Color| {
            let f = |v: u8| u8::try_from(u16::from(v) * 2 / 5).unwrap_or(0);
            Color(f(c.0), f(c.1), f(c.2))
        };
        for c in &mut self.cells {
            c.style.fg = Some(mix(c.style.fg.unwrap_or(default_fg)));
            c.style.bg = Some(mix(c.style.bg.unwrap_or(default_bg)));
        }
    }

    /// The text of one row, for tests and plain dumps.
    pub fn row_text(&self, y: u16) -> String {
        (0..self.width)
            .filter_map(|x| self.cell(x, y))
            .filter(|c| c.ch != '\0')
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// The whole grid as text.
    pub fn text(&self) -> String {
        (0..self.height)
            .map(|y| self.row_text(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Writes the rows that differ from `prev` as escape codes.
    ///
    /// # Errors
    ///
    /// Returns the writer's error.
    pub fn write_diff<W: Write>(
        &self,
        prev: Option<&Self>,
        out: &mut W,
        truecolor: bool,
    ) -> io::Result<()> {
        let prev = prev.filter(|p| p.width == self.width && p.height == self.height);
        let w = usize::from(self.width);
        for y in 0..usize::from(self.height) {
            let row = &self.cells[y * w..(y + 1) * w];
            if prev.is_some_and(|p| p.cells[y * w..(y + 1) * w] == *row) {
                continue;
            }
            write!(out, "\x1b[{};1H\x1b[0m", y + 1)?;
            let mut current = BLANK.style;
            for cell in row {
                if cell.ch == '\0' {
                    continue;
                }
                if cell.style != current {
                    write_sgr(out, cell.style, truecolor)?;
                    current = cell.style;
                }
                let mut buf = [0u8; 4];
                out.write_all(cell.ch.encode_utf8(&mut buf).as_bytes())?;
            }
        }
        out.write_all(b"\x1b[0m")
    }
}

/// The nearest colour in the 256-colour palette (6x6x6 cube or grey ramp).
fn ansi256(c: Color) -> u8 {
    let (r, g, b) = (u16::from(c.0), u16::from(c.1), u16::from(c.2));
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max - min < 12 {
        if r < 8 {
            return 16;
        }
        if r > 238 {
            return 231;
        }
        return u8::try_from(232 + (r - 8) * 24 / 231).unwrap_or(255);
    }
    let q = |v: u16| (v * 5 + 127) / 255;
    u8::try_from(16 + 36 * q(r) + 6 * q(g) + q(b)).unwrap_or(255)
}

fn write_sgr<W: Write>(out: &mut W, s: Style, truecolor: bool) -> io::Result<()> {
    out.write_all(b"\x1b[0")?;
    if s.bold {
        out.write_all(b";1")?;
    }
    if s.italic {
        out.write_all(b";3")?;
    }
    if s.underline {
        out.write_all(b";4")?;
    }
    for (color, base) in [(s.fg, 38), (s.bg, 48)] {
        if let Some(c) = color {
            if truecolor {
                write!(out, ";{base};2;{};{};{}", c.0, c.1, c.2)?;
            } else {
                write!(out, ";{base};5;{}", ansi256(c))?;
            }
        }
    }
    out.write_all(b"m")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fg(c: Color) -> Style {
        Style {
            fg: Some(c),
            ..Style::default()
        }
    }

    #[test]
    fn put_clips_and_handles_wide_chars() {
        let mut g = Grid::new(6, 2);
        assert_eq!(g.put(0, 0, 6, "ab日本語", Style::default()), 6);
        assert_eq!(g.row_text(0), "ab日本");
        assert_eq!(g.cell(3, 0).unwrap().ch, '\0');
        // A wide char that does not fit is dropped whole.
        assert_eq!(g.put(0, 1, 3, "a日本", Style::default()), 3);
        assert_eq!(g.row_text(1), "a日");
        assert_eq!(g.put(4, 1, 5, "e\u{301}x", Style::default()), 5);
        g.put(9, 9, 20, "off screen", Style::default());
        assert_eq!(g.text(), "ab日本\na日 e");
    }

    #[test]
    fn styles_layer_and_diffs_skip_unchanged_rows() {
        let mut g = Grid::new(3, 2);
        let bg = Style {
            bg: Some(Color(1, 2, 3)),
            ..Style::default()
        };
        g.paint(0, 0, 3, 1, bg);
        g.put(0, 0, 3, "x", fg(Color(9, 9, 9)));
        let c = g.cell(0, 0).unwrap();
        assert_eq!(
            (c.style.fg, c.style.bg),
            (Some(Color(9, 9, 9)), Some(Color(1, 2, 3)))
        );

        let mut out = Vec::new();
        g.write_diff(None, &mut out, true).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("\x1b[1;1H") && s.contains("\x1b[2;1H"));
        assert!(s.contains("38;2;9;9;9;48;2;1;2;3"), "{s:?}");

        let prev = g.clone();
        g.put(0, 1, 3, "y", Style::default());
        let mut out = Vec::new();
        g.write_diff(Some(&prev), &mut out, false).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("\x1b[1;1H") && s.contains("\x1b[2;1H"), "{s:?}");

        g.dim(Color(200, 200, 200), Color(0, 0, 0));
        assert_eq!(g.cell(0, 0).unwrap().style.fg, Some(Color(3, 3, 3)));
    }

    #[test]
    fn palette_fallback() {
        assert_eq!(ansi256(Color(0, 0, 0)), 16);
        assert_eq!(ansi256(Color(255, 255, 255)), 231);
        assert_eq!(ansi256(Color(255, 0, 0)), 196);
        assert!((232..=255).contains(&ansi256(Color(128, 128, 128))));
    }
}
