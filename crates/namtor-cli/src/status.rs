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
    pub stream_requester: Option<namtor_proto::pam::Requester>,
    pub warm_remaining_ms: Option<u64>,
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
            stream_requester: None,
            warm_remaining_ms: None,
        }
    }

    pub fn with_requester(
        mut self,
        requester: Option<namtor_proto::pam::Requester>,
        warm_remaining_ms: Option<u64>,
    ) -> Self {
        self.stream_requester = requester;
        self.warm_remaining_ms = warm_remaining_ms;
        self
    }

    /// Build from a daemon [`StatusReply`]. The daemon's three-phase stream
    /// state (Closed/Warming/Verifying) collapses to the CLI's open/closed
    /// view: Warming and Verifying both surface as `Open`.
    pub fn from_status_reply(
        reply: namtor_proto::pam::StatusReply,
        device_path: Option<std::path::PathBuf>,
        vid_pid: Option<String>,
        emitter_method: Option<String>,
        protection: ProtectionLevel,
    ) -> Self {
        let stream = match reply.stream {
            namtor_proto::pam::StreamState::Closed => StreamState::Closed,
            namtor_proto::pam::StreamState::Warming | namtor_proto::pam::StreamState::Verifying => {
                StreamState::Open
            }
        };
        Self {
            device_path,
            vid_pid,
            emitter_method,
            stream,
            protection,
            stream_requester: reply.requester,
            warm_remaining_ms: reply.warm_remaining_ms,
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

    match camera.stream {
        StreamState::Open => match &camera.stream_requester {
            Some(r) => {
                let warm = camera
                    .warm_remaining_ms
                    .map(|ms| format!(", warm {:.1}s remaining", ms as f64 / 1000.0))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "stream: open (requested by uid {} \"{}\"{})\n",
                    r.uid, r.label, warm
                ));
            }
            None => out.push_str("stream: open\n"),
        },
        StreamState::Closed => out.push_str("stream: closed\n"),
    }

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

    #[test]
    fn status_open_stream_with_requester_and_warm_remaining() {
        let mgr = CameraManager::new(
            Some(Path::new("/dev/video2").to_path_buf()),
            None,
            None,
            StreamState::Open,
            ProtectionLevel::Full,
        )
        .with_requester(
            Some(namtor_proto::pam::Requester {
                uid: 1000,
                label: "gdm".into(),
            }),
            Some(12_300),
        );
        let report = run_status(&mgr);

        assert!(report.contains("stream: open"), "got: {report}");
        assert!(report.contains("uid 1000"), "got: {report}");
        assert!(report.contains("\"gdm\""), "got: {report}");
        assert!(report.contains("12.3s remaining"), "got: {report}");
    }

    #[test]
    fn status_open_stream_without_requester() {
        let mgr = CameraManager::new(None, None, None, StreamState::Open, ProtectionLevel::Full);
        let report = run_status(&mgr);
        assert!(
            report.contains("stream: open\n"),
            "open without requester should be bare: {report}"
        );
    }

    #[test]
    fn status_closed_stream_has_no_requester_line() {
        let mgr = CameraManager::new(None, None, None, StreamState::Closed, ProtectionLevel::Full)
            .with_requester(None, None);
        let report = run_status(&mgr);
        assert!(report.contains("stream: closed\n"));
        assert!(!report.contains("requested by"));
    }

    #[test]
    fn from_status_reply_maps_warming_to_open() {
        let reply = namtor_proto::pam::StatusReply {
            stream: namtor_proto::pam::StreamState::Warming,
            requester: Some(namtor_proto::pam::Requester {
                uid: 1000,
                label: "gdm".into(),
            }),
            warm_remaining_ms: Some(5_000),
        };
        let mgr = CameraManager::from_status_reply(reply, None, None, None, ProtectionLevel::Full);
        assert_eq!(mgr.stream, StreamState::Open);
        assert_eq!(mgr.stream_requester.as_ref().unwrap().uid, 1000);
        assert_eq!(mgr.warm_remaining_ms, Some(5_000));

        let report = run_status(&mgr);
        assert!(report.contains("stream: open"));
        assert!(report.contains("5.0s remaining"));
    }

    #[test]
    fn from_status_reply_maps_verifying_to_open() {
        let reply = namtor_proto::pam::StatusReply {
            stream: namtor_proto::pam::StreamState::Verifying,
            requester: Some(namtor_proto::pam::Requester {
                uid: 1000,
                label: "gdm".into(),
            }),
            warm_remaining_ms: None,
        };
        let mgr = CameraManager::from_status_reply(reply, None, None, None, ProtectionLevel::Full);
        assert_eq!(mgr.stream, StreamState::Open);
    }

    #[test]
    fn from_status_reply_maps_closed_to_closed() {
        let reply = namtor_proto::pam::StatusReply {
            stream: namtor_proto::pam::StreamState::Closed,
            requester: None,
            warm_remaining_ms: None,
        };
        let mgr = CameraManager::from_status_reply(reply, None, None, None, ProtectionLevel::Full);
        assert_eq!(mgr.stream, StreamState::Closed);
        assert!(mgr.stream_requester.is_none());
        assert!(mgr.warm_remaining_ms.is_none());
    }
}
