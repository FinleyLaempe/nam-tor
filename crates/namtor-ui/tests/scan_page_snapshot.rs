// SPDX-License-Identifier: GPL-3.0-only
//! A readable snapshot of what the enrollment scan page actually shows.
//!
//! The live camera preview was dropped deliberately; the scan quality guidance
//! is the thing that replaced it, so it is worth being able to see, in one
//! place, exactly what a user is told while scanning.

mod common;

use gtk4::prelude::*;
use namtor_ui::components::enrollment::EnrollmentViewComponent;
use namtor_ui::view_model::{EnrollmentViewV3, GuidanceView};

fn view(
    quality: u32,
    bucket: u8,
    diversity: u32,
    seen: u32,
    acc: u32,
    guidance: &str,
) -> EnrollmentViewV3 {
    EnrollmentViewV3 {
        phase: "capturing".into(),
        quality_percent: quality,
        diversity_percent: diversity,
        frames_seen: seen,
        frames_accepted: acc,
        frames_rejected: seen - acc,
        quality_bucket: bucket,
        guidance: GuidanceView::from_wire(guidance),
    }
}

#[test]
fn print_the_scan_page() {
    common::run_gtk_test(|| {
        for (label, v) in [
            (
                "just started, nothing measured yet",
                view(0, 0, 0, 0, 0, "capturing"),
            ),
            (
                "poor light, early",
                view(22, 0, 15, 40, 6, "better_lighting"),
            ),
            (
                "needs more angles",
                view(71, 2, 35, 160, 120, "insufficient_pose"),
            ),
            ("going well", view(88, 2, 100, 300, 260, "looking_good")),
        ] {
            let mut c = EnrollmentViewComponent::new();
            let page = c.render(Some(&v), true, |_| {}, || {}, || {});
            let texts = common::collect_widget_text(page.upcast_ref::<gtk4::Widget>());
            println!("\n=== {label} ===");
            for t in texts.iter().filter(|t| !t.trim().is_empty()) {
                println!("   {t}");
            }
        }
    });
}
