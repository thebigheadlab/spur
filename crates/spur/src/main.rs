//! `spur`: the editor. Starts an editor session with the default UX pack and runs
//! the terminal client on it, in one process.

mod args;
mod state;

use std::io::Write as _;
use std::process::ExitCode;

use args::Command;

/// The default UX pack, embedded at build time.
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/ux_fresh.rs"));
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => return fail(&format!("cannot read the current folder: {e}")),
    };
    match args::parse(&argv, &cwd) {
        Ok(Command::Version) => {
            let _ = writeln!(std::io::stdout(), "spur {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Command::Help) => {
            let _ = writeln!(std::io::stdout(), "{}", args::USAGE);
            ExitCode::SUCCESS
        }
        Ok(Command::Run(run)) => editor(run),
        Err(e) => fail(&format!("spur: {e}\n\n{}", args::USAGE)),
    }
}

/// Exit code 2 with a message on stderr.
fn fail(msg: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "{msg}");
    ExitCode::from(2)
}

#[cfg(not(feature = "tui"))]
fn editor(_run: args::Run) -> ExitCode {
    fail("spur: built without the terminal client")
}

#[cfg(feature = "tui")]
fn editor(run: args::Run) -> ExitCode {
    use std::io::IsTerminal as _;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;
    use std::time::Duration;

    use spur_server::{LogSink, PackSource, RescueSink, Session, SessionConfig};

    if !std::io::stdout().is_terminal() {
        return fail("spur: the terminal client needs a terminal (stdout is not one)");
    }
    let base = state::base();
    let log = state::Log::new(base.as_ref().map(|b| b.join("log")));
    let rescue = state::Rescue::new(base.as_ref().map(|b| b.join("rescue")), log.clone());
    install_panic_hook(log.clone());
    // A pack folder is watched and reloaded on change; the embedded one is fixed.
    let (pack, watch) = match run.ux {
        Some(dir) => (PackSource::Dir(dir), Some(Duration::from_millis(250))),
        None => (PackSource::Embedded(embedded::UX_FRESH), None),
    };
    let sink_log = log.clone();
    let sink_rescue = rescue.clone();
    let session = match Session::spawn(SessionConfig {
        name: "spur".into(),
        open: run.open,
        pack: Some(pack),
        theme: run.theme,
        root: run.root,
        log: Some(LogSink(Arc::new(move |l| sink_log.write(l)))),
        rescue: Some(RescueSink(Arc::new(move |docs| sink_rescue.write(docs)))),
        watch,
        ..SessionConfig::default()
    }) {
        Ok(s) => s,
        Err(e) => return fail(&format!("spur: {e}")),
    };
    let (end, server) = spur_rpc::in_process();
    if session.attach(server).is_err() {
        return fail("spur: the session ended at once");
    }
    let (runner, stop) = spur_tui::prepare();
    watch_signals(stop);
    let client_log = log.clone();
    let opts = spur_tui::Options {
        name: "tui".into(),
        log: Some(Arc::new(move |l| client_log.write(l))),
        ..spur_tui::Options::default()
    };
    let res = catch_unwind(AssertUnwindSafe(|| runner.run(end, opts)));
    spur_tui::restore();
    let quit = matches!(res, Ok(Ok(spur_tui::ExitReason::Quit)));
    match &res {
        Ok(Ok(spur_tui::ExitReason::Quit)) => {}
        Ok(Ok(r)) => log.write(&format!("client ended: {r:?}")),
        Ok(Err(e)) => log.write(&format!("client failed: {e}")),
        Err(_) => log.write("client panicked"),
    }
    if !quit && let Some(docs) = session.modified_documents(Duration::from_secs(1)) {
        rescue.write(&docs);
    }
    drop(session);
    let mut err = std::io::stderr();
    let written = rescue.written.lock().map(|w| w.clone()).unwrap_or_default();
    if !written.is_empty() {
        let _ = writeln!(err, "spur: unsaved work was saved to:");
        for p in &written {
            let _ = writeln!(err, "  {}", p.display());
        }
    }
    if let Ok(Err(e)) = &res {
        let _ = writeln!(err, "spur: {e}");
    }
    if let Some(p) = log.written() {
        let _ = writeln!(err, "spur: log at {}", p.display());
    }
    if quit {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// On the terminal thread: restore the terminal, then report as usual. Elsewhere:
/// write to the log only (the terminal belongs to the client).
#[cfg(feature = "tui")]
fn install_panic_hook(log: state::Log) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if spur_tui::tui_thread() == Some(std::thread::current().id()) {
            spur_tui::restore();
            default(info);
        } else {
            let name = std::thread::current().name().unwrap_or("?").to_owned();
            log.write(&format!("panic on thread {name}: {info}"));
        }
    }));
}

/// SIGHUP and SIGTERM stop the client, so modified work is rescued.
#[cfg(all(feature = "tui", unix))]
fn watch_signals(stop: spur_tui::StopHandle) {
    use signal_hook::consts::{SIGHUP, SIGTERM};
    if let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGHUP, SIGTERM]) {
        let _ = std::thread::Builder::new()
            .name("spur-signals".into())
            .spawn(move || {
                if signals.forever().next().is_some() {
                    stop.stop();
                }
            });
    }
}

/// On Windows a closed console ends the terminal reader, which stops the client.
#[cfg(all(feature = "tui", not(unix)))]
fn watch_signals(_stop: spur_tui::StopHandle) {}

#[cfg(test)]
mod tests {
    #[test]
    fn the_embedded_pack_loads() {
        assert!(super::embedded::UX_FRESH.len() >= 10);
        if let Err(errors) = spur_server::Pack::check(super::embedded::UX_FRESH) {
            panic!("{errors:?}");
        }
    }
}
