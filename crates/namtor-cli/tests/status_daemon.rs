// SPDX-License-Identifier: GPL-3.0-only
//! USA-4: `namtor-cli status` reads real daemon state via the D-Bus
//! `GetStatus` method instead of fabricating a closed/full snapshot.
//!
//! The D-Bus `StatusReport` carries the API version, the caller's enrolled
//! profiles, and whether the IR emitter is armed. It does NOT carry device
//! paths, vid:pid, stream state, or protection level — those are the PAM
//! socket status surface, exposed via a different path. The CLI status
//! command renders the D-Bus report directly and falls back to a clear
//! "daemon unreachable" message when the daemon is not available.

use std::process::ExitCode;

use namtor_cli::status::{
    render_status_report, render_status_unreachable, run_status_command, ProfileInfo, StatusError,
    StatusReport, StatusSource,
};

struct OkSource(StatusReport);
impl StatusSource for OkSource {
    fn fetch_status(&self) -> Result<StatusReport, StatusError> {
        Ok(self.0.clone())
    }
}

struct ErrSource(StatusError);
impl StatusSource for ErrSource {
    fn fetch_status(&self) -> Result<StatusReport, StatusError> {
        Err(self.0.clone())
    }
}

fn profile(id: &str, name: &str, enabled: bool) -> ProfileInfo {
    ProfileInfo {
        id: id.into(),
        name: name.into(),
        enabled,
        created: 1_700_000_000,
        last_used: 0,
    }
}

#[test]
fn status_renders_real_daemon_report_with_profiles() {
    let report = StatusReport {
        api_version: 1,
        profiles: vec![
            profile("default", "alice", true),
            profile("work", "alice-work", false),
        ],
        emitter_armed: true,
    };
    let out = render_status_report(&report);
    assert!(out.contains("api version: 1"), "got: {out}");
    assert!(out.contains("profiles:"), "got: {out}");
    assert!(out.contains("alice"), "got: {out}");
    assert!(out.contains("alice-work"), "got: {out}");
    assert!(out.contains("enabled"), "got: {out}");
    assert!(out.contains("emitter: armed"), "got: {out}");
    // Must NOT fabricate the old closed/full snapshot.
    assert!(
        !out.contains("stream: closed"),
        "must not fabricate closed stream: {out}"
    );
    assert!(
        !out.contains("protection: full"),
        "must not fabricate protection level: {out}"
    );
    assert!(
        !out.contains("device: (none)"),
        "must not fabricate missing device: {out}"
    );
}

#[test]
fn status_renders_empty_profile_list() {
    let report = StatusReport {
        api_version: 1,
        profiles: vec![],
        emitter_armed: false,
    };
    let out = render_status_report(&report);
    assert!(out.contains("no profiles"), "got: {out}");
    assert!(out.contains("emitter: disarmed"), "got: {out}");
}

#[test]
fn status_unreachable_does_not_fabricate_state() {
    let err = StatusError::Transport("org.namtor.Manager not owned".into());
    let out = render_status_unreachable(&err);
    assert!(
        out.to_lowercase().contains("daemon unreachable"),
        "got: {out}"
    );
    // The error path must not print fabricated daemon state.
    assert!(!out.contains("stream: closed"), "got: {out}");
    assert!(!out.contains("protection: full"), "got: {out}");
    assert!(!out.contains("device: (none)"), "got: {out}");
}

#[test]
fn status_command_success_returns_success_exit_code() {
    let report = StatusReport {
        api_version: 1,
        profiles: vec![profile("default", "alice", true)],
        emitter_armed: false,
    };
    let src = OkSource(report);
    let code = run_status_command(&src);
    assert_eq!(code, ExitCode::SUCCESS);
}

#[test]
fn status_command_daemon_unreachable_returns_failure() {
    let src = ErrSource(StatusError::Transport("no bus".into()));
    let code = run_status_command(&src);
    assert_eq!(code, ExitCode::FAILURE);
}
