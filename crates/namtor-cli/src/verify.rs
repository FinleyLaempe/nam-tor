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
        Ok(reply) => {
            let (line, code) = render_reply(&reply);
            println!("{line}");
            ExitCode::from(code)
        }
        Err(e) => {
            eprintln!("verify failed: {e}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

/// Exit codes. Scripts branch on these, so they are named rather than
/// spelled at the one site that returns them.
pub const EXIT_NO_MATCH: u8 = 1;
pub const EXIT_UNAVAILABLE: u8 = 2;
pub const EXIT_RATE_LIMITED: u8 = 3;
pub const EXIT_REFUSED: u8 = 4;
pub const EXIT_ERROR: u8 = 5;

/// The line to print for a reply, and the exit code to leave with.
///
/// Split out of [`run_verify`] because the whole of it — five distinct exit
/// codes and the §8.3 rule below — was only reachable with a live daemon
/// socket, so none of it was tested.
pub fn render_reply(reply: &namtor_proto::pam::AuthReply) -> (String, u8) {
    match reply.verdict {
        Verdict::Match => (format!("MATCH (score={:.4})", reply.score), 0),
        // Spec §8.3: a non-match says only that it did not match. The reason
        // string can carry PAD-veto detail, and telling an attacker which
        // signal caught them is how they learn what to change. The score is
        // already published on the Match line, so it is not new information
        // here.
        Verdict::NoMatch => (
            format!("NO MATCH (score={:.4})", reply.score),
            EXIT_NO_MATCH,
        ),
        Verdict::Unavailable => (
            format!("UNAVAILABLE: {}", reply.reason.clone().unwrap_or_default()),
            EXIT_UNAVAILABLE,
        ),
        Verdict::RateLimited => (
            format!("RATE LIMITED: {}", reply.reason.clone().unwrap_or_default()),
            EXIT_RATE_LIMITED,
        ),
        Verdict::Refused => (
            format!("REFUSED: {}", reply.reason.clone().unwrap_or_default()),
            EXIT_REFUSED,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_proto::pam::{AuthReply, FailureReason};

    fn reply(verdict: Verdict, reason: Option<&str>, failure: Option<FailureReason>) -> AuthReply {
        AuthReply {
            nonce: [0; 16],
            verdict,
            score: 0.4242,
            reason: reason.map(String::from),
            failure,
        }
    }

    /// The §8.3 rule, which had no test: a PAD veto arrives as a `NoMatch`
    /// whose reason names it, and the CLI must not repeat that. An edit
    /// adding `reply.reason` to this arm would hand an attacker the name of
    /// the signal that caught them.
    #[test]
    fn a_pad_veto_is_not_named_on_the_no_match_line() {
        let (line, code) = render_reply(&reply(
            Verdict::NoMatch,
            Some("pad veto: specular highlight"),
            Some(FailureReason::PadVeto),
        ));
        assert_eq!(code, EXIT_NO_MATCH);
        assert!(!line.to_lowercase().contains("pad"), "{line}");
        assert!(!line.contains("specular"), "{line}");
        assert!(!line.contains("veto"), "{line}");
        assert_eq!(line, "NO MATCH (score=0.4242)");
    }

    /// A plain non-match reads the same as a vetoed one. If it did not, the
    /// exit code and the line would together distinguish them.
    #[test]
    fn a_plain_non_match_is_indistinguishable_from_a_vetoed_one() {
        let vetoed = render_reply(&reply(
            Verdict::NoMatch,
            Some("pad veto: moire"),
            Some(FailureReason::PadVeto),
        ));
        let plain = render_reply(&reply(Verdict::NoMatch, Some("below threshold"), None));
        assert_eq!(vetoed, plain);
    }

    /// Scripts branch on these. Each verdict gets its own code, and a match
    /// is the only success.
    #[test]
    fn each_verdict_has_its_own_exit_code() {
        let codes = [
            (Verdict::Match, 0),
            (Verdict::NoMatch, EXIT_NO_MATCH),
            (Verdict::Unavailable, EXIT_UNAVAILABLE),
            (Verdict::RateLimited, EXIT_RATE_LIMITED),
            (Verdict::Refused, EXIT_REFUSED),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (verdict, want) in codes {
            let (_, got) = render_reply(&reply(verdict, Some("detail"), None));
            assert_eq!(got, want, "{verdict:?}");
            assert!(seen.insert(got), "exit code {got} is used twice");
        }
        assert!(
            !seen.contains(&EXIT_ERROR),
            "the transport-failure code must not collide with a verdict"
        );
    }

    /// The conditions the user can act on keep their reason: an unavailable
    /// daemon, a rate limit, a refused remote login.
    #[test]
    fn actionable_conditions_keep_their_reason() {
        for verdict in [Verdict::Unavailable, Verdict::RateLimited, Verdict::Refused] {
            let (line, _) = render_reply(&reply(verdict, Some("the camera is busy"), None));
            assert!(
                line.contains("the camera is busy"),
                "{verdict:?} dropped its reason: {line}"
            );
        }
    }

    /// A missing reason must not print "None" or panic.
    #[test]
    fn a_missing_reason_prints_nothing_extra() {
        let (line, _) = render_reply(&reply(Verdict::Unavailable, None, None));
        assert_eq!(line, "UNAVAILABLE: ");
    }

    /// The score is printed to four places on both match and non-match, so
    /// the two lines cannot be told apart by their formatting.
    #[test]
    fn the_score_is_formatted_identically_either_way() {
        let (m, _) = render_reply(&reply(Verdict::Match, None, None));
        let (n, _) = render_reply(&reply(Verdict::NoMatch, None, None));
        assert!(m.ends_with("(score=0.4242)"), "{m}");
        assert!(n.ends_with("(score=0.4242)"), "{n}");
    }
}
