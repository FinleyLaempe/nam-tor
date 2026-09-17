// SPDX-License-Identifier: GPL-3.0-only
//! Nam-tor management UI — D-Bus client (spec §9.1, §9.2).
//!
//! Desktop-neutral: GTK4/libadwaita, but no GNOME-only runtime dependency.
//!
//! GTK4 + libadwaita application. The UI talks to the daemon exclusively
//! over the **system** D-Bus `org.namtor.Manager` (v1) and `org.namtor.Manager2`
//! (v2) interfaces (task 14). There is no direct template-store access from
//! the UI process: every operation — list, enroll, commit, rename,
//! enable, delete, merge, replace, PAM toggle — goes through the bus. This
//! keeps biometric data inside the privileged daemon and lets polkit gate
//! every mutation.
//!
//! Architecture:
//! - [`client`] — the `ManagerClient` trait + UI-facing data types.
//! - [`fake`] — in-memory test double.
//! - [`dbus_client`] — live zbus-backed client (feature `dbus`).
//! - [`presenter`] — headless state machine the GTK view renders.
//! - [`components`] — the single enrollment rendering path (capture, review,
//!   profile list, PAM services, system status).
//! - [`privacy`] — runtime scan ensuring no biometric data leaks into UI
//!   structs.
//!
//! The wizard binary (`namtor-enroll-wizard`) lives in `src/bin/wizard.rs`
//! and is the only place GTK is constructed.
//!
//! ---------------------------------------------------------------------------

/// GTK application id.
///
/// MUST equal the installed desktop entry's basename and its
/// `StartupWMClass`, or the compositor cannot associate the window with its
/// `.desktop` file and shows a fallback icon and name. It is also the
/// D-Bus name GTK claims for window activation, so it is ABI for installed
/// systems: it does NOT track the crate name.
pub const APP_ID: &str = "org.namtor.Wizard";

pub mod a11y;
pub mod app;
pub mod client;
pub mod components;
pub mod fake;
pub mod polkit_auth;
pub mod presenter;
pub mod privacy;
pub mod shell;
pub mod view_model;

/// Translation seam for user-facing strings.
///
/// Every label, tooltip, accessible name, status title, and guidance phrase
/// shown in the GTK UI MUST pass through `tr()` so the string is the gettext
/// msgid.
///
/// The doc here used to say "in production the wizard binary loads a
/// `gettext::Catalog` and rebinds this". It did not: nothing in the crate
/// ever touched the `gettext` dependency, so every call was the identity
/// function everywhere and no translation could be loaded even if a
/// distributor shipped one. [`init_translations`] now actually loads a
/// catalog, and the wizard calls it at startup.
///
/// No catalogs ship with this repository yet. A missing catalog is the
/// ordinary case and falls through to the msgid unchanged.
///
/// Tests in `tests/shell_navigation.rs` assert that user-facing string
/// literals are wrapped in `tr()` calls and that no forbidden biometric /
/// internal wording leaks through.
pub fn tr(s: &str) -> String {
    match CATALOG.get() {
        Some(Some(catalog)) => catalog.gettext(s).to_string(),
        // No catalog, or `init_translations` was never called: the msgid IS
        // the English string, so this is a correct rendering, not a fallback
        // that loses information.
        _ => s.to_string(),
    }
}

/// Translate a message that has runtime parts, keeping the msgid stable.
///
/// `tr(&format!("Rename profile {name}"))` cannot work: the msgid handed to
/// gettext contains the interpolated value, so it can never match a catalog
/// entry. Translate the *pattern*, then substitute:
///
/// ```ignore
/// tr_args("Rename profile {}", &[&row.name]);
/// ```
///
/// `{}` placeholders are replaced left to right with the given values, after
/// lookup, so a translation may not reorder them — adequate for the short
/// labels here, and honest about it rather than silently untranslatable.
pub fn tr_args(msgid: &str, args: &[&str]) -> String {
    let mut out = tr(msgid);
    for a in args {
        if let Some(i) = out.find("{}") {
            out.replace_range(i..i + 2, a);
        }
    }
    out
}

static CATALOG: std::sync::OnceLock<Option<gettext::Catalog>> = std::sync::OnceLock::new();

/// Load the message catalog for the current locale, once.
///
/// Looks for `<dir>/<lang>/LC_MESSAGES/namtor.mo` under each directory in
/// `NAMTOR_LOCALE_DIR` or `/usr/share/locale`, trying the full locale
/// (`de_DE`) before the bare language (`de`). Calling it more than once, or
/// not at all, is safe: `tr()` falls through to the msgid.
///
/// Returns true when a catalog was loaded.
pub fn init_translations() -> bool {
    CATALOG.get_or_init(load_catalog).is_some()
}

fn load_catalog() -> Option<gettext::Catalog> {
    // Honour the gettext precedence the rest of the desktop uses.
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.is_empty() && v != "C" && v != "POSIX")?;
    // Strip the encoding and modifier: `de_DE.UTF-8@euro` -> `de_DE`.
    let locale = locale
        .split('.')
        .next()
        .unwrap_or(&locale)
        .split('@')
        .next()
        .unwrap_or(&locale)
        .to_string();
    let language = locale.split('_').next().unwrap_or(&locale).to_string();

    let dir = std::env::var("NAMTOR_LOCALE_DIR").unwrap_or_else(|_| "/usr/share/locale".into());
    for candidate in [locale.as_str(), language.as_str()] {
        let path = std::path::Path::new(&dir)
            .join(candidate)
            .join("LC_MESSAGES")
            .join("namtor.mo");
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        match gettext::Catalog::parse(file) {
            Ok(c) => return Some(c),
            Err(e) => {
                // A corrupt catalog must not take the UI down; English is a
                // correct rendering.
                eprintln!(
                    "namtor: ignoring unreadable catalog {}: {e}",
                    path.display()
                );
            }
        }
    }
    None
}

