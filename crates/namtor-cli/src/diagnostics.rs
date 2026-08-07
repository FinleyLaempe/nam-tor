// SPDX-License-Identifier: GPL-3.0-only
//! Privacy-safe diagnostics and support bundles.
//!
//! `run_doctor` runs a fixed set of checks (device, emitter, protection,
//! storage, model, package, API) and returns a structured report. Auth
//! outcomes are reduced to the generic tokens `match` / `no match` /
//! `unavailable` — raw scores, gate names, embeddings, and biometric
//! material never appear.
//!
//! `create_bundle` writes a support bundle (diagnostic report + redacted
//! config summary). Every file is run through `PrivacyScanner` before it
//! lands on disk; the scanner rejects frames, crops, embeddings,
//! thresholds, nonces, key material, and raw score numbers.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::install_layout::{InstallRoot, LayoutPaths};
use namtor_camera::inventory::CameraInventory;

/// Operator-coded diagnostic check result. Codes are stable operator
/// identifiers (`E001`, `W001`, ...) that support tooling, not free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticCheck {
    Ok {
        check: &'static str,
        detail: String,
    },
    Warning {
        code: &'static str,
        check: &'static str,
        detail: String,
        remediation: String,
    },
    Error {
        code: &'static str,
        check: &'static str,
        detail: String,
        remediation: String,
    },
}

impl DiagnosticCheck {
    pub fn check(&self) -> &'static str {
        match self {
            DiagnosticCheck::Ok { check, .. }
            | DiagnosticCheck::Warning { check, .. }
            | DiagnosticCheck::Error { check, .. } => check,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, DiagnosticCheck::Error { .. })
    }

    pub fn is_warning(&self) -> bool {
        matches!(self, DiagnosticCheck::Warning { .. })
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, DiagnosticCheck::Ok { .. })
    }

    pub fn code(&self) -> Option<&'static str> {
        match self {
            DiagnosticCheck::Ok { .. } => None,
            DiagnosticCheck::Warning { code, .. } | DiagnosticCheck::Error { code, .. } => {
                Some(code)
            }
        }
    }

    pub fn remediation(&self) -> Option<&str> {
        match self {
            DiagnosticCheck::Ok { .. } => None,
            DiagnosticCheck::Warning { remediation, .. }
            | DiagnosticCheck::Error { remediation, .. } => Some(remediation),
        }
    }
}

impl std::fmt::Display for DiagnosticCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiagnosticCheck::Ok { check, detail } => {
                write!(f, "[OK] {check}: {detail}")
            }
            DiagnosticCheck::Warning {
                code,
                check,
                detail,
                remediation,
            } => write!(
                f,
                "[{code}] {check}: {detail} -- remediation: {remediation}"
            ),
            DiagnosticCheck::Error {
                code,
                check,
                detail,
                remediation,
            } => write!(
                f,
                "[{code}] {check}: {detail} -- remediation: {remediation}"
            ),
        }
    }
}

/// Generic auth outcome. Never carries a raw score or a gate name; the
/// diagnostic bundle is safe to share because this is the only auth signal
/// it can contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthOutcome {
    Match,
    NoMatch,
    Unavailable,
}

impl std::fmt::Display for AuthOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthOutcome::Match => f.write_str("match"),
            AuthOutcome::NoMatch => f.write_str("no match"),
            AuthOutcome::Unavailable => f.write_str("unavailable"),
        }
    }
}

/// Structured diagnostic report across all check categories.
#[derive(Debug, Clone)]
pub struct DiagnosticReport {
    pub device: DiagnosticCheck,
    pub emitter: DiagnosticCheck,
    pub protection: DiagnosticCheck,
    pub storage: DiagnosticCheck,
    pub model: DiagnosticCheck,
    pub package: DiagnosticCheck,
    pub api: DiagnosticCheck,
    pub auth_outcome: AuthOutcome,
}

impl DiagnosticReport {
    /// Ordered list of all check categories, in the order `run_doctor`
    /// evaluates them.
    pub fn checks(&self) -> [&DiagnosticCheck; 7] {
        [
            &self.device,
            &self.emitter,
            &self.protection,
            &self.storage,
            &self.model,
            &self.package,
            &self.api,
        ]
    }

    /// True if any check is an error.
    pub fn has_error(&self) -> bool {
        self.checks().iter().any(|c| c.is_error())
    }

    /// True if any check is a warning (and none are errors).
    pub fn has_warning(&self) -> bool {
        !self.has_error() && self.checks().iter().any(|c| c.is_warning())
    }
}

impl std::fmt::Display for DiagnosticReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "=== Nam-tor Diagnostic Report ===")?;
        for c in self.checks() {
            writeln!(f, "{c}")?;
        }
        write!(f, "auth: {}", self.auth_outcome)?;
        Ok(())
    }
}

