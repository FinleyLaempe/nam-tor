// SPDX-License-Identifier: GPL-3.0-only
//! `status` subcommand: reports the selected device, stream state, and
//! protection level.

/// Protection level surfaced to the user. `Reduced` when the emitter is
/// always-on (spec §4.10.5): liveness via strobe differential is unavailable,
/// so only the embedding + colour fusion carry the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionLevel {
    Full,
    Reduced,
}

impl std::fmt::Display for ProtectionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtectionLevel::Full => f.write_str("full"),
            ProtectionLevel::Reduced => f.write_str("reduced"),
        }
    }
}

/// Stream state as reported to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    Open,
    Closed,
}

impl std::fmt::Display for StreamState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamState::Open => f.write_str("open"),
            StreamState::Closed => f.write_str("closed"),
        }
    }
}

/// A snapshot of camera manager state, suitable for status reporting.
/// The daemon builds this from its live `namtor-camera` components; the CLI
/// formats it. Decoupling the snapshot from the live state lets the CLI test
/// without a real device.
#[derive(Debug, Clone)]
pub struct CameraManager {
    pub device_path: Option<std::path::PathBuf>,
    pub vid_pid: Option<String>,
    pub emitter_method: Option<String>,
    pub stream: StreamState,
    pub protection: ProtectionLevel,
}

impl CameraManager {
    pub fn new(
        device_path: Option<std::path::PathBuf>,
        vid_pid: Option<String>,
        emitter_method: Option<String>,
        stream: StreamState,
        protection: ProtectionLevel,
    ) -> Self {
        Self {
            device_path,
            vid_pid,
            emitter_method,
            stream,
            protection,
        }
    }
}

/// Formats the camera manager state as a human-readable status report.
pub fn run_status(camera: &CameraManager) -> String {
    let mut out = String::new();

    match &camera.device_path {
        Some(p) => out.push_str(&format!("device: {}\n", p.display())),
        None => out.push_str("device: (none)\n"),
    }
    match &camera.vid_pid {
        Some(v) => out.push_str(&format!("vid:pid: {v}\n")),
        None => out.push_str("vid:pid: (unknown)\n"),
    }
    match &camera.emitter_method {
        Some(m) => out.push_str(&format!("emitter: {m}\n")),
        None => out.push_str("emitter: (unknown)\n"),
    }
    out.push_str(&format!("stream: {}\n", camera.stream));
    out.push_str(&format!("protection: {}\n", camera.protection));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn status_reports_selected_device_and_full_protection() {
        let mgr = CameraManager::new(
            Some(Path::new("/dev/video2").to_path_buf()),
            Some("3277:0018".into()),
            Some("msxu_mode".into()),
            StreamState::Closed,
            ProtectionLevel::Full,
        );
        let report = run_status(&mgr);

        assert!(
            report.contains("/dev/video2"),
            "report must name the device"
        );
        assert!(report.contains("stream: closed"));
        assert!(report.contains("protection: full"));
        assert!(report.contains("3277:0018"));
        assert!(report.contains("msxu_mode"));
    }

    #[test]
    fn status_reports_reduced_protection_for_always_on() {
        let mgr = CameraManager::new(
            Some(Path::new("/dev/video2").to_path_buf()),
            Some("3277:0018".into()),
            Some("always_on".into()),
            StreamState::Open,
            ProtectionLevel::Reduced,
        );
        let report = run_status(&mgr);

        assert!(report.contains("protection: reduced"));
        assert!(report.contains("stream: open"));
    }

    #[test]
    fn status_with_no_device() {
        let mgr = CameraManager::new(None, None, None, StreamState::Closed, ProtectionLevel::Full);
        let report = run_status(&mgr);

        assert!(report.contains("device: (none)"));
        assert!(report.contains("vid:pid: (unknown)"));
        assert!(report.contains("emitter: (unknown)"));
    }
}
