// SPDX-License-Identifier: GPL-3.0-only
//! Persistent per-uid authentication backoff (spec §8.4 / G5 rate-limit gate).
//!
//! Each failed authentication attempt advances an exponential backoff schedule
//! keyed by `target_uid`. Successful auth resets the per-uid state. The store
//! persists to disk as JSON with an atomic temp-write+fsync+rename so a crash
//! mid-write cannot leave a corrupt or half-written file.
//!
//! The daemon consults [`BackoffStore::is_locked`] before dispatching a verify
//! and returns [`Verdict::RateLimited`](namtor_proto::pam::Verdict::RateLimited)
//! when locked. Persistence survives daemon restarts, so a restart does not
//! forgive an attacker mid-bruteforce.
//!
//! Corruption policy: a missing file is the normal first-boot state and yields
//! an empty store (`Ok`). A file that exists but fails to parse as the expected
//! JSON shape is treated as tampering and yields `Err` so the operator is
//! forced to inspect the path rather than silently starting from zero.
//!
//! `Instant` is a monotonic, non-serialisable clock bound to a single process.
//! We therefore persist only the *attempt count* per uid (the durable signal
//! of an in-progress bruteforce) and let the lock window restart with the
//! daemon process. This is the conservative choice: a restart shortens a
//! current lock but never erases the attempt history that drives the schedule.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Schedule parameters. `base_ms` is the unit delay; each subsequent failure
/// multiplies the previous delay by `multiplier`, capped at `max_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BackoffConfig {
    pub base_ms: u64,
    pub max_ms: u64,
    pub multiplier: f32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            base_ms: 1000,
            max_ms: 300_000,
            multiplier: 2.0,
        }
    }
}

/// Per-uid backoff state held in memory. The `Instant` fields are process-local
/// and not persisted; only `attempts` survives a restart (see module docs).
#[derive(Debug, Clone, PartialEq)]
pub struct BackoffState {
    attempts: u32,
    last_failure: Option<Instant>,
    backoff_until: Option<Instant>,
    config: BackoffConfig,
}

impl BackoffState {
    pub fn new(config: BackoffConfig) -> Self {
        Self {
            attempts: 0,
            last_failure: None,
            backoff_until: None,
            config,
        }
    }

    /// Record a failure at `now`. Advances `attempts`, recomputes
    /// `backoff_until = now + min(base * multiplier^attempts, max)`, and
    /// stamps `last_failure`.
    pub fn record_failure(&mut self, now: Instant) {
        self.attempts = self.attempts.saturating_add(1);
        let delay_ms = self.next_delay_ms();
        self.backoff_until = Some(now + std::time::Duration::from_millis(delay_ms));
        self.last_failure = Some(now);
    }

    /// Record a success: reset attempts and clear both instants.
    pub fn record_success(&mut self) {
        self.attempts = 0;
        self.backoff_until = None;
        self.last_failure = None;
    }

    /// True if `backoff_until` is set and lies strictly in the future relative
    /// to `now`. A `None` backoff is never locked.
    pub fn is_locked(&self, now: Instant) -> bool {
        match self.backoff_until {
            Some(until) => until > now,
            None => false,
        }
    }

    /// Milliseconds remaining until unlock. Zero if not locked or already
    /// past the deadline.
    pub fn remaining_ms(&self, now: Instant) -> u64 {
        match self.backoff_until {
            Some(until) if until > now => (until - now).as_millis() as u64,
            _ => 0,
        }
    }

    /// Current attempt count (observable for tests and status reporting).
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    fn next_delay_ms(&self) -> u64 {
        // Spec: base * multiplier^attempts. After the first record_failure
        // attempts==1, so the first delay is base*mult^1 = base*mult.
        let exp = self.attempts;
        let mut factor: f32 = 1.0;
        for _ in 0..exp {
            factor *= self.config.multiplier;
            if !factor.is_finite() {
                return self.config.max_ms;
            }
        }
        let raw = (self.config.base_ms as f32) * factor;
        if !raw.is_finite() || raw < 0.0 {
            return self.config.max_ms;
        }
        let capped = raw.min(self.config.max_ms as f32);
        if capped > (u64::MAX as f32) {
            self.config.max_ms
        } else {
            capped as u64
        }
    }
}

/// Per-uid backoff store with atomic disk persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct BackoffStore {
    states: HashMap<u32, BackoffState>,
    config: BackoffConfig,
}

