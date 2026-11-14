//! Command-line arguments.

use std::path::{Path, PathBuf};

/// The usage text.
pub const USAGE: &str = "usage: spur [--ux DIR] [--theme NAME] [--] [PATH...]
       spur -V | --version
       spur -h | --help

PATH is a folder to work in, or files to open.
--ux DIR      use the UX pack in DIR (reloaded when it changes)
--theme NAME  use theme NAME from the pack";

/// What to run.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Print the version.
    Version,
    /// Print the usage.
    Help,
    /// Run the editor.
    Run(Run),
}

/// How to run the editor.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Run {
    /// UX pack folder.
    pub ux: Option<PathBuf>,
    /// Theme override.
    pub theme: Option<String>,
    /// Workspace root.
    pub root: PathBuf,
    /// Files to open, in order.
    pub open: Vec<PathBuf>,
}

/// Parses arguments (without the program name). `cwd` is the current folder.
///
/// # Errors
///
/// A usage message.
pub fn parse(args: &[String], cwd: &Path) -> Result<Command, String> {
    let mut ux = None;
    let mut theme = None;
    let mut paths = Vec::new();
    let mut it = args.iter();
    let mut flags = true;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" if flags => flags = false,
            "-V" | "--version" if flags => return Ok(Command::Version),
            "-h" | "--help" if flags => return Ok(Command::Help),
            "--ux" if flags => {
                let dir = it.next().ok_or("--ux needs a folder")?;
                ux = Some(PathBuf::from(dir));
            }
            "--theme" if flags => {
                theme = Some(it.next().ok_or("--theme needs a name")?.clone());
            }
            s if flags && s.starts_with('-') && s.len() > 1 => {
                return Err(format!(
                    "unknown option {s} (a path starting with - goes after --)"
                ));
            }
            s => paths.push(normal(&cwd.join(s))),
        }
    }
    if let Some(dir) = &ux
        && !cwd.join(dir).is_dir()
    {
        return Err(format!("--ux {}: not a folder", dir.display()));
    }
    let ux = ux.map(|d| normal(&cwd.join(d)));
    let (dirs, files): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| p.is_dir());
    let (root, open) = match (dirs.as_slice(), files.is_empty()) {
        ([], _) => {
            // The current folder, unless the first file lies outside it.
            let root = match files.first().and_then(|f| f.parent()) {
                Some(parent) if !parent.starts_with(cwd) => parent.to_path_buf(),
                _ => cwd.to_path_buf(),
            };
            (root, files)
        }
        ([dir], true) => (dir.clone(), Vec::new()),
        ([_], false) => return Err("give a folder or files, not both".into()),
        _ => return Err("give at most one folder".into()),
    };
    Ok(Command::Run(Run {
        ux,
        theme,
        root,
        open,
    }))
}

/// `path` with `.` and `..` resolved without touching the file system.
fn normal(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("spur-args-{}", std::process::id()));
        std::fs::create_dir_all(d.join("pack")).unwrap();
        std::fs::create_dir_all(d.join("proj")).unwrap();
        std::fs::write(d.join("a.rs"), "").unwrap();
        d
    }

    fn run(args: &[&str], cwd: &Path) -> Result<Command, String> {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        parse(&args, cwd)
    }

    #[test]
    fn argument_table() {
        let cwd = temp();
        assert_eq!(run(&["-V"], &cwd), Ok(Command::Version));
        assert_eq!(run(&["--help"], &cwd), Ok(Command::Help));
        let Ok(Command::Run(r)) = run(&[], &cwd) else {
            panic!()
        };
        assert_eq!((r.root.as_path(), r.open.len()), (cwd.as_path(), 0));
        let Ok(Command::Run(r)) = run(&["proj"], &cwd) else {
            panic!()
        };
        assert_eq!(r.root, cwd.join("proj"));
        assert!(r.open.is_empty());
        let Ok(Command::Run(r)) = run(&["a.rs", "new.txt"], &cwd) else {
            panic!()
        };
        assert_eq!(r.root, cwd);
        assert_eq!(r.open, [cwd.join("a.rs"), cwd.join("new.txt")]);
        let Ok(Command::Run(r)) = run(&["--ux", "pack", "--theme", "paper", "--", "-x"], &cwd)
        else {
            panic!()
        };
        assert_eq!(r.ux, Some(cwd.join("pack")));
        assert_eq!(r.theme.as_deref(), Some("paper"));
        assert_eq!(r.open, [cwd.join("-x")]);
        // A file outside the current folder makes its folder the root.
        let inner = cwd.join("proj");
        let Ok(Command::Run(r)) = run(&["../a.rs"], &inner) else {
            panic!()
        };
        assert_eq!(r.root, cwd);
        let Ok(Command::Run(r)) = run(&["../../outside.rs"], &inner) else {
            panic!()
        };
        assert_eq!(Some(r.root.as_path()), cwd.parent());
        for bad in [
            &["proj", "pack"][..],
            &["proj", "a.rs"],
            &["--ux", "a.rs"],
            &["--ux"],
            &["--nope"],
        ] {
            assert!(run(bad, &cwd).is_err(), "{bad:?}");
        }
    }
}
