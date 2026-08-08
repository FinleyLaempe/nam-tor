// SPDX-License-Identifier: GPL-3.0-only
//! In-memory [`ManagerClient`] for tests and headless runs.
//!
//! `FakeManagerClient` is a scripted test double, not a real D-Bus service.
//! It owns a profile table, an optional in-flight enrollment session, a PAM
//! service table, and a protection status. It can be primed (via [`Mode`])
//! to simulate the failure modes the GNOME UI must handle: daemon
//! unavailable, polkit denial, low-diversity capture, etc.
//!
//! All state is behind `Mutex` / atomics so the trait methods can take
//! `&self` — this allows the presenter to share the client via `Arc` across
//! async dispatch threads.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::client::{
    EnrollmentProgress, ManagerClient, ManagerError, ManagerResult, PamServiceCompatibility,
    PamServiceInfo, ProfileId, ProfileInfo, ProtectionKeyKind, ProtectionStatus, QualityLabel,
    ReviewOutcome, SessionId, StatusReport,
};

/// Behavioural mode the fake honours until reset. Tests prime this before
/// driving the presenter.
#[derive(Debug, Clone, Default)]
pub enum Mode {
    /// Normal happy-path operation.
    #[default]
    Normal,
    /// `org.namtor.Error.Denied` equivalent on every mutating call.
    DaemonDown(String),
    /// `org.namtor.Error.Denied` (polkit) on every mutating call.
    PolkitDenied(String),
    /// Force progress to report `quality == Low`.
    LowDiversity,
}

#[derive(Debug, Clone)]
struct ProfileRow {
    name: String,
    enabled: bool,
    created: u64,
    last_used: u64,
}

#[derive(Debug, Clone)]
struct SessionRow {
    #[allow(dead_code)]
    uid: u32,
    profile_name: String,
    frames: u32,
    coverage: u32,
    quality: QualityLabel,
    committed: bool,
}

#[derive(Debug, Clone)]
struct PamServiceRow {
    display_name: String,
    enabled: bool,
    compatibility: PamServiceCompatibility,
    description: String,
}

/// Test double. Interior-mutable so a single instance can be shared across
/// presenter turns without `&mut` gymnastics in test assertions.
pub struct FakeManagerClient {
    mode: Mutex<Mode>,
    profiles: Mutex<HashMap<String, ProfileRow>>,
    sessions: Mutex<HashMap<String, SessionRow>>,
    pam_services: Mutex<HashMap<String, PamServiceRow>>,
    protection: Mutex<ProtectionStatus>,
    progress_signals: Mutex<HashMap<String, EnrollmentProgress>>,
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
        let mut pam_services = HashMap::new();
        pam_services.insert(
            "gdm-password".into(),
            PamServiceRow {
                display_name: "GDM Password".into(),
                enabled: false,
                compatibility: PamServiceCompatibility::Safe,
                description: "GNOME Display Manager".into(),
            },
        );
        pam_services.insert(
            "sddm".into(),
            PamServiceRow {
                display_name: "SDDM".into(),
                enabled: false,
                compatibility: PamServiceCompatibility::Risky,
                description: "Simple Desktop Display Manager".into(),
            },
        );
        pam_services.insert(
            "sudo".into(),
            PamServiceRow {
                display_name: "sudo".into(),
                enabled: false,
                compatibility: PamServiceCompatibility::Risky,
                description: "Privilege escalation via sudo".into(),
            },
        );
        pam_services.insert(
            "su".into(),
            PamServiceRow {
                display_name: "su".into(),
                enabled: false,
                compatibility: PamServiceCompatibility::Risky,
                description: "Substitute user identity".into(),
            },
        );
        pam_services.insert(
            "polkit".into(),
            PamServiceRow {
                display_name: "polkit".into(),
                enabled: false,
                compatibility: PamServiceCompatibility::Risky,
                description: "Polkit authentication agent".into(),
            },
        );
        pam_services.insert(
            "systemd-user".into(),
            PamServiceRow {
                display_name: "systemd-user".into(),
                enabled: false,
                compatibility: PamServiceCompatibility::Safe,
                description: "systemd user session".into(),
            },
        );
        pam_services.insert(
            "fingerprint-auth".into(),
            PamServiceRow {
                display_name: "fingerprint-auth".into(),
                enabled: false,
                compatibility: PamServiceCompatibility::Unsupported,
                description: "Fingerprint-greeter (deny-first, no fallback)".into(),
            },
        );

