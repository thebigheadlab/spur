//! The palette: rows from the workspace files, recent documents, pack actions and
//! go-to-line, matched as the query changes. Files are matched off the session
//! thread by one `nucleo` instance per client; the small sources are matched here.
//! The selection is kept as a row id, so what runs is what was shown.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Nucleo, Utf32Str};
use spur_rpc::{Item, ItemKind, ListView};
use unicode_segmentation::UnicodeSegmentation as _;

use super::index::{Index, Path32, Status};
use super::pack::Pack;

/// Rows each section of the `all` scope shows at most.
pub const SECTION_ROWS: usize = 50;
/// Recent documents shown in `all` with an empty query.
pub const RECENT_IN_ALL: usize = 10;
/// Recent documents kept per client.
pub const MAX_RECENT: usize = 50;

/// Best file matches searched for a selected file that moved.
const FIND_FILES: u32 = 10_000;

/// Called with a client's id when its matcher has news.
#[derive(Clone)]
pub struct Notify(pub Arc<dyn Fn(u64) + Send + Sync>);

impl std::fmt::Debug for Notify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Notify")
    }
}

/// One client's file matcher.
pub struct Matcher {
    nucleo: Nucleo<Path32>,
    /// A `MatchReady` is queued and not yet handled.
    pending: Arc<AtomicBool>,
    /// The query the matcher was last given.
    query: String,
    /// The index walk it was filled from.
    pub index_generation: u64,
    /// For match indices of shown rows.
    indices: nucleo::Matcher,
}

impl std::fmt::Debug for Matcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Matcher")
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

impl Matcher {
    /// A matcher for client `conn`, filled from `index`.
    pub fn new(conn: u64, notify: &Notify, index: &Index) -> Self {
        let pending = Arc::new(AtomicBool::new(false));
        let (p, n) = (pending.clone(), notify.clone());
        let cb = Arc::new(move || {
            if !p.swap(true, Ordering::AcqRel) {
                (n.0)(conn);
            }
        });
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
        let nucleo = Nucleo::new(Config::DEFAULT.match_paths(), cb, Some(threads), 1);
        let index_generation = index.attach(conn, nucleo.injector());
        Self {
            nucleo,
            pending,
            query: String::new(),
            index_generation,
            indices: nucleo::Matcher::new(Config::DEFAULT.match_paths()),
        }
    }

    /// Refills from a newer walk of `index`.
    pub fn refill(&mut self, conn: u64, index: &Index) {
        self.nucleo.restart(true);
        self.index_generation = index.attach(conn, self.nucleo.injector());
        self.nucleo.tick(0);
    }

    /// Gives the matcher a new query and starts matching; returns at once.
    pub fn set_query(&mut self, query: &str) {
        if query == self.query {
            return;
        }
        // Only an extension of the old query may reuse the old matches.
        let append = !self.query.is_empty() && query.starts_with(self.query.as_str());
        self.nucleo
            .pattern
            .reparse(0, query, CaseMatching::Smart, Normalization::Smart, append);
        query.clone_into(&mut self.query);
        self.nucleo.tick(0);
    }

    /// Takes in the latest results (after a notify). True if they changed.
    pub fn ready(&mut self) -> bool {
        self.pending.store(false, Ordering::Release);
        self.nucleo.tick(0).changed
    }

    /// Matched files.
    pub fn count(&self) -> u32 {
        self.nucleo.snapshot().matched_item_count()
    }

    /// The `n`th matched path.
    fn path(&self, n: u32) -> Option<&Path32> {
        self.nucleo.snapshot().get_matched_item(n).map(|i| i.data)
    }

    /// Indices of the query's chars in `path`.
    fn match_indices(&mut self, path: &str) -> Vec<u32> {
        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut out = Vec::new();
        pattern.indices(Utf32Str::new(path, &mut buf), &mut self.indices, &mut out);
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// A client's palette state (cloned with its UI state).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteState {
    /// The source shown.
    pub scope: String,
    /// The scope it was opened with (backspace in an empty query returns to it).
    pub open_scope: String,
    /// The selected row's id.
    pub selected: Option<String>,
    /// Its index when last seen.
    pub index: u32,
    /// The selection follows the first row (until the person moves it).
    pub follow: bool,
    /// First row shown.
    pub offset: u32,
    /// What the selected row ran when last drawn or chosen, for the generation
    /// it was drawn in: Enter runs this, so a tick in between cannot change it.
    pub shown: Option<(u64, Option<Run>)>,
    /// Rises with each query or scope change.
    pub generation: u64,
}

/// What running a row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    /// Opens a document.
    Open(PathBuf),
    /// Runs a pack action in the remembered region.
    Action(String),
    /// Moves the cursor: line, then column (1-based on screen, 0-based here).
    Line(usize, Option<usize>),
}

