// SPDX-License-Identifier: GPL-3.0-only
//! Dev-only: render the enrollment scan page so it can be looked at.
//!
//! Not shipped — `examples/` is not part of any package. It exists because
//! "guidance on how good the scan really is" is a visual claim, and the only
//! honest way to check a visual claim is to look at it. The wizard itself
//! always talks to the real daemon; this borrows the same component with a
//! fixed view so the page can be captured without a camera or a daemon.
//!
//! Run: cargo run -p namtor-ui --example scan_page -- [start|poor|angles|good|review-ok|review-thin]
//!
//! With a second argument it writes the rendered page to that PNG and exits
//! instead of staying on screen:
//!
//! ```text
//! cargo run -p namtor-ui --example scan_page -- angles /tmp/angles.png
//! ```
//!
//! The capture renders the window's own widget tree through GTK's renderer —
//! it never reads the screen, so it cannot pick up anything else that happens
//! to be on the desktop, and it produces the same image with or without a
//! compositor in front of it.

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use namtor_ui::components::enrollment::EnrollmentViewComponent;
use namtor_ui::components::ReviewViewComponent;
use namtor_ui::view_model::{EnrollmentViewV3, GuidanceView, ReviewViewV3};

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

/// Render `window`'s widget tree to `path` as a PNG.
fn capture(window: &adw::ApplicationWindow, path: &str) -> Result<(), String> {
    use gtk4::prelude::*;
    let widget: &gtk4::Widget = window.upcast_ref();
    let paintable = gtk4::WidgetPaintable::new(Some(widget));
    let (w, h) = (widget.width() as f64, widget.height() as f64);
    if w <= 0.0 || h <= 0.0 {
        return Err(format!("window has no size yet ({w}x{h})"));
    }
    let snapshot = gtk4::Snapshot::new();
    paintable.snapshot(&snapshot, w, h);
    let node = snapshot.to_node().ok_or("the page rendered to nothing")?;
    let renderer = widget
        .native()
        .and_then(|n| n.renderer())
        .ok_or("no renderer: the window is not realized")?;
    let texture = renderer.render_texture(&node, None);
    texture
        .save_to_png(path)
        .map_err(|e| format!("write {path}: {e}"))
}

fn review(which: &str) -> ReviewViewV3 {
    if which == "review-thin" {
        ReviewViewV3 {
            ready_to_save: false,
            quality_percent: 41,
            diversity_percent: 30,
            frames_accepted: 22,
            guidance: GuidanceView::from_wire("insufficient_pose"),
        }
    } else {
        ReviewViewV3 {
            ready_to_save: true,
            quality_percent: 88,
            diversity_percent: 100,
            frames_accepted: 260,
            guidance: GuidanceView::from_wire("looking_good"),
        }
    }
}

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "angles".into());
    let png = std::env::args().nth(2);
    let app = adw::Application::builder()
        .application_id("org.namtor.ScanPagePreview")
        .build();
    app.connect_activate(move |app| {
        let mut comp = EnrollmentViewComponent::new();
        let mut rev = ReviewViewComponent::new();
        let page: &adw::PreferencesPage = if which.starts_with("review") {
            rev.render(&review(&which), || {}, || {}, || {}, false)
        } else {
            comp.render(Some(&view(&which)), true, |_| {}, || {}, || {})
        };
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

        // Capture once the window has been through a frame, so it has a size
        // and the page has been laid out.
        if let Some(path) = png.clone() {
            let win = window.clone();
            let app = app.clone();
            // Retry rather than fail: how many frames the window needs before
            // it paints depends on the compositor, so a fixed delay is a
            // race. Losing it produced "the page rendered to nothing" for one
            // state out of six, intermittently.
            let tries = std::cell::Cell::new(0u32);
            glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                tries.set(tries.get() + 1);
                match capture(&win, &path) {
                    Ok(()) => {
                        println!("wrote {path}");
                        app.quit();
                        glib::ControlFlow::Break
                    }
                    Err(e) if tries.get() < 40 => {
                        let _ = e;
                        glib::ControlFlow::Continue
                    }
                    Err(e) => {
                        eprintln!("capture failed after {} tries: {e}", tries.get());
                        app.quit();
                        glib::ControlFlow::Break
                    }
                }
            });
        }
    });
    app.run_with_args::<&str>(&[]);
}
