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
        let page = comp.render(Some(&view), true, |_| {}, || {}, || {});
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

// ---------------------------------------------------------------------------
// F16: the meter bar and its percentage tell one story
// ---------------------------------------------------------------------------

/// Every `GtkProgressBar` in the rendered capture page, as
/// `(fraction_percent, css_classes)`.
fn capture_bars(view: &EnrollmentViewV3) -> Vec<(u32, Vec<String>)> {
    fn walk(w: &gtk4::Widget, out: &mut Vec<(u32, Vec<String>)>) {
        if let Some(bar) = w.downcast_ref::<gtk4::ProgressBar>() {
            out.push((
                (bar.fraction() * 100.0).round() as u32,
                bar.css_classes().iter().map(|c| c.to_string()).collect(),
            ));
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            walk(&c, out);
            child = c.next_sibling();
        }
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let view = view.clone();
    common::run_gtk_test(move || {
        let mut comp = EnrollmentViewComponent::new();
        let page = comp.render(Some(&view), true, |_| {}, || {}, || {});
        let mut out = Vec::new();
        walk(page.upcast_ref::<gtk4::Widget>(), &mut out);
        let _ = tx.send(out);
    });
    rx.recv().expect("capture render produced bars")
}

fn view_with_quality(quality_percent: u32, quality_bucket: u8, guidance: &str) -> EnrollmentViewV3 {
    EnrollmentViewV3 {
        phase: "capturing".into(),
        quality_percent,
        diversity_percent: 18,
        frames_seen: 50,
        frames_accepted: 8,
        frames_rejected: 42,
        quality_bucket,
        guidance: GuidanceView::from_wire(guidance),
    }
}

/// Review finding F16: the image-quality bar used to render accent blue while
/// the percentage beside it was tinted by the quality bucket, so a blue bar
/// sat next to an amber "34%" — two signals about one measurement. The bar
/// must now carry the same libadwaita class that tints the number.
#[test]
fn f16_image_quality_bar_is_tinted_like_its_percentage() {
    let low = capture_bars(&view_with_quality(34, 0, "better_lighting"));
    let (_, classes) = low
        .iter()
        .find(|(pct, _)| *pct == 34)
        .expect("the 34% image-quality bar is rendered");
    assert!(
        classes.iter().any(|c| c == "warning"),
        "low-quality bar must carry the same `warning` tint as its number: {classes:?}"
    );

    let good = capture_bars(&view_with_quality(87, 2, "looking_good"));
    let (_, classes) = good
        .iter()
        .find(|(pct, _)| *pct == 87)
        .expect("the 87% image-quality bar is rendered");
    assert!(
        classes.iter().any(|c| c == "success"),
        "good-quality bar must carry the same `success` tint as its number: {classes:?}"
    );
}

/// The tint is *added*, never assigned. `GtkProgressBar` carries an
/// orientation style class of its own, and Adwaita hangs the trough's
/// `min-height` off it — replacing the class list drops it and the bar
/// collapses to zero height, which is what the first attempt at F16 did (the
/// tinted bar vanished from the screenshot entirely). Pin it: a tinted bar
/// keeps every class an untinted one has.
#[test]
fn f16_tinting_a_bar_does_not_strip_its_orientation_class() {
    let low = capture_bars(&view_with_quality(34, 0, "better_lighting"));
    let (_, tinted) = low
        .iter()
        .find(|(pct, _)| *pct == 34)
        .expect("the tinted image-quality bar is rendered");
    let (_, plain) = low
        .iter()
        .find(|(pct, _)| *pct == 18)
        .expect("the untinted angle-variety bar is rendered");
    assert!(!plain.is_empty(), "no baseline classes to compare against");
    for class in plain {
        assert!(
            tinted.contains(class),
            "tinting dropped {class:?} (tinted {tinted:?}, plain {plain:?})"
        );
    }
}

/// Progress towards a goal stays neutral: only the signals the daemon itself
/// judges are tinted, so the one coloured row on the page always means "this
/// is what to fix".
#[test]
fn f16_neutral_meters_keep_the_default_fill() {
    let bars = capture_bars(&view_with_quality(34, 0, "better_lighting"));
    let (_, classes) = bars
        .iter()
        .find(|(pct, _)| *pct == 18)
        .expect("the 18% angle-variety bar is rendered");
    for tint in ["success", "warning", "error"] {
        assert!(
            !classes.iter().any(|c| c == tint),
            "neutral meter must not be tinted {tint}: {classes:?}"
        );
    }
}

// --- honest numbers ---------------------------------------------------------

/// Before the first frame there is no measurement, and the view must say so.
///
/// `start_enrollment_v3` seeds the view with literal zeros and
/// `quality_bucket: 0` so the page has something to draw. Bucket 0 is "Poor",
/// so the scan used to open by announcing "0% — Poor" and tinting the
/// silhouette amber before the camera had returned anything at all.
#[test]
fn quality_and_diversity_are_unmeasured_until_a_frame_is_seen() {
    let fresh = view_with_quality(0, 0, "capturing");
    let fresh = namtor_ui::view_model::EnrollmentViewV3 {
        frames_seen: 0,
        frames_accepted: 0,
        frames_rejected: 0,
        ..fresh
    };
    assert!(!fresh.has_observation());
    assert_eq!(fresh.measured_quality(), None, "no quality before a frame");
    assert_eq!(
        fresh.measured_diversity_percent(),
        None,
        "no diversity before a frame"
    );
    assert_eq!(fresh.accept_rate_percent(), None);

    let seen = view_with_quality(34, 0, "better_lighting");
    assert!(seen.has_observation(), "frames_seen is 50 in the fixture");
    assert_eq!(seen.measured_quality(), Some((34, 0)));
    assert_eq!(seen.measured_diversity_percent(), Some(18));
}

/// The capture page must report the daemon's bucket, not one re-derived from
/// the percentage. The daemon computes the bucket from the underlying
/// observation, so the two can legitimately disagree at a boundary, and the
/// daemon's is the measurement.
#[test]
fn the_capture_page_keeps_the_daemons_bucket() {
    // 34% is one point above the LOW boundary, so a re-derivation would say
    // bucket 1 while the daemon said 0.
    let view = view_with_quality(34, 0, "better_lighting");
    assert_eq!(view.measured_quality(), Some((34, 0)));
    assert_ne!(
        namtor_ui::view_model::quality_bucket_from_percent(34),
        0,
        "this test is only meaningful while 34% re-derives to a different bucket"
    );
}

/// The client's bucket boundaries must match the daemon's, or the word beside
/// a percentage means different things on the two sides of the wire. The
/// review page has no bucket on the wire and derives one with these.
#[test]
fn client_quality_boundaries_match_the_daemon() {
    use namtor_ui::view_model::{quality_bucket_from_percent, QUALITY_LOW_MAX, QUALITY_MEDIUM_MAX};
    assert_eq!(QUALITY_LOW_MAX, 33, "daemon observation::QUALITY_LOW_MAX");
    assert_eq!(
        QUALITY_MEDIUM_MAX, 66,
        "daemon observation::QUALITY_MEDIUM_MAX"
    );
    assert_eq!(quality_bucket_from_percent(0), 0);
    assert_eq!(quality_bucket_from_percent(33), 0);
    assert_eq!(quality_bucket_from_percent(34), 1);
    assert_eq!(quality_bucket_from_percent(66), 1);
    assert_eq!(quality_bucket_from_percent(67), 2);
    assert_eq!(quality_bucket_from_percent(100), 2);
}

/// The review verdict must follow the quality number, not readiness.
///
/// It was `if ready_to_save { 2 } else { 1 }` — a different measurement — so a
/// barely-usable scan with enough angles announced "12% — Good", and a sharp
/// scan short on angles announced "88% — Fair".
#[test]
fn the_review_verdict_follows_quality_not_readiness() {
    use namtor_ui::view_model::quality_bucket_from_percent;
    // Ready to save, but the image quality is poor: must not read "Good".
    assert_eq!(quality_bucket_from_percent(12), 0, "12% is Poor");
    // Not ready to save (angles missing), but the image quality is high.
    assert_eq!(quality_bucket_from_percent(88), 2, "88% is Good");
}

/// A guidance word this build does not know must not be printed verbatim.
///
/// `Raw(s) => tr(s)` put raw snake_case on screen as the largest text on the
/// page — a headline reading `move_way_back`. And because `Raw` has no signal,
/// the attention row fell through to its "nothing is wrong" branch and
/// announced "Nothing — every signal is good enough right now" directly
/// underneath advice the client had just failed to interpret.
#[test]
fn an_unrecognised_guidance_token_is_never_shown_raw() {
    let g = GuidanceView::from_wire("move_way_back");
    assert!(g.is_unrecognised(), "the token is not one of the nine");
    let label = g.label();
    assert!(
        !label.contains('_'),
        "a raw snake_case token reached the headline: {label:?}"
    );
    assert!(!label.is_empty(), "the headline must still say something");
    assert_eq!(
        g.signal(),
        None,
        "an unknown token maps to no signal — the attention row must not read \
         that as 'nothing is wrong'"
    );
}

/// The nine known variants must not be caught by the unrecognised path.
#[test]
fn known_guidance_is_not_flagged_unrecognised() {
    for token in [
        "centering",
        "move_closer",
        "move_back",
        "better_lighting",
        "hold_still",
        "face_front",
        "looking_good",
        "capturing",
        "insufficient_pose",
    ] {
        let g = GuidanceView::from_wire(token);
        assert!(!g.is_unrecognised(), "{token} should be Known");
    }
    assert!(
        !GuidanceView::from_wire("").is_unrecognised(),
        "an empty token is Silent, not unrecognised"
    );
}

/// The guidance headline must only carry `AccessibleRole::Status` when the
/// announcer allows it.
///
/// `ProgressAnnouncer` documents the exact flood it prevents — "Status
/// triggers an AT-SPI announcement every time the label text changes" — and
/// was written, tested and then wired to nothing, while the capture headline
/// carried that role and was rebuilt on every poll.
#[test]
fn the_guidance_headline_announces_only_when_the_announcer_allows() {
    use gtk4::prelude::*;
    common::run_gtk_test(|| {
        let view = view_with_quality(50, 1, "centering");

        let mut announcing = EnrollmentViewComponent::new();
        let page = announcing.render(Some(&view), true, |_| {}, || {}, || {});
        assert!(
            common::has_accessible_role(
                page.upcast_ref::<gtk4::Widget>(),
                gtk4::AccessibleRole::Status
            ),
            "with announce = true the headline must be a Status for AT-SPI"
        );

        let mut quiet = EnrollmentViewComponent::new();
        let page = quiet.render(Some(&view), false, |_| {}, || {}, || {});
        assert!(
            !common::has_accessible_role(
                page.upcast_ref::<gtk4::Widget>(),
                gtk4::AccessibleRole::Status
            ),
            "with announce = false the headline must render without \
             re-interrupting the screen reader"
        );
    });
}

/// The announcement throttle must actually suppress something.
///
/// It was time-based only, at 250 ms, on a page that re-renders every 500 ms —
/// so every render was "due" and nothing was ever throttled, while the module
/// documented preventing exactly that flood. Unchanged guidance repeated on
/// each redraw is what floods a screen reader.
#[test]
fn unchanged_guidance_is_not_re_announced() {
    use namtor_ui::ProgressAnnouncer;
    use std::time::{Duration, Instant};

    let mut a = ProgressAnnouncer::new();
    let t0 = Instant::now();
    assert!(a.should_announce_text("Centre your face", t0), "first time");

    // A later render, well past the interval, with the SAME guidance.
    let t1 = t0 + Duration::from_secs(5);
    assert!(
        !a.should_announce_text("Centre your face", t1),
        "repeating unchanged guidance must not interrupt the screen reader again"
    );

    // New guidance announces.
    let t2 = t1 + Duration::from_secs(1);
    assert!(a.should_announce_text("Hold still", t2), "changed guidance");

    // And back again is announced, because it changed from the last one.
    let t3 = t2 + Duration::from_secs(1);
    assert!(a.should_announce_text("Centre your face", t3));
}

/// Rapid changes are still rate-limited, so a flapping signal cannot flood.
#[test]
fn rapidly_changing_guidance_is_still_rate_limited() {
    use namtor_ui::ProgressAnnouncer;
    use std::time::{Duration, Instant};

    let mut a = ProgressAnnouncer::new();
    let t0 = Instant::now();
    assert!(a.should_announce_text("a", t0));
    assert!(
        !a.should_announce_text("b", t0 + Duration::from_millis(10)),
        "a different string 10 ms later must still be throttled"
    );
}

/// A message with runtime parts must keep a stable msgid.
///
/// `tr(&format!("Rename profile {name}"))` hands gettext a msgid containing
/// the interpolated value, so it can never match a catalog entry — every such
/// string was permanently untranslatable while a test asserting strings are
/// "wrapped in tr()" still passed.
#[test]
fn tr_args_substitutes_without_destroying_the_msgid() {
    use namtor_ui::tr_args;
    assert_eq!(
        tr_args("Rename profile {}", &["Alice"]),
        "Rename profile Alice"
    );
    assert_eq!(
        tr_args("{}: {}", &["Daemon unavailable", "not running"]),
        "Daemon unavailable: not running"
    );
    // Extra placeholders with no argument are left alone rather than panicking.
    assert_eq!(tr_args("{} and {}", &["one"]), "one and {}");
    // Extra arguments are ignored, not appended.
    assert_eq!(tr_args("just {}", &["a", "b"]), "just a");
    // No placeholder: the msgid passes through.
    assert_eq!(tr_args("no placeholder", &["x"]), "no placeholder");
}

/// Before the first frame, the scan must not claim anything is good.
///
/// `Guidance::Capturing` has no attention signal, so the "Needs attention"
/// row fell through to "Nothing — every signal is good enough right now" while
/// the three meters beside it all read "Waiting for the first view". It was a
/// claim about measurements that did not exist.
#[test]
fn the_scan_does_not_reassure_before_it_has_measured_anything() {
    use gtk4::prelude::*;
    common::run_gtk_test(|| {
        let fresh = namtor_ui::view_model::EnrollmentViewV3 {
            phase: "capturing".into(),
            quality_percent: 0,
            diversity_percent: 0,
            frames_seen: 0,
            frames_accepted: 0,
            frames_rejected: 0,
            quality_bucket: 0,
            guidance: GuidanceView::from_wire("capturing"),
        };
        let mut c = EnrollmentViewComponent::new();
        let page = c.render(Some(&fresh), true, |_| {}, || {}, || {});
        let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
        assert!(
            !texts.iter().any(|t| t.contains("good enough right now")),
            "nothing has been measured; the page must not say every signal is fine: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Needs attention")),
            "the row must still be present: {texts:?}"
        );
    });
}

