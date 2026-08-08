// SPDX-License-Identifier: GPL-3.0-only
//! Physics-based synthetic strobe differentials for analytic PAD calibration.
//!
//! When no real consented attack corpus is available (the owner-gated case),
//! calibration thresholds can be set from synthetic `D = On − Off` images that
//! encode the same physics signals the four analytic gates check (spec §6.4):
//!
//! - **Live** — strong positive `D`, 1/r² radial falloff, specular hotspot,
//!   subsurface scattering (smooth low-frequency gradient field).
//! - **Screen replay** — near-zero `D` (LCD/OLED panels emit ~nothing at 850 nm).
//! - **Print** — weak, flat `D` (IR-transparent inks), no subsurface scattering.
//! - **Cutout mask** — uniform `D`, no radial falloff.
//!
//! This is the **same physics model** used by `ml/recipes/train_pad.py` for
//! MiniFASNet training data, ported to Rust so the calibration can run in-process
//! without a Python dependency. The generator is deterministic (seeded
//! splitmix64) — no `rand` crate, no system randomness — so a calibration run is
//! reproducible.
//!
//! **G4.1 is WITHHELD** when calibrating from synthetic data: synthetic scores
//! lock the calibration arithmetic and produce data-driven thresholds, but they
//! are not measured APCER/BPCER on a real device. The [`Provenance`] struct on
//! the resulting [`CalibrationReport`](crate::calibration::CalibrationReport)
//! records `synthetic: true` so no consumer can mistake the output for a
//! real-data gate pass.

use crate::calibration::{Provenance, ScoreCohort, calibrate_from_scores};

/// Default synthetic image edge length (matches MiniFASNet 80×80 input).
pub const SYNTH_SIZE: u32 = 80;

/// Deterministic splitmix64 PRNG — no external dependency, reproducible.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform f32 in `[0, 1)`.
    fn uniform(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform f32 in `[lo, hi)`.
    fn uniform_range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.uniform() * (hi - lo)
    }

    /// Standard-normal-ish via Box-Muller (sufficient for synthetic data).
    fn normal(&mut self, mean: f32, std: f32) -> f32 {
        // Two uniforms → one normal via Box-Muller.
        let u1 = self.uniform().max(1e-10);
        let u2 = self.uniform();
        let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f32::consts::PI * u2).cos();
        mean + std * z
    }
}

/// A synthetic strobe differential sample: the `D` image plus the `on_mean`
/// (mean of the illuminated On frame) used to normalise the energy gate.
#[derive(Debug, Clone)]
pub struct SyntheticSample {
    pub d: Vec<f32>,
    pub w: u32,
    pub h: u32,
    pub on_mean: f32,
}

impl SyntheticSample {
    /// Generate a **live** face strobe differential: 1/r² falloff + specular
    /// hotspot + subsurface scattering + noise. The On frame is the Off frame
    /// (ambient ~40) plus `D`; `on_mean` is derived from the clamped On frame.
    pub fn live(size: u32, rng: &mut Rng) -> Self {
        let s = size as usize;
        let cx = size as f32 / 2.0 + rng.normal(0.0, 3.0);
        let cy = size as f32 / 2.0 + rng.normal(0.0, 3.0);
        let a = rng.uniform_range(3000.0, 8000.0);
        let b = rng.uniform_range(3.0, 8.0);

        let mut d = vec![0.0f32; s * s];
        for y in 0..s {
            for x in 0..s {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let r = (dx * dx + dy * dy).sqrt();
                d[y * s + x] = a / (r + b).powi(2);
            }
        }

        // Specular hotspot near centroid.
        let spx = cx + rng.normal(0.0, 5.0);
        let spy = cy + rng.normal(0.0, 5.0);
        let sp_amp = rng.uniform_range(50.0, 150.0);
        let sp_sigma = rng.uniform_range(2.0, 5.0);
        for y in 0..s {
            for x in 0..s {
                let dx = x as f32 - spx;
                let dy = y as f32 - spy;
                let r2 = dx * dx + dy * dy;
                d[y * s + x] += sp_amp * (-r2 / (2.0 * sp_sigma * sp_sigma)).exp();
            }
        }

        // Subsurface scattering: smooth low-frequency modulation.
        let ss_amp = rng.uniform_range(-10.0, 10.0);
        for y in 0..s {
            for x in 0..s {
                d[y * s + x] += ss_amp * (x as f32 * 0.1).sin() * (y as f32 * 0.1).cos();
            }
        }

        // Noise.
        for v in d.iter_mut() {
            *v += rng.normal(0.0, 3.0);
        }

        clamp_d(&mut d);
        Self::from_d(d, size, size, 40.0)
    }

