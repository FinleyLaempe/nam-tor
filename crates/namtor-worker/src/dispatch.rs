// SPDX-License-Identifier: GPL-3.0-only
//! CPU feature detection and INT8/FP32 dispatch (Task 19, spec §5.3).
//!
//! Spec §5.3 commits the auth path to CPU inference. INT8 quantized inference
//! benefits from AVX-VNNI (or AVX-512 VNNI) instructions; on CPUs without
//! VNNI, FP32 is faster. The GPU execution provider is NEVER selected — the
//! auth path stays CPU-only by design (spec §5.3, threat model).
//!
//! # Dispatch rule
//!
//! - AVX-VNNI or AVX-512 VNNI present → [`Quant::Int8`]
//! - otherwise → [`Quant::Fp32`] (including aarch64 NEON, which does not
//!   accelerate INT8 quantized matmul the way VNNI does)
//!
//! # WITHHELD semantics
//!
//! [`QuantizationReport`] is `withheld = true` when (a) no lawful comparison
//! cohort exists (R5 WITHHELD — owner-gated dataset selection), or (b) INT8
//! accuracy degradation exceeds the configured thresholds. In case (a) the
//! architectural default from [`select_quant`] is used (spec §5.3 Path B);
//! in case (b) FP32 is retained and the INT8 optimization is marked WITHHELD
//! — a degraded model is never silently promoted.

use namtor_proto::worker::Quant;

/// Detected CPU features relevant to INT8/FP32 dispatch.
///
/// All fields are `pub` so tests can construct feature sets without depending
/// on the host CPU. Use [`CpuFeatures::detect`] for production probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuFeatures {
    /// AVX2 (baseline for modern x86_64).
    pub avx2: bool,
    /// AVX-VNNI (the VNNI extension riding on AVX2, e.g. Intel Alder Lake+).
    pub avx_vnni: bool,
    /// AVX-512 foundation (AVX-512F).
    pub avx512: bool,
    /// AVX-512 VNNI.
    pub avx512_vnni: bool,
    /// AArch64 NEON.
    pub neon: bool,
    /// Architecture string for the report (`"x86_64"` / `"aarch64"` /
    /// `"unsupported"`).
    pub arch: &'static str,
}

impl CpuFeatures {
    /// Detect features on the running CPU at runtime. Features unavailable on
    /// the current architecture report `false`. The GPU path is never probed.
    pub fn detect() -> Self {
        let mut f = Self {
            arch: current_arch(),
            ..Default::default()
        };
        detect_arch(&mut f);
        f
    }
}

#[cfg(target_arch = "x86_64")]
fn current_arch() -> &'static str {
    "x86_64"
}

#[cfg(target_arch = "aarch64")]
fn current_arch() -> &'static str {
    "aarch64"
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn current_arch() -> &'static str {
    "unsupported"
}

/// Per-architecture runtime detection. On x86_64 the
/// `is_x86_feature_detected!` macro is the runtime check; on aarch64 the
/// `is_aarch64_feature_detected!` macro. On any other arch this is a no-op
/// (compile-time gate means the unsupported instruction path is unreachable).
#[cfg(target_arch = "x86_64")]
fn detect_arch(f: &mut CpuFeatures) {
    f.avx2 = std::arch::is_x86_feature_detected!("avx2");
    f.avx_vnni = std::arch::is_x86_feature_detected!("avxvnni");
    f.avx512 = std::arch::is_x86_feature_detected!("avx512f");
    f.avx512_vnni = std::arch::is_x86_feature_detected!("avx512vnni");
}

#[cfg(target_arch = "aarch64")]
fn detect_arch(f: &mut CpuFeatures) {
    f.neon = std::arch::is_aarch64_feature_detected!("neon");
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn detect_arch(_f: &mut CpuFeatures) {}

/// Select the auth-path quantisation based on detected CPU features.
///
/// Returns [`Quant::Int8`] when VNNI acceleration is available (AVX-VNNI or
/// AVX-512 VNNI); otherwise [`Quant::Fp32`]. The GPU execution provider is
/// never selected — there is no GPU path in this function or anywhere in the
/// auth path (spec §5.3).
pub fn select_quant(features: &CpuFeatures) -> Quant {
    if features.avx_vnni || features.avx512_vnni {
        Quant::Int8
    } else {
        Quant::Fp32
    }
}

/// Thresholds beyond which INT8 is considered degraded and FP32 is retained.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DegradationThresholds {
    /// Maximum absolute FAR delta (|INT8 − FP32|) before FP32 is retained.
    pub max_far_delta: f64,
    /// Maximum absolute FRR delta (|INT8 − FP32|) before FP32 is retained.
    pub max_frr_delta: f64,
}

