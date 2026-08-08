// SPDX-License-Identifier: GPL-3.0-only
//! In-memory [`ManagerClient`] for tests and headless runs.
//!
//! `FakeManagerClient` is a scripted test double, not a real D-Bus service.
//! It owns a profile table and an optional in-flight enrollment session, and
//! it can be primed (via [`Mode`]) to simulate the failure modes the GNOME
//! UI must handle: daemon unavailable, polkit denial, low-diversity capture,
//! etc.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::client::{
    EnrollmentProgress, FinishResult, ManagerClient, ManagerError, ManagerResult, ProfileId,
    ProfileInfo, QualityLabel, SessionId, StatusInfo,
};

/// Behavioural mode the fake honours until reset. Tests prime this before
/// driving the presenter.
#[derive(Debug, Clone, Default)]
pub enum Mode {
    /// Normal happy-path operation.
    #[default]
    Normal,
    /// `org.namtor.Error.DaemonUnavailable` on every call.
    DaemonDown(String),
    /// `org.namtor.Error.AuthRequired` (polkit) on every mutating call.
    PolkitDenied(String),
    /// Force `get_progress` to report `quality_label == "low"`.
    LowDiversity,
}

#[derive(Debug, Clone)]
struct ProfileRow {
    name: String,
    enabled: bool,
    created: u64,
    last_used: Option<u64>,
}

#[derive(Debug, Clone)]
struct SessionRow {
    profile_name: String,
    frames: u32,
    coverage: u32,
    quality: QualityLabel,
    finished: Option<FinishResult>,
}

/// Test double. Interior-mutable so a single instance can be shared across
/// presenter turns without `&mut` gymnastics in test assertions.
pub struct FakeManagerClient {
    mode: Mutex<Mode>,
    profiles: Mutex<HashMap<String, ProfileRow>>,
    sessions: Mutex<HashMap<String, SessionRow>>,
    seq: AtomicU64,
    clock: AtomicU64,
}

impl Default for FakeManagerClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeManagerClient {
    pub fn new() -> Self {
        Self {
            mode: Mutex::new(Mode::Normal),
            profiles: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(1),
            clock: AtomicU64::new(1_700_000_000),
        }
    }

    pub fn set_mode(&self, mode: Mode) {
        *self.mode.lock().expect("fake mode poisoned") = mode;
    }

    fn mode(&self) -> Mode {
        self.mode.lock().expect("fake mode poisoned").clone()
    }

    fn next_id(&self) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("id-{n}")
    }

    fn next_session(&self) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("sess-{n}")
    }

    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    /// Test helper: did the last `finish_enrollment` produce a stored profile?
    pub fn has_profile(&self, name: &str) -> bool {
        self.profiles
            .lock()
            .expect("profiles poisoned")
            .values()
            .any(|p| p.name == name)
    }
}

