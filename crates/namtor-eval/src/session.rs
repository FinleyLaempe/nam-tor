// SPDX-License-Identifier: GPL-3.0-only
//! Recording and scoring a live error-rate measurement session.
//!
//! The crate already had the statistics — Wilson bounds, the rule-of-three FAR
//! bound, the G3 report contract — and nothing that could actually conduct a
//! measurement. Gates G3 and G4 were therefore blocked twice over: on inputs
//! nobody had (a licensed cohort, physical attack artefacts) *and* on there
//! being no way to run a session even once those inputs existed.
//!
//! This is the second half. It records one attempt at a time and turns a run
//! into a report with an N and a confidence interval, which is what G3.2 and
//! G4.2 ask for. It does not invent the inputs; it means that the moment a
//! subject or an attack artefact is in front of the camera, measuring is a
//! command rather than a project.
//!
//! What it deliberately does not do: average across attack types. G4.2
//! requires a rate **per** attack type, because a system that resists print
//! but falls to a screen replay is not "half safe".

use serde::{Deserialize, Serialize};

use crate::protocol::{far_upper_bound_95, wilson_upper_95};

/// What one presentation to the camera was, and how it was answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    /// Monotonic index within the run, starting at 1.
    pub index: u32,
    /// Seconds since the run started, for spotting a drifting session.
    pub at_s: u64,
    /// What was presented.
    pub kind: AttemptKind,
    /// What the system answered.
    pub outcome: Outcome,
    /// End-to-end latency of the attempt.
    pub latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptKind {
    /// The enrolled subject presenting their own face.
    Genuine,
    /// A different live person presenting their face.
    ZeroEffortImpostor,
    /// A presentation attack, named. Never merged with another name.
    Attack { instrument: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Authenticated.
    Accepted,
    /// Rejected by matching or by PAD — deliberately not distinguished here,
    /// because the rates below count what the *system* did, not why.
    Rejected,
    /// Neither: no face, camera error, timeout. Excluded from every rate, and
    /// reported separately so a run that mostly failed to capture cannot look
    /// like a clean result.
    Inconclusive,
}

/// One measurement run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Free-text description of the conditions: lighting, distance, glasses.
    pub conditions: String,
    pub attempts: Vec<Attempt>,
}

/// A rate with the N it came from and its 95% upper bound.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    pub events: u64,
    pub trials: u64,
    pub point: f64,
    pub upper_95: f64,
}

impl Rate {
    fn new(events: u64, trials: u64) -> Self {
        Self {
            events,
            trials,
            point: if trials == 0 {
                f64::NAN
            } else {
                events as f64 / trials as f64
            },
            // For zero observed events the rule-of-three bound is the one the
            // FAR gate is held to and is tighter to state honestly than a
            // Wilson interval that collapses to a point.
            upper_95: if events == 0 {
                far_upper_bound_95(trials)
            } else {
                wilson_upper_95(events, trials)
            },
        }
    }
}

/// The result of scoring a [`Session`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReport {
    pub conditions: String,
    /// False reject rate: genuine presentations that were not accepted.
    pub frr: Rate,
    /// Zero-effort impostor acceptance, when any were presented.
    pub zero_effort_far: Option<Rate>,
    /// Attack presentation classification error rate, one per instrument.
    /// Never averaged across instruments — G4.2.
    pub apcer_by_instrument: Vec<(String, Rate)>,
    /// Attempts excluded from every rate.
    pub inconclusive: u64,
    /// Median and p95 latency over conclusive attempts.
    pub latency_median_ms: u64,
    pub latency_p95_ms: u64,
}

