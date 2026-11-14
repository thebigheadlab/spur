//! Setting the terminal up and putting it back.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{cursor, execute, terminal};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static KITTY: AtomicBool = AtomicBool::new(false);

/// Raw mode, the alternate screen, no cursor, bracketed paste, presses and the
/// wheel only, and the kitty keyboard protocol where the terminal has it. Every
/// terminal query happens here, before any reader thread starts.
pub fn setup() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    ACTIVE.store(true, Ordering::SeqCst);
    let mut out = io::stdout();
    execute!(
        out,
        terminal::EnterAlternateScreen,
        cursor::Hide,
        EnableBracketedPaste
    )?;
    // Button presses and the wheel, SGR coordinates; no motion tracking.
    #[cfg(unix)]
    {
        out.write_all(b"\x1b[?1000h\x1b[?1006h")?;
        out.flush()?;
    }
    #[cfg(windows)]
    execute!(out, crossterm::event::EnableMouseCapture)?;
    if terminal::supports_keyboard_enhancement().unwrap_or(false) {
        execute!(
            out,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        )?;
        KITTY.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// Puts the terminal back. Idempotent, and safe from a panic hook.
pub fn restore() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    if KITTY.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    #[cfg(unix)]
    {
        let _ = out.write_all(b"\x1b[?1006l\x1b[?1000l");
    }
    #[cfg(windows)]
    {
        let _ = execute!(out, crossterm::event::DisableMouseCapture);
    }
    let _ = out.write_all(b"\x1b[0m\x1b[?2026l");
    let _ = execute!(
        out,
        DisableBracketedPaste,
        cursor::Show,
        terminal::LeaveAlternateScreen
    );
    let _ = out.flush();
    let _ = terminal::disable_raw_mode();
}