/// Paths and identifiers the doctor probes. Defaults are derived from the
/// install-layout contract ([`LayoutPaths`]) and the camera inventory; tests
/// override them with tempdirs.
#[derive(Debug, Clone)]
pub struct DiagnosticConfig {
    pub device_path: PathBuf,
    pub vid_pid: String,
    pub templates_dir: PathBuf,
    pub key_path: PathBuf,
    pub models_dir: PathBuf,
    pub daemon_socket: PathBuf,
    pub daemon_bin: PathBuf,
    pub cli_bin: PathBuf,
    pub quirks_dirs: Vec<PathBuf>,
    pub auth_outcome: AuthOutcome,
}

impl DiagnosticConfig {
    /// Production defaults for an installed system: paths come from the
    /// install manifest at `/usr/share/namtor/install-manifest.toml`, and
    /// the device/vid:pid come from the sysfs camera inventory. No
    /// reference-hardware constant is hard-coded here.
    pub fn production() -> Self {
        let layout = LayoutPaths::resolve(
            Path::new("/"),
            Path::new("/usr/share/namtor/install-manifest.toml"),
            InstallRoot::Installed,
        );
        let (device_path, vid_pid) = discover_device(&layout.quirks_dirs);
        Self::from_layout(&layout, device_path, vid_pid)
    }

    /// Build a config from a resolved layout plus an explicit device.
    pub fn from_layout(layout: &LayoutPaths, device_path: PathBuf, vid_pid: String) -> Self {
        Self {
            device_path,
            vid_pid,
            templates_dir: layout.templates_dir.clone(),
            key_path: layout.key_path.clone(),
            models_dir: layout.models_dir.clone(),
            daemon_socket: layout.daemon_socket.clone(),
            daemon_bin: layout.daemon_bin.clone(),
            cli_bin: layout.cli_bin.clone(),
            quirks_dirs: layout.quirks_dirs.clone(),
            auth_outcome: AuthOutcome::Unavailable,
        }
    }
}

/// Discover the selected IR camera's device path and `vid:pid` from the
/// sysfs inventory, using the same quirks directories the doctor probes.
/// Returns `("/dev/video0", "0000:0000")`-style fallbacks when no IR camera
/// is found so the doctor reports an actionable E001 rather than panicking.
fn discover_device(quirks_dirs: &[PathBuf]) -> (PathBuf, String) {
    let inventory =
        namtor_camera::inventory::SysfsInventory::new(PathBuf::from("/sys"), quirks_dirs.to_vec());
    match inventory.enumerate() {
        Ok(cameras) if !cameras.is_empty() => {
            if let Some(sel) =
                namtor_camera::inventory::select_camera_from_inventory(&cameras, None, None)
            {
                let dev = sel.camera.ir_node.dev_path;
                let vid_pid = format!("{:04x}:{:04x}", sel.camera.usb.vid, sel.camera.usb.pid);
                (dev, vid_pid)
            } else {
                (PathBuf::from("/dev/video0"), String::from("0000:0000"))
            }
        }
        _ => (PathBuf::from("/dev/video0"), String::from("0000:0000")),
    }
}

/// Run every diagnostic check and return the structured report.
pub fn run_doctor(cfg: &DiagnosticConfig) -> DiagnosticReport {
    let device = check_device(cfg);
    let emitter = check_emitter(cfg);
    let protection = check_protection(&emitter);
    let storage = check_storage(cfg);
    let model = check_model(cfg);
    let package = check_package(cfg);
    let api = check_api(cfg);
    DiagnosticReport {
        device,
        emitter,
        protection,
        storage,
        model,
        package,
        api,
        auth_outcome: cfg.auth_outcome,
    }
}

fn check_device(cfg: &DiagnosticConfig) -> DiagnosticCheck {
    if cfg.device_path.exists() {
        DiagnosticCheck::Ok {
            check: "device",
            detail: format!("present ({})", cfg.device_path.display()),
        }
    } else {
        DiagnosticCheck::Error {
            code: "E001",
            check: "device",
            detail: format!("not found at {}", cfg.device_path.display()),
            remediation: format!(
                "connect an IR camera at {} (vid:pid {} expected)",
                cfg.device_path.display(),
                cfg.vid_pid
            ),
        }
    }
}

