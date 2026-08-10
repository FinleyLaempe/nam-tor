// SPDX-License-Identifier: GPL-3.0-only
//! Service classification — determines compatibility and typed reasons.

use std::path::Path;

use crate::model::{
    PamLine, PamServiceCompatibility, PamServiceInfo, PamType, ScanError, StackSummary,
};
use crate::parser::{parse_service, resolve_includes, ParsedService};

/// Context for classification — the resolved service + its includes.
#[derive(Debug, Clone)]
pub struct ClassificationContext {
    /// The primary service.
    pub service: ParsedService,
    /// Included services (resolved, in order).
    pub includes: Vec<ParsedService>,
}

/// Look up a human-readable label for a known service ID.
pub fn known_service_label(id: &str) -> &'static str {
    match id {
        "gdm-password" => "GDM Password Login (GNOME Display Manager)",
        "gdm-autologin" => "GDM Auto Login (GNOME Display Manager)",
        "sddm" => "SDDM (Simple Desktop Display Manager)",
        "lightdm" => "LightDM (Light Display Manager)",
        "lxdm" => "LXDM (LXDE Display Manager)",
        "sudo" => "sudo",
        "su" => "su",
        "polkit-1" => "Polkit (PolicyKit Authentication Agent)",
        "login" => "Login (console)",
        "sshd" => "SSH Daemon (remote — PAM_RHOST set, refused by pam_namtor)",
        "system-auth" => "System Auth (Arch/RHEL shared auth stack)",
        "password-auth" => "Password Auth (Fedora/RHEL shared auth stack)",
        "common-auth" => "Common Auth (Debian shared auth stack)",
        "systemd-user" => "systemd user session",
        "runuser" => "runuser",
        "su-l" => "su (login shell)",
        _ => "",
    }
}

/// Build a [`PamServiceInfo`] for a scanned PAM service file.
pub fn classify_service(
    id: &str,
    path: &Path,
    pamd_root: &Path,
) -> Result<PamServiceInfo, ScanError> {
    let service = parse_service(id, path)?;
    let includes = resolve_includes(&service, pamd_root)?;

    let ctx = ClassificationContext { service, includes };
    classify_context(&ctx)
}

/// Classify a pre-parsed context (for testing with fixtures already loaded).
pub fn classify_context(ctx: &ClassificationContext) -> Result<PamServiceInfo, ScanError> {
    let summary = summarise_stack(ctx);

    let label = known_service_label(&ctx.service.id);
    let label = if label.is_empty() {
        ctx.service.id.as_str()
    } else {
        label
    };

    let (compatibility, reason) = determine_compatibility(&ctx.service.id, &summary);

    let enabled = summary.has_namtor;

    Ok(PamServiceInfo {
        id: ctx.service.id.clone(),
        label: label.to_string(),
        enabled,
        compatibility,
        reason,
    })
}

/// Summarise the auth-relevant lines across the service and its includes.
fn summarise_stack(ctx: &ClassificationContext) -> StackSummary {
    let mut summary = StackSummary::default();

    // Check the primary service.
    summarise_lines(&ctx.service.lines, &mut summary);

    // Check included services.
    for inc in &ctx.includes {
        summarise_lines(&inc.lines, &mut summary);
    }

    summary
}

/// Update the summary with lines from one file.
fn summarise_lines(lines: &[PamLine], summary: &mut StackSummary) {
    for line in lines {
        match line {
            PamLine::Entry {
                pam_type: PamType::Auth,
                module,
                ..
            } => {
                summary.auth_entry_count += 1;
                if module.contains("pam_namtor") {
                    summary.has_namtor = true;
                }
                if is_password_fallback_module(module) {
                    summary.has_password_fallback = true;
                }
                if module.contains("pam_deny") {
                    summary.has_deny = true;
                }
            }
            PamLine::Substack { .. } => {
                summary.has_substack = true;
            }
            PamLine::Include {
                pam_type: PamType::Auth,
                file,
            } => {
                // Includes of known shared-auth stacks count as fallback.
                if is_password_fallback_include(file) {
                    summary.has_password_fallback = true;
                }
            }
            PamLine::AtInclude(file) if is_password_fallback_include(file) => {
                summary.has_password_fallback = true;
            }
            _ => {}
        }
    }
}

/// True if a module name is a known password-fallback module.
fn is_password_fallback_module(module: &str) -> bool {
    module.contains("pam_unix") || module.contains("pam_systemd_home") || module.contains("pam_env")
}

/// True if an include target is a known shared-auth stack that provides
/// password fallback.
fn is_password_fallback_include(file: &str) -> bool {
    matches!(file, "common-auth" | "system-auth" | "password-auth")
}

/// Determine compatibility and typed reason from the stack summary.
fn determine_compatibility(id: &str, summary: &StackSummary) -> (PamServiceCompatibility, String) {
    // Already enabled — compatible, no action needed.
    if summary.has_namtor {
        return (
            PamServiceCompatibility::Safe,
            "pam_namtor.so already present in auth stack".to_string(),
        );
    }

    // No auth lines at all — not auth-relevant, unsupported for insertion.
    if summary.auth_entry_count == 0 && !summary.has_password_fallback {
        return (
            PamServiceCompatibility::Unsupported,
            format!("service '{id}' has no auth entries — not auth-relevant"),
        );
    }

    // No password fallback — inserting pam_namtor would lock users out
    // on daemon failure.
    if !summary.has_password_fallback {
        return (
            PamServiceCompatibility::Unsupported,
            format!("service '{id}' has no password-fallback auth line — daemon outage would lock users out"),
        );
    }

    // Substacks make insertion risky — the block might not be reached.
    if summary.has_substack {
        return (
            PamServiceCompatibility::Risky,
            format!("service '{id}' uses substack directives — insertion point may be ambiguous"),
        );
    }

    // pam_deny present — risky, fall-through could hit deny before password.
    if summary.has_deny {
        return (
            PamServiceCompatibility::Risky,
            format!("service '{id}' contains pam_deny.so — verify deny does not appear before password fallback"),
        );
    }

    // SSH — remote auth, pam_namtor refuses PAM_RHOST.
    if id == "sshd" {
        return (
            PamServiceCompatibility::Unsupported,
            "sshd sets PAM_RHOST — pam_namtor refuses remote authentication (spec §8.2)"
                .to_string(),
        );
    }

    (
        PamServiceCompatibility::Safe,
        format!("service '{id}' is safe for pam_namtor insertion — password fallback present"),
    )
}
