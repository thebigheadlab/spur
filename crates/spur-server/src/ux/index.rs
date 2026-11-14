//! The workspace file index for the palette: a walker thread lists the files under
//! the root (honouring `.gitignore` and `.ignore`, hidden files skipped, links not
//! followed) and feeds every registered matcher directly, so the session thread
//! never pushes paths itself.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use nucleo::{Injector, Utf32String};

use super::palette::Notify;

/// Most files the index lists.
pub const MAX_FILES: usize = 200_000;

/// A walk older than this is redone when the palette opens.
pub const STALE: Duration = Duration::from_secs(30);

/// Paths sent to the matchers at a time.
const BATCH: usize = 512;

/// A path in the index: relative to the root, `/`-separated.
pub type Path32 = Arc<str>;

/// Fills a matcher's one column with the path.
pub fn fill(p: &Path32, cols: &mut [Utf32String]) {
    cols[0] = p.as_ref().into();
}

#[derive(Default)]
struct Shared {
    /// Rises with each walk; a matcher filled from an older one is restarted.
    generation: u64,
    paths: Vec<Path32>,
    /// The same paths, to look one up.
    known: std::collections::HashSet<Path32>,
    done: bool,
    capped: bool,
    /// Matchers fed by the walk, by client.
    sinks: Vec<(u64, Injector<Path32>)>,
}

/// The index of one session.
pub struct Index {
    root: PathBuf,
    shared: Arc<Mutex<Shared>>,
    /// Stops the current walk.
    stop: Arc<AtomicBool>,
    started: Option<Instant>,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the palette shows about the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// The walk generation.
    pub generation: u64,
    /// Files listed so far.
    pub count: usize,
    /// The walk finished.
    pub done: bool,
    /// It stopped at [`MAX_FILES`].
    pub capped: bool,
}

impl Index {
    /// An index of `root`; nothing is walked until [`Index::walk`].
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            shared: Arc::new(Mutex::new(Shared::default())),
            stop: Arc::new(AtomicBool::new(false)),
            started: None,
        }
    }

    /// The root folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the walk is.
    pub fn status(&self) -> Status {
        let s = lock(&self.shared);
        Status {
            generation: s.generation,
            count: s.paths.len(),
            done: s.done,
            capped: s.capped,
        }
    }

    /// True if never walked, or the last walk started more than [`STALE`] ago.
    pub fn stale(&self) -> bool {
        self.started.is_none_or(|t| t.elapsed() > STALE)
    }

    /// Starts a new walk; the old one stops. The first walk (or one after an
    /// empty result) streams: every registered matcher is dropped (its owner
    /// refills it and registers again), the generation rises and paths reach the
    /// matchers as they are found. A later walk runs aside while the old paths keep
    /// serving, and swaps in when it ends: then the generation rises and the
    /// matchers are dropped. When it ends, `done` is called for each client whose
    /// matcher it fed (or dropped), so the owner can refill.
    pub fn walk(&mut self, done: Option<Notify>) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        self.started = Some(Instant::now());
        let (generation, stream) = {
            let mut s = lock(&self.shared);
            if s.paths.is_empty() {
                s.generation += 1;
                s.done = false;
                s.capped = false;
                s.sinks.clear();
                (s.generation, true)
            } else {
                (s.generation, false)
            }
        };
        let shared = self.shared.clone();
        let root = self.root.clone();
        let spawned = std::thread::Builder::new()
            .name("spur-index".into())
            .spawn(move || {
                let clients = if stream {
                    walk(&root, &shared, &stop, generation);
                    lock(&shared).sinks.iter().map(|(c, _)| *c).collect()
                } else {
                    let Some((paths, capped)) = collect(&root, &stop) else {
                        return;
                    };
                    let mut s = lock(&shared);
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    s.generation += 1;
                    s.known = paths.iter().cloned().collect();
                    s.paths = paths;
                    s.done = true;
                    s.capped = capped;
                    std::mem::take(&mut s.sinks)
                        .into_iter()
                        .map(|(c, _)| c)
                        .collect::<Vec<u64>>()
                };
                if let Some(n) = done {
                    for c in clients {
                        (n.0)(c);
                    }
                }
            });
        if spawned.is_err() {
            lock(&self.shared).done = true;
        }
        generation
    }

    /// Registers a matcher's injector: the paths found so far are pushed on a
    /// helper thread, later ones by the walker. Returns the generation it follows.
    pub fn attach(&self, client: u64, injector: Injector<Path32>) -> u64 {
        let (existing, generation) = {
            let mut s = lock(&self.shared);
            s.sinks.retain(|(c, _)| *c != client);
            s.sinks.push((client, injector.clone()));
            (s.paths.clone(), s.generation)
        };
        if !existing.is_empty() {
            let _ = std::thread::Builder::new()
                .name("spur-index-fill".into())
                .spawn(move || {
                    for p in existing {
                        injector.push(p, fill);
                    }
                });
        }
        generation
    }

    /// Stops feeding `client`'s matcher.
    pub fn detach(&self, client: u64) {
        lock(&self.shared).sinks.retain(|(c, _)| *c != client);
    }

    /// A file was saved: if the index does not list it, the next palette open
    /// walks again (the walk decides whether ignore rules hide it).
    pub fn saved(&mut self, rel: &str) {
        if !lock(&self.shared).known.contains(rel) {
            self.started = None;
        }
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Lists the files under `root` into one vector; `None` if stopped.
fn collect(root: &Path, stop: &AtomicBool) -> Option<(Vec<Path32>, bool)> {
    let mut paths = Vec::new();
    for rel in files(root) {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        paths.push(rel);
        if paths.len() >= MAX_FILES {
            return Some((paths, true));
        }
    }
    Some((paths, false))
}

/// The files under `root`, relative and `/`-separated.
fn files(root: &Path) -> impl Iterator<Item = Path32> + '_ {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .follow_links(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .ignore(true)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(move |e| {
            let rel = e.path().strip_prefix(root).ok()?.to_str()?;
            Some(Path32::from(if std::path::MAIN_SEPARATOR == '/' {
                rel.to_owned()
            } else {
                rel.replace(std::path::MAIN_SEPARATOR, "/")
            }))
        })
}

