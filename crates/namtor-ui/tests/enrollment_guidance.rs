// SPDX-License-Identifier: GPL-3.0-only
//! Behavioural tests for the scan screen: the guidance the daemon sends has
//! to reach the user, drive the UI *typed* rather than by matching translated
//! text, and be accompanied by an honest breakdown of the counters.
//!
//! Every assertion renders the real widget tree and reads the text out of it
//! — no source scanning.

use gtk4::prelude::*;
use namtor_ui::client::Guidance;
use namtor_ui::components::{EnrollmentViewComponent, ReviewViewComponent};
use namtor_ui::view_model::{CaptureSignal, EnrollmentViewV3, GuidanceView, ReviewViewV3};

mod common;

fn capture_view(seen: u32, accepted: u32, rejected: u32, guidance: &str) -> EnrollmentViewV3 {
    EnrollmentViewV3 {
        phase: "capturing".into(),
        quality_percent: 41,
        diversity_percent: 12,
        frames_seen: seen,
        frames_accepted: accepted,
        frames_rejected: rejected,
        quality_bucket: 0,
        guidance: guidance.into(),
    }
}

fn render_capture_text(view: &EnrollmentViewV3) -> Vec<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let view = view.clone();
    common::run_gtk_test(move || {
        let mut comp = EnrollmentViewComponent::new();
        let page = comp.render(Some(&view), |_| {}, || {}, || {});
        let _ = tx.send(common::collect_widget_text(
            page.upcast_ref::<gtk4::Widget>(),
        ));
    });
    rx.recv().expect("capture render produced text")
}

fn render_review_text(view: &ReviewViewV3) -> Vec<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let view = view.clone();
    common::run_gtk_test(move || {
        let mut comp = ReviewViewComponent::new();
        let page = comp.render(&view, || {}, || {}, || {}, false);
        let _ = tx.send(common::collect_widget_text(
            page.upcast_ref::<gtk4::Widget>(),
        ));
    });
    rx.recv().expect("review render produced text")
}

// ---------------------------------------------------------------------------
// Locale safety
// ---------------------------------------------------------------------------

/// The "good position" decision must come from typed data, not from matching
/// an already-translated label. Verified behaviourally: a view whose label
/// text says "looking good" but whose *typed* guidance is `MoveCloser` must
/// not be treated as a good position.
#[test]
fn good_position_is_not_decided_by_matching_translated_text() {
    // A daemon token the client does not know, whose text happens to contain
    // the English phrase. Any `contains("looking good")` check would call
    // this a good position.
    let impostor = capture_view(10, 4, 6, "Looking good in some other locale");
    assert_eq!(
        impostor.guidance,
        GuidanceView::Raw("Looking good in some other locale".into()),
        "an unknown token stays Raw and drives no decision"
    );
    assert!(
        !impostor.guidance.is_encouraging(),
        "text that merely reads 'looking good' must not be taken as the \
         LookingGood signal"
    );

    let real = capture_view(10, 4, 6, "looking_good");
    assert_eq!(real.guidance, GuidanceView::Known(Guidance::LookingGood));
    assert!(
        real.guidance.is_encouraging(),
        "the typed LookingGood token is the only thing that means good"
    );
}

/// Every one of the nine guidance variants survives the wire round-trip into
/// a typed view-model value with a non-empty translated label.
#[test]
fn all_nine_guidance_variants_render() {
    let variants = [
        Guidance::Centering,
        Guidance::BetterLighting,
        Guidance::MoveCloser,
        Guidance::MoveBack,
        Guidance::HoldStill,
        Guidance::FaceFront,
        Guidance::InsufficientPose,
        Guidance::LookingGood,
        Guidance::Capturing,
    ];
    for variant in variants {
        let view = capture_view(10, 4, 6, variant.as_str());
        assert_eq!(
            view.guidance,
            GuidanceView::Known(variant),
            "{} must parse to its typed variant",
            variant.as_str()
        );
        assert!(
            !view.guidance.label().is_empty(),
            "{} must have a label",
            variant.as_str()
        );
        assert!(
            view.guidance.hint().is_some(),
            "{} must have a how-to line under the headline",
            variant.as_str()
        );
    }
}

/// The guidance headline is rendered on the capture page.
#[test]
fn capture_screen_renders_guidance() {
    let texts = render_capture_text(&capture_view(120, 30, 90, "move_closer"));
    assert!(
        texts.iter().any(|t| t.contains("Move closer")),
        "guidance dropped: {texts:?}"
    );
}

// ---------------------------------------------------------------------------
// Review screen
// ---------------------------------------------------------------------------

/// The review screen used to discard `view.guidance` entirely.
#[test]
fn review_screen_renders_guidance() {
    let view = ReviewViewV3 {
        ready_to_save: false,
        quality_percent: 41,
        diversity_percent: 12,
        frames_accepted: 30,
        guidance: "move_closer".into(),
    };
    let texts = render_review_text(&view);
    assert!(
        texts.iter().any(|t| t.contains("Move closer")),
        "guidance dropped: {texts:?}"
    );
}