/// Once frames have been seen, the attention row reports the real signal.
#[test]
fn the_scan_names_the_signal_it_is_waiting_on() {
    use gtk4::prelude::*;
    common::run_gtk_test(|| {
        let v = namtor_ui::view_model::EnrollmentViewV3 {
            phase: "capturing".into(),
            quality_percent: 71,
            diversity_percent: 35,
            frames_seen: 160,
            frames_accepted: 120,
            frames_rejected: 40,
            quality_bucket: 2,
            guidance: GuidanceView::from_wire("insufficient_pose"),
        };
        let mut c = EnrollmentViewComponent::new();
        let page = c.render(Some(&v), true, |_| {}, || {}, || {});
        let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
        for expected in ["71%", "35%", "75%", "Head angles", "Turn your head slowly"] {
            assert!(
                texts.iter().any(|t| t.contains(expected)),
                "the scan must show {expected:?}: {texts:?}"
            );
        }
    });
}

/// An instruction must not be styled as success.
///
/// The headline greened on `encouraging || quality_bucket >= 2`, so "Adjust
/// your pose" — shown while the scan waits on head angles — rendered in
/// success green because the image quality happened to be good. The largest
/// text on the page said "all is well" directly above a row naming the signal
/// that was blocking progress. Caught by looking at the rendered page.
#[test]
fn only_encouraging_guidance_is_styled_as_success() {
    use gtk4::prelude::*;
    common::run_gtk_test(|| {
        // Good image quality, but the scan is waiting on pose.
        let blocked = namtor_ui::view_model::EnrollmentViewV3 {
            phase: "capturing".into(),
            quality_percent: 71,
            diversity_percent: 35,
            frames_seen: 160,
            frames_accepted: 120,
            frames_rejected: 40,
            quality_bucket: 2,
            guidance: GuidanceView::from_wire("insufficient_pose"),
        };
        let mut c = EnrollmentViewComponent::new();
        let page = c.render(Some(&blocked), true, |_| {}, || {}, || {});
        assert!(
            !common::has_label_with_class(
                page.upcast_ref::<gtk4::Widget>(),
                "Adjust your pose",
                "success"
            ),
            "an instruction must not be green while a signal is blocking"
        );

        // And the genuinely good state still is green.
        let good = namtor_ui::view_model::EnrollmentViewV3 {
            guidance: GuidanceView::from_wire("looking_good"),
            diversity_percent: 100,
            ..blocked.clone()
        };
        let mut c2 = EnrollmentViewComponent::new();
        let page2 = c2.render(Some(&good), true, |_| {}, || {}, || {});
        assert!(
            common::has_label_with_class(
                page2.upcast_ref::<gtk4::Widget>(),
                "Enrollment complete",
                "success"
            ),
            "a finished scan must still read as success"
        );
    });
}