/// A row built here (every source but the matched files).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    id: String,
    text: String,
    detail: Option<String>,
    key: Option<String>,
    matches: Vec<[u32; 2]>,
    detail_matches: Vec<[u32; 2]>,
    kind: ItemKind,
    run: Option<Run>,
}

impl Row {
    fn section(id: &str, text: String) -> Self {
        Self {
            id: format!("s:{id}"),
            text,
            detail: None,
            key: None,
            matches: Vec::new(),
            detail_matches: Vec::new(),
            kind: ItemKind::Section,
            run: None,
        }
    }
}

enum Seg {
    Rows(Vec<Row>),
    /// Matched files `0..count`.
    Files(u32),
}

/// An action row's source data.
pub struct ActionRow {
    /// Action id.
    pub id: String,
    /// Title.
    pub title: String,
    /// Key hint.
    pub key: Option<String>,
}

/// What the rows are built from.
pub struct Input<'a> {
    /// The pack (messages).
    pub pack: &'a Pack,
    /// The query.
    pub query: &'a str,
    /// The scope.
    pub scope: &'a str,
    /// Actions that apply, in pack order, with their hints.
    pub actions: Vec<ActionRow>,
    /// Recent documents, newest first (current excluded).
    pub recent: Vec<PathBuf>,
    /// The workspace root.
    pub root: &'a Path,
    /// The index.
    pub status: Status,
    /// Lines in the current document.
    pub lines: usize,
}

/// The rows of one frame.
pub struct Rows {
    segs: Vec<Seg>,
    /// The note under the query (empty state, indexing).
    pub note: String,
}

impl Rows {
    /// Builds the rows; matching the small sources happens here.
    #[allow(clippy::many_single_char_names)]
    pub fn build(x: &Input<'_>, m: Option<&Matcher>) -> Self {
        let p = x.pack;
        let q = x.query.trim();
        let files = m.map_or(0, Matcher::count);
        let mut segs = Vec::new();
        match x.scope {
            "all" => {
                if q.is_empty() {
                    let recent = recent_rows(x, q, RECENT_IN_ALL);
                    if !recent.is_empty() {
                        let mut r = vec![Row::section(
                            "recent",
                            p.message("palette.section.recent", &[]),
                        )];
                        r.extend(recent);
                        segs.push(Seg::Rows(r));
                    }
                } else if files > 0 {
                    let n = files.min(u32::try_from(SECTION_ROWS).unwrap_or(u32::MAX));
                    segs.push(Seg::Rows(vec![Row::section(
                        "files",
                        p.message("palette.section.files", &[]),
                    )]));
                    segs.push(Seg::Files(n));
                }
                let mut actions = action_rows(x, q);
                actions.truncate(SECTION_ROWS);
                if !actions.is_empty() {
                    let mut r = vec![Row::section(
                        "actions",
                        p.message("palette.section.actions", &[]),
                    )];
                    r.extend(actions);
                    segs.push(Seg::Rows(r));
                }
            }
            "files" => {
                if q.is_empty() {
                    segs.push(Seg::Rows(recent_rows(x, q, MAX_RECENT)));
                }
                segs.push(Seg::Files(files));
            }
            "recent" => segs.push(Seg::Rows(recent_rows(x, q, MAX_RECENT))),
            "actions" => segs.push(Seg::Rows(action_rows(x, q))),
            "line" => segs.push(Seg::Rows(line_row(x, q).into_iter().collect())),
            _ => {}
        }
        let mut rows = Self {
            segs,
            note: String::new(),
        };
        let uses_files = matches!(x.scope, "all" | "files");
        rows.note = if rows.total() == 0 {
            if x.scope == "line" {
                p.message("palette.line.hint", &[])
            } else if uses_files && !x.root.is_dir() {
                p.message("palette.no_folder", &[])
            } else if uses_files && !x.status.done {
                p.message(
                    "palette.indexing",
                    &[("count", &x.status.count.to_string())],
                )
            } else {
                p.message("palette.no_matches", &[])
            }
        } else if uses_files && !x.status.done {
            p.message(
                "palette.indexing",
                &[("count", &x.status.count.to_string())],
            )
        } else if uses_files && x.status.capped {
            p.message("palette.capped", &[("count", &x.status.count.to_string())])
        } else {
            String::new()
        };
        rows
    }

