//! `spur`: a native, agent-first editor.

use std::io::Write as _;
use std::process::ExitCode;

const USAGE: &str = "\
usage: spur [--preset NAME] [FILE]

Type to edit. ctrl+space opens the Ask card for everything else; ctrl+q leaves.

options:
  --preset NAME         load a key preset extension on top of Spur's keys (vim)
  --reset-terminal      restore a terminal left in raw mode by a crash
  -V, --version         print the version
  -h, --help            print this help";

#[derive(Debug, PartialEq, Eq)]
enum Cmd {
    Edit {
        preset: Option<String>,
        file: Option<String>,
    },
    Version,
    Help,
    ResetTerminal,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Cmd, String> {
    let mut preset = None;
    let mut file = None;
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-V" | "--version" => return Ok(Cmd::Version),
            "-h" | "--help" => return Ok(Cmd::Help),
            "--reset-terminal" => return Ok(Cmd::ResetTerminal),
            "--preset" => match args.next().as_deref() {
                Some("vim") => preset = Some("vim".to_owned()),
                Some(other) => return Err(format!("unknown preset {other:?} (known: vim)")),
                None => return Err("--preset needs a name".into()),
            },
            "--" => file = args.next(),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ if file.is_some() => return Err("only one file for now".into()),
            _ => file = Some(a),
        }
    }
    Ok(Cmd::Edit { preset, file })
}

fn main() -> ExitCode {
    let cmd = match parse(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spur: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let mut out = std::io::stdout().lock();
    let res = match cmd {
        Cmd::Version => writeln!(out, "spur {}", env!("CARGO_PKG_VERSION")),
        Cmd::Help => writeln!(out, "spur {}\n{USAGE}", env!("CARGO_PKG_VERSION")),
        Cmd::ResetTerminal => {
            #[cfg(feature = "tui")]
            spur_tui::restore_terminal();
            Ok(())
        }
        Cmd::Edit { preset, file } => {
            drop(out);
            return edit(preset.as_deref(), file.as_deref());
        }
    };
    if res.is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(feature = "tui")]
fn edit(preset: Option<&str>, file: Option<&str>) -> ExitCode {
    use std::io::IsTerminal as _;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("spur: needs a terminal");
        return ExitCode::FAILURE;
    }
    let mut s = spur_keymap::Session::new();
    if let Some(p) = preset
        && let Err(e) = load_preset(&mut s, p)
    {
        eprintln!("spur: {e}");
        return ExitCode::FAILURE;
    }
    if let Some(path) = file
        && let Err(e) = s.command("editor.open", serde_json::json!({ "path": path }))
    {
        eprintln!("spur: {e}");
        return ExitCode::FAILURE;
    }
    match spur_tui::run(&mut s) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spur: terminal error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Loads a key preset extension.
#[cfg(feature = "tui")]
fn load_preset(s: &mut spur_keymap::Session, name: &str) -> Result<(), String> {
    match name {
        #[cfg(feature = "preset-vim")]
        "vim" => {
            spur_vim::install(s);
            Ok(())
        }
        _ => {
            let _ = s;
            Err(format!("preset {name} is not built in"))
        }
    }
}

#[cfg(not(feature = "tui"))]
fn edit(_preset: Option<&str>, _file: Option<&str>) -> ExitCode {
    eprintln!("spur: built without the terminal frontend");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::{Cmd, parse};

    fn p(args: &[&str]) -> Result<Cmd, String> {
        parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn args() {
        assert_eq!(
            p(&[]),
            Ok(Cmd::Edit {
                preset: None,
                file: None
            })
        );
        assert_eq!(
            p(&["--preset", "vim", "a.txt"]),
            Ok(Cmd::Edit {
                preset: Some("vim".into()),
                file: Some("a.txt".into())
            })
        );
        assert_eq!(
            p(&["--", "-x"]),
            Ok(Cmd::Edit {
                preset: None,
                file: Some("-x".into())
            })
        );
        assert_eq!(p(&["-V"]), Ok(Cmd::Version));
        assert!(p(&["--preset", "emacs"]).is_err());
        assert!(p(&["--preset"]).is_err());
        assert!(p(&["a", "b"]).is_err());
        assert!(p(&["--bogus"]).is_err());
    }
}