fn check_emitter(cfg: &DiagnosticConfig) -> DiagnosticCheck {
    let entry = cfg
        .quirks_dirs
        .iter()
        .find_map(|d| namtor_hwdb::quirks::load_quirks_dir(d).ok());
    match entry {
        Some(db) => match db.entries.get(cfg.vid_pid.as_str()) {
            Some(e)
                if e.emitter.method == namtor_hwdb::quirks::EmitterMethod::MsxuMode
                    || e.emitter.method == namtor_hwdb::quirks::EmitterMethod::Msxu
                    || e.emitter.method == namtor_hwdb::quirks::EmitterMethod::FirmwareStrobed =>
            {
                DiagnosticCheck::Ok {
                    check: "emitter",
                    detail: "msxu (firmware-strobed)".into(),
                }
            }
            Some(_) => DiagnosticCheck::Warning {
                code: "W002",
                check: "emitter",
                detail: "non-msxu emitter method".into(),
                remediation: "verify optical probe passes for this device".into(),
            },
            None => DiagnosticCheck::Error {
                code: "E002",
                check: "emitter",
                detail: format!("no quirk entry for {}", cfg.vid_pid),
                remediation: "add a quirk entry or run namtor-probe".into(),
            },
        },
        None => DiagnosticCheck::Warning {
            code: "W002",
            check: "emitter",
            detail: "quirk database not found".into(),
            remediation: "install namtor-hwdb quirks".into(),
        },
    }
}

fn check_protection(emitter: &DiagnosticCheck) -> DiagnosticCheck {
    match emitter {
        DiagnosticCheck::Ok { .. } => DiagnosticCheck::Ok {
            check: "protection",
            detail: "full".into(),
        },
        DiagnosticCheck::Warning { .. } => DiagnosticCheck::Warning {
            code: "W001",
            check: "protection",
            detail: "reduced".into(),
            remediation: "liveness gating depends on a working strobe emitter".into(),
        },
        DiagnosticCheck::Error { .. } => DiagnosticCheck::Error {
            code: "E003",
            check: "protection",
            detail: "unavailable without emitter".into(),
            remediation: "resolve the emitter error before relying on auth".into(),
        },
    }
}

fn check_storage(cfg: &DiagnosticConfig) -> DiagnosticCheck {
    if !cfg.templates_dir.exists() {
        return DiagnosticCheck::Error {
            code: "E003",
            check: "storage",
            detail: "enrollment directory missing".into(),
            remediation: format!(
                "create {} with mode 0700 owned by namtor",
                cfg.templates_dir.display()
            ),
        };
    }
    if !cfg.key_path.exists() {
        return DiagnosticCheck::Warning {
            code: "W003",
            check: "storage",
            detail: "wrapping key file missing".into(),
            remediation: format!("generate a key at {}", cfg.key_path.display()),
        };
    }
    let count = match fs::read_dir(&cfg.templates_dir) {
        Ok(rd) => rd.filter_map(Result::ok).count(),
        Err(_) => {
            return DiagnosticCheck::Error {
                code: "E003",
                check: "storage",
                detail: "enrollment directory unreadable".into(),
                remediation: "check permissions on the storage directory".into(),
            }
        }
    };
    DiagnosticCheck::Ok {
        check: "storage",
        detail: format!("{count} enrollments"),
    }
}

fn check_model(cfg: &DiagnosticConfig) -> DiagnosticCheck {
    let yunet = cfg.models_dir.join("yunet_2023mar.onnx");
    let sface = cfg.models_dir.join("sface_2021dec.onnx");
    let pad = cfg.models_dir.join("minifasnet_pad.onnx");
    let mut missing = Vec::new();
    if !yunet.exists() {
        missing.push("yunet");
    }
    if !sface.exists() {
        missing.push("sface");
    }
    if !pad.exists() {
        missing.push("minifasnet");
    }
    if missing.is_empty() {
        DiagnosticCheck::Ok {
            check: "model",
            detail: "all inference models present".into(),
        }
    } else {
        DiagnosticCheck::Error {
            code: "E004",
            check: "model",
            detail: format!("missing: {}", missing.join(", ")),
            remediation: format!("download models into {}", cfg.models_dir.display()),
        }
    }
}

fn check_package(cfg: &DiagnosticConfig) -> DiagnosticCheck {
    let mut missing = Vec::new();
    if !cfg.cli_bin.exists() {
        missing.push("namtor-cli");
    }
    if !cfg.daemon_bin.exists() {
        missing.push("namtor-daemon");
    }
    if missing.is_empty() {
        DiagnosticCheck::Ok {
            check: "package",
            detail: "cli and daemon binaries present".into(),
        }
    } else {
        DiagnosticCheck::Warning {
            code: "W004",
            check: "package",
            detail: format!("missing: {}", missing.join(", ")),
            remediation: "reinstall the namtor package".into(),
        }
    }
}

