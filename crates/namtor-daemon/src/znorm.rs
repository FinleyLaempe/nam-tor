// SPDX-License-Identifier: GPL-3.0-only
//! Score normalisation — re-export from `namtor-recog`.
//!
//! The z-norm implementation (cohort sources, calibrator, score normalisation
//! functions) lives in `namtor_recog::znorm` because score normalisation is a
//! recognition concern, not a daemon concern. This module re-exports it so
//! the daemon's `crate::znorm` path keeps resolving for any caller that wants
//! to construct a calibrator at daemon startup. See
//! `docs/evidence/trust/znorm.md` for the cohort gate status.

pub use namtor_recog::znorm::{
    normalize_score, normalize_score_scalar, t_norm, CohortSource, DirCohort, FileCohort,
    Provenance, SyntheticCohort, ZNormCalibration, ZNormCalibrator, ZNormError,
};