/// Score a run into rates with confidence intervals.
pub fn score_session(session: &Session) -> SessionReport {
    let mut genuine = (0u64, 0u64); // (rejects, trials)
    let mut impostor = (0u64, 0u64); // (accepts, trials)
                                     // A map, so adding an instrument needs no fallible lookup: gate G5.6
                                     // forbids `.expect()` outside tests, and a `last_mut().expect(...)` after
                                     // a push is exactly the kind of "cannot fail" that the gate exists to
                                     // keep out of shipped code.
    let mut by_instrument: std::collections::BTreeMap<String, (u64, u64)> =
        std::collections::BTreeMap::new();
    let mut inconclusive = 0u64;
    let mut latencies: Vec<u64> = Vec::new();

    for a in &session.attempts {
        if a.outcome == Outcome::Inconclusive {
            inconclusive += 1;
            continue;
        }
        latencies.push(a.latency_ms);
        match &a.kind {
            AttemptKind::Genuine => {
                genuine.1 += 1;
                if a.outcome == Outcome::Rejected {
                    genuine.0 += 1;
                }
            }
            AttemptKind::ZeroEffortImpostor => {
                impostor.1 += 1;
                if a.outcome == Outcome::Accepted {
                    impostor.0 += 1;
                }
            }
            AttemptKind::Attack { instrument } => {
                let slot = by_instrument.entry(instrument.clone()).or_insert((0, 0));
                slot.1 += 1;
                if a.outcome == Outcome::Accepted {
                    slot.0 += 1;
                }
            }
        }
    }

    latencies.sort_unstable();
    let pick = |q: f64| -> u64 {
        if latencies.is_empty() {
            return 0;
        }
        let idx = ((latencies.len() as f64 - 1.0) * q).round() as usize;
        latencies[idx.min(latencies.len() - 1)]
    };

    SessionReport {
        conditions: session.conditions.clone(),
        frr: Rate::new(genuine.0, genuine.1),
        zero_effort_far: (impostor.1 > 0).then(|| Rate::new(impostor.0, impostor.1)),
        apcer_by_instrument: by_instrument
            .into_iter()
            .map(|(k, (e, t))| (k, Rate::new(e, t)))
            .collect(),
        inconclusive,
        latency_median_ms: pick(0.5),
        latency_p95_ms: pick(0.95),
    }
}

