// SPDX-License-Identifier: GPL-3.0-only
//! Re-exports of the `org.namtor.Manager` D-Bus proxies from [`namtor_client`].
//!
//! Both GUI front-ends share the same proxy definitions to prevent drift.

pub use namtor_client::proxy::{Manager2Proxy, Manager3Proxy, ManagerProxy};
