// SPDX-License-Identifier: GPL-3.0-only
//! Dev-only: render the settings app's three pages so they can be looked at.
//!
//! Not shipped — `examples/` is not part of any package. "A clean and
//! polished UI" is a visual claim, and the honest way to check one is to look
//! at it. The wizard always talks to the real daemon; this borrows the same
//! controller with `namtor_client::fake_bus::FakeBus` behind it, so the pages
//! render with real data and no daemon, camera or root.
//!
//! Run: `cargo run -p namtor-ui --example app_pages -- <out-dir>`
//!
//! Each capture renders the window's own widget tree through GTK's renderer.
//! It never reads the screen, so it cannot pick up anything else on the
//! desktop, and it gives the same image with or without a compositor.

use gtk4::prelude::*;
use libadwaita as adw;
use namtor_client::fake_bus::FakeBus;
use namtor_ui::app::AppController;
use namtor_ui::shell::Destination;
use namtor_ui::ManagerClient;

fn capture(window: &adw::ApplicationWindow, path: &str) -> Result<(), String> {
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
    renderer
        .render_texture(&node, None)
        .save_to_png(path)
        .map_err(|e| format!("write {path}: {e}"))
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let app = adw::Application::builder()
        .application_id("org.namtor.AppPagesPreview")
        .build();

    app.connect_activate(move |app| {
        let bus = FakeBus::spawn().expect("fake bus");
        // The fake starts with no profiles, and an empty list is one row
        // saying so — not the page anyone needs to look at.
        for name in ["Everyday", "Glasses"] {
            let session = bus.client.start_enrollment(1000, name).expect("start");
            bus.client.commit_profile(&session).expect("commit");
        }

        let rc = AppController::new_online_for_test(app, bus.client);
        let window = rc.borrow().shell_for_test().window().clone();
        window.set_default_size(560, 820);
        window.present();

        let out = out.clone();
        let rc2 = std::rc::Rc::clone(&rc);
        let app2 = app.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
            for dest in Destination::all() {
                rc2.borrow_mut().shell_for_test_mut().set_destination(dest);
                // Let the stack settle on the new page before capturing.
                while glib::MainContext::default().iteration(false) {}
                let path = format!("{out}/page-{}.png", dest.name());
                match capture(&window, &path) {
                    Ok(()) => println!("wrote {path}"),
                    Err(e) => eprintln!("capture {path} failed: {e}"),
                }
            }
            app2.quit();
        });
    });
    app.run_with_args::<&str>(&[]);
}