    /// Rows in all.
    pub fn total(&self) -> u32 {
        self.segs
            .iter()
            .map(|s| match s {
                Seg::Rows(r) => u32::try_from(r.len()).unwrap_or(u32::MAX),
                Seg::Files(n) => *n,
            })
            .sum()
    }

    /// Row `i`: its id, kind and what running it does.
    fn at(
        &self,
        i: u32,
        m: Option<&Matcher>,
        root: &Path,
    ) -> Option<(String, ItemKind, Option<Run>)> {
        let mut i = i;
        for s in &self.segs {
            match s {
                Seg::Rows(r) => {
                    let n = u32::try_from(r.len()).unwrap_or(u32::MAX);
                    if i < n {
                        let r = &r[i as usize];
                        return Some((r.id.clone(), r.kind, r.run.clone()));
                    }
                    i -= n;
                }
                Seg::Files(n) => {
                    if i < *n {
                        let path = m?.path(i)?;
                        return Some((
                            format!("f:{path}"),
                            ItemKind::Entry,
                            Some(Run::Open(root.join(&**path))),
                        ));
                    }
                    i -= n;
                }
            }
        }
        None
    }

    /// The first selectable row at or after `from` (or before it, going back).
    fn selectable(
        &self,
        from: i64,
        forward: bool,
        m: Option<&Matcher>,
        root: &Path,
    ) -> Option<u32> {
        let total = i64::from(self.total());
        let mut i = from.clamp(0, (total - 1).max(0));
        while (0..total).contains(&i) {
            let n = u32::try_from(i).ok()?;
            if self
                .at(n, m, root)
                .is_some_and(|(_, k, _)| k != ItemKind::Section)
            {
                return Some(n);
            }
            i += if forward { 1 } else { -1 };
        }
        None
    }

    /// Fixes the selection after the rows changed: the stored id if it is still
    /// where it was (or elsewhere in the rows), else the first selectable row.
    pub fn settle(&self, st: &mut PaletteState, m: Option<&Matcher>, root: &Path) {
        if !st.follow
            && let Some(id) = &st.selected
        {
            if self.at(st.index, m, root).is_some_and(|(r, _, _)| &r == id) {
                return;
            }
            if let Some(i) = self.find(id, m) {
                st.index = i;
                return;
            }
        }
        if let Some(i) = self.selectable(0, true, m, root) {
            st.index = i;
            st.selected = self.at(i, m, root).map(|(id, _, _)| id);
            st.offset = 0;
        } else {
            st.index = 0;
            st.selected = None;
        }
    }

    fn find(&self, id: &str, m: Option<&Matcher>) -> Option<u32> {
        let mut base = 0u32;
        for s in &self.segs {
            match s {
                Seg::Rows(r) => {
                    if let Some(i) = r.iter().position(|x| x.id == id) {
                        return Some(base + u32::try_from(i).ok()?);
                    }
                    base += u32::try_from(r.len()).ok()?;
                }
                Seg::Files(n) => {
                    // A file that moved is looked for among the best matches.
                    if let (Some(m), Some(path)) = (m, id.strip_prefix("f:")) {
                        let found = (0..(*n).min(FIND_FILES))
                            .find(|&k| m.path(k).is_some_and(|p| &**p == path));
                        if let Some(k) = found {
                            return Some(base + k);
                        }
                    }
                    base += n;
                }
            }
        }
        None
    }

    /// Moves the selection by `by` rows, skipping section rows.
    pub fn move_by(
        &self,
        st: &mut PaletteState,
        by: i64,
        height: u32,
        m: Option<&Matcher>,
        root: &Path,
    ) {
        if by == 0 {
            return;
        }
        let from = i64::from(st.index) + by;
        let found = self
            .selectable(from, by > 0, m, root)
            .or_else(|| self.selectable(from, by < 0, m, root));
        if let Some(i) = found {
            self.choose(st, i, m, root);
            keep_visible(st, height);
        }
    }