// ===========================================================================
// The silhouette is read as the overall verdict
// ===========================================================================

/// Every style class on the silhouette's icon.
fn silhouette_classes(bucket: u8, encouraging: bool) -> Vec<String> {
    use gtk4::prelude::*;
    let w = namtor_ui::components::scan_feedback::silhouette(bucket, encouraging);
    let icon = first_image(&w).expect("the silhouette carries an icon");
    icon.css_classes().iter().map(|s| s.to_string()).collect()
}

fn first_image(widget: &gtk4::Widget) -> Option<gtk4::Image> {
    use gtk4::prelude::*;
    if let Some(i) = widget.downcast_ref::<gtk4::Image>() {
        return Some(i.clone());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(found) = first_image(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

/// The silhouette is the largest element on the capture page, so it is read
/// as the overall verdict whatever else is on screen. It was coloured from
/// image quality alone, so on "Adjust your pose" — headline an instruction,
/// "Needs attention" naming the missing head angles — the biggest thing on
/// the page was the same success-green as "Enrollment complete". Found by
/// rendering the page and looking at it.
#[test]
fn a_scan_that_still_needs_something_is_not_painted_green() {
    common::run_gtk_test(|| {
        for bucket in [2u8, 3] {
            let classes = silhouette_classes(bucket, false);
            assert!(
                !classes.iter().any(|c| c == "success"),
                "bucket {bucket} with work outstanding claimed success: {classes:?}"
            );
        }
    });
}

/// Green is reserved for a scan that needs nothing, so the finished state is
/// still distinguishable at a glance.
#[test]
fn a_finished_scan_is_green() {
    common::run_gtk_test(|| {
        let classes = silhouette_classes(2, true);
        assert!(
            classes.iter().any(|c| c == "success"),
            "a scan that needs nothing must look finished: {classes:?}"
        );
    });
}

/// A dim view is something the user can see and fix, so it keeps its warning
/// whether or not the scan is otherwise content.
#[test]
fn a_dim_view_keeps_its_warning() {
    common::run_gtk_test(|| {
        for encouraging in [false, true] {
            let classes = silhouette_classes(0, encouraging);
            assert!(
                classes.iter().any(|c| c == "warning"),
                "a dim view must stay flagged (encouraging={encouraging}): {classes:?}"
            );
        }
    });
}