fn walk(root: &Path, shared: &Mutex<Shared>, stop: &AtomicBool, generation: u64) {
    let mut batch = Vec::with_capacity(BATCH);
    let mut total = 0;
    let mut capped = false;
    for rel in files(root) {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        batch.push(rel);
        total += 1;
        if total >= MAX_FILES {
            capped = true;
            break;
        }
        if batch.len() == BATCH && !flush(shared, &mut batch, generation) {
            return;
        }
    }
    if flush(shared, &mut batch, generation) {
        let mut s = lock(shared);
        if s.generation == generation {
            s.done = true;
            s.capped = capped;
        }
    }
}

/// Adds the batch and feeds the matchers; false if a newer walk took over.
fn flush(shared: &Mutex<Shared>, batch: &mut Vec<Path32>, generation: u64) -> bool {
    let mut s = lock(shared);
    if s.generation != generation {
        return false;
    }
    for (_, i) in &s.sinks {
        for p in batch.iter() {
            i.push(p.clone(), fill);
        }
    }
    s.known.extend(batch.iter().cloned());
    s.paths.append(batch);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_honour_ignore_files_and_skip_hidden() {
        let dir = std::env::temp_dir().join(format!("spur-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join(".ignore"), "target/\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        std::fs::write(dir.join("README.md"), "").unwrap();
        std::fs::write(dir.join("target/out.o"), "").unwrap();
        std::fs::write(dir.join(".hidden/x"), "").unwrap();
        let mut idx = Index::new(dir.clone());
        assert!(idx.stale());
        idx.walk(None);
        let t = Instant::now();
        while !idx.status().done && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut paths: Vec<String> = lock(&idx.shared)
            .paths
            .iter()
            .map(ToString::to_string)
            .collect();
        paths.sort();
        assert_eq!(paths, ["README.md", "src/main.rs"]);
        // A later walk keeps serving the old paths, then swaps.
        std::fs::write(dir.join("src/new.rs"), "").unwrap();
        idx.saved("src/main.rs");
        assert!(!idx.stale(), "a listed file changes nothing");
        idx.saved("src/new.rs");
        assert!(idx.stale());
        let g = idx.status().generation;
        idx.walk(None);
        let t = Instant::now();
        while idx.status().generation == g && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(idx.status().count, 3);
        assert!(idx.status().done);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
