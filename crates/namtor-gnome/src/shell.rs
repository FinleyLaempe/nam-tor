// SPDX-License-Identifier: GPL-3.0-only
//! Adaptive application shell for the Nam-tor GNOME settings app.
//!
//! The shell owns the top-level [`adw::ApplicationWindow`] and implements
//! adaptive navigation using `AdwViewStack` with three destinations:
//! Profiles, Authentication, and System. Navigation switches between
//! `AdwViewSwitcher` (wide, ≥ 600px) and `AdwViewSwitcherBar` (narrow, < 600px)
//! via an `AdwBreakpoint`.

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Orientation, PolicyType, ScrolledWindow};
use libadwaita as adw;
use libadwaita::prelude::AdwApplicationWindowExt;
use libadwaita::{BreakpointCondition, BreakpointConditionLengthType, LengthUnit};

use crate::tr;

/// Destination identifiers for the settings shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Destination {
    #[default]
    Profiles,
    Authentication,
    System,
}

impl Destination {
    /// Human-readable title for the destination.
    pub fn title(&self) -> &'static str {
        match self {
            Self::Profiles => "Profiles",
            Self::Authentication => "Authentication",
            Self::System => "System",
        }
    }

    /// Icon name for the destination (for switcher).
    pub fn icon_name(&self) -> &'static str {
        match self {
            Self::Profiles => "system-users-symbolic",
            Self::Authentication => "authenticator-symbolic",
            Self::System => "emblem-system-symbolic",
        }
    }

    /// Name for the view stack page.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Profiles => "profiles",
            Self::Authentication => "authentication",
            Self::System => "system",
        }
    }

    /// Iterate over all destinations in order.
    pub fn all() -> impl Iterator<Item = Self> {
        [Self::Profiles, Self::Authentication, Self::System].into_iter()
    }
}

/// Adaptive settings shell with three destinations.
pub struct Shell {
    pub window: adw::ApplicationWindow,
    pub view_stack: adw::ViewStack,
    pub switcher_bar: adw::ViewSwitcherBar,
    pub header_bar: adw::HeaderBar,
    pub profiles_page: GtkBox,
    pub auth_page: GtkBox,
    pub system_page: GtkBox,
    current_destination: Destination,
}