        Self {
            mode: Mutex::new(Mode::Normal),
            profiles: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            pam_services: Mutex::new(pam_services),
            protection: Mutex::new(ProtectionStatus {
                sealing: ProtectionKeyKind::Software,
                has_tpm: false,
                warning: "TPM2 not available; using software fallback".into(),
            }),
            progress_signals: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(1),
            clock: AtomicU64::new(1_700_000_000),
        }
    }

    pub fn set_mode(&self, mode: Mode) {
        *self.mode.lock().expect("fake mode poisoned") = mode;
    }

    /// Set the protection status returned by `get_protection_status`.
    pub fn set_protection_status(&self, status: ProtectionStatus) {
        *self.protection.lock().expect("protection poisoned") = status;
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

    /// Test helper: did the last `commit_profile` produce a stored profile?
    pub fn has_profile(&self, name: &str) -> bool {
        self.profiles
            .lock()
            .expect("profiles poisoned")
            .values()
            .any(|p| p.name == name)
    }

    /// Test helper: inject a progress signal as if the daemon emitted it.
    pub fn emit_progress(&self, progress: EnrollmentProgress) {
        self.progress_signals
            .lock()
            .expect("progress poisoned")
            .insert(progress.session_id.clone(), progress);
    }

    /// Test helper: is the named PAM service currently enabled in the fake?
    pub fn is_pam_service_enabled(&self, service_id: &str) -> bool {
        self.pam_services
            .lock()
            .expect("pam services poisoned")
            .get(service_id)
            .map(|r| r.enabled)
            .unwrap_or(false)
    }

    fn check_mut(&self) -> ManagerResult<()> {
        match self.mode() {
            Mode::DaemonDown(msg) => Err(ManagerError::DaemonUnavailable(msg)),
            Mode::PolkitDenied(msg) => Err(ManagerError::Denied(msg)),
            Mode::Normal | Mode::LowDiversity => Ok(()),
        }
    }

    fn check_read(&self) -> ManagerResult<()> {
        match self.mode() {
            Mode::DaemonDown(msg) => Err(ManagerError::DaemonUnavailable(msg)),
            Mode::Normal | Mode::PolkitDenied(_) | Mode::LowDiversity => Ok(()),
        }
    }
}