    /// Selects the row `id` if it is in the window shown (a click). False if not.
    pub fn select_id(
        &self,
        st: &mut PaletteState,
        id: &str,
        height: u32,
        m: Option<&Matcher>,
        root: &Path,
    ) -> bool {
        let end = st.offset.saturating_add(height.max(1)).min(self.total());
        for i in st.offset..end {
            if self
                .at(i, m, root)
                .is_some_and(|(rid, kind, _)| rid == id && kind != ItemKind::Section)
            {
                self.choose(st, i, m, root);
                return true;
            }
        }
        false
    }

    /// The person picked row `i`: it stays selected and is what Enter runs.
    fn choose(&self, st: &mut PaletteState, i: u32, m: Option<&Matcher>, root: &Path) {
        let row = self.at(i, m, root);
        st.index = i;
        st.selected = row.as_ref().map(|(id, _, _)| id.clone());
        st.shown = Some((st.generation, row.and_then(|(_, _, r)| r)));
        st.follow = false;
    }

    /// Records what the selected row runs as drawn (after [`Rows::settle`]).
    pub fn mark_shown(&self, st: &mut PaletteState, m: Option<&Matcher>, root: &Path) {
        st.shown = Some((st.generation, self.run(st, m, root)));
    }

    /// What Enter runs: the row as last drawn or chosen in this generation, else
    /// the selection settled now (the query changed since the last frame).
    pub fn to_run(&self, st: &mut PaletteState, m: Option<&Matcher>, root: &Path) -> Option<Run> {
        if let Some((g, run)) = &st.shown
            && *g == st.generation
        {
            return run.clone();
        }
        self.settle(st, m, root);
        self.run(st, m, root)
    }

    /// Scrolls the window by `lines`, leaving the selection.
    pub fn scroll(&self, st: &mut PaletteState, lines: i32, height: u32) {
        let max = self.total().saturating_sub(height.max(1));
        let o = i64::from(st.offset) + i64::from(lines);
        st.offset = u32::try_from(o.clamp(0, i64::from(max))).unwrap_or(0);
        st.follow = false;
    }

    /// What the selected row runs, if it is still the row with the stored id.
    pub fn run(&self, st: &PaletteState, m: Option<&Matcher>, root: &Path) -> Option<Run> {
        let (id, _, run) = self.at(st.index, m, root)?;
        (Some(&id) == st.selected.as_ref()).then_some(run).flatten()
    }

    /// The window of `height` rows, with match ranges for the files shown.
    pub fn window(&self, st: &mut PaletteState, height: u32, m: Option<&mut Matcher>) -> ListView {
        let total = self.total();
        let h = height.max(1);
        st.offset = st.offset.min(total.saturating_sub(h));
        let mut m = m;
        let mut items = Vec::new();
        let end = st.offset.saturating_add(h).min(total);
        let mut i = 0u32;
        for s in &self.segs {
            if i >= end {
                break;
            }
            match s {
                Seg::Rows(r) => {
                    for row in r {
                        if (st.offset..end).contains(&i) {
                            items.push(item(row));
                        }
                        i += 1;
                    }
                }
                Seg::Files(n) => {
                    let lo = st.offset.max(i);
                    let hi = end.min(i + n);
                    if let Some(m) = m.as_deref_mut() {
                        for k in lo..hi {
                            if let Some(path) = m.path(k - i).cloned() {
                                items.push(file_item(&path, m));
                            }
                        }
                    }
                    i += n;
                }
            }
        }
        let selected = st
            .selected
            .is_some()
            .then_some(st.index)
            .filter(|i| *i < total);
        ListView {
            offset: st.offset,
            total,
            selected,
            items,
            generation: st.generation,
        }
    }
}

fn keep_visible(st: &mut PaletteState, height: u32) {
    let h = height.max(1);
    if st.index < st.offset {
        st.offset = st.index;
    } else if st.index >= st.offset + h {
        st.offset = st.index + 1 - h;
    }
}

