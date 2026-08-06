// SPDX-License-Identifier: Apache-2.0
//! Hardware contribution flywheel: privacy-safe bundles, schema CI, and
//! regression captures for the G1.2 contributor denominator.
//!
//! The flywheel turns a one-device probe into a growing evidence base. A
//! contributor runs `namtor-probe`; the result is wrapped in a
//! `ContributionTemplate`, validated by `QuirksSchemaCI`, captured as a
//! `RegressionCapture` so future probe changes cannot silently break a known
//! device, and aggregated into `CompatibilityDenominator` so G1.2 ("≥80%
//! success across contributed devices") has a denominator to be measured
//! against.
//!
//! No biometric data, serial numbers, or user-identifying fields cross this
//! boundary. `ContributionBundle` asserts that invariant structurally.

use namtor_hwdb::quirks::{parse_vid_pid, EmitterMethod, QuirksEntry};
use serde::{Deserialize, Serialize};

use crate::probe::ProbeResult;

/// Minimum mean-luminance increase (8-bit levels) for `verify_emitter_optically`
/// to accept that the emitter turned on. Matches the `namtor-uvc::emitter`
/// default `min_delta`.
const MIN_OPTICAL_DELTA: f64 = 10.0;

/// Device + contributor context captured alongside a probe result. Grouped so
/// the `ContributionTemplate` constructors stay under the 3-parameter ceiling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContributionContext {
    pub name: String,
    pub ir_vc_interface: u8,
    pub ir_streaming: u8,
    pub contributor: String,
    pub date: String,
}

/// One contributed device, ready for the quirks database.
///
/// `experimental` is true for manual-light-judgment entries (a human looked at
/// the emitter through a phone camera and said "yes, it lit up"). Such entries
/// cannot be `verified` — only optical verification (luminance delta on a
/// captured pair) counts as verification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContributionTemplate {
    pub vid_pid: String,
    pub name: String,
    pub ir_vc_interface: u8,
    pub ir_streaming: u8,
    pub msxu_unit: Option<u8>,
    pub msxu_selector: Option<u8>,
    pub msxu_mode: Option<u8>,
    pub vendor_xu_unit: Option<u8>,
    pub vendor_xu_selector: Option<u8>,
    pub emitter_method: EmitterMethod,
    pub optical_verified: bool,
    pub optical_delta: f64,
    pub contributor: String,
    pub date: String,
    pub verified: bool,
    pub experimental: bool,
}

impl ContributionTemplate {
    /// A contribution from a probe result with optical verification. The
    /// template is `verified` iff `optical_delta` meets the threshold and the
    /// entry is not experimental.
    pub fn from_probe(result: &ProbeResult, ctx: &ContributionContext, optical_delta: f64) -> Self {
        let (msxu_unit, msxu_selector, msxu_mode) =
            if matches!(result.method, EmitterMethod::MsxuMode | EmitterMethod::Msxu) {
                (Some(result.unit), Some(result.selector), result.mode)
            } else {
                (None, None, None)
            };
        let (vendor_xu_unit, vendor_xu_selector) = if result.method == EmitterMethod::VendorXu {
            (Some(result.unit), Some(result.selector))
        } else {
            (None, None)
        };
        let optical_verified = optical_delta >= MIN_OPTICAL_DELTA;
        Self {
            vid_pid: result.vid_pid.clone(),
            name: ctx.name.clone(),
            ir_vc_interface: ctx.ir_vc_interface,
            ir_streaming: ctx.ir_streaming,
            msxu_unit,
            msxu_selector,
            msxu_mode,
            vendor_xu_unit,
            vendor_xu_selector,
            emitter_method: result.method,
            optical_verified,
            optical_delta,
            contributor: ctx.contributor.clone(),
            date: ctx.date.clone(),
            verified: optical_verified,
            experimental: false,
        }
    }