    /// Generate a **screen replay** differential: near-zero `D` (panels emit
    /// ~nothing at 850 nm). Just noise around zero.
    pub fn screen_replay(size: u32, rng: &mut Rng) -> Self {
        let s = size as usize;
        let mut d = vec![0.0f32; s * s];
        for v in d.iter_mut() {
            *v = rng.normal(0.0, 2.0);
        }
        // Screen replay D clamps to [0, 255] but is centred at 0 → most pixels
        // become 0 after clamp. This is the intended physics: near-zero D.
        clamp_d(&mut d);
        Self::from_d(d, size, size, 40.0)
    }

    /// Generate a **print** differential: weak, flat `D` (IR-transparent inks),
    /// no subsurface scattering. Uniform base + noise.
    pub fn print(size: u32, rng: &mut Rng) -> Self {
        let s = size as usize;
        let base = rng.uniform_range(2.0, 15.0);
        let mut d = vec![base; s * s];
        for v in d.iter_mut() {
            *v += rng.normal(0.0, 5.0);
        }
        clamp_d(&mut d);
        Self::from_d(d, size, size, 40.0)
    }

    /// Generate a **cutout mask** differential: uniform illumination, no radial
    /// falloff. Slight lens vignetting only.
    pub fn cutout(size: u32, rng: &mut Rng) -> Self {
        let s = size as usize;
        let base = rng.uniform_range(20.0, 50.0);
        let mut d = vec![base; s * s];
        let cx = size as f32 / 2.0;
        let cy = size as f32 / 2.0;
        for y in 0..s {
            for x in 0..s {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let r = (dx * dx + dy * dy).sqrt();
                let i = y * s + x;
                d[i] *= 1.0 - 0.1 * (r / (size as f32 / 2.0)).powi(2);
            }
        }
        for v in d.iter_mut() {
            *v += rng.normal(0.0, 4.0);
        }
        clamp_d(&mut d);
        Self::from_d(d, size, size, 40.0)
    }

    /// Build a `SyntheticSample` from a pre-computed `D`, simulating an On
    /// frame with ambient level `ambient` so `on_mean` is realistic.
    fn from_d(d: Vec<f32>, w: u32, h: u32, ambient: f32) -> Self {
        // On = clamp(ambient + D, 0, 255); on_mean = mean(On).
        let n = d.len();
        let mut on_sum = 0.0f32;
        for &dv in &d {
            on_sum += (ambient + dv).clamp(0.0, 255.0);
        }
        let on_mean = on_sum / n as f32;
        Self { d, w, h, on_mean }
    }
}

fn clamp_d(d: &mut [f32]) {
    for v in d.iter_mut() {
        *v = v.clamp(0.0, 255.0);
    }
}

/// Provenance for synthetic-only calibration. Records that the data is
/// synthetic and consent does not apply (no human subjects).
pub fn synthetic_provenance() -> Provenance {
    Provenance {
        dataset: "physics-based-synthetic-strobe-differential".into(),
        licence: "self-generated, no upstream licence constraints".into(),
        consent: false,
        synthetic: true,
    }
}

/// Run the four analytic gates on a synthetic sample and collect the
/// normalised `[0, 1]` scores (one per gate). Returns `None` for a gate that
/// errors (should not happen on well-formed synthetic data, but fail-closed).
fn gate_scores(sample: &SyntheticSample) -> Option<[f32; 4]> {
    let cfg = crate::PadConfig::default();
    let verdict = crate::run_analytic_pad(
        &sample.d,
        sample.w,
        sample.h,
        sample.on_mean,
        None,
        &cfg,
    )
    .ok()?;
    Some([
        verdict.energy.score,
        verdict.gradient.score,
        verdict.specular.score,
        verdict.radial.score,
    ])
}