impl Default for DegradationThresholds {
    fn default() -> Self {
        // Spec §5.3 targets FAR ≤ 1/100,000 (1e-5) at FRR ≤ 3% (3e-2).
        // The INT8 optimization must not move either gate by more than half
        // its budget headroom — beyond that, retain FP32 and mark WITHHELD.
        Self {
            max_far_delta: 5e-6,
            max_frr_delta: 5e-3,
        }
    }
}

/// Measured FP32-vs-INT8 comparison data. `None` for any delta field means
/// the measurement was not made (e.g. R5 cohort WITHHELD).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComparisonData {
    /// INT8 FAR − FP32 FAR (positive = INT8 worse). `None` if no cohort.
    pub far_delta: Option<f64>,
    /// INT8 FRR − FP32 FRR (positive = INT8 worse). `None` if no cohort.
    pub frr_delta: Option<f64>,
    /// INT8 median latency − FP32 median latency, ms (negative = INT8 faster).
    pub latency_delta_ms: Option<f64>,
    /// sha256 of the FP32 model bytes the comparison ran against.
    pub fp32_sha256: [u8; 32],
    /// sha256 of the INT8 model bytes the comparison ran against.
    pub int8_sha256: [u8; 32],
}

/// FP32 vs INT8 comparison report.
///
/// `withheld = true` means the INT8 optimization is not promoted: either the
/// comparison set is unavailable (no lawful cohort, R5 WITHHELD) or measured
/// degradation exceeded the configured thresholds. The `selected` field
/// records the quantisation the auth path should use given the report.
#[derive(Debug, Clone, PartialEq)]
pub struct QuantizationReport {
    pub far_delta: Option<f64>,
    pub frr_delta: Option<f64>,
    pub latency_delta_ms: Option<f64>,
    pub fp32_sha256: [u8; 32],
    pub int8_sha256: [u8; 32],
    /// Architecture the report was produced on.
    pub arch: &'static str,
    /// Quantisation the auth path should use given this report.
    pub selected: Quant,
    /// `true` when the INT8 optimization is not promoted.
    pub withheld: bool,
    /// Stable machine-readable reason when `withheld` is true.
    pub withheld_reason: Option<&'static str>,
}

