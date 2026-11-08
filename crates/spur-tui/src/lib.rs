//! Terminal frontend. Draws a [`Session`] into the terminal and feeds it keys.
//!
//! In-process for M1; with the daemon it becomes a client of the session server.

pub mod input;
pub mod render;

use std::io::{self, Write as _};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    self, BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{cursor, execute, queue};
use spur_keymap::Session;

use crate::render::{Frame, UiState};

/// Name of this crate, used in diagnostics.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// Longest wait for input when nothing is due, so key hints, timeouts and messages
/// update.
const IDLE_POLL: Duration = Duration::from_millis(500);

/// Set while our keyboard flags are pushed, so restoring pops them exactly once.
static PUSHED_FLAGS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Copper caret (OSC 12); terminals that do not know it ignore it.
const CURSOR_COLOR: &str = "\x1b]12;#d98b48\x07";
/// Back to the user's caret color (OSC 112).
const CURSOR_COLOR_RESET: &str = "\x1b]112\x07";

/// Puts the terminal back the way the user had it. Safe to call more than once and from
/// a panic hook.
pub fn restore_terminal() {
    let mut out = io::stdout();
    if PUSHED_FLAGS.swap(false, std::sync::atomic::Ordering::SeqCst) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        DisableBracketedPaste,
        cursor::SetCursorStyle::DefaultUserShape,
        crossterm::style::Print(CURSOR_COLOR_RESET),
        cursor::Show,
        LeaveAlternateScreen
    );
    let _ = terminal::disable_raw_mode();
}

/// Raw mode + alternate screen for the guard's lifetime.
struct Guard;

impl Guard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self;
        let mut out = io::stdout();
        execute!(
            out,
            EnterAlternateScreen,
            EnableBracketedPaste,
            cursor::Hide,
            crossterm::style::Print(CURSOR_COLOR)
        )?;
        // Kitty protocol: lets the terminal tell Esc from Alt and C-i from Tab.
        if terminal::supports_keyboard_enhancement().unwrap_or(false) {
            execute!(
                out,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            PUSHED_FLAGS.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
        Ok(guard)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Runs the editor until the session quits.
///
/// # Errors
///
/// Returns terminal I/O errors. The terminal is restored either way.
pub fn run(session: &mut Session) -> io::Result<()> {
    let _guard = Guard::enter()?;
    // With the kitty protocol the terminal reports real alt keys; no need to guess.
    session.split_alt = !PUSHED_FLAGS.load(std::sync::atomic::Ordering::SeqCst);
    let mut ui = UiState::default();
    let mut prev: Option<Frame> = None;
    let mut buf = Vec::with_capacity(64 * 1024);
    loop {
        let now = Instant::now();
        session.tick(now);
        if session.should_quit() {
            return Ok(());
        }
        let (w, h) = terminal::size()?;
        let frame = render::render(session, w, h, &mut ui, now);
        if prev.as_ref() != Some(&frame) {
            buf.clear();
            queue!(buf, BeginSynchronizedUpdate)?;
            render::diff(prev.as_ref(), &frame, &mut buf)?;
            queue!(buf, EndSynchronizedUpdate)?;
            let mut out = io::stdout().lock();
            out.write_all(&buf)?;
            out.flush()?;
            prev = Some(frame);
        }

        // Sleep until input, a keymap timeout, or key hints are due.
        let mut wait = IDLE_POLL;
        if let Some(d) = session.keymap.deadline() {
            wait = wait.min(d.saturating_duration_since(Instant::now()));
        }
        if let Some(since) = ui.pending_since {
            let due = since + render::WHICH_KEY_DELAY;
            wait = wait.min(due.saturating_duration_since(Instant::now()));
        }
        if !event::poll(wait)? {
            continue;
        }
        // Drain everything queued so a burst (a paste without bracketing, key repeat)
        // draws once.
        loop {
            handle(session, event::read()?, &mut prev);
            if session.should_quit() || !event::poll(Duration::ZERO)? {
                break;
            }
        }
    }
}

fn handle(session: &mut Session, ev: Event, prev: &mut Option<Frame>) {
    match ev {
        Event::Key(k) => {
            if let Some(key) = input::map_key(&k) {
                session.key(key, Instant::now());
            }
        }
        Event::Paste(text) => {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            session.paste(&text);
        }
        // Force a full redraw.
        Event::Resize(..) => *prev = None,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(super::NAME, "spur-tui");
    }
}
