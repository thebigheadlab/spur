//! Watching a pack folder: a thread that polls the pack's files and sends their
//! text to the session whenever any of it changed. The session loads them.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use super::pack::{PackSource, read_source};

/// The pack's files, if they could all be read, and their hash.
fn snapshot(dir: &Path) -> Result<(u64, Vec<(String, String)>), String> {
    let files = read_source(&PackSource::Dir(dir.to_path_buf())).map_err(|e| e.to_string())?;
    let mut h = DefaultHasher::new();
    files.hash(&mut h);
    Ok((h.finish(), files))
}

/// The hash of the pack's files, if they could all be read.
pub fn hash(dir: &Path) -> Option<u64> {
    snapshot(dir).ok().map(|(h, _)| h)
}

/// The shortest interval: a zero one would spin.
const MIN_EVERY: Duration = Duration::from_millis(10);

/// Polls `dir` every `every` (at least 10 ms), starting from the hash `first`,
/// until `stop` is dropped; calls `send` with the files after each change
/// (contents are hashed, so same-size edits count). Returns when
/// `send` returns false. A read error is logged once until it changes.
pub fn watch(
    dir: &Path,
    every: Duration,
    first: Option<u64>,
    stop: &Receiver<()>,
    log: &dyn Fn(&str),
    send: &dyn Fn(Vec<(String, String)>) -> bool,
) {
    let every = every.max(MIN_EVERY);
    let mut last = first;
    let mut failed: Option<String> = None;
    loop {
        match stop.recv_timeout(every) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => return,
        }
        let polled = catch_unwind(AssertUnwindSafe(|| snapshot(dir)));
        match polled {
            Ok(Ok((h, files))) => {
                failed = None;
                if last != Some(h) {
                    last = Some(h);
                    if !send(files) {
                        return;
                    }
                }
            }
            Ok(Err(e)) => {
                if failed.as_ref() != Some(&e) {
                    log(&format!("pack watcher: {e}"));
                    failed = Some(e);
                }
            }
            Err(_) => log("pack watcher: caught a panic; polling again"),
        }
    }
}
