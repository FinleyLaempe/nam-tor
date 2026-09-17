// SPDX-License-Identifier: GPL-3.0-only
//! Ctrl-C during an enrollment must be caught, not fatal.
//!
//! `namtor cli enroll`'s help promises "SIGINT discards cleanly", and the
//! driver checks a flag that `handle_sigint_interrupt` acts on. Those tests
//! set the flag by hand. Nothing checked that the signal is ever wired to it
//! — and if registration silently failed, Ctrl-C would take the default
//! action and kill the CLI outright with the daemon's session still open, the
//! camera still streaming and the IR emitter still armed. That is the one
//! outcome the handler exists to prevent.
//!
//! This runs in its own test binary, and as a single test, so the process's
//! SIGINT disposition is known to start at the default — a second test in
//! the same process would find the handler already installed and its
//! precondition would be meaningless.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use namtor_cli::enroll_session::install_sigint_handler;
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

/// Read the current SIGINT disposition by installing the default and putting
/// back whatever was there.
fn current_sigint_handler() -> SigHandler {
    let probe = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
    // SAFETY: sigaction with SIG_DFL is always valid; the previous action is
    // restored immediately below, before any signal can be delivered by this
    // single-threaded test.
    let previous = unsafe { sigaction(Signal::SIGINT, &probe) }.expect("read SIGINT disposition");
    // SAFETY: restoring the action that was just read back.
    unsafe { sigaction(Signal::SIGINT, &previous) }.expect("restore SIGINT disposition");
    previous.handler()
}

#[test]
fn ctrl_c_is_caught_and_sets_the_cancel_flag() {
    assert!(
        matches!(current_sigint_handler(), SigHandler::SigDfl),
        "the test starts from the default disposition, or it proves nothing"
    );

    let flag = Arc::new(AtomicBool::new(false));
    install_sigint_handler(Arc::clone(&flag)).expect("install the handler");

    assert!(
        !matches!(
            current_sigint_handler(),
            SigHandler::SigDfl | SigHandler::SigIgn
        ),
        "SIGINT still takes its default action: Ctrl-C would kill the CLI \
         with the camera and emitter still live"
    );
    assert!(!flag.load(Ordering::SeqCst), "nothing has interrupted yet");

    // Ctrl-C.
    nix::sys::signal::raise(Signal::SIGINT).expect("raise SIGINT");

    // Surviving the raise is half the assertion; the flag is the other half.
    assert!(
        flag.load(Ordering::SeqCst),
        "the handler ran but did not set the flag the driver polls"
    );

    // A second interrupt must stay caught: the driver asks the operator to
    // confirm, so Ctrl-C can arrive more than once in one session.
    flag.store(false, Ordering::SeqCst);
    nix::sys::signal::raise(Signal::SIGINT).expect("raise SIGINT again");
    assert!(
        flag.load(Ordering::SeqCst),
        "the handler must stay installed across interrupts"
    );
}
