// SPDX-License-Identifier: GPL-3.0-only
//! Pure read/write logic for KDE's `kscreenlockerrc`.
//!
//! `kscreenlockerd` (the lock screen) reads `[Authenticators] Face=true` from
//! `$HOME/.config/kscreenlockerrc` to decide whether to offer face unlock as
//! an authentication method on the lock screen. This module owns that single
//! line, preserving every other section/key in the file untouched. It never
//! touches D-Bus or PAM — see [`crate::bridge::KcmBridge::set_face_unlock_enabled`]
//! for the combined operation.

use std::fs;
use std::io;
use std::path::Path;

/// Set or clear `Face=true` in the `[Authenticators]` section of
/// `$HOME/.config/kscreenlockerrc`. Creates the file/section if absent when
/// `enabled` is `true`; removes only the `Face=` line (leaving the section
/// and file otherwise intact) when `enabled` is `false`.
pub fn set_face(home: &Path, enabled: bool) -> io::Result<()> {
    let config_dir = home.join(".config");
    let path = config_dir.join("kscreenlockerrc");

    let existing = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };

    let updated = apply_face_setting(&existing, enabled);
    if updated == existing {
        return Ok(());
    }

    fs::create_dir_all(&config_dir)?;
    fs::write(&path, updated)
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
}