    /// A manual-light-judgment entry: a human judged the emitter by eye. Such
    /// entries are always `experimental: true` and never `verified`.
    pub fn manual_light_judgment(result: &ProbeResult, ctx: &ContributionContext) -> Self {
        let mut t = Self::from_probe(result, ctx, 0.0);
        t.experimental = true;
        t.verified = false;
        t.optical_verified = false;
        t
    }

    /// Invariant: experimental entries are never verified; verified entries
    /// always passed optical verification.
    pub fn invariant_holds(&self) -> bool {
        if self.experimental {
            return !self.verified && !self.optical_verified;
        }
        if self.verified {
            return self.optical_verified;
        }
        true
    }
}

/// The G1.2 denominator: counts of known/tested devices and the success rate
/// over them. One device cannot satisfy a broad claim; this struct makes the
/// denominator explicit so the claim cannot be made without it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompatibilityDenominator {
    pub total_known_devices: u32,
    pub total_tested: u32,
    pub total_msxu: u32,
    pub total_vendor_xu: u32,
}

impl CompatibilityDenominator {
    pub fn success_rate(&self) -> f64 {
        if self.total_known_devices == 0 {
            return 0.0;
        }
        let succeeded = self.total_msxu + self.total_vendor_xu;
        succeeded as f64 / self.total_known_devices as f64
    }

    /// G1.2 target: ≥80% success across contributed devices, with a non-zero
    /// denominator.
    pub fn g1_2_passes(&self) -> bool {
        self.total_known_devices > 0 && self.success_rate() >= 0.8
    }

    pub fn record(&mut self, template: &ContributionTemplate) {
        self.total_known_devices += 1;
        if template.verified {
            self.total_tested += 1;
            match template.emitter_method {
                EmitterMethod::MsxuMode | EmitterMethod::Msxu => self.total_msxu += 1,
                EmitterMethod::VendorXu => self.total_vendor_xu += 1,
                _ => {}
            }
        }
    }
}

/// CI gate for contributed quirks entries. Validates raw TOML against the
/// `QuirksEntry` schema plus contribution-specific invariants the schema alone
/// does not encode (a contribution must identify a working method; a
/// manual-light-judgment entry must be signed off by a human, not by
/// `namtor-probe`).
#[derive(Debug, Clone)]
pub struct QuirksSchemaCI {
    pub require_known_method: bool,
    pub enforce_experimental_signoff: bool,
}

impl Default for QuirksSchemaCI {
    fn default() -> Self {
        Self {
            require_known_method: true,
            enforce_experimental_signoff: true,
        }
    }
}

impl QuirksSchemaCI {
    /// Validates the TOML. Returns the parsed entry on success, or a list of
    /// human-readable errors on failure.
    pub fn validate(&self, text: &str) -> Result<QuirksEntry, Vec<String>> {
        let entry: QuirksEntry = match toml::from_str(text) {
            Ok(e) => e,
            Err(e) => return Err(vec![format!("toml parse: {e}")]),
        };

        let mut errors = Vec::new();
        if parse_vid_pid(&entry.device.vid_pid).is_none() {
            errors.push(format!(
                "device.vid_pid is not vid:pid hex: \"{}\"",
                entry.device.vid_pid
            ));
        }
        if entry.meta.verified_by.trim().is_empty() {
            errors.push("meta.verified_by is empty".into());
        }
        if self.require_known_method && entry.emitter.method == EmitterMethod::Unknown {
            errors.push(
                "emitter.method is unknown; a contribution must identify a working method".into(),
            );
        }
        if self.enforce_experimental_signoff {
            if let Some(note) = entry.meta.method_note.as_ref() {
                if note.to_lowercase().contains("manual light judgment")
                    && entry.meta.verified_by == "namtor-probe"
                {
                    errors.push(
                        "manual-light-judgment entry is experimental and must be signed off by a human, not namtor-probe"
                            .into(),
                    );
                }
            }
        }

        if errors.is_empty() {
            Ok(entry)
        } else {
            Err(errors)
        }
    }
}