fn item(r: &Row) -> Item {
    Item {
        id: r.id.clone(),
        text: r.text.clone(),
        kind: r.kind,
        detail: r.detail.clone(),
        key: r.key.clone(),
        matches: r.matches.clone(),
        detail_matches: r.detail_matches.clone(),
        ..Item::default()
    }
}

/// A matched file: name, then its folder, with the matched chars split between them.
fn file_item(path: &str, m: &mut Matcher) -> Item {
    let indices = m.match_indices(path);
    let (folder, name) = split(path);
    let folder_chars = if folder.is_empty() {
        0
    } else {
        u32::try_from(folder.graphemes(true).count() + 1).unwrap_or(u32::MAX)
    };
    let name_idx: Vec<u32> = indices
        .iter()
        .filter(|i| **i >= folder_chars)
        .map(|i| i - folder_chars)
        .collect();
    let folder_idx: Vec<u32> = indices
        .iter()
        .copied()
        .filter(|i| *i < folder_chars)
        .collect();
    Item {
        id: format!("f:{path}"),
        text: name.to_owned(),
        kind: ItemKind::Entry,
        detail: (!folder.is_empty()).then(|| folder.to_owned()),
        matches: ranges(&name_idx),
        detail_matches: ranges(&folder_idx),
        ..Item::default()
    }
}

/// (folder, name) of a `/`-separated path.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// Sorted char indices as `[start, end)` runs.
fn ranges(idx: &[u32]) -> Vec<[u32; 2]> {
    let mut out: Vec<[u32; 2]> = Vec::new();
    for &i in idx {
        match out.last_mut() {
            Some(r) if r[1] == i => r[1] = i + 1,
            _ => out.push([i, i + 1]),
        }
    }
    out
}

/// Matches `q` against each haystack; the best first, ties to the shorter, then
/// to the earlier. An empty query keeps every item in order.
fn fuzzy(hay: &[String], q: &str) -> Vec<(usize, Vec<u32>)> {
    if q.is_empty() {
        return (0..hay.len()).map(|i| (i, Vec::new())).collect();
    }
    let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
    let mut matcher = nucleo::Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut hits: Vec<(u32, usize, Vec<u32>)> = Vec::new();
    for (i, h) in hay.iter().enumerate() {
        let mut idx = Vec::new();
        if let Some(score) = pattern.indices(Utf32Str::new(h, &mut buf), &mut matcher, &mut idx) {
            idx.sort_unstable();
            idx.dedup();
            hits.push((score, i, idx));
        }
    }
    hits.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| hay[a.1].len().cmp(&hay[b.1].len()))
            .then_with(|| a.1.cmp(&b.1))
    });
    hits.into_iter().map(|(_, i, idx)| (i, idx)).collect()
}

fn recent_rows(x: &Input<'_>, q: &str, max: usize) -> Vec<Row> {
    let shown: Vec<String> = x
        .recent
        .iter()
        .map(|p| {
            p.strip_prefix(x.root).map_or_else(
                |_| p.to_string_lossy().into_owned(),
                |r| r.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"),
            )
        })
        .collect();
    fuzzy(&shown, q)
        .into_iter()
        .take(max)
        .map(|(i, idx)| {
            let path = &shown[i];
            let (folder, name) = split(path);
            let folder_chars = if folder.is_empty() {
                0
            } else {
                u32::try_from(folder.graphemes(true).count() + 1).unwrap_or(u32::MAX)
            };
            let name_idx: Vec<u32> = idx
                .iter()
                .filter(|i| **i >= folder_chars)
                .map(|i| i - folder_chars)
                .collect();
            let folder_idx: Vec<u32> = idx.iter().copied().filter(|i| *i < folder_chars).collect();
            Row {
                id: format!("r:{path}"),
                text: name.to_owned(),
                detail: (!folder.is_empty()).then(|| folder.to_owned()),
                key: None,
                matches: ranges(&name_idx),
                detail_matches: ranges(&folder_idx),
                kind: ItemKind::Entry,
                run: Some(Run::Open(x.recent[i].clone())),
            }
        })
        .collect()
}