impl ManagerClient for FakeManagerClient {
    fn list_profiles(&self) -> ManagerResult<Vec<ProfileInfo>> {
        self.check_read()?;
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

    fn start_enrollment(&self, uid: u32, name: &str) -> ManagerResult<SessionId> {
        self.check_mut()?;
        let profiles = self.profiles.lock().expect("profiles poisoned");
        if profiles.values().any(|p| p.name == name) {
            return Err(ManagerError::Exists(format!(
                "profile named {name:?} already exists"
            )));
        }
        drop(profiles);
        let session = self.next_session();
        let quality = match self.mode() {
            Mode::LowDiversity => QualityLabel::Low,
            _ => QualityLabel::High,
        };
        self.sessions.lock().expect("sessions poisoned").insert(
            session.clone(),
            SessionRow {
                uid,
                profile_name: name.to_string(),
                frames: 1,
                coverage: 5,
                quality,
                committed: false,
            },
        );
        // Emit an initial progress signal so the first poll has data.
        self.emit_progress(EnrollmentProgress {
            session_id: session.clone(),
            frames_captured: 1,
            quality,
            coverage_percent: 5,
        });
        Ok(SessionId(session))
    }

    fn cancel_enrollment(&self, session: &SessionId) -> ManagerResult<()> {
        self.check_read()?;
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        match sessions.remove(&session.0) {
            Some(_) => Ok(()),
            None => Err(ManagerError::SessionNotFound(session.0.clone())),
        }
    }

    fn commit_profile(&self, session: &SessionId) -> ManagerResult<String> {
        self.check_mut()?;
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        let row = sessions
            .get_mut(&session.0)
            .ok_or_else(|| ManagerError::SessionNotFound(session.0.clone()))?;
        if row.committed {
            return Err(ManagerError::State(format!(
                "session {} already committed",
                session.0
            )));
        }
        let (success, profile_id) = match self.mode() {
            Mode::LowDiversity => (false, None),
            _ => {
                let id = self.next_id();
                let created = self.tick();
                self.profiles.lock().expect("profiles poisoned").insert(
                    id.clone(),
                    ProfileRow {
                        name: row.profile_name.clone(),
                        enabled: true,
                        created,
                        last_used: 0,
                    },
                );
                (true, Some(id))
            }
        };
        row.committed = true;
        let session_id = session.0.clone();
        sessions.remove(&session_id);
        if success {
            Ok(profile_id.expect("profile id on success"))
        } else {
            Err(ManagerError::State("insufficient diversity".into()))
        }
    }

    fn review_profile(&self, _uid: u32, id: &ProfileId) -> ManagerResult<ReviewOutcome> {
        self.check_read()?;
        let profiles = self.profiles.lock().expect("profiles poisoned");
        let row = profiles
            .get(&id.0)
            .ok_or_else(|| ManagerError::NotFound(id.0.clone()))?;
        Ok(ReviewOutcome {
            profile_id: id.0.clone(),
            needs_reenroll: false,
            quality: if row.enabled {
                QualityLabel::High
            } else {
                QualityLabel::Medium
            },
        })
    }

    fn rename_profile(&self, _uid: u32, id: &ProfileId, new_name: &str) -> ManagerResult<()> {
        self.check_mut()?;
        let mut profiles = self.profiles.lock().expect("profiles poisoned");
        if profiles.values().any(|p| p.name == new_name) {
            return Err(ManagerError::Exists(format!(
                "profile named {new_name:?} already exists"
            )));
        }
        match profiles.get_mut(&id.0) {
            Some(row) => {
                row.name = new_name.to_string();
                Ok(())
            }
            None => Err(ManagerError::NotFound(id.0.clone())),
        }
    }

    fn enable_profile(&self, _uid: u32, id: &ProfileId, enabled: bool) -> ManagerResult<()> {
        self.check_mut()?;
        let mut profiles = self.profiles.lock().expect("profiles poisoned");
        match profiles.get_mut(&id.0) {
            Some(row) => {
                row.enabled = enabled;
                Ok(())
            }
            None => Err(ManagerError::NotFound(id.0.clone())),
        }
    }

    fn delete_profile(&self, _uid: u32, id: &ProfileId) -> ManagerResult<()> {
        self.check_mut()?;
        let mut profiles = self.profiles.lock().expect("profiles poisoned");
        match profiles.remove(&id.0) {
            Some(_) => Ok(()),
            None => Err(ManagerError::NotFound(id.0.clone())),
        }
    }

    fn merge_profiles(&self, _uid: u32, src: &ProfileId, dst: &ProfileId) -> ManagerResult<()> {
        self.check_mut()?;
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

    fn replace_profile(&self, _uid: u32, id: &ProfileId, name: &str) -> ManagerResult<()> {
        self.check_mut()?;
        let mut profiles = self.profiles.lock().expect("profiles poisoned");
        match profiles.get_mut(&id.0) {
            Some(row) => {
                row.name = name.to_string();
                Ok(())
            }
            None => Err(ManagerError::NotFound(id.0.clone())),
        }
    }

    fn get_status(&self) -> ManagerResult<StatusReport> {
        self.check_read()?;
        let profiles = self.profiles.lock().expect("profiles poisoned");
        let list: Vec<ProfileInfo> = profiles
            .iter()
            .map(|(id, row)| ProfileInfo {
                id: id.clone(),
                name: row.name.clone(),
                enabled: row.enabled,
                created: row.created,
                last_used: row.last_used,
            })
            .collect();
        Ok(StatusReport {
            api_version: 2,
            profiles: list,
            emitter_armed: self
                .sessions
                .lock()
                .expect("sessions poisoned")
                .values()
                .any(|s| !s.committed),
        })
    }

    fn poll_progress(&self, session: &SessionId) -> ManagerResult<Option<EnrollmentProgress>> {
        self.check_read()?;
        // Drain the cached signal if present.
        let cached = self
            .progress_signals
            .lock()
            .expect("progress poisoned")
            .remove(&session.0);
        if cached.is_some() {
            return Ok(cached);
        }
        // No cached signal: advance the scripted capture one step and emit.
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        let row = sessions
            .get_mut(&session.0)
            .ok_or_else(|| ManagerError::SessionNotFound(session.0.clone()))?;
        row.frames = row.frames.saturating_add(3);
        row.coverage = row.coverage.saturating_add(20).min(100);
        if matches!(self.mode(), Mode::LowDiversity) {
            row.quality = QualityLabel::Low;
            row.coverage = row.coverage.min(40);
        } else if row.coverage >= 80 {
            row.quality = QualityLabel::High;
        } else if row.coverage >= 40 {
            row.quality = QualityLabel::Medium;
        }
        Ok(Some(EnrollmentProgress {
            session_id: session.0.clone(),
            frames_captured: row.frames,
            quality: row.quality,
            coverage_percent: row.coverage,
        }))
    }

    // v2 methods

    fn list_pam_services(&self) -> ManagerResult<Vec<PamServiceInfo>> {
        self.check_read()?;
        let services = self.pam_services.lock().expect("pam services poisoned");
        let mut out: Vec<PamServiceInfo> = services
            .iter()
            .map(|(id, row)| PamServiceInfo {
                id: id.clone(),
                display_name: row.display_name.clone(),
                enabled: row.enabled,
                compatibility: row.compatibility,
                description: row.description.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn set_pam_service_enabled(&self, service_id: &str, enabled: bool) -> ManagerResult<bool> {
        self.check_mut()?;
        let mut services = self.pam_services.lock().expect("pam services poisoned");
        match services.get_mut(service_id) {
            Some(row) => {
                row.enabled = enabled;
                Ok(true)
            }
            None => Err(ManagerError::NotFound(service_id.to_string())),
        }
    }

    fn get_protection_status(&self) -> ManagerResult<ProtectionStatus> {
        self.check_read()?;
        Ok(self.protection.lock().expect("protection poisoned").clone())
    }
}