/// Render a report as the lines a human should read before believing it.
pub fn summarise(report: &SessionReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("conditions: {}\n", report.conditions));
    out.push_str(&format!(
        "FRR: {}/{} = {:.4} (95% upper {:.4})\n",
        report.frr.events, report.frr.trials, report.frr.point, report.frr.upper_95
    ));
    if let Some(far) = report.zero_effort_far {
        out.push_str(&format!(
            "zero-effort FAR: {}/{} = {:.6} (95% upper {:.6})\n",
            far.events, far.trials, far.point, far.upper_95
        ));
    }
    for (instrument, rate) in &report.apcer_by_instrument {
        out.push_str(&format!(
            "APCER[{instrument}]: {}/{} = {:.4} (95% upper {:.4})\n",
            rate.events, rate.trials, rate.point, rate.upper_95
        ));
    }
    out.push_str(&format!(
        "inconclusive (excluded): {}\nlatency median {} ms, p95 {} ms\n",
        report.inconclusive, report.latency_median_ms, report.latency_p95_ms
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(i: u32, kind: AttemptKind, outcome: Outcome) -> Attempt {
        Attempt {
            index: i,
            at_s: i as u64,
            kind,
            outcome,
            latency_ms: 100 + i as u64,
        }
    }

    #[test]
    fn frr_counts_genuine_rejections_only() {
        let s = Session {
            conditions: "desk lamp".into(),
            attempts: vec![
                attempt(1, AttemptKind::Genuine, Outcome::Accepted),
                attempt(2, AttemptKind::Genuine, Outcome::Rejected),
                attempt(3, AttemptKind::Genuine, Outcome::Accepted),
                attempt(4, AttemptKind::Genuine, Outcome::Accepted),
                // An attack rejection must not improve the FRR.
                attempt(
                    5,
                    AttemptKind::Attack {
                        instrument: "print".into(),
                    },
                    Outcome::Rejected,
                ),
            ],
        };
        let r = score_session(&s);
        assert_eq!((r.frr.events, r.frr.trials), (1, 4));
        assert!((r.frr.point - 0.25).abs() < 1e-9);
        assert!(
            r.frr.upper_95 > 0.25,
            "an upper bound must exceed the point"
        );
    }

    /// G4.2: a rate per instrument, never averaged. A system that resists
    /// print but falls to a screen is not "half safe", and an averaged number
    /// would say exactly that.
    #[test]
    fn apcer_is_reported_per_instrument_and_never_averaged() {
        let mut attempts = Vec::new();
        for i in 0..10 {
            attempts.push(attempt(
                i,
                AttemptKind::Attack {
                    instrument: "print".into(),
                },
                Outcome::Rejected,
            ));
        }
        for i in 10..20 {
            attempts.push(attempt(
                i,
                AttemptKind::Attack {
                    instrument: "screen_replay".into(),
                },
                Outcome::Accepted,
            ));
        }
        let r = score_session(&Session {
            conditions: "bench".into(),
            attempts,
        });
        assert_eq!(r.apcer_by_instrument.len(), 2);
        let print = &r.apcer_by_instrument[0];
        let screen = &r.apcer_by_instrument[1];
        assert_eq!(print.0, "print");
        assert_eq!(print.1.events, 0);
        assert_eq!(screen.0, "screen_replay");
        assert_eq!(screen.1.events, 10);
        assert!(
            (screen.1.point - 1.0).abs() < 1e-9,
            "every screen attack won"
        );
        // The dangerous reading this shape exists to prevent.
        assert!(
            (print.1.point - screen.1.point).abs() > 0.5,
            "the two instruments must stay distinguishable"
        );
    }

    #[test]
    fn inconclusive_attempts_are_excluded_and_reported() {
        let s = Session {
            conditions: "dark".into(),
            attempts: vec![
                attempt(1, AttemptKind::Genuine, Outcome::Inconclusive),
                attempt(2, AttemptKind::Genuine, Outcome::Inconclusive),
                attempt(3, AttemptKind::Genuine, Outcome::Accepted),
            ],
        };
        let r = score_session(&s);
        assert_eq!(r.inconclusive, 2);
        assert_eq!(
            r.frr.trials, 1,
            "a run that mostly failed to capture must not look like a clean result"
        );
    }

    /// Zero observed events uses the rule-of-three bound, so "we saw none" is
    /// never reported as a rate of zero with no uncertainty.
    #[test]
    fn zero_events_still_carries_an_upper_bound() {
        let attempts = (0..100)
            .map(|i| attempt(i, AttemptKind::ZeroEffortImpostor, Outcome::Rejected))
            .collect();
        let r = score_session(&Session {
            conditions: "bench".into(),
            attempts,
        });
        let far = r.zero_effort_far.expect("impostors were presented");
        assert_eq!(far.events, 0);
        assert!((far.point - 0.0).abs() < 1e-12);
        assert!(
            (far.upper_95 - 0.03).abs() < 1e-9,
            "3/100 rule-of-three bound, not 0: {}",
            far.upper_95
        );
    }

    #[test]
    fn no_impostors_means_no_far_rather_than_a_fabricated_zero() {
        let r = score_session(&Session {
            conditions: "bench".into(),
            attempts: vec![attempt(1, AttemptKind::Genuine, Outcome::Accepted)],
        });
        assert!(
            r.zero_effort_far.is_none(),
            "a FAR must not be invented from a run that presented no impostors"
        );
    }

    #[test]
    fn latency_percentiles_cover_conclusive_attempts() {
        let attempts = (1..=20)
            .map(|i| attempt(i, AttemptKind::Genuine, Outcome::Accepted))
            .collect();
        let r = score_session(&Session {
            conditions: "bench".into(),
            attempts,
        });
        assert!(r.latency_median_ms >= 101 && r.latency_median_ms <= 120);
        assert!(r.latency_p95_ms >= r.latency_median_ms);
    }

    #[test]
    fn a_report_round_trips_as_json() {
        let r = score_session(&Session {
            conditions: "bench".into(),
            attempts: vec![attempt(1, AttemptKind::Genuine, Outcome::Accepted)],
        });
        let back: SessionReport =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn the_summary_names_every_instrument() {
        let r = score_session(&Session {
            conditions: "bench".into(),
            attempts: vec![
                attempt(
                    1,
                    AttemptKind::Attack {
                        instrument: "print".into(),
                    },
                    Outcome::Rejected,
                ),
                attempt(
                    2,
                    AttemptKind::Attack {
                        instrument: "mask".into(),
                    },
                    Outcome::Accepted,
                ),
            ],
        });
        let text = summarise(&r);
        assert!(text.contains("APCER[print]"), "{text}");
        assert!(text.contains("APCER[mask]"), "{text}");
    }
}