/// On-disk shape. Only the attempt count is durable; lock-window instants are
/// process-local and reconstructed on load (see module docs).
#[derive(Serialize, Deserialize)]
struct PersistedStore {
    states: HashMap<u32, u32>,
}

impl BackoffStore {
    pub fn new(config: BackoffConfig) -> Self {
        Self {
            states: HashMap::new(),
            config,
        }
    }

    /// Record a failure for `uid` at `now`, creating a fresh state entry if
    /// the uid is new. New entries use the store's `config`; existing entries
    /// retain their own config so a config change mid-run does not rewrite
    /// history.
    pub fn record_failure(&mut self, uid: u32, now: Instant) {
        let config = self.config;
        self.states
            .entry(uid)
            .or_insert_with(|| BackoffState::new(config))
            .record_failure(now);
    }

    /// Record a success for `uid` and drop the entry (an absent entry and an
    /// entry with attempts==0 are observationally equivalent; dropping keeps
    /// the persisted file small).
    pub fn record_success(&mut self, uid: u32) {
        self.states.remove(&uid);
    }

    pub fn is_locked(&self, uid: u32, now: Instant) -> bool {
        self.states.get(&uid).is_some_and(|s| s.is_locked(now))
    }

    pub fn remaining_ms(&self, uid: u32, now: Instant) -> u64 {
        self.states.get(&uid).map_or(0, |s| s.remaining_ms(now))
    }

