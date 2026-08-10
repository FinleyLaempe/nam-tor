// SPDX-License-Identifier: GPL-3.0-only
//! PAM line parser and include resolver.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{ControlField, PamLine, PamType, ScanError};

/// A fully parsed PAM service: its lines and the resolved include targets.
#[derive(Debug, Clone)]
pub struct ParsedService {
    /// Service ID (filename in `pam.d`).
    pub id: String,
    /// Raw parsed lines from this service's file.
    pub lines: Vec<PamLine>,
    /// Include directives found in this file, with resolved paths.
    pub includes: Vec<IncludeResult>,
}

/// A resolved include directive.
#[derive(Debug, Clone)]
pub struct IncludeResult {
    /// The include keyword used (`@include` or `include`).
    pub kind: IncludeKind,
    /// The target filename as written in the config.
    pub target: String,
    /// Resolved path within the descriptor root.
    pub resolved_path: PathBuf,
}

/// Kind of include directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeKind {
    /// Debian `@include <file>`.
    AtInclude,
    /// Arch/RHEL `include <file>` (as a management-type line).
    Include,
}

/// Parse a single PAM service file into lines + include directives.
pub fn parse_service(id: &str, path: &Path) -> Result<ParsedService, ScanError> {
    let content =
        fs::read_to_string(path).map_err(|_| ScanError::ReadFailed(path.display().to_string()))?;

    let mut lines = Vec::new();
    let mut includes = Vec::new();

    for raw in content.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            lines.push(PamLine::Comment);
            continue;
        }

        // Debian @include
        if let Some(rest) = trimmed.strip_prefix("@include") {
            let target = rest.trim().to_string();
            if target.is_empty() {
                lines.push(PamLine::Other(trimmed.to_string()));
                continue;
            }
            includes.push(IncludeResult {
                kind: IncludeKind::AtInclude,
                target: target.clone(),
                resolved_path: PathBuf::from(&target),
            });
            lines.push(PamLine::AtInclude(target));
            continue;
        }

        // Tokenise: type control module [args...]
        // PAM bracket control fields can contain spaces, e.g.
        // `[success=1 default=ignore]`. We need to rejoin those tokens.
        let tokens = tokenise_pam_line(trimmed);
        if tokens.is_empty() {
            lines.push(PamLine::Comment);
            continue;
        }

        let first = tokens[0].as_str();

        if first == "include" && tokens.len() >= 2 {
            let target = tokens[1].clone();
            includes.push(IncludeResult {
                kind: IncludeKind::Include,
                target: target.clone(),
                resolved_path: PathBuf::from(&target),
            });
            lines.push(PamLine::Other(trimmed.to_string()));
            continue;
        }

        let pam_type = match first {
            "auth" => PamType::Auth,
            "account" => PamType::Account,
            "password" => PamType::Password,
            "session" => PamType::Session,
            _ => {
                lines.push(PamLine::Other(trimmed.to_string()));
                continue;
            }
        };

        if tokens.len() < 2 {
            lines.push(PamLine::Other(trimmed.to_string()));
            continue;
        }

        let second = tokens[1].as_str();

        if second == "include" && tokens.len() >= 3 {
            let target = tokens[2].clone();
            includes.push(IncludeResult {
                kind: IncludeKind::Include,
                target: target.clone(),
                resolved_path: PathBuf::from(&target),
            });
            lines.push(PamLine::Include {
                pam_type,
                file: target,
            });
            continue;
        }

        if second == "substack" && tokens.len() >= 3 {
            let target = tokens[2].clone();
            lines.push(PamLine::Substack {
                pam_type,
                control: ControlField::Sufficient,
                file: target,
            });
            continue;
        }

        let control = parse_control(second);
        if tokens.len() < 3 {
            lines.push(PamLine::Other(trimmed.to_string()));
            continue;
        }

        let module = tokens[2].clone();
        let args = tokens[3..].to_vec();

        lines.push(PamLine::Entry {
            pam_type,
            control,
            module,
            args,
        });
    }

    if lines.iter().all(|l| *l == PamLine::Comment) {
        return Err(ScanError::Malformed(id.to_string()));
    }

    Ok(ParsedService {
        id: id.to_string(),
        lines,
        includes,
    })
}

/// Parse the control field (second column) of a PAM line.
fn parse_control(s: &str) -> ControlField {
    match s {
        "required" => ControlField::Required,
        "requisite" => ControlField::Requisite,
        "sufficient" => ControlField::Sufficient,
        "optional" => ControlField::Optional,
        other if other.starts_with('[') && other.ends_with(']') => {
            ControlField::Bracket(other.to_string())
        }
        _ => ControlField::Required,
    }
}

/// Tokenise a PAM line, rejoining bracket control fields that contain spaces.
/// E.g. `auth [success=1 default=ignore] pam_unix.so` →
/// `["auth", "[success=1 default=ignore]", "pam_unix.so"]`.
fn tokenise_pam_line(line: &str) -> Vec<String> {
    let raw: Vec<&str> = line.split_whitespace().collect();
    let mut tokens = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let tok = raw[i];
        if tok.starts_with('[') && !tok.ends_with(']') {
            let mut joined = tok.to_string();
            i += 1;
            while i < raw.len() {
                joined.push(' ');
                joined.push_str(raw[i]);
                i += 1;
                if joined.ends_with(']') {
                    break;
                }
            }
            tokens.push(joined);
        } else {
            tokens.push(tok.to_string());
            i += 1;
        }
    }
    tokens
}

/// Recursively resolve includes for a parsed service, detecting cycles
/// and traversal attempts. Returns the include chain (flattened).
pub fn resolve_includes(
    service: &ParsedService,
    pamd_root: &Path,
) -> Result<Vec<ParsedService>, ScanError> {
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(service.id.clone());
    let mut result = Vec::new();
    resolve_includes_inner(service, pamd_root, &mut visited, &mut result, &service.id)?;
    Ok(result)
}

fn resolve_includes_inner(
    service: &ParsedService,
    pamd_root: &Path,
    visited: &mut HashSet<String>,
    result: &mut Vec<ParsedService>,
    chain_root: &str,
) -> Result<(), ScanError> {
    for inc in &service.includes {
        // Reject traversal: the target must not contain path separators
        // that escape the pam.d root.
        if inc.target.contains("..") || inc.target.starts_with('/') {
            return Err(ScanError::TraversalRejected(format!(
                "{} -> {}",
                chain_root, inc.target
            )));
        }

        if visited.contains(&inc.target) {
            return Err(ScanError::IncludeCycle(format!(
                "{} -> {} (already visited: {:?})",
                chain_root, inc.target, visited
            )));
        }

        let inc_path = pamd_root.join(&inc.target);
        if !inc_path.is_file() {
            // Missing include target — skip (may be distro-specific).
            continue;
        }

        visited.insert(inc.target.clone());
        let parsed = parse_service(&inc.target, &inc_path)?;
        resolve_includes_inner(&parsed, pamd_root, visited, result, chain_root)?;
        result.push(parsed);
        visited.remove(&inc.target);
    }
    Ok(())
}