impl Shell {
    /// Construct the adaptive settings shell.
    pub fn new(app: &adw::Application) -> Self {
        // Create the view stack with three pages
        let view_stack = adw::ViewStack::new();

        // Profiles page
        let profiles_page = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .spacing(8)
            .build();
        let profiles_scroll = ScrolledWindow::builder()
            .vscrollbar_policy(PolicyType::Automatic)
            .hscrollbar_policy(PolicyType::Never)
            .propagate_natural_height(false)
            .propagate_natural_width(false)
            .child(&profiles_page)
            .build();
        let profiles_stack_page =
            view_stack.add_named(&profiles_scroll, Some(Destination::Profiles.name()));
        profiles_stack_page.set_title(Some(&tr(Destination::Profiles.title())));
        profiles_stack_page.set_icon_name(Some(Destination::Profiles.icon_name()));

        // Authentication page
        let auth_page = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .spacing(8)
            .build();
        let auth_scroll = ScrolledWindow::builder()
            .vscrollbar_policy(PolicyType::Automatic)
            .hscrollbar_policy(PolicyType::Never)
            .propagate_natural_height(false)
            .propagate_natural_width(false)
            .child(&auth_page)
            .build();
        let auth_stack_page =
            view_stack.add_named(&auth_scroll, Some(Destination::Authentication.name()));
        auth_stack_page.set_title(Some(&tr(Destination::Authentication.title())));
        auth_stack_page.set_icon_name(Some(Destination::Authentication.icon_name()));

        // System page
        let system_page = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .spacing(8)
            .build();
        let system_scroll = ScrolledWindow::builder()
            .vscrollbar_policy(PolicyType::Automatic)
            .hscrollbar_policy(PolicyType::Never)
            .propagate_natural_height(false)
            .propagate_natural_width(false)
            .child(&system_page)
            .build();
        let system_stack_page =
            view_stack.add_named(&system_scroll, Some(Destination::System.name()));
        system_stack_page.set_title(Some(&tr(Destination::System.title())));
        system_stack_page.set_icon_name(Some(Destination::System.icon_name()));

        // Create header bar with title widget
        let title_label = gtk4::Label::new(Some(&tr(Destination::Profiles.title())));
        title_label.add_css_class("title");
        title_label.set_accessible_role(gtk4::AccessibleRole::Heading);
        let header_bar = adw::HeaderBar::builder().title_widget(&title_label).build();

        // Create toolbar view
        let toolbar_view = adw::ToolbarView::builder()
            .top_bar_style(adw::ToolbarStyle::Raised)
            .build();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&view_stack));

        // Create window
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(tr("Nam-tor"))
            .default_width(720)
            .default_height(540)
            .content(&toolbar_view)
            .build();
        window.update_property(&[gtk4::accessible::Property::Label(&tr("Nam-tor settings"))]);

        // Create view switcher bar (for narrow windows, visible by default)
        let switcher_bar = adw::ViewSwitcherBar::builder()
            .stack(&view_stack)
            .reveal(false) // Start hidden, breakpoint will reveal on narrow
            .build();
        toolbar_view.add_bottom_bar(&switcher_bar);

        // Create view switcher for header bar (for wide windows)
        let switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Narrow)
            .build();

        // Set switcher as title widget for wide windows
        header_bar.set_title_widget(Some(&switcher));

        // Create breakpoint for adaptive switching
        // Narrow < 600px: switcher bar revealed, header switcher hidden
        // Wide >= 600px: switcher bar hidden, header switcher visible
        let condition = BreakpointCondition::new_length(
            BreakpointConditionLengthType::MaxWidth,
            600.0,
            LengthUnit::Px,
        );
        let breakpoint = adw::Breakpoint::new(condition);
        breakpoint.add_setter(&switcher_bar, "reveal", Some(&glib::Value::from(true)));
        window.add_breakpoint(breakpoint);

        Self {
            window,
            view_stack,
            switcher_bar,
            header_bar,
            profiles_page,
            auth_page,
            system_page,
            current_destination: Destination::Profiles,
        }
    }

    /// Get the current active destination.
    pub fn active_destination(&self) -> Destination {
        self.current_destination
    }

    /// Switch to a different destination.
    pub fn set_destination(&mut self, destination: Destination) {
        self.current_destination = destination;
        self.view_stack.set_visible_child_name(destination.name());
    }

    /// Get the content area for the Profiles page.
    pub fn profiles_content(&self) -> &GtkBox {
        &self.profiles_page
    }

    /// Get the content area for the Authentication page.
    pub fn auth_content(&self) -> &GtkBox {
        &self.auth_page
    }

    /// Get the content area for the System page.
    pub fn system_content(&self) -> &GtkBox {
        &self.system_page
    }

    /// Set the content of the Profiles page.
    pub fn set_profiles_content(&self, widget: &impl gtk4::prelude::IsA<gtk4::Widget>) {
        self.clear_content(&self.profiles_page);
        self.profiles_page.append(widget);
    }

    /// Set the content of the Authentication page.
    pub fn set_auth_content(&self, widget: &impl gtk4::prelude::IsA<gtk4::Widget>) {
        self.clear_content(&self.auth_page);
        self.auth_page.append(widget);
    }

    /// Set the content of the System page.
    pub fn set_system_content(&self, widget: &impl gtk4::prelude::IsA<gtk4::Widget>) {
        self.clear_content(&self.system_page);
        self.system_page.append(widget);
    }

    /// Clear all children from a content area.
    fn clear_content(&self, area: &GtkBox) {
        while let Some(child) = area.first_child() {
            area.remove(&child);
        }
    }

    /// Show a loading status page in the current destination.
    pub fn show_loading(&self) {
        let status_page = adw::StatusPage::builder()
            .icon_name("content-loading-symbolic")
            .title(tr("Loading…"))
            .build();
        status_page.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Loading Nam-tor settings",
        ))]);

        match self.current_destination {
            Destination::Profiles => self.set_profiles_content(&status_page),
            Destination::Authentication => self.set_auth_content(&status_page),
            Destination::System => self.set_system_content(&status_page),
        }
    }

    /// Show an offline/error status page with retry action.
    pub fn show_offline(&self, error_detail: &str, on_retry: impl Fn() + 'static) {
        let status_page = adw::StatusPage::builder()
            .icon_name("computer-fail-symbolic")
            .title(tr("Cannot reach the Nam-tor daemon"))
            .description(tr(error_detail))
            .build();
        status_page.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Nam-tor daemon unreachable",
        ))]);

        let retry_button = gtk4::Button::builder()
            .label(tr("_Retry"))
            .halign(gtk4::Align::Center)
            .use_underline(true)
            .tooltip_text(tr("Retry connecting to the Nam-tor daemon"))
            .build();
        retry_button.update_property(&[
            gtk4::accessible::Property::Label(&tr("Retry connection")),
            gtk4::accessible::Property::Description(&tr("Try reconnecting to the Nam-tor daemon")),
        ]);

        retry_button.connect_clicked(move |_| {
            on_retry();
        });

        let button_box = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .build();
        button_box.append(&retry_button);
        status_page.set_child(Some(&button_box));

        match self.current_destination {
            Destination::Profiles => self.set_profiles_content(&status_page),
            Destination::Authentication => self.set_auth_content(&status_page),
            Destination::System => self.set_system_content(&status_page),
        }
    }

    /// Present the window on screen.
    pub fn present(&self) {
        self.window.present();
    }

    /// Get a reference to the window.
    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }
}
