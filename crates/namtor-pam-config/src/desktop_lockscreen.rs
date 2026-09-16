// SPDX-License-Identifier: GPL-3.0-only
//! Desktop lock-screen authenticator provisioning.
//!
//! A desktop's lock screen decides for itself which PAM-backed
//! authenticators it offers. Enabling the `pam_namtor.so` stack for a
//! service under `/etc/pam.d` (the [`crate::editor`] module's job) is
//! therefore only half of "let me unlock this session with my face" — the
//! lock screen also has to be told to *offer* face as an authenticator.
//!
//! This module owns that second half for KDE Plasma, whose
//! `kscreenlockerd` reads `[Authenticators] Face=true` from
//! `$HOME/.config/kscreenlockerrc`. It is a pure config-file writer: no
//! D-Bus, no PAM, no GUI toolkit, so both `namtor-cli` and the GTK4 UI can
//! call it. The module is named for the general operation rather than for
//! Plasma so it can later grow a GDM/greetd arm alongside [`set_face`].
//!
//! ## Ordering guarantee (callers MUST preserve this)
//!
//! Face unlock on the lock screen is two coupled state changes: the PAM
//! service (`kde-face` under `/etc/pam.d`, toggled through the daemon's
//! `SetPamServiceEnabled` D-Bus method) and this file. **The PAM toggle
//! runs first and the file is written only if it succeeded.** If the PAM
//! call fails, `kscreenlockerrc` must be left untouched, so the file never
//! advertises an authenticator that PAM cannot satisfy. Disabling reverses
//! both, in the same order.
//!
//! ## Lockout safety
//!
//! [`would_cause_lockout`] reports the state where `Face=true` is the only
//! enabled authenticator — no password fallback, so a camera failure locks
//! the user out of their session. Any caller that is about to disable a
//! non-face authenticator must check it first and refuse, or demand
//! explicit confirmation.
//!
//! ## Status: no caller yet
//!
//! Nothing calls [`set_face`] or [`would_cause_lockout`] today. The Plasma
//! KCM that was going to (`crates/namtor-kde`) never wired them into its
//! QML or its C ABI before it was removed in favour of a single
//! desktop-neutral GTK4 UI, so this is an unfinished feature rather than a
//! regression. The logic and its tests are kept here, out of the deleted
//! Qt crate, as the documented API for a future UI toggle.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

/// A lock-screen provisioning failure.
///
/// Deliberately small and free of backend internals so a UI can
/// pattern-match on it without depending on this crate's editor types.
#[derive(Debug)]
pub enum LockscreenError {
    /// A local filesystem operation failed (e.g. writing `kscreenlockerrc`).
    Io(String),
}

impl fmt::Display for LockscreenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "local filesystem operation failed: {msg}"),
        }
    }
}

impl std::error::Error for LockscreenError {}

impl From<io::Error> for LockscreenError {
    fn from(err: io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

/// Set or clear `Face=true` in the `[Authenticators]` section of
/// `$HOME/.config/kscreenlockerrc`. Creates the file/section if absent when
/// `enabled` is `true`; removes only the `Face=` line (leaving the section
/// and file otherwise intact) when `enabled` is `false`.
pub fn set_face(home: &Path, enabled: bool) -> Result<(), LockscreenError> {
    let config_dir = home.join(".config");
    let path = config_dir.join("kscreenlockerrc");

    let existing = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };

    let updated = apply_face_setting(&existing, enabled);
    if updated == existing {
        return Ok(());
    }

    fs::create_dir_all(&config_dir)?;
    fs::write(&path, updated)?;
    Ok(())
}

/// Returns `true` if `Face=true` is the only enabled authenticator in the
/// `[Authenticators]` section — a lockout-unsafe state with no password
/// fallback. The KCM must call this before allowing any action that would
/// disable a non-face authenticator and refuse (or require explicit
/// confirmation) when it returns `true`.
///
/// Returns `false` when there is no `[Authenticators]` section (password is
/// the implicit default in `kscreenlockerd`) or when any non-face key is also
/// set to `true`.
pub fn would_cause_lockout(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();

    let Some(start) = lines.iter().position(|l| l.trim() == "[Authenticators]") else {
        return false;
    };

    let section_end = lines
        .iter()
        .skip(start + 1)
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |offset| start + 1 + offset);