/// Generate synthetic score cohorts and calibrate the four gates.
///
/// Produces `n_live` live samples and `n_attack` samples per attack class
/// (print, screen-replay, cutout). IR-print is left empty (WITHHELD) because
/// the synthetic model does not distinguish IR-print from visible-light print.
/// Each sample is generated from a deterministic RNG stream keyed by
/// `(seed, class, index)` so the calibration is reproducible.
///
/// Returns a [`CalibrationReport`](crate::calibration::CalibrationReport) with
/// `synthetic: true` provenance. G4.1 remains WITHHELD — synthetic calibration
/// produces data-driven thresholds but is not a measured real-device gate pass.
pub fn calibrate_synthetic(seed: u64, n_live: usize, n_attack: usize) -> crate::calibration::CalibrationReport {
    let mut live = ScoreCohort::empty();
    let mut print = ScoreCohort::empty();
    let mut screen = ScoreCohort::empty();
    let mut cutout = ScoreCohort::empty();

    for i in 0..n_live {
        let mut rng = Rng::new(seed.wrapping_mul(2).wrapping_add(i as u64));
        let s = SyntheticSample::live(SYNTH_SIZE, &mut rng);
        if let Some([e, g, sp, r]) = gate_scores(&s) {
            live.energy.push(e);
            live.gradient.push(g);
            live.specular.push(sp);
            live.radial.push(r);
        }
    }

    for (class_tag, cohort) in [
        (3u64, &mut print),
        (1u64, &mut screen),
        (4u64, &mut cutout),
    ] {
        for i in 0..n_attack {
            let mut rng = Rng::new(seed.wrapping_mul(2).wrapping_add(class_tag * 1000).wrapping_add(i as u64));
            let s = match class_tag {
                3 => SyntheticSample::print(SYNTH_SIZE, &mut rng),
                1 => SyntheticSample::screen_replay(SYNTH_SIZE, &mut rng),
                4 => SyntheticSample::cutout(SYNTH_SIZE, &mut rng),
                _ => continue,
            };
            if let Some([e, g, sp, r]) = gate_scores(&s) {
                cohort.energy.push(e);
                cohort.gradient.push(g);
                cohort.specular.push(sp);
                cohort.radial.push(r);
            }
        }
    }

    calibrate_from_scores(
        &live,
        &print,
        &ScoreCohort::empty(), // IR-print WITHHELD
        &screen,
        &cutout,
        synthetic_provenance(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::{AttackClass, ApcerResult, GateId};

    #[test]
    fn live_sample_has_strong_positive_d() {
        let mut rng = Rng::new(42);
        let s = SyntheticSample::live(SYNTH_SIZE, &mut rng);
        let mean_d: f32 = s.d.iter().copied().sum::<f32>() / s.d.len() as f32;
        assert!(mean_d > 20.0, "live D mean should be strongly positive: {mean_d}");
        assert!(s.on_mean > 50.0, "on_mean should be realistic: {}", s.on_mean);
    }

    #[test]
    fn screen_replay_sample_has_near_zero_d() {
        let mut rng = Rng::new(42);
        let s = SyntheticSample::screen_replay(SYNTH_SIZE, &mut rng);
        let mean_d: f32 = s.d.iter().copied().sum::<f32>() / s.d.len() as f32;
        assert!(mean_d < 5.0, "screen replay D mean should be near zero: {mean_d}");
    }

    #[test]
    fn print_sample_has_weak_flat_d() {
        let mut rng = Rng::new(42);
        let s = SyntheticSample::print(SYNTH_SIZE, &mut rng);
        let mean_d: f32 = s.d.iter().copied().sum::<f32>() / s.d.len() as f32;
        assert!(mean_d < 25.0, "print D mean should be weak: {mean_d}");
        // Flat: low variance.
        let var: f32 = s.d.iter().map(|&v| (v - mean_d).powi(2)).sum::<f32>() / s.d.len() as f32;
        assert!(var < 50.0, "print D should be flat (low variance): var={var}");
    }

    #[test]
    fn cutout_sample_is_uniform() {
        let mut rng = Rng::new(42);
        let s = SyntheticSample::cutout(SYNTH_SIZE, &mut rng);
        let mean_d: f32 = s.d.iter().copied().sum::<f32>() / s.d.len() as f32;
        assert!(mean_d > 15.0, "cutout D should be non-trivial: {mean_d}");
        // Uniform: low variance (no radial falloff).
        let var: f32 = s.d.iter().map(|&v| (v - mean_d).powi(2)).sum::<f32>() / s.d.len() as f32;
        assert!(var < 80.0, "cutout D should be uniform: var={var}");
    }

    #[test]
    fn calibration_is_deterministic_for_same_seed() {
        let r1 = calibrate_synthetic(42, 50, 20);
        let r2 = calibrate_synthetic(42, 50, 20);
        for i in 0..4 {
            assert_eq!(r1.thresholds[i].threshold, r2.thresholds[i].threshold);
        }
    }

    #[test]
    fn synthetic_calibration_produces_non_withheld_thresholds() {
        let report = calibrate_synthetic(42, 80, 30);
        for t in &report.thresholds {
            assert!(t.n_live > 0, "gate {:?} should have live samples", t.gate);
            assert!(t.threshold > 0.0, "gate {:?} threshold should be set", t.gate);
            assert!(t.bpcer <= 0.05 + 1e-6, "BPCER should be ≤ 5%");
        }
    }

    #[test]
    fn synthetic_calibration_live_scores_above_threshold() {
        // The live cohort used for calibration should mostly pass at the
        // calibrated threshold (BPCER ≤ 5% by construction).
        let report = calibrate_synthetic(42, 80, 30);
        for (gi, gate) in GateId::ALL.iter().enumerate() {
            let t = report.thresholds[gi].threshold;
            let bpcer = report.thresholds[gi].bpcer;
            // BPCER ≤ 5% means ≥ 95% of live samples pass.
            assert!(bpcer <= 0.05 + 1e-6, "gate {:?} BPCER {bpcer} exceeds 5%", gate);
            let _ = t;
        }
    }

    #[test]
    fn synthetic_calibration_attack_apcer_is_low() {
        // Screen replay should be rejected by the energy gate (near-zero D).
        let report = calibrate_synthetic(42, 80, 30);
        let energy_screen = &report.apcer[0][2]; // gate 0 (energy), class 2 (screen_replay)
        match energy_screen {
            ApcerResult::Evaluated { apcer, n, .. } => {
                assert!(*n > 0, "screen replay should have samples");
                assert!(
                    *apcer <= 0.1,
                    "screen replay APCER on energy gate should be ≤ 10%: {apcer}"
                );
            }
            ApcerResult::Withheld => panic!("screen replay should be evaluated"),
        }
    }

    #[test]
    fn ir_print_is_withheld_in_synthetic_calibration() {
        let report = calibrate_synthetic(42, 80, 30);
        for gi in 0..4 {
            assert!(
                matches!(report.apcer[gi][1], ApcerResult::Withheld),
                "IR-print should be WITHHELD for gate {gi}"
            );
        }
    }

    #[test]
    fn provenance_records_synthetic() {
        let report = calibrate_synthetic(42, 50, 20);
        assert!(report.provenance.synthetic);
        assert!(!report.provenance.consent);
    }

    #[test]
    fn rng_uniform_is_in_range() {
        let mut rng = Rng::new(1);
        for _ in 0..1000 {
            let v = rng.uniform();
            assert!((0.0..1.0).contains(&v), "uniform out of range: {v}");
        }
    }

    #[test]
    fn rng_normal_has_reasonable_mean() {
        let mut rng = Rng::new(1);
        let samples: Vec<f32> = (0..10_000).map(|_| rng.normal(5.0, 2.0)).collect();
        let mean = samples.iter().sum::<f32>() / samples.len() as f32;
        assert!((mean - 5.0).abs() < 0.2, "normal mean drift: {mean}");
    }

    #[test]
    fn synthetic_sample_dimensions_match() {
        let mut rng = Rng::new(42);
        let s = SyntheticSample::live(64, &mut rng);
        assert_eq!(s.w, 64);
        assert_eq!(s.h, 64);
        assert_eq!(s.d.len(), 64 * 64);
    }
}
