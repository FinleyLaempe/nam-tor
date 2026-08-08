// SPDX-License-Identifier: GPL-3.0-only
//! Application shell for the Nam-tor management UI.
//!
//! The shell owns the top-level [`adw::ApplicationWindow`] and its core
//! layout components (HeaderBar, ToolbarView). It provides the structural
//! frame that hosts the active page.

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Orientation};
use libadwaita as adw;

pub struct Shell {
    pub window: adw::ApplicationWindow,
    pub content_area: GtkBox,
    pub header: adw::HeaderBar,
}

impl Shell {
    /// Construct the main application frame.
    pub fn new(app: &adw::Application) -> Self {
        let header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new("Nam-tor", "Face Authentication"))
            .build();

        let content_area = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .spacing(8)
            .build();

        // Ensure the content area can scroll when text is enlarged or window is small
        let scrolled = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .child(&content_area)
            .build();

        let toolbar_view = adw::ToolbarView::builder().build();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&scrolled));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Nam-tor — Face Profiles")
            .default_width(560)
            .default_height(420)
            .content(&toolbar_view)
            .build();

        Self {
            window,
            content_area,
            header,
        }
    }

    /// Set the content of the main area.
    pub fn set_content(&self, widget: &impl IsA<gtk4::Widget>) {
        // Clear existing content before adding new page
        while let Some(child) = self.content_area.first_child() {
            self.content_area.remove(&child);
        }
        self.content_area.append(widget);
    }

    /// Present the window on screen.
    pub fn present(&self) {
        self.window.present();
    }
}