    let mut face_enabled = false;
    let mut other_enabled = false;

    for line in &lines[start + 1..section_end] {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key == "Face" {
            if value == "true" {
                face_enabled = true;
            }
        } else if value == "true" {
            other_enabled = true;
        }
    }

    face_enabled && !other_enabled
}

/// Pure text transform: given the current file content, return the new
/// content with `Face=true` set or cleared in `[Authenticators]`.
fn apply_face_setting(content: &str, enabled: bool) -> String {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

    let section_start = lines.iter().position(|l| l.trim() == "[Authenticators]");

    match section_start {
        Some(start) => {
            let section_end = lines
                .iter()
                .skip(start + 1)
                .position(|l| l.trim_start().starts_with('['))
                .map_or(lines.len(), |offset| start + 1 + offset);
            let face_line = (start + 1..section_end).find(|&i| {
                lines[i]
                    .split('=')
                    .next()
                    .is_some_and(|key| key.trim() == "Face")
            });

            match (face_line, enabled) {
                (Some(i), true) => lines[i] = "Face=true".to_string(),
                (Some(i), false) => {
                    lines.remove(i);
                }
                (None, true) => lines.insert(section_end, "Face=true".to_string()),
                (None, false) => {}
            }
        }
        None if enabled => {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push("[Authenticators]".to_string());
            lines.push("Face=true".to_string());
        }
        None => {}
    }

    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique temp dir per test so parallel `cargo test` runs never collide.
    fn temp_home(tag: &str) -> std::path::PathBuf {
        let unique = format!(
            "namtor-kscreenlocker-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn enabling_creates_file_and_section_when_absent() {
        let home = temp_home("create");
        set_face(&home, true).unwrap();
        let content = fs::read_to_string(home.join(".config/kscreenlockerrc")).unwrap();
        assert!(content.contains("[Authenticators]"));
        assert!(content.contains("Face=true"));
    }

    #[test]
    fn disabling_absent_file_is_a_noop() {
        let home = temp_home("disable-absent");
        set_face(&home, false).unwrap();
        assert!(!home.join(".config/kscreenlockerrc").exists());
    }

    #[test]
    fn disabling_removes_face_line_but_preserves_section_and_other_keys() {
        let home = temp_home("preserve");
        let config_dir = home.join(".config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("kscreenlockerrc"),
            "[Greeter]\nWallpaperPlugin=org.kde.image\n\n[Authenticators]\nFace=true\nFprint=false\n",
        )
        .unwrap();

        set_face(&home, false).unwrap();

        let content = fs::read_to_string(config_dir.join("kscreenlockerrc")).unwrap();
        assert!(!content.contains("Face="), "Face key must be removed");
        assert!(
            content.contains("[Authenticators]"),
            "section header must survive"
        );
        assert!(content.contains("Fprint=false"), "sibling key must survive");
        assert!(
            content.contains("WallpaperPlugin=org.kde.image"),
            "other section must survive"
        );
    }

    #[test]
    fn enabling_preserves_other_sections_and_keys() {
        let home = temp_home("enable-preserve");
        let config_dir = home.join(".config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("kscreenlockerrc"),
            "[Daemon]\nTimeout=300\n\n[Authenticators]\nFprint=false\n",
        )
        .unwrap();

        set_face(&home, true).unwrap();

        let content = fs::read_to_string(config_dir.join("kscreenlockerrc")).unwrap();
        assert!(content.contains("Face=true"));
        assert!(content.contains("Fprint=false"));
        assert!(content.contains("Timeout=300"));
    }

    #[test]
    fn enabling_twice_is_idempotent() {
        let home = temp_home("idempotent");
        set_face(&home, true).unwrap();
        set_face(&home, true).unwrap();
        let content = fs::read_to_string(home.join(".config/kscreenlockerrc")).unwrap();
        assert_eq!(content.matches("Face=true").count(), 1);
    }

    // -----------------------------------------------------------------
    // Lockout safety: Face must never be the sole enabled authenticator.
    // Ported verbatim (bar the module path) from the deleted namtor-kde
    // crate's `src/tests.rs`.
    // -----------------------------------------------------------------

    /// A `kscreenlockerrc` with only `Face=true` under `[Authenticators]`
    /// (no password fallback) is a lockout-unsafe state. A caller must
    /// check [`would_cause_lockout`] before allowing any
    /// authenticator-disabling action and refuse (or confirm) if it would
    /// produce this state.
    #[test]
    fn lockout_safety_face_only_is_unsafe() {
        let home = temp_home("lockout-face-only");
        let config_dir = home.join(".config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("kscreenlockerrc"),
            "[Greeter]\nWallpaperPlugin=org.kde.image\n\n[Authenticators]\nFace=true\n",
        )
        .unwrap();

        let content = fs::read_to_string(config_dir.join("kscreenlockerrc")).unwrap();
        assert!(
            would_cause_lockout(&content),
            "Face=true with no other enabled authenticator must be lockout-unsafe"
        );
    }

    /// `Face=true` alongside `Password=true` is safe — the password
    /// authenticator is an explicit fallback.
    #[test]
    fn lockout_safety_face_plus_password_is_safe() {
        let home = temp_home("lockout-face-plus-password");
        let config_dir = home.join(".config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("kscreenlockerrc"),
            "[Authenticators]\nFace=true\nPassword=true\n",
        )
        .unwrap();

        let content = fs::read_to_string(config_dir.join("kscreenlockerrc")).unwrap();
        assert!(
            !would_cause_lockout(&content),
            "Face=true + Password=true must be safe (password fallback exists)"
        );
    }

    /// No `[Authenticators]` section at all is safe — password is the
    /// implicit default authenticator in `kscreenlockerd`.
    #[test]
    fn lockout_safety_no_authenticators_section_is_safe() {
        let home = temp_home("lockout-no-section");
        let config_dir = home.join(".config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("kscreenlockerrc"),
            "[Greeter]\nWallpaperPlugin=org.kde.image\n",
        )
        .unwrap();

        let content = fs::read_to_string(config_dir.join("kscreenlockerrc")).unwrap();
        assert!(
            !would_cause_lockout(&content),
            "No [Authenticators] section means password is the implicit default — safe"
        );
    }

    /// `Face=true` alongside `Fingerprint=true` is safe — any non-face
    /// authenticator enabled counts as a fallback.
    #[test]
    fn lockout_safety_face_plus_fingerprint_is_safe() {
        let home = temp_home("lockout-face-plus-fingerprint");
        let config_dir = home.join(".config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("kscreenlockerrc"),
            "[Authenticators]\nFace=true\nFingerprint=true\n",
        )
        .unwrap();

        let content = fs::read_to_string(config_dir.join("kscreenlockerrc")).unwrap();
        assert!(
            !would_cause_lockout(&content),
            "Face=true + Fingerprint=true must be safe (non-face fallback exists)"
        );
    }

    /// `Face=false` alone is not a lockout risk — Face is not enabled, so
    /// the lockout condition (Face as sole authenticator) does not apply.
    #[test]
    fn lockout_safety_face_disabled_is_safe() {
        let home = temp_home("lockout-face-disabled");
        let config_dir = home.join(".config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("kscreenlockerrc"),
            "[Authenticators]\nFace=false\n",
        )
        .unwrap();

        let content = fs::read_to_string(config_dir.join("kscreenlockerrc")).unwrap();
        assert!(
            !would_cause_lockout(&content),
            "Face=false must be safe (face not enabled)"
        );
    }
}
