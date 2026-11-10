//! `spur`: the launcher. The editor itself is a server (`spur-server`); this binary
//! starts or finds a session and attaches a frontend to it.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use spur_server::{Pack, Session, SessionConfig, daemon};

/// The default experience and the fallback, bundled as data.
const UX_FRESH: &str = include_str!("../../../extensions/ux_fresh/pack.toml");
const UX_FALLBACK: &str = include_str!("../../../extensions/fallback.toml");

const USAGE: &str = "\
spur: an editor server with attachable frontends

Usage:
  spur [PATH]                 edit a file or folder (private session, terminal UI)
  spur serve [PATH]           run a session others can attach to (no UI)
  spur attach                 attach a terminal UI to a running session
  spur ls                     list running sessions

Options:
  --name NAME                 session name for serve/attach (default: main)
  --ux FILE                   experience pack to use instead of the built-in one
  -V, --version               print the version
  -h, --help                  print this help
";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    command: String,
    path: Option<PathBuf>,
    name: Option<String>,
    ux: Option<PathBuf>,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut words = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-V" | "--version" => out.command = "version".into(),
            "-h" | "--help" => out.command = "help".into(),
            "--name" => out.name = Some(it.next().ok_or("--name needs a value")?),
            "--ux" => out.ux = Some(it.next().ok_or("--ux needs a file")?.into()),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            _ => words.push(a),
        }
    }
    if !out.command.is_empty() {
        return Ok(out);
    }
    let mut words = words.into_iter().peekable();
    out.command = match words.peek().map(String::as_str) {
        Some("serve" | "attach" | "ls") => words.next().unwrap_or_default(),
        _ => "edit".into(),
    };
    out.path = words.next().map(PathBuf::from);
    if let Some(extra) = words.next() {
        return Err(format!("unexpected argument {extra:?}"));
    }
    if out.path.is_some() && matches!(out.command.as_str(), "attach" | "ls") {
        return Err(format!("{} takes no path", out.command));
    }
    Ok(out)
}

/// The pack to use, and a notice if the requested one failed and the fallback stepped in.
fn load_pack(ux: Option<&Path>) -> Result<(Pack, Option<String>), String> {
    let Some(path) = ux else {
        return Pack::parse(UX_FRESH)
            .map(|p| (p, None))
            .map_err(|e| format!("built-in pack: {e}"));
    };
    let problem = match std::fs::read_to_string(path) {
        Ok(text) => match Pack::parse(&text) {
            Ok(p) => return Ok((p, None)),
            Err(e) => e.to_string(),
        },
        Err(e) => e.to_string(),
    };
    let fallback = Pack::parse(UX_FALLBACK).map_err(|e| format!("fallback pack: {e}"))?;
    let first = problem.lines().next().unwrap_or_default();
    Ok((fallback, Some(format!("{}: {first}", path.display()))))
}

/// Workspace root and file to open for a path argument.
fn workspace(path: Option<&Path>) -> Result<(PathBuf, Option<PathBuf>), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let Some(path) = path else {
        return Ok((cwd, None));
    };
    let full = cwd.join(path);
    if full.is_dir() {
        return Ok((full.canonicalize().map_err(|e| e.to_string())?, None));
    }
    // A file: the workspace is where we are if the file is inside it, else its folder.
    let parent = full.parent().unwrap_or(&cwd);
    let parent = parent
        .canonicalize()
        .map_err(|e| format!("{}: {e}", parent.display()))?;
    let file = parent.join(full.file_name().ok_or("not a file name")?);
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let root = if file.starts_with(&cwd) { cwd } else { parent };
    Ok((root, Some(file)))
}

fn session(args: &Args, name: &str, exit_when_empty: bool) -> Result<Session, String> {
    let (pack, notice) = load_pack(args.ux.as_deref())?;
    let (root, open) = workspace(args.path.as_deref())?;
    Session::spawn(SessionConfig {
        name: name.to_owned(),
        root,
        pack,
        open,
        exit_when_empty,
        notice,
    })
}

#[cfg(feature = "tui")]
fn frontend(end: spur_rpc::ClientEnd) -> Result<String, String> {
    spur_tui::run(end).map_err(|e| format!("terminal: {e}"))
}

#[cfg(not(feature = "tui"))]
fn frontend(_: spur_rpc::ClientEnd) -> Result<String, String> {
    Err("this build has no terminal frontend; use `spur serve`".into())
}

fn run(args: &Args) -> Result<String, String> {
    let name = args.name.as_deref().unwrap_or("main");
    match args.command.as_str() {
        "version" => Ok(format!("spur {}", env!("CARGO_PKG_VERSION"))),
        "help" => Ok(USAGE.trim_end().to_owned()),
        "ls" => {
            let names = daemon::list().map_err(|e| e.to_string())?;
            Ok(if names.is_empty() {
                "no sessions are running".into()
            } else {
                names.join("\n")
            })
        }
        "attach" => {
            let end = daemon::connect(name).map_err(|e| e.to_string())?;
            let reason = frontend(end)?;
            Ok(if reason == "quit" {
                format!("detached from session {name}")
            } else {
                reason
            })
        }
        "serve" => {
            daemon::check_name(name).map_err(|e| e.to_string())?;
            let session = session(args, name, false)?;
            let _listener = daemon::serve(session.attacher(), name).map_err(|e| e.to_string())?;
            let mut err = std::io::stderr().lock();
            let _ = writeln!(
                err,
                "serving session {name}; attach with: spur attach --name {name}"
            );
            drop(err);
            session.join();
            Ok(String::new())
        }
        _ => {
            let session = session(args, "local", true)?;
            let (client, server) = spur_rpc::in_process();
            session.attach(server);
            let reason = frontend(client)?;
            Ok(if reason == "quit" {
                String::new()
            } else {
                reason
            })
        }
    }
}

fn main() -> ExitCode {
    let result = parse(std::env::args().skip(1)).and_then(|a| run(&a));
    match result {
        Ok(text) => {
            if !text.is_empty() && writeln!(std::io::stdout().lock(), "{text}").is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = writeln!(std::io::stderr().lock(), "spur: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Result<Args, String> {
        parse(s.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn parses_commands_and_options() {
        assert_eq!(args("").unwrap().command, "edit");
        let a = args("src/main.rs --ux my.toml").unwrap();
        assert_eq!(
            (a.command.as_str(), a.path, a.ux),
            ("edit", Some("src/main.rs".into()), Some("my.toml".into()))
        );
        let a = args("serve --name work .").unwrap();
        assert_eq!(
            (a.command.as_str(), a.name.as_deref(), a.path),
            ("serve", Some("work"), Some(".".into()))
        );
        assert_eq!(args("attach").unwrap().command, "attach");
        assert_eq!(args("--version x").unwrap().command, "version");
        assert!(args("attach extra").is_err());
        assert!(args("a b").is_err());
        assert!(args("--nope").is_err());
        assert!(args("--name").is_err());
    }

    #[test]
    fn bundled_packs_load_and_a_broken_pack_falls_back() {
        let (fresh, notice) = load_pack(None).unwrap();
        assert_eq!((fresh.id.as_str(), notice), ("ux_fresh", None));
        let (pack, notice) = load_pack(Some(Path::new("no/such/pack.toml"))).unwrap();
        assert_eq!(pack.id, "ux_fallback");
        assert!(notice.unwrap().contains("pack.toml"));
    }

    #[test]
    fn version_and_help() {
        assert!(run(&args("-V").unwrap()).unwrap().starts_with("spur "));
        assert!(
            run(&args("--help").unwrap())
                .unwrap()
                .contains("spur attach")
        );
    }
}