fn action_rows(x: &Input<'_>, q: &str) -> Vec<Row> {
    let titles: Vec<String> = x.actions.iter().map(|a| a.title.clone()).collect();
    fuzzy(&titles, q)
        .into_iter()
        .map(|(i, idx)| {
            let a = &x.actions[i];
            Row {
                id: format!("a:{}", a.id),
                text: a.title.clone(),
                detail: None,
                key: a.key.clone(),
                matches: ranges(&idx),
                detail_matches: Vec::new(),
                kind: ItemKind::Entry,
                run: Some(Run::Action(a.id.clone())),
            }
        })
        .collect()
}

/// `120` or `120:8` (1-based).
fn line_row(x: &Input<'_>, q: &str) -> Option<Row> {
    let (l, c) = match q.split_once(':') {
        Some((l, c)) => (l, Some(c)),
        None => (q, None),
    };
    let line: usize = l.trim().parse().ok().filter(|n| *n > 0)?;
    let col: Option<usize> = match c.map(str::trim) {
        Some("") | None => None,
        Some(c) => Some(c.parse().ok().filter(|n| *n > 0)?),
    };
    let last = x.lines.max(1);
    let (text, run) = if line > last {
        (
            x.pack
                .message("palette.line.last", &[("line", &last.to_string())]),
            Run::Line(last - 1, None),
        )
    } else if let Some(col) = col {
        (
            x.pack.message(
                "palette.line.go_col",
                &[("line", &line.to_string()), ("col", &col.to_string())],
            ),
            Run::Line(line - 1, Some(col - 1)),
        )
    } else {
        (
            x.pack
                .message("palette.line.go", &[("line", &line.to_string())]),
            Run::Line(line - 1, None),
        )
    };
    Some(Row {
        id: "l".into(),
        text,
        detail: None,
        key: None,
        matches: Vec::new(),
        detail_matches: Vec::new(),
        kind: ItemKind::Entry,
        run: Some(run),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ux::index::Index;

    /// Waits for the walk and the match to settle; the matched paths in order.
    fn top(m: &mut Matcher, idx: &Index, q: &str) -> Vec<String> {
        m.set_query(q);
        let t = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let running = m.nucleo.tick(10).running;
            if idx.status().done
                && !running
                && m.nucleo.snapshot().item_count() as usize == idx.status().count
            {
                break;
            }
            assert!(t.elapsed().as_secs() < 10, "matching did not finish");
        }
        (0..m.count().min(5))
            .filter_map(|i| m.path(i))
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn file_matches_rank_word_starts_first_then_shorter_paths() {
        let dir = std::env::temp_dir().join(format!("spur-palette-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in [
            "src/main.rs",
            "src/highlight.rs",
            "src/ui/main_view.rs",
            "main/readme.md",
            "docs/highlights/intro.md",
            "crates/spur-server/src/highlight.rs",
            "tests/hlt_cases.rs",
        ] {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let mut idx = Index::new(dir.clone());
        idx.walk(None);
        let notify = Notify(Arc::new(|_| {}));
        let mut m = Matcher::new(1, &notify, &idx);
        assert_eq!(
            top(&mut m, &idx, "mainrs"),
            ["src/main.rs", "src/ui/main_view.rs"]
        );
        assert_eq!(
            top(&mut m, &idx, "highlight"),
            // A hit at the start of a folder name scores like one at the start of
            // the file name; the shorter path wins the tie.
            [
                "src/highlight.rs",
                "docs/highlights/intro.md",
                "crates/spur-server/src/highlight.rs",
            ]
        );
        assert_eq!(top(&mut m, &idx, "hlt")[0], "tests/hlt_cases.rs");
        assert!(top(&mut m, &idx, "zzzz").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ranges_join_runs() {
        assert_eq!(ranges(&[0, 1, 2, 5, 7, 8]), [[0, 3], [5, 6], [7, 9]]);
        assert!(ranges(&[]).is_empty());
    }

    #[test]
    fn small_sources_rank_best_first_then_shorter() {
        let hay: Vec<String> = ["Toggle sidebar", "Show Files", "Save", "Sidebar files"]
            .into_iter()
            .map(String::from)
            .collect();
        let got: Vec<usize> = fuzzy(&hay, "sf").into_iter().map(|(i, _)| i).collect();
        assert_eq!(got[0], 1);
        let all: Vec<usize> = fuzzy(&hay, "").into_iter().map(|(i, _)| i).collect();
        assert_eq!(all, [0, 1, 2, 3]);
        assert!(fuzzy(&hay, "zzzz").is_empty());
    }
}
