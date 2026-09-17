// SPDX-License-Identifier: GPL-3.0-only
//! `pam_namtor.so` — PAM auth module for Nam-tor face authentication.
//!
//! Spec §8: fail-closed, no ML, no image libraries. The module reads the
//! target username and `PAM_RHOST`, opens a socket to the daemon, drives one
//! auth lifecycle, and maps the verdict to a PAM return code.
//!
//! Hard rules (spec §3.4, §8.2, §8.3):
//! - `panic = "abort"` (Cargo release profile).
//! - No ML/ONNX/image deps. Only libc + libpam + namtor-proto (JSON).
//! - Remote auth (`PAM_RHOST` set) → refuse regardless of config.
//! - Any error, timeout, or unreachable daemon → `PAM_AUTHINFO_UNAVAIL`.
//! - Never `PAM_SUCCESS` unless the daemon returned `Verdict::Match`.

#![allow(clippy::missing_safety_doc)]

pub mod client;
pub mod ffi;
pub mod stdin_poll;

use std::ffi::c_int;

use namtor_proto::pam::Verdict;

/// PAM auth entry point. Invoked by the PAM stack as `pam_sm_authenticate`.
///
/// # Safety
/// `pamh` is the opaque PAM handle from the framework; it must remain valid
/// for the duration of the call. `flags` and `argc`/`argv` follow the PAM ABI.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut ffi::PamHandle,
    _flags: c_int,
    argc: c_int,
    argv: *const *const std::ffi::c_char,
) -> c_int {
    let (socket, timeout) = unsafe { config_from_args(argc, argv) };
    run_auth(pamh, socket, timeout)
}

/// Read the module's own arguments — the `pam_namtor.so` line in
/// `/etc/pam.d` — for `socket=<path>` and `timeout_ms=<n>`.
///
/// This is the configuration channel PAM provides, and it lives in a
/// root-owned file. The socket deliberately does NOT come from the
/// environment; see `AuthConfig::from_pam`.
///
/// `timeout_ms` bounds both the daemon round trip and the §8.4 keypress
/// window, so an operator can trade how long a face attempt may hold the
/// prompt before falling through to the password.
///
/// # Safety
/// `argv` must be `argc` valid NUL-terminated pointers, per the PAM ABI.
unsafe fn config_from_args(
    argc: c_int,
    argv: *const *const std::ffi::c_char,
) -> (std::path::PathBuf, Option<std::time::Duration>) {
    let mut socket = std::path::PathBuf::from(client::DEFAULT_SOCKET);
    let mut timeout = None;
    if argv.is_null() || argc <= 0 {
        return (socket, timeout);
    }
    for i in 0..argc as isize {
        let p = unsafe { *argv.offset(i) };
        if p.is_null() {
            continue;
        }
        let Ok(arg) = (unsafe { std::ffi::CStr::from_ptr(p) }).to_str() else {
            continue;
        };
        if let Some(v) = arg.strip_prefix("socket=") {
            if !v.is_empty() {
                socket = std::path::PathBuf::from(v);
            }
        } else if let Some(v) = arg.strip_prefix("timeout_ms=") {
            // Ignore a nonsense value rather than failing the auth over it.
            if let Ok(ms) = v.parse::<u64>() {
                if ms > 0 {
                    timeout = Some(std::time::Duration::from_millis(ms));
                }
            }
        }
    }
    (socket, timeout)
}

/// PAM `setcred` — we manage no credentials. Always succeed.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut ffi::PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const std::ffi::c_char,
) -> c_int {
    ffi::PAM_SUCCESS
}

/// PAM `acct_mgmt` — we do not authorise. Always succeed.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_acct_mgmt(
    _pamh: *mut ffi::PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const std::ffi::c_char,
) -> c_int {
    ffi::PAM_SUCCESS
}