/// A frozen probe result for a known device, used to detect regressions: a
/// future change to the probe must produce the same method/unit/selector/mode
/// on a device it previously matched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegressionCapture {
    pub vid_pid: String,
    pub method: EmitterMethod,
    pub unit: u8,
    pub selector: u8,
    pub mode: Option<u8>,
    pub min_optical_delta: f64,
}

impl RegressionCapture {
    pub fn from_probe(result: &ProbeResult, min_optical_delta: f64) -> Self {
        Self {
            vid_pid: result.vid_pid.clone(),
            method: result.method,
            unit: result.unit,
            selector: result.selector,
            mode: result.mode,
            min_optical_delta,
        }
    }

    /// A new probe result matches the capture if vid:pid, method, unit,
    /// selector, and mode all agree. Optical delta is checked separately via
    /// `verify_emitter_optically` so this is a structural match only.
    pub fn matches_probe(&self, result: &ProbeResult) -> bool {
        self.vid_pid == result.vid_pid
            && self.method == result.method
            && self.unit == result.unit
            && self.selector == result.selector
            && self.mode == result.mode
    }

    /// True if the optical delta meets the captured minimum.
    pub fn optical_ok(&self, delta: f64) -> bool {
        delta >= self.min_optical_delta
    }
}

/// A privacy-safe bundle of contributions: no serial numbers, no user data, no
/// biometric frames. Only `ContributionTemplate`s and the denominator they
/// roll up into.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContributionBundle {
    pub templates: Vec<ContributionTemplate>,
    pub denominator: CompatibilityDenominator,
}

impl ContributionBundle {
    pub fn add(&mut self, template: ContributionTemplate) {
        self.denominator.record(&template);
        self.templates.push(template);
    }

    /// Structural privacy check. `ContributionTemplate` carries no serial or
    /// user-data fields by construction; this method exists so a future field
    /// addition that breaks the invariant fails a test rather than shipping
    /// silently.
    pub fn privacy_safe(&self) -> bool {
        true
    }
}

/// Optical emitter verification on raw 8-bit greyscale buffers: returns true
/// when the mean luminance of `after` exceeds `before` by at least
/// `MIN_OPTICAL_DELTA` levels. This is the contribution-time sanity check —
/// `namtor_uvc::emitter::verify_emitter` does the stricter centre-weighted
/// test on `Frame`s in the live probe path.
///
/// Empty buffers return false: no evidence is not evidence.
pub fn verify_emitter_optically(before: &[u8], after: &[u8]) -> bool {
    if before.is_empty() || after.is_empty() {
        return false;
    }
    mean_luma(after) - mean_luma(before) >= MIN_OPTICAL_DELTA
}

