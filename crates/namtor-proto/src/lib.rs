// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-proto`: wire protocol shared between the daemon, the worker, and the
//! future PAM module.
//!
//! Two independent seams live here, kept in separate modules so the PAM module
//! (M5) can depend on [`pam`] without pulling worker or capture types:
//! - [`worker`] — daemon↔worker pipe: frame metadata in, embedding vectors out.
//! - [`pam`] — daemon↔PAM socket: `SO_PEERCRED`-authenticated, nonce-bound,
//!   three-phase (Prepare/Verify/Release) request/reply.
//!
//! Both modules are serialised with the length-delimited framing in
//! [`framing`]: a 4-byte big-endian length header followed by a JSON payload.
//! JSON is chosen for M3 because it is trivially inspectable and the auth path
//! is not bandwidth-bound (one request/reply per auth, small control messages
//! over the pipe; frame *bytes* travel a shared `memfd`, not this wire).

pub mod framing;
pub mod pam;
pub mod worker;
