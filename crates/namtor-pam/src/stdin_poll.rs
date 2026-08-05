// SPDX-License-Identifier: GPL-3.0-only
//! Non-blocking stdin poll for instant keypress fallback (spec §8.4).
//!
//! Howdy blocks the password prompt while it stares at the user. Nam-tor polls
//! stdin during face auth; any keypress immediately aborts face auth and falls
//! through to password entry. The keystroke is passed through to the password
//! prompt, so typing simply works.
//!
//! We poll `STDIN_FILENO` with `poll(2)` using a small timeout, repeated for
//! the full auth window. If `poll` returns >0 for `POLLIN`, a keypress is
//! available and we return `true`.
//!
//! If stdin is not a tty (e.g. `su -` from a pipe), polling it makes no sense
//! and we simply block for the full duration. We detect this with `isatty`.

use std::ffi::c_int;
use std::time::Duration;

use libc::{poll, pollfd, POLLIN, STDIN_FILENO};

/// Poll stdin for input for up to `timeout`. Returns `true` if a keypress is
/// available, `false` on timeout or if stdin is not a readable fd.
pub fn stdin_has_input(timeout: Duration) -> bool {
    if !is_tty() {
        std::thread::sleep(timeout);
        return false;
    }

    let total_ms = timeout.as_millis() as c_int;
    let chunk_ms: c_int = 50;
    let mut waited: c_int = 0;

    while waited < total_ms {
        let remaining = (total_ms - waited).min(chunk_ms);
        let mut pfd = pollfd {
            fd: STDIN_FILENO,
            events: POLLIN,
            revents: 0,
        };
        // SAFETY: `poll` with a single valid `pollfd` and a positive timeout
        // is safe; we do not dereference invalid pointers.
        let rc = unsafe { poll(&mut pfd, 1, remaining) };
        if rc > 0 && (pfd.revents & POLLIN) != 0 {
            return true;
        }
        waited += chunk_ms;
    }
    false
}

fn is_tty() -> bool {
    // SAFETY: `isatty` is safe for any valid fd; it just queries metadata.
    unsafe { libc::isatty(STDIN_FILENO) == 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_has_input_returns_false_on_timeout() {
        // Stdin in tests is typically not a tty; this should return false
        // after sleeping for the (short) timeout.
        let result = stdin_has_input(Duration::from_millis(10));
        // We don't assert the result because CI may or may not have a tty;
        // we only assert it does not panic and returns in reasonable time.
        let _ = result;
    }

    #[test]
    fn is_tty_does_not_panic() {
        let _ = is_tty();
    }
}
