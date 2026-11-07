//! `spur`: native, modal, agent-first editor.

use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let mut out = std::io::stdout().lock();
    let res = match arg.as_deref() {
        Some("-V" | "--version") => writeln!(out, "spur {}", env!("CARGO_PKG_VERSION")),
        _ => writeln!(
            out,
            "spur {}: nothing to run yet (scaffolding only).\ncrates: {}",
            env!("CARGO_PKG_VERSION"),
            crates().join(", ")
        ),
    };
    if res.is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg_attr(not(any(feature = "tui", feature = "gui")), allow(unused_mut))]
fn crates() -> Vec<&'static str> {
    let mut v = vec![
        spur_text::NAME,
        spur_core::NAME,
        spur_keymap::NAME,
        spur_ext::NAME,
        spur_rpc::NAME,
    ];
    #[cfg(feature = "tui")]
    v.push(spur_tui::NAME);
    #[cfg(feature = "gui")]
    v.push(spur_gui::NAME);
    v
}

#[cfg(test)]
mod tests {
    #[test]
    fn links_all_crates() {
        assert!(super::crates().iter().all(|c| c.starts_with("spur-")));
    }
}