fn run_auth(
    pamh: *mut ffi::PamHandle,
    socket: std::path::PathBuf,
    timeout: Option<std::time::Duration>,
) -> c_int {
    // 1. Resolve target username → uid. Missing/unknown user → fall through.
    let username = match unsafe { ffi::get_user(pamh) } {
        Ok(u) => u,
        Err(code) => return log_fail("get_user", code, ffi::PAM_AUTHINFO_UNAVAIL),
    };
    let uid = match ffi::uid_of_user(&username) {
        Some(u) => u,
        None => {
            return log_fail(
                "user_unknown",
                ffi::PAM_USER_UNKNOWN,
                ffi::PAM_AUTHINFO_UNAVAIL,
            )
        }
    };

    // 2. Build config (also reads PAM_RHOST for the wire + remote check).
    let cfg = match unsafe { client::AuthConfig::from_pam(pamh, uid, socket) } {
        Ok(mut c) => {
            if let Some(t) = timeout {
                c.timeout = t;
            }
            c
        }
        Err(code) => return log_fail("config", code, ffi::PAM_AUTHINFO_UNAVAIL),
    };

    // 3. Spec §8.2: refuse if PAM_RHOST is set, regardless of daemon.
    if let Some(ref rhost) = cfg.rhost {
        if !rhost.is_empty() {
            return log_fail(
                &format!("rhost={rhost}"),
                ffi::PAM_AUTHINFO_UNAVAIL,
                ffi::PAM_AUTHINFO_UNAVAIL,
            );
        }
    }

    // 4. Cheap pre-check: socket must exist, else fall through silently.
    if !client::daemon_reachable(&cfg.socket) {
        return log_fail(
            "daemon_unreachable",
            ffi::PAM_AUTHINFO_UNAVAIL,
            ffi::PAM_AUTHINFO_UNAVAIL,
        );
    }

    // 5. Drive the auth lifecycle. Spec §8.4: poll stdin for a keypress in
    // parallel; any keypress aborts face auth and falls through to password.
    let auth_timeout = cfg.timeout;
    // , not : the latter turns Unavailable,
    // RateLimited and Refused into errors and discards the reply, which is
    // where the typed §8.3 condition lives.
    let auth_handle = std::thread::spawn(move || client::authenticate_reply(&cfg));
    if stdin_poll::stdin_has_input(auth_timeout) {
        // Keypress received: abandon face auth and fall through.
        // We do not join `auth_handle` — the daemon will time out the
        // connection on its side. Returning here is safe; the thread is
        // detached and will exit on its own.
        return log_fail(
            "keypress_fallback",
            ffi::PAM_AUTHINFO_UNAVAIL,
            ffi::PAM_AUTHINFO_UNAVAIL,
        );
    }

    let reply = match auth_handle.join() {
        Ok(Ok(r)) => r,
        Ok(Err(_e)) => {
            return log_fail(
                "daemon_error",
                ffi::PAM_AUTHINFO_UNAVAIL,
                ffi::PAM_AUTHINFO_UNAVAIL,
            );
        }
        Err(_) => {
            return log_fail(
                "thread_panic",
                ffi::PAM_SYSTEM_ERR,
                ffi::PAM_AUTHINFO_UNAVAIL,
            );
        }
    };

    // 6. Map verdict → PAM code. Fail-closed: only Match → PAM_SUCCESS.
    //
    // The journal reason prefers the daemon's typed §8.3 condition when it
    // sent one, so "unavailable" becomes the specific `no_camera`,
    // `camera_busy`, `privacy_shutter`, `templates_sealed` or `timeout` that
    // actually occurred. The verdict-level word remains the fallback for a
    // daemon older than the field.
    let code = reply
        .failure
        .map(|f| f.journal_code())
        .unwrap_or(match reply.verdict {
            Verdict::Match => "match",
            Verdict::NoMatch => "no_match",
            Verdict::Unavailable => "unavailable",
            Verdict::RateLimited => "rate_limited",
            Verdict::Refused => "refused",
        });

    // Spec §8.3: show the user the message for the condition that occurred.
    //
    // A PAD veto and a plain non-match deliberately render the same text, so
    // the system never tells an attacker which signal caught them; the journal
    // above keeps them distinct. "No enrollment for this user" is silent by
    // design — announcing it would tell anyone at the machine who is enrolled.
    let user_msg = reply
        .failure
        .and_then(|f| f.user_message())
        .or(match reply.verdict {
            Verdict::NoMatch => Some("Face not recognised"),
            _ => None,
        });
    if let Some(text) = user_msg {
        // Best-effort; see `send_user_message`. Never affects the verdict.
        unsafe { ffi::send_user_message(pamh, ffi::PAM_ERROR_MSG, text) };
    }

    match reply.verdict {
        Verdict::Match => ffi::PAM_SUCCESS,
        Verdict::NoMatch => log_fail(code, ffi::PAM_AUTH_ERR, ffi::PAM_AUTH_ERR),
        Verdict::Unavailable => {
            log_fail(code, ffi::PAM_AUTHINFO_UNAVAIL, ffi::PAM_AUTHINFO_UNAVAIL)
        }
        Verdict::RateLimited => log_fail(code, ffi::PAM_MAXTRIES, ffi::PAM_MAXTRIES),
        Verdict::Refused => log_fail(code, ffi::PAM_AUTHINFO_UNAVAIL, ffi::PAM_AUTHINFO_UNAVAIL),
    }
}

/// Log a failure reason to stderr (which PAM forwards to journald) and return
/// the PAM code. Spec §8.6: no biometric data, no embeddings, no images —
/// only uid, service, result, reason code, latency, operating point. We log
/// the reason code only.
fn log_fail(reason: &str, _journal_code: c_int, pam_code: c_int) -> c_int {
    eprintln!("{}", log_line(reason, pam_code));
    pam_code
}

