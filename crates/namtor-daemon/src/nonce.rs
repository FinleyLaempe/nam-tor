// SPDX-License-Identifier: GPL-3.0-only
//! Nonce service: issue daemon-side nonces, bind replies to them, reject
//! replays (spec §3.2).
//!
//! The daemon issues a nonce in `HelloMsg` on connect. The client echoes it in
//! `AuthBody::nonce`; the daemon returns it in `AuthReply::nonce`. A nonce
//! used once is marked seen and cannot validate again — a captured success
//! reply cannot be replayed.

use std::collections::HashSet;
use std::sync::Mutex;

use rand::RngCore;

use crate::DaemonError;

/// Thread-safe nonce issuer + anti-replay validator.
pub struct NonceService {
    seen: Mutex<HashSet<[u8; 16]>>,
}

impl NonceService {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(HashSet::new()),
        }
    }

    /// Issue a fresh random nonce.
    pub fn issue(&self) -> [u8; 16] {
        let mut nonce = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut nonce);
        nonce
    }

    /// Validate that `reply_nonce` equals `req_nonce` and has not been seen
    /// before. Marks the nonce as seen on success.
    pub fn validate_reply(
        &self,
        req_nonce: [u8; 16],
        reply_nonce: [u8; 16],
    ) -> Result<(), DaemonError> {
        if req_nonce != reply_nonce {
            return Err(DaemonError::Acl("nonce mismatch".into()));
        }
        let mut seen = self.seen.lock().expect("nonce mutex poisoned");
        if !seen.insert(req_nonce) {
            return Err(DaemonError::Acl("nonce replay detected".into()));
        }
        Ok(())
    }
}

impl Default for NonceService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn issues_distinct_nonces() {
        let svc = NonceService::new();
        let a = svc.issue();
        let b = svc.issue();
        assert_ne!(a, b);
    }

    #[test]
    fn validate_matching_nonce_succeeds() {
        let svc = NonceService::new();
        let n = svc.issue();
        svc.validate_reply(n, n).expect("first use ok");
    }

    #[test]
    fn validate_mismatched_nonce_fails() {
        let svc = NonceService::new();
        let n = svc.issue();
        let other = [0xff; 16];
        let err = svc.validate_reply(n, other).unwrap_err();
        assert!(matches!(err, DaemonError::Acl(_)));
    }

    #[test]
    fn replayed_nonce_fails() {
        let svc = NonceService::new();
        let n = svc.issue();
        svc.validate_reply(n, n).expect("first use ok");
        let err = svc.validate_reply(n, n).unwrap_err();
        assert!(matches!(err, DaemonError::Acl(_)));
    }

    #[test]
    fn concurrent_issue_no_collisions() {
        let svc = Arc::new(NonceService::new());
        let mut handles = Vec::new();
        for _ in 0..10 {
            let svc = Arc::clone(&svc);
            handles.push(thread::spawn(move || {
                let mut nonces = Vec::new();
                for _ in 0..1000 {
                    nonces.push(svc.issue());
                }
                nonces
            }));
        }
        let mut all: Vec<[u8; 16]> = Vec::new();
        for h in handles {
            all.extend(h.join().expect("thread"));
        }
        let set: HashSet<[u8; 16]> = all.iter().copied().collect();
        assert_eq!(set.len(), all.len(), "nonce collision detected");
    }
}