/// Build a [`QuantizationReport`] from the detected features and optional
/// comparison data.
///
/// - `comparison = None` → WITHHELD, `selected = select_quant(features)`
///   (architectural default from spec §5.3 Path B).
/// - `comparison = Some` with `far_delta`/`frr_delta` `None` → WITHHELD
///   (no lawful cohort).
/// - `comparison = Some` with both deltas present and within thresholds →
///   not withheld, `selected = select_quant(features)`.
/// - `comparison = Some` with both deltas present but either exceeds the
///   thresholds → WITHHELD, `selected = Quant::Fp32` (retain FP32, never
///   silently use a degraded model).
pub fn build_report(
    features: &CpuFeatures,
    comparison: Option<ComparisonData>,
    thresholds: &DegradationThresholds,
) -> QuantizationReport {
    let Some(c) = comparison else {
        return QuantizationReport {
            far_delta: None,
            frr_delta: None,
            latency_delta_ms: None,
            fp32_sha256: [0; 32],
            int8_sha256: [0; 32],
            arch: features.arch,
            selected: select_quant(features),
            withheld: true,
            withheld_reason: Some("no comparison data available (R5 cohort WITHHELD)"),
        };
    };

    let (withheld, reason, selected) = match (c.far_delta, c.frr_delta) {
        (Some(fd), Some(rd)) => {
            if fd.abs() > thresholds.max_far_delta || rd.abs() > thresholds.max_frr_delta {
                (
                    true,
                    Some("INT8 accuracy degradation exceeds thresholds; FP32 retained"),
                    Quant::Fp32,
                )
            } else {
                (false, None, select_quant(features))
            }
        }
        _ => (
            true,
            Some("no lawful comparison cohort (R5 WITHHELD)"),
            select_quant(features),
        ),
    };

    QuantizationReport {
        far_delta: c.far_delta,
        frr_delta: c.frr_delta,
        latency_delta_ms: c.latency_delta_ms,
        fp32_sha256: c.fp32_sha256,
        int8_sha256: c.int8_sha256,
        arch: features.arch,
        selected,
        withheld,
        withheld_reason: reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn x86_features(avx_vnni: bool, avx512_vnni: bool) -> CpuFeatures {
        CpuFeatures {
            avx2: true,
            avx_vnni,
            avx512: avx512_vnni,
            avx512_vnni,
            neon: false,
            arch: "x86_64",
        }
    }

    #[test]
    fn select_quant_picks_int8_when_avx_vnni_present() {
        let f = x86_features(true, false);
        assert_eq!(select_quant(&f), Quant::Int8);
    }

    #[test]
    fn select_quant_picks_int8_when_avx512_vnni_present() {
        let f = x86_features(false, true);
        assert_eq!(select_quant(&f), Quant::Int8);
    }

    #[test]
    fn select_quant_picks_fp32_when_no_vnni() {
        let f = x86_features(false, false);
        assert_eq!(select_quant(&f), Quant::Fp32);
    }

    #[test]
    fn select_quant_aarch64_neon_picks_fp32() {
        // NEON does not accelerate INT8 quantized matmul the way VNNI does,
        // so a NEON-only CPU stays on FP32 (spec §5.3).
        let f = CpuFeatures {
            avx2: false,
            avx_vnni: false,
            avx512: false,
            avx512_vnni: false,
            neon: true,
            arch: "aarch64",
        };
        assert_eq!(select_quant(&f), Quant::Fp32);
    }

    #[test]
    fn select_quant_unsupported_arch_picks_fp32_safe_default() {
        let f = CpuFeatures {
            arch: "unsupported",
            ..Default::default()
        };
        assert_eq!(select_quant(&f), Quant::Fp32);
    }

    #[test]
    fn select_quant_never_returns_gpu_path() {
        // The Quant enum has only Fp32 and Int8 variants — there is no GPU
        // path anywhere in the auth path (spec §5.3). Sweep every feature
        // combination and assert the result stays a CPU variant.
        for avx2 in [false, true] {
            for avx_vnni in [false, true] {
                for avx512 in [false, true] {
                    for avx512_vnni in [false, true] {
                        for neon in [false, true] {
                            let f = CpuFeatures {
                                avx2,
                                avx_vnni,
                                avx512,
                                avx512_vnni,
                                neon,
                                arch: "sweep",
                            };
                            let q = select_quant(&f);
                            assert!(
                                matches!(q, Quant::Fp32 | Quant::Int8),
                                "select_quant returned non-CPU variant {q:?} for {f:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn report_withheld_when_no_comparison_data() {
        let f = x86_features(true, false);
        let r = build_report(&f, None, &DegradationThresholds::default());
        assert!(
            r.withheld,
            "report must be WITHHELD without comparison data"
        );
        assert!(r.withheld_reason.is_some());
        // Path B: architectural default is preserved when the cohort is WITHHELD.
        assert_eq!(r.selected, Quant::Int8);
    }

    #[test]
    fn report_withheld_when_deltas_missing() {
        let f = x86_features(true, false);
        let c = ComparisonData {
            far_delta: None,
            frr_delta: None,
            latency_delta_ms: None,
            fp32_sha256: [0; 32],
            int8_sha256: [0; 32],
        };
        let r = build_report(&f, Some(c), &DegradationThresholds::default());
        assert!(r.withheld);
        assert_eq!(r.selected, select_quant(&f));
    }

    #[test]
    fn report_retains_fp32_when_int8_far_degrades() {
        let f = x86_features(true, false);
        let c = ComparisonData {
            far_delta: Some(1e-4), // 0.01% — exceeds the 5e-6 threshold
            frr_delta: Some(1e-5),
            latency_delta_ms: Some(-5.0),
            fp32_sha256: [1; 32],
            int8_sha256: [2; 32],
        };
        let r = build_report(&f, Some(c), &DegradationThresholds::default());
        assert!(r.withheld, "degraded INT8 must be WITHHELD");
        assert_eq!(r.selected, Quant::Fp32, "FP32 must be retained, not INT8");
        assert_eq!(r.fp32_sha256, [1; 32]);
        assert_eq!(r.int8_sha256, [2; 32]);
        assert_eq!(r.arch, "x86_64");
    }

    #[test]
    fn report_retains_fp32_when_int8_frr_degrades() {
        let f = x86_features(true, false);
        let c = ComparisonData {
            far_delta: Some(1e-7),
            frr_delta: Some(1e-2), // 1% — exceeds the 5e-3 threshold
            latency_delta_ms: Some(-5.0),
            fp32_sha256: [0; 32],
            int8_sha256: [0; 32],
        };
        let r = build_report(&f, Some(c), &DegradationThresholds::default());
        assert!(r.withheld, "degraded INT8 must be WITHHELD");
        assert_eq!(r.selected, Quant::Fp32);
    }

    #[test]
    fn report_promotes_int8_when_within_thresholds() {
        let f = x86_features(true, false);
        let c = ComparisonData {
            far_delta: Some(1e-7),
            frr_delta: Some(1e-4),
            latency_delta_ms: Some(-50.0),
            fp32_sha256: [1; 32],
            int8_sha256: [2; 32],
        };
        let r = build_report(&f, Some(c), &DegradationThresholds::default());
        assert!(!r.withheld, "INT8 within thresholds must not be WITHHELD");
        assert_eq!(r.selected, Quant::Int8);
    }

    #[test]
    fn detect_runs_without_panic_on_host_cpu() {
        // Smoke test: detect() runs on the host CPU and returns a non-empty
        // arch label. We do not assert specific features — the reference CPU
        // (i9-13900H) has AVX2+AVX-VNNI, but CI runners vary.
        let f = CpuFeatures::detect();
        assert!(!f.arch.is_empty(), "arch label must not be empty");
    }
}
