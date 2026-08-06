// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-cli verify`: test authentication via the daemon socket.

use std::process::ExitCode;

use namtor_pam::client::{authenticate, AuthConfig};
use namtor_proto::pam::Verdict;

pub fn run_verify(user: Option<String>) -> ExitCode {
    let (uid, username) = match &user {
        Some(name) => {
            let u = nix::unistd::User::from_name(name)
                .ok()
                .flatten()
                .unwrap_or_else(|| {
                    eprintln!("error: user '{name}' not found");
                    std::process::exit(1);
                });
            (u.uid.as_raw(), u.name)
        }
        None => {
            let uid = nix::unistd::getuid().as_raw();
            let username = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
                .ok()
                .flatten()
                .map(|u| u.name)
                .unwrap_or_else(|| format!("uid-{uid}"));
            (uid, username)
        }
    };

    eprintln!("verifying uid={uid} ({username})...");

    let cfg = AuthConfig {
        socket: std::env::var("NAMTOR_SOCKET")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(namtor_pam::client::DEFAULT_SOCKET)),
        timeout: namtor_pam::client::DEFAULT_TIMEOUT,
        target_uid: uid,
        rhost: None,
    };

    match authenticate(&cfg) {
        Ok(reply) => match reply.verdict {
            Verdict::Match => {
                println!("MATCH (score={:.4})", reply.score);
                ExitCode::SUCCESS
            }
            Verdict::NoMatch => {
                // Spec §8.3: do not surface the NoMatch reason — it could
                // carry PAD-veto detail if the daemon ever regresses. A
                // plain NoMatch is all the user/attacker should see.
                println!("NO MATCH (score={:.4})", reply.score);
                ExitCode::from(1)
            }
            Verdict::Unavailable => {
                println!("UNAVAILABLE: {}", reply.reason.unwrap_or_default());
                ExitCode::from(2)
            }
            Verdict::RateLimited => {
                println!("RATE LIMITED: {}", reply.reason.unwrap_or_default());
                ExitCode::from(3)
            }
            Verdict::Refused => {
                println!("REFUSED: {}", reply.reason.unwrap_or_default());
                ExitCode::from(4)
            }
        },
        Err(e) => {
            eprintln!("verify failed: {e}");
            ExitCode::from(5)
        }
    }
}