impl ManagerClient for FakeManagerClient {
    fn list_profiles(&mut self) -> ManagerResult<Vec<ProfileInfo>> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::Normal | Mode::PolkitDenied(_) | Mode::LowDiversity => {}
        }
        let profiles = self.profiles.lock().expect("profiles poisoned");
        let mut out: Vec<ProfileInfo> = profiles
            .iter()
            .map(|(id, row)| ProfileInfo {
                id: id.clone(),
                name: row.name.clone(),
                enabled: row.enabled,
                created: row.created,
                last_used: row.last_used,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn create_profile(&mut self, name: &str) -> ManagerResult<SessionId> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::PolkitDenied(msg) => return Err(ManagerError::PolkitDenied(msg)),
            Mode::Normal | Mode::LowDiversity => {}
        }
        let profiles = self.profiles.lock().expect("profiles poisoned");
        if profiles.values().any(|p| p.name == name) {
            return Err(ManagerError::Conflict(format!(
                "profile named {name:?} already exists"
            )));
        }
        let session = self.next_session();
        let quality = match self.mode() {
            Mode::LowDiversity => QualityLabel::Low,
            _ => QualityLabel::High,
        };
        self.sessions.lock().expect("sessions poisoned").insert(
            session.clone(),
            SessionRow {
                profile_name: name.to_string(),
                frames: 0,
                coverage: 0,
                quality,
                finished: None,
            },
        );
        Ok(SessionId(session))
    }

    fn start_enrollment(&mut self, session: &SessionId) -> ManagerResult<()> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::PolkitDenied(msg) => return Err(ManagerError::PolkitDenied(msg)),
            Mode::Normal | Mode::LowDiversity => {}
        }
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        match sessions.get_mut(&session.0) {
            Some(row) => {
                row.frames = 1;
                row.coverage = 5;
                Ok(())
            }
            None => Err(ManagerError::NotFound(session.0.clone())),
        }
    }

    fn get_progress(&mut self, session: &SessionId) -> ManagerResult<EnrollmentProgress> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::Normal | Mode::PolkitDenied(_) | Mode::LowDiversity => {}
        }
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        let row = sessions
            .get_mut(&session.0)
            .ok_or_else(|| ManagerError::NotFound(session.0.clone()))?;
        // Advance the scripted capture one step per poll.
        row.frames = row.frames.saturating_add(3);
        row.coverage = row.coverage.saturating_add(20).min(100);
        if matches!(self.mode(), Mode::LowDiversity) {
            row.quality = QualityLabel::Low;
            // Low-diversity sessions never reach full coverage.
            row.coverage = row.coverage.min(40);
        } else if row.coverage >= 80 {
            row.quality = QualityLabel::High;
        } else if row.coverage >= 40 {
            row.quality = QualityLabel::Medium;
        }
        Ok(EnrollmentProgress {
            session_id: session.0.clone(),
            frames_captured: row.frames,
            quality_label: row.quality,
            coverage_percent: row.coverage,
        })
    }

    fn finish_enrollment(&mut self, session: &SessionId) -> ManagerResult<FinishResult> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::PolkitDenied(msg) => return Err(ManagerError::PolkitDenied(msg)),
            Mode::Normal | Mode::LowDiversity => {}
        }
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        let row = sessions
            .get_mut(&session.0)
            .ok_or_else(|| ManagerError::NotFound(session.0.clone()))?;
        if row.finished.is_some() {
            return Err(ManagerError::NotEnrolling(session.0.clone()));
        }
        let (success, reason) = match self.mode() {
            Mode::LowDiversity => (false, Some("insufficient diversity".into())),
            _ => (true, None),
        };
        let profile_id = if success {
            let id = self.next_id();
            let created = self.tick();
            self.profiles.lock().expect("profiles poisoned").insert(
                id.clone(),
                ProfileRow {
                    name: row.profile_name.clone(),
                    enabled: true,
                    created,
                    last_used: None,
                },
            );
            Some(id)
        } else {
            None
        };
        let result = FinishResult {
            success,
            profile_id: profile_id.clone(),
            reason,
        };
        row.finished = Some(result.clone());
        sessions.remove(&session.0);
        Ok(result)
    }

    fn cancel_enrollment(&mut self, session: &SessionId) -> ManagerResult<()> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::Normal | Mode::PolkitDenied(_) | Mode::LowDiversity => {}
        }
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        match sessions.remove(&session.0) {
            Some(_) => Ok(()),
            None => Err(ManagerError::NotEnrolling(session.0.clone())),
        }
    }

    fn merge_profiles(&mut self, src: &ProfileId, dst: &ProfileId) -> ManagerResult<()> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::PolkitDenied(msg) => return Err(ManagerError::PolkitDenied(msg)),
            Mode::Normal | Mode::LowDiversity => {}
        }
        let mut profiles = self.profiles.lock().expect("profiles poisoned");
        if !profiles.contains_key(&src.0) {
            return Err(ManagerError::NotFound(src.0.clone()));
        }
        if !profiles.contains_key(&dst.0) {
            return Err(ManagerError::NotFound(dst.0.clone()));
        }
        // Merge = delete src, keep dst (the store owns the template merge).
        profiles.remove(&src.0);
        Ok(())
    }

    fn replace_profile(&mut self, id: &ProfileId, name: &str) -> ManagerResult<()> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::PolkitDenied(msg) => return Err(ManagerError::PolkitDenied(msg)),
            Mode::Normal | Mode::LowDiversity => {}
        }
        let mut profiles = self.profiles.lock().expect("profiles poisoned");
        match profiles.get_mut(&id.0) {
            Some(row) => {
                row.name = name.to_string();
                Ok(())
            }
            None => Err(ManagerError::NotFound(id.0.clone())),
        }
    }

    fn rename_profile(&mut self, id: &ProfileId, new_name: &str) -> ManagerResult<()> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::PolkitDenied(msg) => return Err(ManagerError::PolkitDenied(msg)),
            Mode::Normal | Mode::LowDiversity => {}
        }
        let mut profiles = self.profiles.lock().expect("profiles poisoned");
        match profiles.get_mut(&id.0) {
            Some(row) => {
                row.name = new_name.to_string();
                Ok(())
            }
            None => Err(ManagerError::NotFound(id.0.clone())),
        }
    }

    fn enable_profile(&mut self, id: &ProfileId, enabled: bool) -> ManagerResult<()> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::PolkitDenied(msg) => return Err(ManagerError::PolkitDenied(msg)),
            Mode::Normal | Mode::LowDiversity => {}
        }
        let mut profiles = self.profiles.lock().expect("profiles poisoned");
        match profiles.get_mut(&id.0) {
            Some(row) => {
                row.enabled = enabled;
                Ok(())
            }
            None => Err(ManagerError::NotFound(id.0.clone())),
        }
    }

    fn delete_profile(&mut self, id: &ProfileId) -> ManagerResult<()> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::PolkitDenied(msg) => return Err(ManagerError::PolkitDenied(msg)),
            Mode::Normal | Mode::LowDiversity => {}
        }
        let mut profiles = self.profiles.lock().expect("profiles poisoned");
        match profiles.remove(&id.0) {
            Some(_) => Ok(()),
            None => Err(ManagerError::NotFound(id.0.clone())),
        }
    }

    fn status(&mut self) -> ManagerResult<StatusInfo> {
        match self.mode() {
            Mode::DaemonDown(msg) => return Err(ManagerError::DaemonUnavailable(msg)),
            Mode::Normal | Mode::PolkitDenied(_) | Mode::LowDiversity => {}
        }
        Ok(StatusInfo {
            daemon_reachable: true,
            camera_present: true,
            active_profile: None,
            version: Some("0.1.0".into()),
            always_on_emitter: false,
        })
    }
}
