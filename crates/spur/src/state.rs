//! The per-user state folder: the log and rescue copies.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// `$XDG_STATE_HOME/spur`, else `~/.local/state/spur` (Linux and macOS);
/// `%LOCALAPPDATA%\spur` on Windows.
pub fn base() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("spur"))
    }
    #[cfg(not(windows))]
    {
        // Relative values are ignored (the XDG rule), so copies never land in the
        // current folder.
        let abs = |v: std::ffi::OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
        if let Some(d) = std::env::var_os("XDG_STATE_HOME").and_then(abs) {
            return Some(d.join("spur"));
        }
        std::env::var_os("HOME")
            .and_then(abs)
            .map(|h| h.join(".local/state/spur"))
    }
}

/// A file name for a rescue copy: control chars become `_`, and it is cut to 100
/// chars so the suffix still fits the system's name limit.
fn safe_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .take(100)
        .collect();
    if s.is_empty() { "untitled".into() } else { s }
}

/// A path for the terminal: control chars shown escaped.
pub fn printable(p: &Path) -> String {
    p.display()
        .to_string()
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// Creates `dir` (and parents), private to the user.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// A new file that no one else can read; fails if it exists.
fn private_file(path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(0o600);
    }
    o.open(path)
}

/// The log: created on first write.
#[derive(Clone, Default)]
pub struct Log {
    inner: Arc<Mutex<LogInner>>,
}

#[derive(Default)]
struct LogInner {
    path: Option<PathBuf>,
    file: Option<File>,
    failed: bool,
}

impl Log {
    /// A log at `<dir>/spur-<pid>.log`.
    pub fn new(dir: Option<PathBuf>) -> Self {
        let path = dir.map(|d| d.join(format!("spur-{}.log", std::process::id())));
        Self {
            inner: Arc::new(Mutex::new(LogInner {
                path,
                file: None,
                failed: false,
            })),
        }
    }

    /// Appends a line; errors are dropped (there is nowhere to report them).
    pub fn write(&self, line: &str) {
        let Ok(mut l) = self.inner.lock() else {
            return;
        };
        if l.file.is_none() && !l.failed {
            // A log left by an earlier process with the same id is kept: the
            // new one gets a suffix.
            let opened = l.path.clone().and_then(|p| {
                private_dir(p.parent()?).ok()?;
                (0..100).find_map(|n| {
                    let q = if n == 0 {
                        p.clone()
                    } else {
                        p.with_extension(format!("{n}.log"))
                    };
                    private_file(&q).ok().map(|f| (q, f))
                })
            });
            l.failed = opened.is_none();
            if let Some((p, f)) = opened {
                l.path = Some(p);
                l.file = Some(f);
            }
        }
        if let Some(f) = l.file.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }

    /// The log's path, if anything was written.
    pub fn written(&self) -> Option<PathBuf> {
        let l = self.inner.lock().ok()?;
        l.file.as_ref().and(l.path.clone())
    }
}

/// Writes rescue copies into `dir` and remembers their paths.
#[derive(Clone)]
pub struct Rescue {
    dir: Option<PathBuf>,
    /// Copies written so far.
    pub written: Arc<Mutex<Vec<PathBuf>>>,
    log: Log,
}

impl Rescue {
    /// Rescue copies go to `dir`.
    pub fn new(dir: Option<PathBuf>, log: Log) -> Self {
        Self {
            dir,
            written: Arc::default(),
            log,
        }
    }

    /// Writes one copy per document: `<file name>.<unix seconds>.<n>.txt`, never
    /// overwriting.
    pub fn write(&self, docs: &[(Option<PathBuf>, String)]) {
        let Some(dir) = &self.dir else {
            self.log.write("no state folder: rescue copies not written");
            return;
        };
        if let Err(e) = private_dir(dir) {
            self.log
                .write(&format!("rescue folder {}: {e}", dir.display()));
            return;
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        for (path, text) in docs {
            let name = path
                .as_deref()
                .and_then(Path::file_name)
                .map_or_else(|| "untitled".into(), |n| safe_name(&n.to_string_lossy()));
            let mut n = 0;
            let file = loop {
                let p = dir.join(format!("{name}.{secs}.{n}.txt"));
                match private_file(&p) {
                    Ok(f) => break Some((p, f)),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => n += 1,
                    Err(e) => {
                        self.log.write(&format!("rescue copy {}: {e}", p.display()));
                        break None;
                    }
                }
            };
            if let Some((p, mut f)) = file {
                match f.write_all(text.as_bytes()).and_then(|()| f.sync_all()) {
                    Ok(()) => {
                        let from = path
                            .as_deref()
                            .map_or_else(|| "untitled".into(), |p| p.display().to_string());
                        self.log
                            .write(&format!("rescued {from} to {}", p.display()));
                        if let Ok(mut w) = self.written.lock() {
                            w.push(p);
                        }
                    }
                    Err(e) => self.log.write(&format!("rescue copy {}: {e}", p.display())),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rescue_names_are_safe_and_short() {
        assert_eq!(safe_name("a\u{1b}]52;c;x\u{7}.rs"), "a_]52;c;x_.rs");
        assert_eq!(safe_name(&"n".repeat(300)).len(), 100);
        assert_eq!(safe_name(""), "untitled");
        assert_eq!(printable(Path::new("/x/a\u{1b}b")), "/x/a\\u{1b}b");
    }

    #[test]
    fn rescue_copies_are_private_new_files() {
        let dir = std::env::temp_dir().join(format!("spur-rescue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Log::new(Some(dir.join("log")));
        let r = Rescue::new(Some(dir.join("rescue")), log.clone());
        let docs = [
            (Some(PathBuf::from("/x/notes.txt")), "a".to_owned()),
            (None, "b".to_owned()),
        ];
        r.write(&docs);
        r.write(&docs);
        let written = r.written.lock().unwrap().clone();
        assert_eq!(written.len(), 4, "never overwritten");
        let names: Vec<String> = written
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names[0].starts_with("notes.txt.") && names[0].ends_with(".0.txt"));
        assert!(names[1].starts_with("untitled."));
        assert!(names[2].ends_with(".1.txt"));
        assert_eq!(std::fs::read_to_string(&written[1]).unwrap(), "b");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&written[0]).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let mode = std::fs::metadata(dir.join("rescue"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        assert!(log.written().is_some());
    }
}
