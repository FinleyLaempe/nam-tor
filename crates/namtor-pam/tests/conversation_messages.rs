// SPDX-License-Identifier: GPL-3.0-only
//! Gate **G1.3** — every failure mode in spec §8.3 produces its specific
//! user-visible message — driven through **real libpam**.
//!
//! This loads the built `libnamtor_pam.so` into a PAM stack and runs
//! `pam_authenticate()` with a conversation function of our own, then asserts
//! on the text the module actually handed to the application. Nothing here
//! touches `/etc/pam.d` and nothing needs root: `pam_start_confdir()` (PAM
//! 1.4+) points the stack at a temporary directory, and the module is told
//! which socket to use through its own PAM argument.
//!
//! That last part is why this test can exist at all. The module used to read
//! `NAMTOR_SOCKET` from the environment, which was both untestable-in-place
//! and an authentication bypass — see `AuthConfig::from_pam`.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use namtor_proto::framing::{decode, encode};
use namtor_proto::pam::{AuthBody, AuthReply, AuthRequest, FailureReason, HelloMsg, Verdict};

// --- libpam ----------------------------------------------------------------

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: Option<
        unsafe extern "C" fn(
            c_int,
            *mut *const PamMessage,
            *mut *mut PamResponse,
            *mut c_void,
        ) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

enum PamHandle {}

#[link(name = "pam")]
extern "C" {
    fn pam_start_confdir(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        confdir: *const c_char,
        pamh: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut PamHandle, status: c_int) -> c_int;
}

extern "C" {
    fn malloc(n: usize) -> *mut c_void;
}

thread_local! {
    /// Everything the module said to the application, in order.
    ///
    /// Thread-local, not a global: libtest runs these in parallel and PAM
    /// calls the conversation function on the calling thread, so a shared
    /// buffer let one test read another's messages — which is exactly how
    /// this suite first "failed", with every case seeing "Face not
    /// recognised" from a neighbour.
    static SEEN: std::cell::RefCell<Vec<(c_int, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A conversation function that records messages and replies to nothing.
unsafe extern "C" fn recording_conv(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    _appdata: *mut c_void,
) -> c_int {
    for i in 0..num_msg as isize {
        // Linux-PAM passes an array of pointers to messages.
        let m: &PamMessage = unsafe { &**msg.offset(i) };
        let text = if m.msg.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(m.msg) }
                .to_string_lossy()
                .into_owned()
        };
        SEEN.with(|s| s.borrow_mut().push((m.msg_style, text)));
    }
    // Allocate the response array the module is expected to free, exactly as
    // a real application does — so the module's free path is exercised too.
    if num_msg > 0 {
        let n = num_msg as usize * std::mem::size_of::<PamResponse>();
        let p = unsafe { malloc(n) } as *mut PamResponse;
        if !p.is_null() {
            for i in 0..num_msg as isize {
                unsafe {
                    (*p.offset(i)).resp = std::ptr::null_mut();
                    (*p.offset(i)).resp_retcode = 0;
                }
            }
            unsafe { *resp = p };
        }
    }
    0 // PAM_SUCCESS
}

// --- a fake daemon ---------------------------------------------------------

/// Serve exactly one connection, answering Verify with `reply`.
fn spawn_fake_daemon(sock: PathBuf, verdict: Verdict, failure: Option<FailureReason>) {
    let listener = UnixListener::bind(&sock).expect("bind fake daemon socket");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
        let mut writer = std::io::BufWriter::new(stream);
        let nonce = [9u8; 16];
        if encode(
            &HelloMsg {
                server_version: 1,
                nonce,
            },
            &mut writer,
        )
        .is_err()
        {
            return;
        }
        let _ = writer.flush();
        while let Ok(v) = decode(&mut reader) {
            let Ok(AuthRequest::Auth(body)) = serde_json::from_value::<AuthRequest>(v) else {
                continue;
            };
            let reply = build_reply(&body, verdict, failure);
            if encode(&reply, &mut writer).is_err() {
                return;
            }
            let _ = writer.flush();
        }
    });
}

fn build_reply(body: &AuthBody, verdict: Verdict, failure: Option<FailureReason>) -> AuthReply {
    use namtor_proto::pam::AuthPhase;
    match body.phase {
        // Only the Verify phase carries the verdict under test.
        AuthPhase::Verify => AuthReply {
            nonce: body.nonce,
            verdict,
            score: 0.0,
            reason: Some("test".into()),
            failure,
        },
        _ => AuthReply {
            nonce: body.nonce,
            verdict: Verdict::Unavailable,
            score: 0.0,
            reason: Some("ack".into()),
            failure: None,
        },
    }
}

// --- harness ---------------------------------------------------------------

