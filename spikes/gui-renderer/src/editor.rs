//! Minimal editor model for the spike: Vec<String> lines, one cursor, one selection,
//! IME preedit, and one completion popup. The real editor uses the rope from the core
//! (#16+); this only exists to drive the renderer realistically.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    /// Byte offset into the line, always on a char boundary.
    pub col: usize,
}

#[derive(Clone, Copy, Debug)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

#[derive(Clone, Debug)]
pub enum Action {
    Move(Dir, bool),
    Insert(String),
    Backspace,
    Delete,
    Newline,
    ScrollBy(f32),
    Page(i32),
    DocStart,
    DocEnd,
    GotoLine(usize),
    TogglePopup,
    PopupMove(i32),
    PopupAccept,
    Escape,
    SelectAll,
    Click(Pos, bool),
    Drag(Pos),
}

pub const COMPLETIONS: &[&str] = &[
    "render_frame",
    "request_redraw",
    "rope_slice",
    "selection_ranges",
    "shape_line",
    "surface_config",
    "text_atlas",
    "viewport_update",
];

pub struct Editor {
    pub lines: Vec<String>,
    pub cur: Pos,
    pub anchor: Option<Pos>,
    /// Target column in chars for vertical motion.
    want_col: usize,
    pub scroll_y: f32,
    pub preedit: Option<(String, Option<(usize, usize)>)>,
    pub popup: Option<usize>,
}

fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map_or(0, |(b, _)| b)
}
fn next_boundary(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}
fn byte_of_char(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map_or(s.len(), |(b, _)| b)
}