fn check_api(cfg: &DiagnosticConfig) -> DiagnosticCheck {
    if cfg.daemon_socket.exists() {
        DiagnosticCheck::Ok {
            check: "api",
            detail: "daemon socket present".into(),
        }
    } else {
        DiagnosticCheck::Error {
            code: "E005",
            check: "api",
            detail: "daemon socket not found".into(),
            remediation: format!(
                "start namtor-daemon (socket: {})",
                cfg.daemon_socket.display()
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Privacy scanner
// ---------------------------------------------------------------------------

/// Reason the privacy scanner rejected content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivacyError {
    ForbiddenTerm { term: &'static str, snippet: String },
    HexSequence { snippet: String },
    RawScore { snippet: String },
    KeyMaterial { snippet: String },
}

impl std::fmt::Display for PrivacyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrivacyError::ForbiddenTerm { term, snippet } => {
                write!(f, "privacy: forbidden term '{term}' in: {snippet}")
            }
            PrivacyError::HexSequence { snippet } => {
                write!(f, "privacy: hex sequence in: {snippet}")
            }
            PrivacyError::RawScore { snippet } => {
                write!(f, "privacy: raw score in: {snippet}")
            }
            PrivacyError::KeyMaterial { snippet } => {
                write!(f, "privacy: key material in: {snippet}")
            }
        }
    }
}

impl std::error::Error for PrivacyError {}

/// Rejects content that may carry biometric material. The support bundle
/// is safe to share only if every file passes this scanner.
pub struct PrivacyScanner;

impl PrivacyScanner {
    pub fn new() -> Self {
        Self
    }

    pub fn scan(&self, content: &str) -> Result<(), PrivacyError> {
        let lower = content.to_lowercase();
        for term in FORBIDDEN_TERMS {
            if let Some(idx) = lower.find(term) {
                return Err(PrivacyError::ForbiddenTerm {
                    term,
                    snippet: snippet(content, idx, term.len()),
                });
            }
        }
        if let Some(idx) = find_hex_sequence(content) {
            return Err(PrivacyError::HexSequence {
                snippet: snippet(content, idx, 16),
            });
        }
        if let Some(idx) = find_key_material(&lower) {
            return Err(PrivacyError::KeyMaterial {
                snippet: snippet(content, idx, 16),
            });
        }
        if let Some(idx) = find_raw_score(&lower) {
            return Err(PrivacyError::RawScore {
                snippet: snippet(content, idx, 12),
            });
        }
        Ok(())
    }
}

impl Default for PrivacyScanner {
    fn default() -> Self {
        Self::new()
    }
}

const FORBIDDEN_TERMS: [&str; 7] = [
    "embedding",
    "template",
    "pixel",
    "frame",
    "crop",
    "threshold",
    "nonce",
];

fn snippet(content: &str, idx: usize, len: usize) -> String {
    let start = idx.saturating_sub(16);
    let end = (idx + len + 16).min(content.len());
    content
        .get(start..end)
        .map(|s| s.replace('\n', " "))
        .unwrap_or_default()
}

fn is_hex_byte(c: char) -> bool {
    c.is_ascii_hexdigit()
}

/// Detect `0x` followed by 8+ hex digits (keys, hashes, nonce material).
fn find_hex_sequence(content: &str) -> Option<usize> {
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    while i + 2 <= chars.len() {
        if chars[i] == '0' && (chars.get(i + 1) == Some(&'x') || chars.get(i + 1) == Some(&'X')) {
            let start = i;
            let mut j = i + 2;
            while j < chars.len() && is_hex_byte(chars[j]) {
                j += 1;
            }
            if j - start >= 10 {
                return Some(start);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

/// Detect 32+ contiguous hex digits (raw key/hash without `0x` prefix) or
/// PEM key markers (`-----BEGIN ... KEY-----`).
fn find_key_material(lower: &str) -> Option<usize> {
    if let Some(idx) = lower.find("-----begin") {
        return Some(idx);
    }
    let bytes = lower.as_bytes();
    let mut run = 0usize;
    let mut run_start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if (b'a'..=b'f').contains(&b) || b.is_ascii_digit() {
            if run == 0 {
                run_start = i;
            }
            run += 1;
            if run >= 32 {
                return Some(run_start);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// Detect `score=` followed by a digit (raw match score leak).
fn find_raw_score(lower: &str) -> Option<usize> {
    lower.find("score=").filter(|&idx| {
        lower[idx + 6..]
            .bytes()
            .next()
            .map(|b| b.is_ascii_digit())
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Support bundle
// ---------------------------------------------------------------------------

/// Support bundle creation failure.
#[derive(Debug)]
pub enum BundleError {
    Io(io::Error),
    Privacy(PrivacyError),
}

impl From<io::Error> for BundleError {
    fn from(e: io::Error) -> Self {
        BundleError::Io(e)
    }
}

impl From<PrivacyError> for BundleError {
    fn from(e: PrivacyError) -> Self {
        BundleError::Privacy(e)
    }
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BundleError::Io(e) => write!(f, "bundle io: {e}"),
            BundleError::Privacy(e) => write!(f, "bundle {e}"),
        }
    }
}

impl std::error::Error for BundleError {}

/// Create a privacy-safe support bundle at `out_dir/namtor-bundle-<ts>/`.
/// Returns the bundle directory path. Every file written is scanned by
/// `PrivacyScanner` first; if any file would carry biometric material the
/// bundle is removed and the error is returned.
pub fn create_bundle(
    report: &DiagnosticReport,
    cfg: &DiagnosticConfig,
    out_dir: &Path,
) -> Result<PathBuf, BundleError> {
    let scanner = PrivacyScanner::new();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bundle_dir = out_dir.join(format!("namtor-bundle-{ts}"));

    let report_text = format!("{report}");
    scanner.scan(&report_text)?;

    let config_text = redact_config(cfg);
    scanner.scan(&config_text)?;

    let readme_text = bundle_readme();
    scanner.scan(&readme_text)?;

    fs::create_dir_all(&bundle_dir)?;
    write_scanned(&bundle_dir.join("diagnostic.txt"), &report_text, &scanner)?;
    write_scanned(&bundle_dir.join("config.txt"), &config_text, &scanner)?;
    write_scanned(&bundle_dir.join("README.txt"), &readme_text, &scanner)?;
    Ok(bundle_dir)
}

fn write_scanned(path: &Path, content: &str, scanner: &PrivacyScanner) -> Result<(), BundleError> {
    scanner.scan(content)?;
    let mut f = fs::File::create(path)?;
    f.write_all(content.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

/// Redacted config summary: paths and presence flags only, never the
/// words "template" or biometric identifiers.
fn redact_config(cfg: &DiagnosticConfig) -> String {
    let mut s = String::new();
    s.push_str("=== Nam-tor Config (redacted) ===\n");
    s.push_str(&format!("device: {}\n", present(&cfg.device_path)));
    s.push_str(&format!("vid:pid: {}\n", cfg.vid_pid));
    s.push_str(&format!("storage dir: {}\n", present(&cfg.templates_dir)));
    s.push_str(&format!("wrapping key: {}\n", present(&cfg.key_path)));
    s.push_str(&format!("models dir: {}\n", present(&cfg.models_dir)));
    s.push_str(&format!("daemon socket: {}\n", present(&cfg.daemon_socket)));
    s.push_str(&format!("cli bin: {}\n", present(&cfg.cli_bin)));
    s.push_str(&format!("daemon bin: {}\n", present(&cfg.daemon_bin)));
    s.push_str(&format!("quirks: {} searched\n", cfg.quirks_dirs.len()));
    s.push_str(&format!("auth outcome: {}\n", cfg.auth_outcome));
    s
}

fn present(p: &Path) -> String {
    if p.exists() {
        "present".into()
    } else {
        "absent".into()
    }
}

fn bundle_readme() -> String {
    "=== Nam-tor Support Bundle ===\n\
     This bundle was scanned by the privacy filter before write.\n\
     It contains only operator-coded diagnostics and redacted config.\n\
     No biometric data, raw scores, or secret material is included.\n"
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn scanner() -> PrivacyScanner {
        PrivacyScanner::new()
    }

    #[test]
    fn privacy_scanner_rejects_embedding_template_pixel() {
        let s = scanner();
        assert!(s.scan("embedding vector leaked").is_err());
        assert!(s.scan("stored template for uid 1000").is_err());
        assert!(s.scan("mean pixel value 42").is_err());
    }

    #[test]
    fn privacy_scanner_rejects_frame_and_crop_and_threshold_and_nonce() {
        let s = scanner();
        assert!(s.scan("captured frame 12").is_err());
        assert!(s.scan("face crop 64x64").is_err());
        assert!(s.scan("match threshold 0.42").is_err());
        assert!(s.scan("nonce=deadbeef").is_err());
    }

    #[test]
    fn privacy_scanner_rejects_hex_sequence_and_key_material() {
        let s = scanner();
        assert!(s.scan("hash = 0xdeadbeefcafef00d").is_err());
        assert!(s
            .scan("key = aaaaaaaa1111bbbb2222cccc3333dddd4444")
            .is_err());
        assert!(s.scan("-----BEGIN PRIVATE KEY-----").is_err());
    }

    #[test]
    fn privacy_scanner_rejects_raw_score() {
        let s = scanner();
        let err = s.scan("match score=0.8723 accepted").unwrap_err();
        assert!(matches!(err, PrivacyError::RawScore { .. }));
    }

    #[test]
    fn privacy_scanner_accepts_clean_diagnostic_output() {
        let s = scanner();
        let clean = "=== Nam-tor Diagnostic Report ===\n\
                     [OK] device: present (/dev/video2)\n\
                     [OK] emitter: msxu (firmware-strobed)\n\
                     [OK] protection: full\n\
                     [OK] storage: 2 enrollments\n\
                     [OK] model: all inference models present\n\
                     [OK] package: cli and daemon binaries present\n\
                     [OK] api: daemon socket present\n\
                     auth: unavailable";
        s.scan(clean).expect("clean report must pass scanner");
    }

    #[test]
    fn doctor_report_has_all_check_categories() {
        let tmp = TempDir::new().unwrap();
        let cfg = full_ok_config(tmp.path());
        let report = run_doctor(&cfg);
        let names: Vec<&str> = report.checks().iter().map(|c| c.check()).collect();
        assert_eq!(
            names,
            vec![
                "device",
                "emitter",
                "protection",
                "storage",
                "model",
                "package",
                "api",
            ]
        );
    }

    #[test]
    fn doctor_report_all_ok_when_everything_present() {
        let tmp = TempDir::new().unwrap();
        let cfg = full_ok_config(tmp.path());
        let report = run_doctor(&cfg);
        for c in report.checks() {
            assert!(c.is_ok(), "{} not OK: {c}", c.check());
        }
        assert!(!report.has_error());
        assert!(!report.has_warning());
    }

    fn full_ok_config(tmp: &Path) -> DiagnosticConfig {
        let models = tmp.join("models");
        let storage = tmp.join("store");
        let quirks = tmp.join("quirks");
        fs::create_dir_all(&models).unwrap();
        fs::create_dir_all(&storage).unwrap();
        fs::create_dir_all(&quirks).unwrap();
        fs::write(models.join("yunet_2023mar.onnx"), b"x").unwrap();
        fs::write(models.join("sface_2021dec.onnx"), b"x").unwrap();
        fs::write(models.join("minifasnet_pad.onnx"), b"x").unwrap();
        fs::write(storage.join("dummy.sealed"), b"x").unwrap();
        fs::write(tmp.join("key"), b"x").unwrap();
        let src = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../quirks/devices/3277-0018.toml"
        );
        fs::copy(src, quirks.join("3277-0018.toml")).unwrap();
        let cli_bin = tmp.join("namtor-cli");
        let daemon_bin = tmp.join("namtor-daemon");
        let socket = tmp.join("auth.sock");
        let device = tmp.join("video2");
        fs::write(&cli_bin, b"x").unwrap();
        fs::write(&daemon_bin, b"x").unwrap();
        fs::write(&socket, b"x").unwrap();
        fs::write(&device, b"x").unwrap();
        DiagnosticConfig {
            device_path: device,
            vid_pid: "3277:0018".into(),
            templates_dir: storage,
            key_path: tmp.join("key"),
            models_dir: models,
            daemon_socket: socket,
            daemon_bin,
            cli_bin,
            quirks_dirs: vec![quirks],
            auth_outcome: AuthOutcome::Unavailable,
        }
    }

    #[test]
    fn doctor_flags_missing_device_as_e001_error() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = full_ok_config(tmp.path());
        cfg.device_path = tmp.path().join("nonexistent");
        let report = run_doctor(&cfg);
        assert!(report.device.is_error());
        assert_eq!(report.device.code(), Some("E001"));
        let remediation = report.device.remediation().unwrap();
        assert!(!remediation.is_empty(), "error must have remediation");
    }

    #[test]
    fn doctor_flags_missing_model_as_e004_error() {
        let tmp = TempDir::new().unwrap();
        let cfg = full_ok_config(tmp.path());
        fs::remove_file(cfg.models_dir.join("yunet_2023mar.onnx")).unwrap();
        let report = run_doctor(&cfg);
        assert!(report.model.is_error());
        assert_eq!(report.model.code(), Some("E004"));
    }

    #[test]
    fn doctor_flags_missing_socket_as_e005_error() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = full_ok_config(tmp.path());
        cfg.daemon_socket = tmp.path().join("nope.sock");
        let report = run_doctor(&cfg);
        assert!(report.api.is_error());
        assert_eq!(report.api.code(), Some("E005"));
    }

    #[test]
    fn doctor_flags_missing_key_file_as_w003_warning() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = full_ok_config(tmp.path());
        cfg.key_path = tmp.path().join("nokey");
        let report = run_doctor(&cfg);
        assert!(report.storage.is_warning());
        assert_eq!(report.storage.code(), Some("W003"));
    }

    #[test]
    fn doctor_missing_package_binaries_is_warning_not_error() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = full_ok_config(tmp.path());
        cfg.daemon_bin = tmp.path().join("missing-daemon");
        let report = run_doctor(&cfg);
        assert!(report.package.is_warning());
        assert_eq!(report.package.code(), Some("W004"));
        assert!(!report.package.is_error());
    }

    #[test]
    fn warning_and_error_severity_are_distinct() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = full_ok_config(tmp.path());
        cfg.device_path = tmp.path().join("nonexistent");
        cfg.key_path = tmp.path().join("nokey");
        let report = run_doctor(&cfg);
        assert!(report.device.is_error());
        assert!(report.storage.is_warning());
        assert!(report.has_error());
        assert!(report.has_error() != report.has_warning() || !report.has_error());
    }

    #[test]
    fn auth_outcome_is_generic_no_raw_scores() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = full_ok_config(tmp.path());
        cfg.auth_outcome = AuthOutcome::Match;
        let report = run_doctor(&cfg);
        let text = format!("{report}");
        assert!(text.contains("auth: match"));
        assert!(!text.contains("score="), "no raw scores in report");
        assert!(!text.contains("0.87"), "no numeric scores in report");
    }

    #[test]
    fn auth_outcome_display_tokens() {
        assert_eq!(format!("{}", AuthOutcome::Match), "match");
        assert_eq!(format!("{}", AuthOutcome::NoMatch), "no match");
        assert_eq!(format!("{}", AuthOutcome::Unavailable), "unavailable");
    }

    #[test]
    fn operator_codes_are_structured_and_redacted() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = full_ok_config(tmp.path());
        cfg.device_path = tmp.path().join("nonexistent");
        let report = run_doctor(&cfg);
        let text = format!("{}", report.device);
        assert!(
            text.starts_with("[E001] device:"),
            "operator code must be structured: {text}"
        );
    }

    #[test]
    fn error_remediation_is_actionable() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = full_ok_config(tmp.path());
        cfg.device_path = tmp.path().join("nonexistent");
        let report = run_doctor(&cfg);
        let r = report.device.remediation().unwrap();
        assert!(
            r.contains("connect")
                || r.contains("create")
                || r.contains("start")
                || r.contains("install")
                || r.contains("download")
                || r.contains("resolve"),
            "remediation must be actionable: {r}"
        );
    }

    #[test]
    fn bundle_creation_works() {
        let tmp = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let cfg = full_ok_config(tmp.path());
        let report = run_doctor(&cfg);
        let bundle = create_bundle(&report, &cfg, out.path()).expect("bundle creation");
        assert!(bundle.exists(), "bundle dir exists");
        assert!(bundle.join("diagnostic.txt").exists());
        assert!(bundle.join("config.txt").exists());
        assert!(bundle.join("README.txt").exists());
    }

    #[test]
    fn bundle_passes_privacy_scanner() {
        let tmp = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let cfg = full_ok_config(tmp.path());
        let report = run_doctor(&cfg);
        let bundle = create_bundle(&report, &cfg, out.path()).expect("bundle creation");
        let s = scanner();
        for entry in fs::read_dir(&bundle).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                let content = fs::read_to_string(entry.path()).unwrap();
                s.scan(&content)
                    .expect("bundle file must pass privacy scan");
            }
        }
    }

    #[test]
    fn bundle_rejects_content_with_biometric_term() {
        let tmp = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let mut cfg = full_ok_config(tmp.path());
        // inject a forbidden term into the vid:pid field, which lands in
        // the redacted config output, to prove the scanner gates writes.
        cfg.vid_pid = "embedding-leak".into();
        let report = run_doctor(&cfg);
        let err = create_bundle(&report, &cfg, out.path()).unwrap_err();
        assert!(matches!(
            err,
            BundleError::Privacy(PrivacyError::ForbiddenTerm { .. })
        ));
    }

    // -----------------------------------------------------------------
    // Portability: derived defaults must not embed reference constants.
    // -----------------------------------------------------------------

    fn repo_manifest_path() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packaging/install-manifest.toml"
        ))
    }

    #[test]
    fn static_guard_rejects_reference_constants_in_defaults() {
        // The guard checks the compile-time fallback defaults embedded in
        // the layout + device discovery, not the runtime sysfs result (which
        // legitimately finds the reference camera on this dev machine).
        // An unreadable manifest forces LayoutPaths onto its fallback dests;
        // an empty sysfs root forces discover_device onto its fallback path.
        let layout = LayoutPaths::resolve(
            Path::new("/nonexistent-root"),
            Path::new("/nonexistent-manifest.toml"),
            InstallRoot::Installed,
        );
        let layout_text = format!(
            "{} {} {} {} {} {} {:?}",
            layout.daemon_bin.display(),
            layout.cli_bin.display(),
            layout.models_dir.display(),
            layout.templates_dir.display(),
            layout.key_path.display(),
            layout.daemon_socket.display(),
            layout.quirks_dirs
        );
        for c in crate::install_layout::REFERENCE_CONSTANTS {
            assert!(
                !layout_text.contains(c),
                "reference constant {c:?} leaked into layout fallback defaults: {layout_text}"
            );
        }
    }

    #[test]
    fn layout_source_root_uses_target_release_binaries() {
        let layout = LayoutPaths::resolve(
            std::env::current_dir().unwrap().as_path(),
            &repo_manifest_path(),
            InstallRoot::Source,
        );
        assert!(
            layout.daemon_bin.ends_with("target/release/namtor-daemon"),
            "source daemon_bin = {}",
            layout.daemon_bin.display()
        );
        assert!(
            layout.cli_bin.ends_with("target/release/namtor-cli"),
            "source cli_bin = {}",
            layout.cli_bin.display()
        );
    }

    #[test]
    fn layout_staged_root_rebases_paths_under_destdir() {
        let tmp = TempDir::new().unwrap();
        let manifest = repo_manifest_path();
        let staged_manifest = tmp.path().join("usr/share/namtor/install-manifest.toml");
        fs::create_dir_all(staged_manifest.parent().unwrap()).unwrap();
        fs::copy(&manifest, &staged_manifest).unwrap();

        let layout = LayoutPaths::resolve(tmp.path(), &staged_manifest, InstallRoot::Staged);
        assert_eq!(
            layout.daemon_bin,
            tmp.path().join("usr/lib/namtor/namtor-daemon"),
            "staged daemon_bin rebased under DESTDIR"
        );
        assert_eq!(
            layout.cli_bin,
            tmp.path().join("usr/bin/namtor-cli"),
            "staged cli_bin rebased under DESTDIR"
        );
        assert_eq!(layout.models_dir, tmp.path().join("var/lib/namtor/models"));
        assert_eq!(
            layout.daemon_socket,
            tmp.path().join("run/namtor/auth.sock")
        );
        assert_eq!(
            layout.templates_dir,
            tmp.path().join("var/lib/namtor/templates")
        );
    }

    #[test]
    fn layout_alternate_libexec_follows_manifest() {
        let tmp = TempDir::new().unwrap();
        let alt_manifest = tmp.path().join("install-manifest.toml");
        fs::write(
            &alt_manifest,
            "[[files]]\ndest = \"/usr/libexec/namtor-daemon\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n\n\
             [[files]]\ndest = \"/usr/bin/namtor-cli\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n",
        )
        .unwrap();

        let layout = LayoutPaths::resolve(tmp.path(), &alt_manifest, InstallRoot::Installed);
        assert_eq!(
            layout.daemon_bin,
            PathBuf::from("/usr/libexec/namtor-daemon"),
            "alternate libexec dest followed from manifest"
        );
    }

    #[test]
    fn doctor_reports_absent_daemon_in_destdir_fixture() {
        let tmp = TempDir::new().unwrap();
        let manifest = repo_manifest_path();
        let staged_manifest = tmp.path().join("usr/share/namtor/install-manifest.toml");
        fs::create_dir_all(staged_manifest.parent().unwrap()).unwrap();
        fs::copy(&manifest, &staged_manifest).unwrap();

        let layout = LayoutPaths::resolve(tmp.path(), &staged_manifest, InstallRoot::Staged);
        let cfg = DiagnosticConfig::from_layout(
            &layout,
            tmp.path().join("dev/video2"),
            "3277:0018".into(),
        );
        let report = run_doctor(&cfg);
        assert!(
            report.package.is_warning(),
            "absent daemon in DESTDIR must be W004, got {:?}",
            report.package
        );
        assert_eq!(report.package.code(), Some("W004"));
    }

    #[test]
    fn doctor_renumbered_device_reports_correct_path() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = full_ok_config(tmp.path());
        let renumbered = tmp.path().join("video7");
        fs::write(&renumbered, b"x").unwrap();
        cfg.device_path = renumbered.clone();
        let report = run_doctor(&cfg);
        assert!(report.device.is_ok());
        assert!(
            format!("{}", report.device).contains("video7"),
            "renumbered device path reported"
        );
    }

    #[test]
    fn from_layout_carries_all_layout_paths() {
        let tmp = TempDir::new().unwrap();
        let manifest = tmp.path().join("m.toml");
        fs::write(
            &manifest,
            "[[files]]\ndest = \"/usr/lib/namtor/namtor-daemon\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n\n\
             [[files]]\ndest = \"/usr/bin/namtor-cli\"\nsrc = \"x\"\nmode = \"0755\"\nowner = \"root\"\ngroup = \"root\"\n",
        )
        .unwrap();
        let layout = LayoutPaths::resolve(tmp.path(), &manifest, InstallRoot::Staged);
        let cfg = DiagnosticConfig::from_layout(
            &layout,
            PathBuf::from("/dev/video4"),
            "1111:2222".into(),
        );
        assert_eq!(cfg.device_path, PathBuf::from("/dev/video4"));
        assert_eq!(cfg.vid_pid, "1111:2222");
        assert_eq!(
            cfg.daemon_bin,
            tmp.path().join("usr/lib/namtor/namtor-daemon")
        );
        assert_eq!(cfg.cli_bin, tmp.path().join("usr/bin/namtor-cli"));
        assert_eq!(cfg.models_dir, tmp.path().join("var/lib/namtor/models"));
    }
}
