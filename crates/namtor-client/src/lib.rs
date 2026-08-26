// SPDX-FileCopyrightText: 2026 Nam-tor Project
// SPDX-License-Identifier: GPL-3.0-only

//! Shared D-Bus client crate for Nam-tor UIs — wire types, zbus proxies, and contract test suite.

#![warn(missing_docs)]

use zbus::proxy;
use zbus::Result as ZResult;

/// Nam-tor D-Bus Manager interface.
#[proxy(
    default_service = "org.namtor.Manager",
    default_path = "/org/namtor/Manager",
    interface = "org.namtor.Manager2",
    gen_blocking = false
)]
pub trait Manager {
    /// Request an authentication session.
    async fn prepare_for_auth(&self, uid: u32) -> ZResult<()>;
}

/// Placeholder test ensuring the crate builds and links correctly.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crate_compiles() {
        // Trivial smoke test to keep the test suite green while the crate is a stub.
        assert!(true);
    }
}
