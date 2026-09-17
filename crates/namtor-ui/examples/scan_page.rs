// SPDX-License-Identifier: GPL-3.0-only
//! Dev-only: render the enrollment scan page so it can be looked at.
//!
//! Not shipped — `examples/` is not part of any package. It exists because
//! "guidance on how good the scan really is" is a visual claim, and the only
//! honest way to check a visual claim is to look at it. The wizard itself
//! always talks to the real daemon; this borrows the same component with a
//! fixed view so the page can be captured without a camera or a daemon.
//!
//! Run: cargo run -p namtor-ui --example scan_page -- [start|poor|angles|good]

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use namtor_ui::components::enrollment::EnrollmentViewComponent;
use namtor_ui::view_model::{EnrollmentViewV3, GuidanceView};

fn view(which: &str) -> EnrollmentViewV3 {
    let (q, bucket, div, seen, acc, g) = match which {
        "start" => (0, 0, 0, 0, 0, "capturing"),
        "poor" => (22, 0, 15, 40, 6, "better_lighting"),
        "angles" => (71, 2, 35, 160, 120, "insufficient_pose"),
        _ => (88, 2, 100, 300, 260, "looking_good"),
    };
    EnrollmentViewV3 {
        phase: "capturing".into(),
        quality_percent: q,
        diversity_percent: div,
        frames_seen: seen,
        frames_accepted: acc,
        frames_rejected: seen - acc,
        quality_bucket: bucket,
        guidance: GuidanceView::from_wire(g),
    }
}

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "angles".into());
    let app = adw::Application::builder()
        .application_id("org.namtor.ScanPagePreview")
        .build();
    app.connect_activate(move |app| {
        let mut comp = EnrollmentViewComponent::new();
        let page = comp.render(Some(&view(&which)), true, |_| {}, || {}, || {});
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(480)
            .default_height(760)
            .title("Nam-tor — scan page")
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(page));
        window.set_content(Some(&toolbar));
        window.present();
    });
    app.run_with_args::<&str>(&[]);
}
