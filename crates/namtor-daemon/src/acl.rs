// SPDX-License-Identifier: GPL-3.0-only
//! Socket ACL: `SO_PEERCRED` extraction + uid-0-or-`namtor-auth`-group decision.
//!
//! Every connection is credential-checked via `SO_PEERCRED` (spec §3.2). The
//! caller must be uid 0 (root) or a member of group `namtor-auth` (for display
//! managers like GDM). No other caller is served.

use std::os::fd::AsFd;

use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};

use crate::DaemonError;

/// Peer credentials extracted from a connected `AF_UNIX` socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCreds {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

/// Read `SO_PEERCRED` from a connected Unix socket.
pub fn get_peer_creds<F: AsFd>(stream: &F) -> Result<PeerCreds, DaemonError> {
    let creds = getsockopt(stream, PeerCredentials)
        .map_err(|e| DaemonError::Socket(format!("SO_PEERCRED: {e}")))?;
    Ok(PeerCreds {
        pid: creds.pid(),
        uid: creds.uid(),
        gid: creds.gid(),
    })
}

/// Authorise a peer: uid 0 OR member of `namtor-auth` (gid match).
pub fn authorise(creds: &PeerCreds, namtor_auth_gid: u32) -> bool {
    creds.uid == 0 || creds.gid == namtor_auth_gid
}

/// Resolve the `namtor-auth` group's GID by parsing the system group file.
///
/// Honours `NAMTOR_GROUP_FILE` (env override, for tests); defaults to
/// `/etc/group`. Returns an error if the group is not found.
pub fn resolve_namtor_auth_gid() -> Result<u32, DaemonError> {
    let path = std::env::var("NAMTOR_GROUP_FILE").unwrap_or_else(|_| "/etc/group".into());
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| DaemonError::Acl(format!("read {path}: {e}")))?;
    for line in contents.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 3 && fields[0] == "namtor-auth" {
            let gid = fields[2]
                .parse::<u32>()
                .map_err(|e| DaemonError::Acl(format!("parse namtor-auth gid: {e}")))?;
            return Ok(gid);
        }
    }
    Err(DaemonError::Acl("namtor-auth group not found".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockType};
    use nix::unistd::{getgid, getuid};

    #[test]
    fn get_peer_creds_returns_own_uid_gid() {
        let (a, b) = socketpair(
            AddressFamily::Unix,
            SockType::Stream,
            None,
            SockFlag::empty(),
        )
        .expect("socketpair");
        let creds = get_peer_creds(&a).expect("get_peer_creds");
        assert_eq!(creds.uid, getuid().as_raw() as u32);
        assert_eq!(creds.gid, getgid().as_raw() as u32);
        drop((a, b));
    }

    #[test]
    fn authorise_root_passes() {
        let creds = PeerCreds {
            pid: 1,
            uid: 0,
            gid: 0,
        };
        assert!(authorise(&creds, 1000));
    }

    #[test]
    fn authorise_auth_group_member_passes() {
        let creds = PeerCreds {
            pid: 1,
            uid: 1000,
            gid: 1000,
        };
        assert!(authorise(&creds, 1000));
    }

    #[test]
    fn authorise_unauthorised_uid_fails() {
        let creds = PeerCreds {
            pid: 1,
            uid: 1000,
            gid: 999,
        };
        assert!(!authorise(&creds, 1000));
    }

    /// A non-root uid whose gid does not match the auth group is rejected,
    /// while root passes regardless of gid. Proves the ACL cannot be spoofed
    /// by a peer that sets its gid to a non-auth value, and that root is the
    /// only gid-independent bypass.
    #[test]
    fn authorise_rejects_spoofed_non_root_non_auth_group() {
        let spoofed = PeerCreds {
            pid: 1,
            uid: 1000,
            gid: 999,
        };
        assert!(
            !authorise(&spoofed, 1000),
            "non-root with non-auth gid must be rejected"
        );
        let root = PeerCreds {
            pid: 1,
            uid: 0,
            gid: 999,
        };
        assert!(authorise(&root, 1000), "root must pass regardless of gid");
    }

    #[test]
    fn resolve_gid_from_temp_group_file() {
        let tmp = tempfile::NamedTempFile::new().expect("tmpfile");
        std::fs::write(tmp.path(), "root:x:0:\nnamtor-auth:x:998:gdm\n").expect("write");
        std::env::set_var("NAMTOR_GROUP_FILE", tmp.path());
        let gid = resolve_namtor_auth_gid().expect("resolve");
        assert_eq!(gid, 998);
        std::env::remove_var("NAMTOR_GROUP_FILE");
    }

    #[test]
    fn resolve_gid_missing_group_errors() {
        let tmp = tempfile::NamedTempFile::new().expect("tmpfile");
        std::fs::write(tmp.path(), "root:x:0:\nother:x:1:\n").expect("write");
        std::env::set_var("NAMTOR_GROUP_FILE", tmp.path());
        let err = resolve_namtor_auth_gid().unwrap_err();
        assert!(matches!(err, DaemonError::Acl(_)));
        std::env::remove_var("NAMTOR_GROUP_FILE");
    }
}