    /// Persist the store to `path` atomically: write to a sibling temp file,
    /// fsync, then rename over the target. A crash after the temp write but
    /// before rename leaves the old file (or none) intact.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let persisted = PersistedStore {
            states: self
                .states
                .iter()
                .map(|(uid, s)| (*uid, s.attempts))
                .collect(),
        };
        let json = serde_json::to_vec_pretty(&persisted).map_err(io::Error::other)?;
        let tmp = temp_path(path);
        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Load a store from `path`. A missing file yields an empty store (`Ok`).
    /// A file that exists but cannot be parsed as the expected JSON shape
    /// yields `Err` (distinct from the missing-file case) so the operator can
    /// distinguish first-boot from tampering. Loaded uids inherit the caller's
    /// `config`; the persisted per-uid attempt counts are preserved so an
    /// in-progress bruteforce is not forgiven by a restart.
    pub fn load(path: &Path, config: BackoffConfig) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::new(config));
            }
            Err(e) => return Err(e),
        };
        let persisted: PersistedStore = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let mut states = HashMap::with_capacity(persisted.states.len());
        for (uid, attempts) in persisted.states {
            let mut s = BackoffState::new(config);
            s.attempts = attempts;
            states.insert(uid, s);
        }
        Ok(Self { states, config })
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let stem = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "backoff".to_string());
    let mut p = path.to_path_buf();
    p.set_file_name(format!(".{stem}.tmp"));
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn cfg() -> BackoffConfig {
        BackoffConfig {
            base_ms: 1000,
            max_ms: 300_000,
            multiplier: 2.0,
        }
    }

    // Given a fresh store, when a failure is recorded,
    // then the uid is locked and remaining_ms is positive.
    #[test]
    fn record_failure_locks() {
        let mut s = BackoffStore::new(cfg());
        let now = Instant::now();
        s.record_failure(1000, now);
        assert!(s.is_locked(1000, now));
        assert!(s.remaining_ms(1000, now) > 0);
    }

    // Given a locked uid, when record_success is called on the same uid,
    // then the uid is no longer locked.
    #[test]
    fn record_success_unlocks() {
        let mut s = BackoffStore::new(cfg());
        let now = Instant::now();
        s.record_failure(1000, now);
        assert!(s.is_locked(1000, now));
        s.record_success(1000);
        assert!(!s.is_locked(1000, now));
        assert_eq!(s.remaining_ms(1000, now), 0);
    }

    // Given successive failures, when the delay is observed,
    // then it grows exponentially (base*mult, base*mult^2, ...).
    #[test]
    fn exponential_growth() {
        let mut state = BackoffState::new(cfg());
        let now = Instant::now();
        state.record_failure(now);
        // attempts == 1 -> base * 2^1 = 2000
        assert_eq!(state.remaining_ms(now), 2000);
        state.record_failure(now);
        // attempts == 2 -> base * 2^2 = 4000
        assert_eq!(state.remaining_ms(now), 4000);
        state.record_failure(now);
        // attempts == 3 -> base * 2^3 = 8000
        assert_eq!(state.remaining_ms(now), 8000);
    }

    // Given many failures, when the delay is observed,
    // then it is capped at max_ms.
    #[test]
    fn max_cap_enforced() {
        let mut state = BackoffState::new(BackoffConfig {
            base_ms: 1000,
            max_ms: 10_000,
            multiplier: 2.0,
        });
        let now = Instant::now();
        for _ in 0..20 {
            state.record_failure(now);
        }
        assert_eq!(state.remaining_ms(now), 10_000);
    }

    // Given two uids, when one fails,
    // then the other remains unlocked.
    #[test]
    fn per_uid_isolation() {
        let mut s = BackoffStore::new(cfg());
        let now = Instant::now();
        s.record_failure(1000, now);
        assert!(s.is_locked(1000, now));
        assert!(!s.is_locked(1001, now));
        assert_eq!(s.remaining_ms(1001, now), 0);
    }

    // Given a store with state, when save+load round-trips,
    // then the loaded store preserves the per-uid attempt count.
    #[test]
    fn save_load_preserves_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backoff.json");
        let mut s = BackoffStore::new(cfg());
        let now = Instant::now();
        s.record_failure(1000, now);
        s.record_failure(1000, now);
        s.save(&path).unwrap();
        let loaded = BackoffStore::load(&path, cfg()).unwrap();
        let st = loaded.states.get(&1000).expect("uid 1000 must be present");
        assert_eq!(st.attempts, 2);
    }

    // Given a tampered JSON file, when load is called,
    // then it returns Err (distinct from a missing file's Ok).
    #[test]
    fn corrupt_state_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backoff.json");
        fs::write(&path, b"{ this is not valid json").unwrap();
        let res = BackoffStore::load(&path, cfg());
        assert!(res.is_err(), "corrupt file must error, not silently reset");
    }

    // Given a missing file, when load is called,
    // then it returns Ok with an empty store.
    #[test]
    fn missing_file_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let s = BackoffStore::load(&path, cfg()).unwrap();
        assert!(s.states.is_empty());
    }

    // Given a backoff_until set in the future relative to now, when is_locked
    // is queried at an earlier instant, then it returns true — the lock is
    // defined by backoff_until > now, so a backward clock jump (simulated by
    // querying earlier) cannot unlock.
    #[test]
    fn clock_rollback_still_locked() {
        let mut state = BackoffState::new(cfg());
        let base = Instant::now();
        state.record_failure(base);
        // backoff_until = base + 2s; querying at base + 1s is still locked.
        assert!(state.is_locked(base + Duration::from_secs(1)));
        assert!(state.remaining_ms(base + Duration::from_secs(1)) > 0);
    }

    // Given a locked uid, when record_success is called on a *different* uid,
    // then the locked uid stays locked — only a same-uid success unlocks.
    #[test]
    fn untrusted_reset_does_not_unlock_other_uid() {
        let mut s = BackoffStore::new(cfg());
        let now = Instant::now();
        s.record_failure(1000, now);
        s.record_success(1001);
        assert!(
            s.is_locked(1000, now),
            "success on uid 1001 must not unlock 1000"
        );
        s.record_success(1000);
        assert!(!s.is_locked(1000, now));
    }

    // Given a successful save, when the temp file path is inspected,
    // then no .tmp file is left behind (rename succeeded).
    #[test]
    fn save_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backoff.json");
        let s = BackoffStore::new(cfg());
        s.save(&path).unwrap();
        let tmp = temp_path(&path);
        assert!(!tmp.exists(), "temp file must be renamed away");
        assert!(path.exists(), "target file must exist");
    }

    // Given a loaded store, when a new failure is recorded,
    // then the schedule continues from the persisted attempt count
    // (restart does not forgive an in-progress bruteforce).
    #[test]
    fn restart_continues_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backoff.json");
        let mut s = BackoffStore::new(cfg());
        let now = Instant::now();
        s.record_failure(1000, now);
        s.record_failure(1000, now); // attempts == 2
        s.save(&path).unwrap();
        let mut loaded = BackoffStore::load(&path, cfg()).unwrap();
        // One more failure on the loaded store -> attempts == 3 -> 8000ms.
        loaded.record_failure(1000, now);
        let st = loaded.states.get(&1000).expect("uid present");
        assert_eq!(st.attempts, 3);
        assert_eq!(st.remaining_ms(now), 8000);
    }
}