pub use a11y::{ProgressAnnouncer, PROGRESS_ANNOUNCE_INTERVAL};
pub use shell::{Destination, Shell};

#[cfg(feature = "dbus")]
pub mod dbus_client;

#[cfg(feature = "dbus")]
pub use dbus_client::DbusManagerClient;

pub use client::{
    EnrollmentProgress, EnrollmentProgressV3, EnrollmentReviewV3, Guidance, ManagerClient,
    ManagerError, ManagerResult, PamServiceCompatibility, PamServiceInfo, ProfileId, ProfileInfo,
    ProtectionKeyKind, ProtectionStatus, QualityLabel, ReviewOutcome, SessionId, StatusReport,
    API_VERSION_V3,
};
pub use fake::{FakeManagerClient, Mode as FakeMode};
pub use presenter::ProfilePresenter;
pub use view_model::{
    compatibility_label, sealing_label, CaptureSignal, EnrollmentState, EnrollmentView,
    EnrollmentViewV3, ErrorView, GuidanceView, PamServiceRowView, PendingMerge, PendingReplace,
    PresenterModel, ProfileRowView, ProfileSetupView, ProtectionStatusView, ProtectionView,
    QualityView, ReadinessLevel, ReadinessView, RecoveryHint, ReviewView, ReviewViewV3,
};

/// What the local probe can say about an IR camera.
///
/// Three different situations used to collapse into `false`, and the System
/// page rendered all of them as "IR Camera — Not detected": no matching
/// device, an unreadable quirks database, and unreadable sysfs. Only the
/// first is a statement about the hardware. A package installed without its
/// device database sent the user to check their camera when the fix is to
/// reinstall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraPresence {
    /// A supported camera is attached and exposes a capture node.
    Present,
    /// Nothing attached matches the device database.
    #[default]
    Absent,
    /// The device database is missing, unreadable, or empty, so nothing
    /// could be matched against it. Says nothing about the hardware.
    NoDeviceDatabase,
    /// `/sys` could not be enumerated.
    NoSysfs,
}

impl CameraPresence {
    /// Whether a supported camera is attached. `false` for every condition
    /// under which that could not be determined.
    pub fn is_present(self) -> bool {
        matches!(self, Self::Present)
    }
}

/// Is a supported IR camera present on this machine?
///
/// Answered from sysfs and the quirks database, not from the daemon: the
/// `ManagerClient` wire has no readiness method, so the System page used to
/// report `camera_detected = daemon_reachable` and tell a user with no camera
/// that their camera was detected.
///
/// Unprivileged — it reads `/sys` and the quirks directory only. Nothing here
/// returns an error: this drives one status row and must never take the page
/// down. See [`CameraPresence`] for why the reasons are kept apart.
pub fn supported_camera_presence() -> CameraPresence {
    supported_camera_presence_in(
        std::path::Path::new(
            &std::env::var("NAMTOR_SYSFS_ROOT").unwrap_or_else(|_| "/sys".to_string()),
        ),
        std::path::Path::new(
            &std::env::var("NAMTOR_QUIRKS_DIR")
                .unwrap_or_else(|_| "/usr/share/namtor/quirks/devices".to_string()),
        ),
    )
}

/// [`supported_camera_presence`] against explicit roots.
pub fn supported_camera_presence_in(
    sysfs_root: &std::path::Path,
    quirks_dir: &std::path::Path,
) -> CameraPresence {
    // `load_quirks_dir` answers `Ok` with an empty database for a directory
    // that is not there — it is written to merge several candidate roots, so
    // a missing one is not fatal to it. An empty database cannot match
    // anything, so for this probe "missing" and "empty" are one condition,
    // and neither is a statement about the hardware.
    let db = match namtor_hwdb::quirks::load_quirks_dir(quirks_dir) {
        Ok(db) if !db.entries.is_empty() => db,
        _ => return CameraPresence::NoDeviceDatabase,
    };
    let Ok(devices) = namtor_hwdb::sysfs::enumerate_usb(sysfs_root) else {
        return CameraPresence::NoSysfs;
    };
    let found = devices.iter().any(|d| {
        // Try the bcd-qualified key first, then the plain vid:pid. A
        // bcd-qualified lookup deliberately never falls back on its own — the
        // quirks db treats a bcd mismatch as a different device — and most
        // entries, including the reference camera's, are keyed without one.
        // Passing `Some(bcd)` alone made this return false on a machine that
        // has the reference camera attached.
        let supported = db.match_device(d.vid, d.pid, Some(d.bcd_device)).is_some()
            || db.match_device(d.vid, d.pid, None).is_some();
        supported
            && namtor_hwdb::nodes::correlate_nodes(d)
                .map(|nodes| nodes.iter().any(|n| !n.is_metadata))
                .unwrap_or(false)
    });
    if found {
        CameraPresence::Present
    } else {
        CameraPresence::Absent
    }
}

/// Is a supported IR camera present on this machine?
pub fn supported_camera_present() -> bool {
    supported_camera_presence().is_present()
}

/// [`supported_camera_present`] against explicit roots, so it can be tested
/// without a camera attached.
pub fn supported_camera_present_in(
    sysfs_root: &std::path::Path,
    quirks_dir: &std::path::Path,
) -> bool {
    supported_camera_presence_in(sysfs_root, quirks_dir).is_present()
}
