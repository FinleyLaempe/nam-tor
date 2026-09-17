// SPDX-License-Identifier: GPL-3.0-only
//! Block rendering and candidate verification for the PAM editor.
//!
//! The Nam-tor block is a three-line marker:
//!
//! ```text
//! # namtor-begin
//! auth sufficient pam_namtor.so
//! # namtor-end
//! ```
//!
//! It is inserted before the first `auth` line in the file. Stock lines
//! outside the block are never modified, deleted, or reordered.

use std::fs;
use std::path::Path;

use crate::classify::classify_service;
use crate::editor::{EditError, NamtorControl};
use crate::model::PamServiceCompatibility;

pub(crate) const BEGIN_MARKER: &str = "# namtor-begin";
pub(crate) const END_MARKER: &str = "# namtor-end";

/// True if the content already contains a Nam-tor block.
pub(crate) fn has_block(content: &str) -> bool {
    content.lines().any(|l| l.trim() == BEGIN_MARKER)
}

/// True if the block's auth line matches the given control field.
pub(crate) fn block_matches(content: &str, control: NamtorControl) -> bool {
    let expected = format!("auth {} pam_namtor.so", control.as_str());
    content.lines().any(|l| l.trim() == expected)
}

/// Remove the Nam-tor block (markers + content) from the content.
/// Returns the stock content without the block.
pub(crate) fn remove_block(content: &str) -> String {
    let mut result = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        if line.trim() == BEGIN_MARKER {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == END_MARKER {
                in_block = false;
            }
            continue;
        }
        result.push(line);
    }
    let joined = result.join("\n");
    if content.ends_with('\n') && !joined.is_empty() {
        format!("{joined}\n")
    } else {
        joined
    }
}

/// Render the candidate content: insert the Nam-tor block before the
/// first non-comment `auth` line. `stock` must already have any existing
/// block removed.
pub(crate) fn render_enable(stock: &str, control: NamtorControl) -> String {
    let block = format!(
        "{BEGIN_MARKER}\nauth {} pam_namtor.so\n{END_MARKER}",
        control.as_str()
    );
    let lines: Vec<&str> = stock.lines().collect();
    let mut result = Vec::new();
    let mut inserted = false;

    for line in &lines {
        if !inserted && is_insertion_point(line) {
            result.push(block.clone());
            inserted = true;
        }
        result.push((*line).to_string());
    }

    if !inserted && !lines.is_empty() {
        result.push(block);
    }

    let joined = result.join("\n");
    if stock.ends_with('\n') || joined.is_empty() {
        format!("{joined}\n")
    } else {
        joined
    }
}

/// True if the content has any auth-relevant lines: direct `auth` entries
/// or `@include` directives (which can bring in auth lines through includes).
pub(crate) fn has_auth_lines(content: &str) -> bool {
    content
        .lines()
        .any(|l| is_auth_line(l) || l.trim_start().starts_with("@include"))
}

/// True if the line is an insertion point: an `auth` entry or an
/// `@include` directive (Debian-style include that may bring auth lines).
fn is_insertion_point(line: &str) -> bool {
    let trimmed = line.trim_start();
    is_auth_line(trimmed) || trimmed.starts_with("@include")
}

/// True if the line starts with `auth` (a PAM auth-type entry).
fn is_auth_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("auth ") || trimmed.starts_with("auth\t")
}

/// True if the line is an auth entry referencing a password-fallback
/// module or a known shared-auth include.
fn is_password_fallback(line: &str) -> bool {
    if !is_auth_line(line) {
        return false;
    }
    let lower = line.to_lowercase();
    contains_any(
        &lower,
        &[
            "pam_unix",
            "pam_systemd_home",
            "pam_env",
            "common-auth",
            "system-auth",
            "password-auth",
        ],
    )
}

/// True if the line is an auth entry referencing `pam_deny.so`.
fn is_deny_line(line: &str) -> bool {
    is_auth_line(line) && line.contains("pam_deny")
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Verify the rendered candidate is safe:
/// - No `pam_deny.so` before the block.
/// - Password fallback exists after the block (direct or through includes).
/// - No `pam_deny.so` between the block and the first password fallback.
pub(crate) fn verify_candidate(
    candidate: &str,
    pamd_root: &Path,
    service_id: &str,
) -> Result<(), EditError> {
    let lines: Vec<&str> = candidate.lines().collect();
    let begin = lines
        .iter()
        .position(|l| l.trim() == BEGIN_MARKER)
        .ok_or_else(|| EditError::Io("begin marker not found".into()))?;
    let end = lines
        .iter()
        .position(|l| l.trim() == END_MARKER)
        .ok_or_else(|| EditError::Io("end marker not found".into()))?;

    for line in &lines[..begin] {
        if is_deny_line(line) {
            return Err(EditError::DenyBeforeBlock(service_id.into()));
        }
    }

    let mut found_fallback = false;
    for line in &lines[end + 1..] {
        if is_deny_line(line) && !found_fallback {
            return Err(EditError::DenyBeforePassword(service_id.into()));
        }
        if is_password_fallback(line) {
            found_fallback = true;
        }
    }

    if !found_fallback {
        verify_through_includes(candidate, pamd_root, service_id)?;
    }
    Ok(())
}

/// Verify fallback through include chains by writing the candidate to a
/// temp file and classifying it (which resolves includes).
pub(crate) fn verify_fallback_after_disable(
    stock: &str,
    pamd_root: &Path,
    service_id: &str,
) -> Result<(), EditError> {
    verify_through_includes(stock, pamd_root, service_id)
}

fn verify_through_includes(
    content: &str,
    pamd_root: &Path,
    service_id: &str,
) -> Result<(), EditError> {
    // Classify the STOCK stack, with our own block removed.
    //
    // `content` is the CANDIDATE — it already contains the namtor block — and
    // classify_service short-circuits on `has_namtor` and returns Safe before
    // it ever evaluates the password-fallback rule. Handing it the candidate
    // therefore asked "is this safe?" of a file whose namtor block guaranteed
    // the answer yes: the check was defeated by the very block it exists to
    // validate, and a service with no password fallback installed cleanly.
    //
    // The question this function must answer is whether a fallback exists
    // INDEPENDENT of namtor — i.e. whether the user can still get in when the
    // daemon is down — so the stock stack is the correct subject.
    let stock = remove_block(content);
    let temp = pamd_root.join(format!(".namtor.verify.{service_id}"));
    fs::write(&temp, &stock)?;
    let result = classify_service(service_id, &temp, pamd_root);
    let _ = fs::remove_file(&temp);
    match result {
        Ok(info) => {
            if info.compatibility == PamServiceCompatibility::Unsupported {
                Err(EditError::NoPasswordFallback(format!(
                    "{service_id}: no password fallback through includes"
                )))
            } else {
                Ok(())
            }
        }
        Err(e) => Err(EditError::NoPasswordFallback(format!(
            "{service_id}: classification failed: {e:?}"
        ))),
    }
}
