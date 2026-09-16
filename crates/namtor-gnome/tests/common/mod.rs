// SPDX-License-Identifier: GPL-3.0-only
//! Shared helpers for the integration tests.

#![allow(dead_code)]

use std::sync::mpsc::channel;
use std::sync::{Mutex, OnceLock};

use namtor_gnome::presenter::ProfilePresenter;
use namtor_gnome::ManagerClient;

// ---------------------------------------------------------------------------
// GTK test thread: a dedicated thread that initializes GTK and runs all
// GTK/libadwaita operations. The Rust test runner creates a new thread per
// test even with --test-threads=1, but GTK and libadwaita require all
// operations on the thread that called gtk4::init(). This helper spawns a
// single thread, initializes GTK on it, and dispatches test closures to it.
// ---------------------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send + 'static>;

static GTK_THREAD: OnceLock<Mutex<std::sync::mpsc::Sender<Job>>> = OnceLock::new();

fn gtk_thread_sender() -> &'static Mutex<std::sync::mpsc::Sender<Job>> {
    GTK_THREAD.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("gtk-test-thread".into())
            .spawn(move || {
                gtk4::init().expect("gtk4::init() failed in gtk-test-thread");
                while let Ok(job) = rx.recv() {
                    job();
                }
            })
            .expect("failed to spawn gtk test thread");
        Mutex::new(tx)
    })
}

/// Run a closure on the dedicated GTK thread. The closure must be `Send`
/// (it can only capture `Send` data), but it can create and use GTK/libadwaita
/// widgets freely inside. Panics are propagated to the calling test thread.
pub fn run_gtk_test<F: FnOnce() + Send + 'static>(f: F) {
    let (tx, rx) = channel();
    gtk_thread_sender()
        .lock()
        .unwrap()
        .send(Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            let _ = tx.send(result);
        }))
        .expect("gtk test thread channel closed");
    match rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!("gtk test thread died unexpectedly"),
    }
}

/// Recursively collect all user-visible text from a widget tree.
/// Returns labels, action-row titles/subtitles, button labels, and
/// preferences-group/page titles. Useful for asserting that rendered
/// content contains expected strings without scanning source code.
pub fn collect_widget_text(widget: &gtk4::Widget) -> Vec<String> {
    let mut texts = Vec::new();
    collect_text_recursive(widget, &mut texts);
    texts
}

fn collect_text_recursive(widget: &gtk4::Widget, texts: &mut Vec<String>) {
    use gtk4::prelude::*;
    use libadwaita::prelude::*;

    // gtk4::Label
    if let Some(label) = widget.downcast_ref::<gtk4::Label>() {
        texts.push(label.text().to_string());
    }
    // gtk4::Button
    if let Some(btn) = widget.downcast_ref::<gtk4::Button>() {
        if let Some(label) = btn.label() {
            texts.push(label.to_string());
        }
    }
    // Tooltip text (for icon-only buttons that set tooltip_text)
    if let Some(tooltip) = widget.tooltip_text() {
        texts.push(tooltip.to_string());
    }
    // adw::PreferencesRow (base of ActionRow, SwitchRow, EntryRow, etc.)
    if let Some(row) = widget.downcast_ref::<libadwaita::PreferencesRow>() {
        let title = row.title();
        if !title.is_empty() {
            texts.push(title.to_string());
        }
    }
    // adw::ActionRow subtitle
    if let Some(row) = widget.downcast_ref::<libadwaita::ActionRow>() {
        if let Some(subtitle) = row.subtitle() {
            if !subtitle.is_empty() {
                texts.push(subtitle.to_string());
            }
        }
    }
    // adw::PreferencesGroup title and description
    if let Some(group) = widget.downcast_ref::<libadwaita::PreferencesGroup>() {
        let title = group.title();
        if !title.is_empty() {
            texts.push(title.to_string());
        }
        if let Some(desc) = group.description() {
            if !desc.is_empty() {
                texts.push(desc.to_string());
            }
        }
    }
    // adw::PreferencesPage title
    if let Some(page) = widget.downcast_ref::<libadwaita::PreferencesPage>() {
        let title = page.title();
        if !title.is_empty() {
            texts.push(title.to_string());
        }
    }

    // Recurse into children
    let mut child = widget.first_child();
    while let Some(c) = child {
        collect_text_recursive(&c, texts);
        child = c.next_sibling();
    }
}

/// Check if any widget in the tree has a GType name containing `type_name`.
/// Used for negative assertions (e.g. "no ScrolledWindow in the Shell").
pub fn widget_tree_contains_type(widget: &gtk4::Widget, type_name: &str) -> bool {
    use gtk4::prelude::*;
    fn check(widget: &gtk4::Widget, type_name: &str) -> bool {
        if widget.type_().name().contains(type_name) {
            return true;
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if check(&c, type_name) {
                return true;
            }
            child = c.next_sibling();
        }
        false
    }
    check(widget, type_name)
}

/// Visit every `gtk4::Button` in a widget tree. Used by the accessibility
/// tests to assert mnemonics and accessible names on real widgets rather
/// than on a hand-maintained registry.
pub fn for_each_button(widget: &gtk4::Widget, f: &mut dyn FnMut(&gtk4::Button)) {
    use gtk4::prelude::*;
    if let Some(btn) = widget.downcast_ref::<gtk4::Button>() {
        f(btn);
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        for_each_button(&c, f);
        child = c.next_sibling();
    }
}

/// Run a presenter action and return the resulting model clone, so tests can
/// snapshot state without borrowing the presenter.
pub fn after<C: ManagerClient>(
    presenter: &mut ProfilePresenter<C>,
    action: impl FnOnce(&mut ProfilePresenter<C>),
) -> namtor_gnome::PresenterModel {
    action(presenter);
    presenter.model().clone()
}