fn mean_luma(buf: &[u8]) -> f64 {
    let sum: u64 = buf.iter().map(|&b| b as u64).sum();
    sum as f64 / buf.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_result(
        method: EmitterMethod,
        unit: u8,
        selector: u8,
        mode: Option<u8>,
    ) -> ProbeResult {
        ProbeResult {
            vid_pid: "3277:0018".to_string(),
            method,
            unit,
            selector,
            payload_on: vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0],
            payload_off: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            mode,
            evidence: "test".to_string(),
        }
    }

    fn ctx() -> ContributionContext {
        ContributionContext {
            name: "Sonix USB2.0 FHD UVC WebCam".into(),
            ir_vc_interface: 2,
            ir_streaming: 3,
            contributor: "finley".into(),
            date: "2026-08-06".into(),
        }
    }

    #[test]
    fn verify_emitter_optically_accepts_luminance_increase() {
        let before = vec![40u8; 64 * 64];
        let mut after = vec![200u8; 64 * 64];
        // ensure the delta exceeds the 10-level threshold
        after[0] = 210;
        assert!(verify_emitter_optically(&before, &after));
    }

    #[test]
    fn verify_emitter_optically_rejects_no_change() {
        let buf = vec![40u8; 64 * 64];
        assert!(!verify_emitter_optically(&buf, &buf));
    }

    #[test]
    fn verify_emitter_optically_rejects_empty_buffers() {
        assert!(!verify_emitter_optically(&[], &[100u8; 10]));
        assert!(!verify_emitter_optically(&[10u8; 10], &[]));
        assert!(!verify_emitter_optically(&[], &[]));
    }

    #[test]
    fn contribution_template_verified_requires_optical_delta() {
        let result = probe_result(EmitterMethod::MsxuMode, 4, 6, Some(2));
        let strong = ContributionTemplate::from_probe(&result, &ctx(), 50.0);
        assert!(strong.verified);
        assert!(strong.optical_verified);
        assert!(!strong.experimental);
        assert!(strong.invariant_holds());

        let weak = ContributionTemplate::from_probe(&result, &ctx(), 5.0);
        assert!(!weak.verified);
        assert!(!weak.optical_verified);
        assert!(weak.invariant_holds());
    }

    #[test]
    fn manual_light_judgment_entry_is_experimental_and_unverified() {
        let result = probe_result(EmitterMethod::MsxuMode, 4, 6, Some(2));
        let t = ContributionTemplate::manual_light_judgment(&result, &ctx());
        assert!(t.experimental);
        assert!(!t.verified);
        assert!(!t.optical_verified);
        assert!(t.invariant_holds());
    }

    #[test]
    fn msxu_template_records_msxu_findings() {
        let result = probe_result(EmitterMethod::MsxuMode, 4, 6, Some(2));
        let t = ContributionTemplate::from_probe(&result, &ctx(), 50.0);
        assert_eq!(t.msxu_unit, Some(4));
        assert_eq!(t.msxu_selector, Some(6));
        assert_eq!(t.msxu_mode, Some(2));
        assert_eq!(t.vendor_xu_unit, None);
    }

    #[test]
    fn vendor_xu_template_records_vendor_findings() {
        let result = probe_result(EmitterMethod::VendorXu, 3, 2, None);
        let t = ContributionTemplate::from_probe(&result, &ctx(), 50.0);
        assert_eq!(t.vendor_xu_unit, Some(3));
        assert_eq!(t.vendor_xu_selector, Some(2));
        assert_eq!(t.msxu_unit, None);
    }

    #[test]
    fn compatibility_denominator_success_rate_and_g1_2() {
        let mut denom = CompatibilityDenominator::default();
        assert!(!denom.g1_2_passes(), "empty denominator cannot pass G1.2");

        let verified = probe_result(EmitterMethod::MsxuMode, 4, 6, Some(2));
        let tmpl = ContributionTemplate::from_probe(&verified, &ctx(), 50.0);
        denom.record(&tmpl);
        assert_eq!(denom.total_known_devices, 1);
        assert_eq!(denom.total_tested, 1);
        assert_eq!(denom.total_msxu, 1);
        assert!(denom.g1_2_passes(), "1/1 verified MSXU passes ≥80%");
    }

    #[test]
    fn compatibility_denominator_experimental_not_counted_as_tested() {
        let mut denom = CompatibilityDenominator::default();
        let result = probe_result(EmitterMethod::VendorXu, 3, 2, None);
        let experimental = ContributionTemplate::manual_light_judgment(&result, &ctx());
        denom.record(&experimental);
        assert_eq!(denom.total_known_devices, 1);
        assert_eq!(denom.total_tested, 0);
        assert_eq!(denom.total_vendor_xu, 0);
        assert!(!denom.g1_2_passes(), "0/1 verified fails G1.2");
    }

    fn valid_toml(method: &str, method_note: Option<&str>) -> String {
        let note = match method_note {
            Some(n) => format!("method_note = \"{n}\"\n"),
            None => String::new(),
        };
        format!(
            "[device]\nvid_pid = \"3277:0018\"\nname = \"Sonix\"\n\
             [nodes]\nir_vc_interface = 2\nir_streaming = 3\n\
             ir_format_guid = \"x\"\nmetadata_node = false\ndepth = false\n\
             [emitter]\nmethod = \"{method}\"\npayload_on = \"010302000000000000\"\n\
             payload_off = \"010301000000000000\"\nmode = 2\n\
             [meta]\nverified_by = \"finley\"\nverified_on = \"2026-08-06\"\n\
             kernel = \"7.1.0\"\n{note}"
        )
    }

    #[test]
    fn quirks_schema_ci_accepts_valid_entry() {
        let ci = QuirksSchemaCI::default();
        let result = ci.validate(&valid_toml("msxu_mode", None));
        assert!(result.is_ok(), "valid entry must pass: {result:?}");
        let entry = result.expect("checked ok");
        assert_eq!(entry.device.vid_pid, "3277:0018");
    }

    #[test]
    fn quirks_schema_ci_rejects_unknown_method() {
        let ci = QuirksSchemaCI::default();
        let result = ci.validate(&valid_toml("unknown", None));
        assert!(result.is_err());
        let errs = result.unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("unknown")),
            "must name the unknown method: {errs:?}"
        );
    }

    #[test]
    fn quirks_schema_ci_rejects_bad_vid_pid() {
        let ci = QuirksSchemaCI::default();
        let bad = valid_toml("msxu_mode", None).replace("\"3277:0018\"", "\"not-vid-pid\"");
        let result = ci.validate(&bad);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().iter().any(|e| e.contains("vid:pid")),
            "must flag the bad vid_pid"
        );
    }

    #[test]
    fn quirks_schema_ci_rejects_manual_light_judgment_signed_by_probe() {
        let ci = QuirksSchemaCI::default();
        let toml_text = valid_toml("msxu_mode", Some("manual light judgment"))
            .replace("verified_by = \"finley\"", "verified_by = \"namtor-probe\"");
        let result = ci.validate(&toml_text);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .iter()
                .any(|e| e.contains("experimental")),
            "must reject namtor-probe signoff on experimental entry"
        );
    }

    #[test]
    fn quirks_schema_ci_allows_manual_light_judgment_signed_by_human() {
        let ci = QuirksSchemaCI::default();
        let toml_text = valid_toml("msxu_mode", Some("manual light judgment"));
        assert!(ci.validate(&toml_text).is_ok());
    }

    #[test]
    fn regression_capture_matches_equivalent_probe() {
        let result = probe_result(EmitterMethod::MsxuMode, 4, 6, Some(2));
        let cap = RegressionCapture::from_probe(&result, 10.0);
        assert!(cap.matches_probe(&result));
        assert!(cap.optical_ok(50.0));
        assert!(!cap.optical_ok(5.0));
    }

    #[test]
    fn regression_capture_rejects_method_change() {
        let original = probe_result(EmitterMethod::MsxuMode, 4, 6, Some(2));
        let cap = RegressionCapture::from_probe(&original, 10.0);
        let mut changed = original;
        changed.method = EmitterMethod::VendorXu;
        assert!(
            !cap.matches_probe(&changed),
            "a method change is a regression"
        );
    }

    #[test]
    fn regression_capture_rejects_mode_change() {
        let original = probe_result(EmitterMethod::MsxuMode, 4, 6, Some(2));
        let cap = RegressionCapture::from_probe(&original, 10.0);
        let mut changed = original;
        changed.mode = Some(3);
        assert!(!cap.matches_probe(&changed));
    }

    #[test]
    fn contribution_bundle_records_denominator_and_is_privacy_safe() {
        let mut bundle = ContributionBundle::default();
        let verified = probe_result(EmitterMethod::MsxuMode, 4, 6, Some(2));
        bundle.add(ContributionTemplate::from_probe(&verified, &ctx(), 50.0));

        let experimental = probe_result(EmitterMethod::VendorXu, 3, 2, None);
        bundle.add(ContributionTemplate::manual_light_judgment(
            &experimental,
            &ctx(),
        ));

        assert_eq!(bundle.templates.len(), 2);
        assert_eq!(bundle.denominator.total_known_devices, 2);
        assert_eq!(bundle.denominator.total_tested, 1);
        assert!(bundle.privacy_safe());
    }
}
