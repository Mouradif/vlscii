//! Terminal session setup/teardown with signal-safe recovery.
//!
//! Playback runs inside the *alternate screen buffer* (`\x1b[?1049h`), so the
//! player gets its own "page" and the user's scrollback + cursor are restored
//! untouched on exit. Teardown happens on three paths:
//!
//!   - normal end / EOF / error  -> `TermSession`'s `Drop`
//!   - Ctrl+C (SIGINT) / SIGTERM -> a signal handler that runs the *same*
//!     teardown using only async-signal-safe `write(2)` + `_exit(3)`, because
//!     Rust `Drop` does NOT run when the default signal disposition kills us.
//!
//! Without the handler, a Ctrl+C would leave the cursor hidden and the terminal
//! on the alt screen — exactly the breakage we're fixing.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

// Sequences are split so we can emit them from a signal handler via raw write().
// ENTER: alt screen, then hide cursor / disable wrap / black bg.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[40m";
// LEAVE: re-enable wrap, show cursor, reset attrs, leave alt screen, newline.
// Used verbatim by both Drop and the signal handler.
const LEAVE: &[u8] = b"\x1b[?7h\x1b[?25h\x1b[0m\x1b[?1049l\n";

/// Set once a session is active, so the signal handler only emits LEAVE when we
/// actually entered the alt screen (guards against a stray signal pre-setup).
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// RAII handle for the terminal session. Constructing it enters the alt screen
/// and installs the signal handlers; dropping it restores the terminal.
pub struct TermSession {
    _private: (),
}

impl TermSession {
    /// Enter the alternate screen and arm SIGINT/SIGTERM recovery.
    pub fn enter() -> io::Result<TermSession> {
        let mut out = io::stdout();
        out.write_all(ENTER)?;
        out.flush()?;
        ACTIVE.store(true, Ordering::SeqCst);
        install_signal_handlers();
        Ok(TermSession { _private: () })
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        // Mark inactive first so a signal racing with Drop doesn't double-emit.
        if ACTIVE.swap(false, Ordering::SeqCst) {
            let mut out = io::stdout();
            let _ = out.write_all(LEAVE);
            let _ = out.flush();
        }
    }
}

/// Install the same handler for SIGINT and SIGTERM.
fn install_signal_handlers() {
    // Cast through a function pointer (not a direct item->int cast).
    let handler = handle_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: `handle_signal` is async-signal-safe (only write(2)/_exit(3)).
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

/// Async-signal-safe teardown: restore the terminal and exit immediately.
///
/// Only calls that are safe inside a signal handler are used here — a raw
/// `write(2)` of a constant byte string and `_exit(3)`. No allocation, no
/// locks, no Rust `Drop`.
extern "C" fn handle_signal(_sig: libc::c_int) {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        // SAFETY: writing a 'static constant to fd 1 is async-signal-safe.
        unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                LEAVE.as_ptr() as *const libc::c_void,
                LEAVE.len(),
            );
        }
    }
    // 130 is the conventional "terminated by SIGINT" exit code.
    unsafe { libc::_exit(130) };
}