impl Editor {
    pub fn new(text: String) -> Self {
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        Self {
            lines: if lines.is_empty() {
                vec![String::new()]
            } else {
                lines
            },
            cur: Pos::default(),
            anchor: None,
            want_col: 0,
            scroll_y: 0.0,
            preedit: None,
            popup: None,
        }
    }

    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        if a == self.cur {
            return None;
        }
        Some(if a < self.cur {
            (a, self.cur)
        } else {
            (self.cur, a)
        })
    }

    fn delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let tail = self.lines[e.line][e.col..].to_owned();
        self.lines[s.line].truncate(s.col);
        self.lines[s.line].push_str(&tail);
        self.lines.drain(s.line + 1..=e.line);
        self.cur = s;
        self.anchor = None;
        true
    }

    fn insert_str(&mut self, text: &str) {
        self.delete_selection();
        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or("");
        let line = &mut self.lines[self.cur.line];
        let tail = line.split_off(self.cur.col);
        line.push_str(first);
        self.cur.col = line.len();
        for p in parts {
            self.cur.line += 1;
            self.lines.insert(self.cur.line, p.to_owned());
            self.cur.col = p.len();
        }
        self.lines[self.cur.line].push_str(&tail);
    }

    fn sync_want_col(&mut self) {
        self.want_col = self.lines[self.cur.line][..self.cur.col].chars().count();
    }

    /// Apply an action. `line_h`/`view_h` are needed for scrolling. Returns true if
    /// the cursor should be scrolled into view.
    pub fn apply(&mut self, a: Action, line_h: f32, view_h: f32) {
        let mut reveal = true;
        match a {
            Action::Move(d, extend) => {
                if self.popup.is_some() && matches!(d, Dir::Up | Dir::Down) && !extend {
                    let delta = if matches!(d, Dir::Up) { -1 } else { 1 };
                    return self.apply(Action::PopupMove(delta), line_h, view_h);
                }
                if extend {
                    self.anchor.get_or_insert(self.cur);
                } else {
                    self.anchor = None;
                }
                let line = &self.lines[self.cur.line];
                match d {
                    Dir::Left => {
                        if self.cur.col > 0 {
                            self.cur.col = prev_boundary(line, self.cur.col);
                        } else if self.cur.line > 0 {
                            self.cur.line -= 1;
                            self.cur.col = self.lines[self.cur.line].len();
                        }
                        self.sync_want_col();
                    }
                    Dir::Right => {
                        if self.cur.col < line.len() {
                            self.cur.col = next_boundary(line, self.cur.col);
                        } else if self.cur.line + 1 < self.lines.len() {
                            self.cur.line += 1;
                            self.cur.col = 0;
                        }
                        self.sync_want_col();
                    }
                    Dir::Up | Dir::Down => {
                        let up = matches!(d, Dir::Up);
                        if up && self.cur.line > 0 {
                            self.cur.line -= 1;
                        } else if !up && self.cur.line + 1 < self.lines.len() {
                            self.cur.line += 1;
                        }
                        self.cur.col = byte_of_char(&self.lines[self.cur.line], self.want_col);
                    }
                    Dir::Home => {
                        self.cur.col = 0;
                        self.sync_want_col();
                    }
                    Dir::End => {
                        self.cur.col = line.len();
                        self.sync_want_col();
                    }
                }
            }
            Action::Insert(t) => {
                self.insert_str(&t);
                self.sync_want_col();
            }
            Action::Newline => {
                let indent: String = self.lines[self.cur.line]
                    .chars()
                    .take_while(|c| *c == ' ')
                    .collect();
                self.insert_str(&format!("\n{indent}"));
                self.sync_want_col();
            }
            Action::Backspace => {
                if !self.delete_selection() {
                    if self.cur.col > 0 {
                        let p = prev_boundary(&self.lines[self.cur.line], self.cur.col);
                        self.lines[self.cur.line].replace_range(p..self.cur.col, "");
                        self.cur.col = p;
                    } else if self.cur.line > 0 {
                        let l = self.lines.remove(self.cur.line);
                        self.cur.line -= 1;
                        self.cur.col = self.lines[self.cur.line].len();
                        self.lines[self.cur.line].push_str(&l);
                    }
                }
                self.sync_want_col();
            }
            Action::Delete => {
                if !self.delete_selection() {
                    let line = &self.lines[self.cur.line];
                    if self.cur.col < line.len() {
                        let n = next_boundary(line, self.cur.col);
                        self.lines[self.cur.line].replace_range(self.cur.col..n, "");
                    } else if self.cur.line + 1 < self.lines.len() {
                        let l = self.lines.remove(self.cur.line + 1);
                        self.lines[self.cur.line].push_str(&l);
                    }
                }
            }
            Action::ScrollBy(dy) => {
                self.scroll_y += dy;
                reveal = false;
            }
            Action::Page(n) => {
                let rows = (view_h / line_h).floor().max(1.0) as i64 - 1;
                let target = (self.cur.line as i64 + n as i64 * rows)
                    .clamp(0, self.lines.len() as i64 - 1) as usize;
                self.scroll_y += (target as f32 - self.cur.line as f32) * line_h;
                self.anchor = None;
                self.cur.line = target;
                self.cur.col = byte_of_char(&self.lines[target], self.want_col);
            }
            Action::DocStart => {
                self.anchor = None;
                self.cur = Pos::default();
                self.sync_want_col();
            }
            Action::DocEnd => {
                self.anchor = None;
                let l = self.lines.len() - 1;
                self.cur = Pos {
                    line: l,
                    col: self.lines[l].len(),
                };
                self.sync_want_col();
            }
            Action::GotoLine(l) => {
                self.anchor = None;
                self.cur = Pos {
                    line: l.min(self.lines.len() - 1),
                    col: 0,
                };
                self.scroll_y = (self.cur.line as f32 * line_h - view_h / 2.0).max(0.0);
                self.sync_want_col();
            }
            Action::TogglePopup => {
                self.popup = if self.popup.is_some() { None } else { Some(0) };
            }
            Action::PopupMove(d) => {
                if let Some(i) = self.popup.as_mut() {
                    *i = (*i as i32 + d).rem_euclid(COMPLETIONS.len() as i32) as usize;
                }
                reveal = false;
            }
            Action::PopupAccept => {
                if let Some(i) = self.popup.take() {
                    self.insert_str(COMPLETIONS[i]);
                    self.sync_want_col();
                }
            }
            Action::Escape => {
                self.popup = None;
                self.anchor = None;
            }
            Action::SelectAll => {
                self.anchor = Some(Pos::default());
                let l = self.lines.len() - 1;
                self.cur = Pos {
                    line: l,
                    col: self.lines[l].len(),
                };
            }
            Action::Click(p, extend) => {
                self.popup = None;
                if extend {
                    self.anchor.get_or_insert(self.cur);
                } else {
                    self.anchor = Some(p);
                }
                self.cur = p;
                self.sync_want_col();
            }
            Action::Drag(p) => {
                self.cur = p;
                self.sync_want_col();
            }
        }
        if reveal {
            let y = self.cur.line as f32 * line_h;
            if y < self.scroll_y {
                self.scroll_y = y;
            } else if y + line_h > self.scroll_y + view_h {
                self.scroll_y = y + line_h - view_h;
            }
        }
        let max = (self.lines.len() as f32 * line_h - line_h).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max);
    }

    /// Text to display for a line, with IME preedit spliced in at the cursor.
    pub fn display_line(&self, i: usize) -> std::borrow::Cow<'_, str> {
        let l = &self.lines[i];
        match &self.preedit {
            Some((p, _)) if i == self.cur.line && !p.is_empty() => {
                let mut s = String::with_capacity(l.len() + p.len());
                s.push_str(&l[..self.cur.col]);
                s.push_str(p);
                s.push_str(&l[self.cur.col..]);
                s.into()
            }
            _ => l.as_str().into(),
        }
    }
}

/// Deterministic 10k-line Rust-ish file with a few long and non-ASCII lines.
pub fn synthetic_source(n: usize) -> String {
    let words = [
        "frame", "glyph", "atlas", "buffer", "cursor", "layout", "shape", "queue",
    ];
    let mut out = String::with_capacity(n * 48);
    let mut seed: u32 = 0x9e37_79b9;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for i in 0..n {
        let w = words[(rnd() % 8) as usize];
        let v = words[(rnd() % 8) as usize];
        let line = match i % 23 {
            0 => format!(
                "pub fn {w}_{i}(&mut self, {v}: &{}) -> Result<(), Error> {{",
                "Buffer"
            ),
            1 => format!("    // TODO({i}): reuse the {w} cache when the {v} is unchanged"),
            7 => format!("    let {w} = self.{v}.get({i}).unwrap_or(\"default {w} string\");"),
            11 => format!("    // non-ascii: café naïve Ελληνικά 日本語 → {w}"),
            13 => format!(
                "    let long_{i} = [{}];",
                (0..30)
                    .map(|k| (k * i % 997).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            17 => "    }".to_owned(),
            22 => "}".to_owned(),
            _ => format!(
                "    if self.{w}.len() > {} {{ self.{v}.push({w} as u32 + {}); }}",
                rnd() % 100,
                rnd() % 1000
            ),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}
