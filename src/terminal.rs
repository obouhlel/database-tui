//! Terminal setup and (guaranteed) restoration.
//!
//! [`restore`] is idempotent and is called from three places: the normal exit
//! path, [`TerminalGuard`]'s `Drop` (early returns, `?`) and the panic hook.

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use crossterm::{cursor, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static ENHANCED: AtomicBool = AtomicBool::new(false);
static MOUSE: AtomicBool = AtomicBool::new(false);

/// Restores the terminal when dropped.
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Enters raw mode + alternate screen. With a terminal that supports the
/// kitty keyboard protocol, modified keys such as Ctrl+Enter become
/// distinguishable.
pub fn enter(mouse: bool) -> io::Result<(Tui, TerminalGuard)> {
    install_panic_hook();
    let guard = TerminalGuard;
    setup(mouse)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    Ok((terminal, guard))
}

/// Gives the terminal back to another program (an editor), then takes it
/// again; the screen is redrawn from scratch afterwards.
pub fn suspend<T>(tui: &mut Tui, f: impl FnOnce() -> T) -> io::Result<T> {
    let mouse = MOUSE.load(Ordering::SeqCst);
    restore();
    let result = f();
    setup(mouse)?;
    tui.clear()?;
    Ok(result)
}

fn setup(mouse: bool) -> io::Result<()> {
    enable_raw_mode()?;
    ACTIVE.store(true, Ordering::SeqCst);
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
    if supports_keyboard_enhancement().unwrap_or(false) {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        ENHANCED.store(true, Ordering::SeqCst);
        tracing::info!("kitty keyboard protocol enabled");
    } else {
        tracing::info!("kitty keyboard protocol not supported, using fallback keys");
    }
    if mouse {
        execute!(out, EnableMouseCapture)?;
        MOUSE.store(true, Ordering::SeqCst);
    }
    Ok(())
}

pub fn keyboard_enhanced() -> bool {
    ENHANCED.load(Ordering::SeqCst)
}

/// Puts the terminal back in its original state. Safe to call many times.
pub fn restore() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    if ENHANCED.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    if MOUSE.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(
        out,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
    let _ = disable_raw_mode();
    let _ = out.flush();
}

fn install_panic_hook() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        tracing::error!("panic: {info}");
        previous(info);
    }));
}