fn module_path() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    for profile in ["debug", "release"] {
        let p = root.join(profile).join("libnamtor_pam.so");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Run one authentication against the real module and return what it said.
fn authenticate_with(
    verdict: Verdict,
    failure: Option<FailureReason>,
) -> (c_int, Vec<(c_int, String)>) {
    let module = module_path().expect("libnamtor_pam.so must be built");
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("auth.sock");
    spawn_fake_daemon(sock.clone(), verdict, failure);

    // A PAM stack of exactly one module, told where the daemon is through its
    // own argument — the root-owned channel, not the environment.
    let confdir = dir.path().join("pam.d");
    std::fs::create_dir_all(&confdir).expect("mkdir confdir");
    std::fs::write(
        confdir.join("namtor-test"),
        format!(
            "auth required {} socket={}\naccount required {}\n",
            module.display(),
            sock.display(),
            module.display()
        ),
    )
    .expect("write service file");

    SEEN.with(|s| s.borrow_mut().clear());

    let service = CString::new("namtor-test").unwrap();
    let user = CString::new(whoami()).unwrap();
    let confdir_c = CString::new(confdir.to_str().unwrap()).unwrap();
    let conv = PamConv {
        conv: Some(recording_conv),
        appdata_ptr: std::ptr::null_mut(),
    };
    let mut pamh: *mut PamHandle = std::ptr::null_mut();

    let rc = unsafe {
        pam_start_confdir(
            service.as_ptr(),
            user.as_ptr(),
            &conv,
            confdir_c.as_ptr(),
            &mut pamh,
        )
    };
    assert_eq!(rc, 0, "pam_start_confdir failed");

    let auth_rc = unsafe { pam_authenticate(pamh, 0) };
    unsafe { pam_end(pamh, auth_rc) };

    let seen = SEEN.with(|s| s.borrow().clone());
    (auth_rc, seen)
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "root".into())
}

fn messages(seen: &[(c_int, String)]) -> Vec<String> {
    seen.iter().map(|(_, t)| t.clone()).collect()
}

// --- the §8.3 table, through real PAM --------------------------------------

/// Each condition must produce exactly the message spec §8.3 names.
#[test]
fn every_8_3_condition_shows_its_message_through_pam() {
    let cases = [
        (FailureReason::NoCamera, "No infrared camera found"),
        (FailureReason::CameraBusy, "Camera is busy"),
        (
            FailureReason::PrivacyShutter,
            "Camera privacy shutter is closed",
        ),
        (FailureReason::TemplatesSealed, "Face unlock unavailable"),
        (FailureReason::Timeout, "Face not recognised"),
        (FailureReason::RateLimited, "Too many attempts"),
    ];
    for (failure, expected) in cases {
        let verdict = match failure {
            FailureReason::RateLimited => Verdict::RateLimited,
            _ => Verdict::Unavailable,
        };
        let (_rc, seen) = authenticate_with(verdict, Some(failure));
        let msgs = messages(&seen);
        assert!(
            msgs.iter().any(|m| m == expected),
            "{failure:?}: expected {expected:?} to reach the application, got {msgs:?}"
        );
    }
}

/// A PAD veto must be indistinguishable from a plain non-match in what the
/// user is shown.
#[test]
fn a_pad_veto_is_indistinguishable_from_a_non_match() {
    let (_, veto) = authenticate_with(Verdict::NoMatch, Some(FailureReason::PadVeto));
    let (_, plain) = authenticate_with(Verdict::NoMatch, None);
    assert_eq!(
        messages(&veto),
        messages(&plain),
        "a PAD veto must not be tellable apart from a non-match"
    );
    assert!(messages(&veto).iter().any(|m| m == "Face not recognised"));
}

/// "No enrollment for this user" is silent by design: announcing it would
/// tell anyone at the machine who is and is not enrolled.
#[test]
fn a_user_with_no_enrollment_is_told_nothing() {
    let (_, seen) = authenticate_with(Verdict::Unavailable, Some(FailureReason::NotEnrolled));
    assert!(
        messages(&seen).iter().all(|m| m.is_empty()),
        "not-enrolled must stay silent, got {:?}",
        messages(&seen)
    );
}

/// A success says nothing and returns success.
#[test]
fn a_match_says_nothing() {
    let (rc, seen) = authenticate_with(Verdict::Match, None);
    assert_eq!(rc, 0, "a Match must authenticate");
    assert!(
        messages(&seen).iter().all(|m| m.is_empty()),
        "a successful authentication must not narrate itself: {:?}",
        messages(&seen)
    );
}

/// Every §8.3 condition must fail closed — spec G5.3.
#[test]
fn every_condition_fails_closed_through_pam() {
    for failure in FailureReason::ALL {
        let verdict = match failure {
            FailureReason::RateLimited => Verdict::RateLimited,
            FailureReason::PadVeto => Verdict::NoMatch,
            _ => Verdict::Unavailable,
        };
        let (rc, _) = authenticate_with(verdict, Some(failure));
        assert_ne!(
            rc, 0,
            "{failure:?} must NOT authenticate (PAM_SUCCESS is 0)"
        );
    }
}