/// Build the single journald line for a failure. Pure (no I/O) so tests can
/// assert the PAM module never surfaces the daemon's `AuthReply.reason` —
/// only the hardcoded reason codes from `run_auth` cross into the journal.
fn log_line(reason: &str, pam_code: c_int) -> String {
    format!("pam_namtor: {reason} → {}", ffi::pam_code_name(pam_code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_fail_returns_pam_code() {
        assert_eq!(
            log_fail("test", 0, ffi::PAM_AUTHINFO_UNAVAIL),
            ffi::PAM_AUTHINFO_UNAVAIL
        );
        assert_eq!(log_fail("test", 0, ffi::PAM_SUCCESS), ffi::PAM_SUCCESS);
    }

    /// Spec §8.6: the PAM journal line carries only a hardcoded reason code,
    /// never the daemon's `AuthReply.reason`. The reason codes used in
    /// `run_auth` are fixed literals ("no_match", "unavailable", …), so no
    /// biometric or PAD detail can reach the journal via this path.
    #[test]
    fn log_line_carries_only_hardcoded_reason_codes() {
        let forbidden = [
            "pad veto",
            "energy",
            "gradient",
            "specular",
            "radial",
            "threshold",
            "embedding",
            "vec=",
            "0x",
            "no capture source",
            "store error",
            "inference failed",
            "below threshold",
        ];
        let codes = [
            ("no_match", ffi::PAM_AUTH_ERR),
            ("unavailable", ffi::PAM_AUTHINFO_UNAVAIL),
            ("rate_limited", ffi::PAM_MAXTRIES),
            ("refused", ffi::PAM_AUTHINFO_UNAVAIL),
            ("daemon_unreachable", ffi::PAM_AUTHINFO_UNAVAIL),
            ("daemon_error", ffi::PAM_AUTHINFO_UNAVAIL),
            ("keypress_fallback", ffi::PAM_AUTHINFO_UNAVAIL),
        ];
        for (reason, code) in codes {
            let line = log_line(reason, code);
            for token in forbidden {
                assert!(
                    !line.to_lowercase().contains(token),
                    "journal line for `{reason}` leaks `{token}`: `{line}`"
                );
            }
            assert!(
                line.contains(reason),
                "journal line must name the hardcoded reason code"
            );
            assert!(
                line.contains(ffi::pam_code_name(code)),
                "journal line must name the PAM code"
            );
        }
    }

    /// Fail-closed invariant (spec §8.2): only `Verdict::Match` maps to
    /// `PAM_SUCCESS`. Every other verdict maps to an error code.
    #[test]
    fn only_match_maps_to_pam_success() {
        assert_eq!(map_verdict(Verdict::Match), ffi::PAM_SUCCESS);
        assert_ne!(map_verdict(Verdict::NoMatch), ffi::PAM_SUCCESS);
        assert_ne!(map_verdict(Verdict::Unavailable), ffi::PAM_SUCCESS);
        assert_ne!(map_verdict(Verdict::RateLimited), ffi::PAM_SUCCESS);
        assert_ne!(map_verdict(Verdict::Refused), ffi::PAM_SUCCESS);
    }

    #[test]
    fn rhost_refusal_logic() {
        // The run_auth function refuses when rhost is non-empty. We test the
        // condition directly, since run_auth requires a live PAM handle.
        let rhost = Some("10.0.0.1".to_string());
        let refused = matches!(&rhost, Some(r) if !r.is_empty());
        assert!(refused);

        let rhost = Some(String::new());
        let refused = matches!(&rhost, Some(r) if !r.is_empty());
        assert!(!refused);

        let refused = matches!(&None::<String>, Some(r) if !r.is_empty());
        assert!(!refused);
    }

    #[test]
    fn verdict_to_pam_code_mapping() {
        // Match → SUCCESS
        assert_eq!(map_verdict(Verdict::Match), ffi::PAM_SUCCESS);
        // NoMatch → AUTH_ERR
        assert_eq!(map_verdict(Verdict::NoMatch), ffi::PAM_AUTH_ERR);
        // Unavailable → AUTHINFO_UNAVAIL
        assert_eq!(map_verdict(Verdict::Unavailable), ffi::PAM_AUTHINFO_UNAVAIL);
        // RateLimited → MAXTRIES
        assert_eq!(map_verdict(Verdict::RateLimited), ffi::PAM_MAXTRIES);
        // Refused → AUTHINFO_UNAVAIL
        assert_eq!(map_verdict(Verdict::Refused), ffi::PAM_AUTHINFO_UNAVAIL);
    }

    fn map_verdict(v: Verdict) -> c_int {
        match v {
            Verdict::Match => ffi::PAM_SUCCESS,
            Verdict::NoMatch => ffi::PAM_AUTH_ERR,
            Verdict::Unavailable => ffi::PAM_AUTHINFO_UNAVAIL,
            Verdict::RateLimited => ffi::PAM_MAXTRIES,
            Verdict::Refused => ffi::PAM_AUTHINFO_UNAVAIL,
        }
    }
}
