// SPDX-License-Identifier: GPL-3.0-only
//! The v3 handshake, against real servers.
//!
//! `negotiate_v3` used to call `org.namtor.Manager3.NegotiateVersion`, which
//! no interface has ever exported; against a real daemon it answered
//! `UnknownMethod`. It reads the `ApiVersion` property now, and `connect()`
//! calls it, so the read has to work against a server that exports the
//! property — and the two ways it can fail have to be told apart, because
//! they are told to the user differently.

use std::os::unix::net::UnixStream;

use namtor_client::dbus_client::DbusManagerClient;
use namtor_client::types::{ManagerError, API_VERSION_V3};
use namtor_client::ManagerClient;

/// A server that exports `org.namtor.Manager3` with an `ApiVersion` property.
struct Manager3Like {
    version: u32,
}

#[zbus::interface(name = "org.namtor.Manager3")]
impl Manager3Like {
    #[zbus(property, name = "ApiVersion")]
    fn api_version(&self) -> u32 {
        self.version
    }
}

/// A server that exports something else entirely — a daemon too old to have
/// a v3 interface at all.
struct NoV3;

#[zbus::interface(name = "org.namtor.Manager")]
impl NoV3 {
    #[zbus(property, name = "ApiVersion")]
    fn api_version(&self) -> u32 {
        1
    }
}

enum Server {
    V3(u32),
    OnlyV1,
}

fn client(server: Server) -> (DbusManagerClient, zbus::Connection) {
    let (a, b) = UnixStream::pair().expect("socketpair");
    let guid = zbus::Guid::generate();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("fake_manager3".into())
        .spawn(move || {
            let result = zbus::block_on(async {
                let builder = zbus::connection::Builder::unix_stream(a)
                    .server(guid)
                    .map_err(|e| zbus::Error::Failure(e.to_string()))?
                    .p2p();
                let builder = match server {
                    Server::V3(version) => {
                        builder.serve_at("/org/namtor/Manager", Manager3Like { version })?
                    }
                    Server::OnlyV1 => builder.serve_at("/org/namtor/Manager", NoV3)?,
                };
                builder.build().await
            });
            let _ = tx.send(result);
        })
        .expect("spawn");
    let conn = zbus::blocking::connection::Builder::unix_stream(b)
        .p2p()
        .build()
        .expect("client");
    let server = rx.recv().expect("channel").expect("server");
    (DbusManagerClient::from_connection(conn), server)
}

/// The property really is readable over the wire — this is the call that
/// replaced a method that did not exist.
#[test]
fn a_matching_daemon_agrees_on_v3() {
    let (c, _s) = client(Server::V3(API_VERSION_V3));
    assert_eq!(c.negotiate_v3(API_VERSION_V3).expect("negotiate"), 3);
}

/// A daemon on another version of the same interface is a mismatch, and the
/// message names both numbers so the user can see which side to move.
#[test]
fn a_daemon_on_another_version_is_a_mismatch() {
    let (c, _s) = client(Server::V3(4));
    let err = c.negotiate_v3(API_VERSION_V3).unwrap_err();
    let msg = match err {
        ManagerError::IncompatibleVersion(m) => m,
        other => panic!("expected IncompatibleVersion, got {other:?}"),
    };
    assert!(msg.contains('3') && msg.contains('4'), "{msg}");
}

/// A daemon too old to have a v3 interface at all must reach the user as a
/// version mismatch, not as "the daemon could not be reached" — which is
/// what it would be if the D-Bus error fell through to `Transport`, and
/// which would send them looking for a daemon that is running and
/// answering.
#[test]
fn a_daemon_without_v3_is_a_mismatch_not_a_transport_failure() {
    let (c, _s) = client(Server::OnlyV1);
    let err = c.negotiate_v3(API_VERSION_V3).unwrap_err();
    let msg = match err {
        ManagerError::IncompatibleVersion(m) => m,
        other => panic!("expected IncompatibleVersion, got {other:?}"),
    };
    assert!(
        msg.contains("org.namtor.Manager3"),
        "the message must name what is missing: {msg}"
    );
}