/// The review screen says plainly whether the scan can be saved.
#[test]
fn review_screen_states_readiness() {
    let not_ready = ReviewViewV3 {
        ready_to_save: false,
        quality_percent: 41,
        diversity_percent: 12,
        frames_accepted: 30,
        guidance: "insufficient_pose".into(),
    };
    let texts = render_review_text(&not_ready);
    assert!(
        texts.iter().any(|t| t.contains("Not ready to save yet")),
        "a not-ready review must say so: {texts:?}"
    );

    let ready = ReviewViewV3 {
        ready_to_save: true,
        guidance: "looking_good".into(),
        ..not_ready
    };
    let texts = render_review_text(&ready);
    assert!(
        texts.iter().any(|t| t == "Ready to save"),
        "a ready review must say so: {texts:?}"
    );
}

// ---------------------------------------------------------------------------
// Counters
// ---------------------------------------------------------------------------

/// Accepted, seen and rejected are three different numbers and get three
/// distinct, honestly-labelled rows. The old UI showed `frames_accepted`
/// under a label reading "Frames:".
#[test]
fn capture_screen_shows_accepted_seen_and_rejected_distinctly() {
    let texts = render_capture_text(&capture_view(120, 30, 90, "move_closer"));
    for label in ["Accepted", "Seen", "Rejected"] {
        assert!(
            texts.iter().any(|t| t == label),
            "missing row labelled {label}: {texts:?}"
        );
    }
    for value in ["30", "120", "90"] {
        assert!(
            texts.iter().any(|t| t == value),
            "missing counter value {value}: {texts:?}"
        );
    }
}

/// The share of captures kept is derived from two wire counters, so it is a
/// real second reading rather than a restatement of `quality_percent`.
#[test]
fn kept_rate_is_derived_from_the_counters() {
    let view = capture_view(120, 30, 90, "move_closer");
    assert_eq!(view.accept_rate_percent(), Some(25));

    let fresh = capture_view(0, 0, 0, "capturing");
    assert_eq!(
        fresh.accept_rate_percent(),
        None,
        "nothing seen yet must not read as 0%"
    );

    let texts = render_capture_text(&view);
    assert!(
        texts.iter().any(|t| t == "25%"),
        "kept share missing: {texts:?}"
    );
}

// ---------------------------------------------------------------------------
// Per-signal breakdown
// ---------------------------------------------------------------------------

/// The guidance token is the daemon's own statement of which signal it is
/// waiting on, so the UI can name it instead of showing one opaque
/// percentage.
#[test]
fn guidance_attributes_the_limiting_signal() {
    for (token, expected) in [
        ("centering", Some(CaptureSignal::Framing)),
        ("move_closer", Some(CaptureSignal::Framing)),
        ("move_back", Some(CaptureSignal::Framing)),
        ("face_front", Some(CaptureSignal::Framing)),
        ("better_lighting", Some(CaptureSignal::Lighting)),
        ("hold_still", Some(CaptureSignal::Steadiness)),
        ("insufficient_pose", Some(CaptureSignal::HeadAngles)),
        ("looking_good", None),
        ("capturing", None),
    ] {
        assert_eq!(
            GuidanceView::from_wire(token).signal(),
            expected,
            "wrong signal attributed to {token}"
        );
    }
}

/// The capture page shows the three measured signals and names the limiting
/// one, so a low quality reading is explainable.
#[test]
fn capture_screen_breaks_quality_down_by_signal() {
    let texts = render_capture_text(&capture_view(120, 30, 90, "better_lighting"));
    for row in [
        "Image quality",
        "Angle variety",
        "Views kept",
        "Needs attention",
    ] {
        assert!(
            texts.iter().any(|t| t == row),
            "missing breakdown row {row}: {texts:?}"
        );
    }
    assert!(
        texts.iter().any(|t| t == "Lighting"),
        "the limiting signal must be named: {texts:?}"
    );

    let good = EnrollmentViewV3 {
        quality_bucket: 2,
        guidance: "looking_good".into(),
        ..capture_view(120, 110, 10, "looking_good")
    };
    let texts = render_capture_text(&good);
    assert!(
        texts.iter().any(|t| t == "Nothing"),
        "a healthy scan must say nothing needs attention: {texts:?}"
    );
}

// ---------------------------------------------------------------------------
// Privacy
// ---------------------------------------------------------------------------

/// Nothing the scan screens render may carry a forbidden biometric or
/// matcher term.
#[test]
fn no_forbidden_privacy_substring_reaches_the_ui() {
    let mut rendered = Vec::new();
    for token in [
        "centering",
        "better_lighting",
        "move_closer",
        "move_back",
        "hold_still",
        "face_front",
        "insufficient_pose",
        "looking_good",
        "capturing",
        "",
    ] {
        rendered.extend(render_capture_text(&capture_view(120, 30, 90, token)));
        rendered.extend(render_review_text(&ReviewViewV3 {
            ready_to_save: false,
            quality_percent: 41,
            diversity_percent: 12,
            frames_accepted: 30,
            guidance: token.into(),
        }));
    }
    for s in rendered {
        let hits = namtor_ui::privacy::forbidden_words_in_ui_string(&s);
        assert!(hits.is_empty(), "leak in {s:?}: {hits:?}");
    }
}
