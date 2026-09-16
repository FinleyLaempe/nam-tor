// SPDX-License-Identifier: GPL-3.0-only
//! Settings shell for the Nam-tor GNOME settings app.
//!
//! The shell owns a top-level [`adw::ApplicationWindow`] whose content *is*
//! the user interface — the standalone settings-app shape:
//!
//! ```text
//! AdwApplicationWindow
//! └── AdwNavigationView                (enrollment / review sub-pages)
//!     └── AdwNavigationPage "Nam-tor"
//!         └── AdwToolbarView
//!             ├── top:    AdwHeaderBar (title widget: AdwViewSwitcher)
//!             ├── content:AdwViewStack (Profiles / Authentication / System)
//!             └── bottom: AdwViewSwitcherBar (revealed on narrow layouts)
//! ```
//!
//! An earlier revision hosted an [`adw::PreferencesDialog`] inside an empty,
//! never-presented window. A libadwaita 1.5 `AdwDialog` renders *inside* its
//! parent window, so an unmapped, contentless host displayed nothing at all.
//! The window is now the UI and [`Shell::present`] presents it.
//!
//! The enrollment flow pushes capture and review sub-pages onto the
//! [`adw::NavigationView`] via [`Shell::push_subpage`] / [`Shell::pop_subpage`].
//! Sub-pages MUST be wrapped in [`adw::NavigationPage`] (that is what
//! `AdwNavigationView` accepts).
//!
//! Note: the Rust `libadwaita` crate drops the `Adw` prefix — the types are
//! `adw::ViewStack`, `adw::PreferencesPage`, etc. The `v1_5` feature is
//! enabled in `crates/namtor-gnome/Cargo.toml`.

use std::rc::Rc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{
    ActionRowExt, AdwApplicationWindowExt, PreferencesGroupExt, PreferencesPageExt,
};

use crate::tr;

/// Width, in scalable pixels, below which the header-bar view switcher is
/// replaced by a bottom [`adw::ViewSwitcherBar`].
const NARROW_BREAKPOINT_SP: f64 = 550.0;

/// Destination identifiers for the settings shell. Each destination maps to
/// one [`adw::PreferencesPage`] in the window's [`adw::ViewStack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Destination {
    #[default]
    Profiles,
    Authentication,
    System,
}

impl Destination {
    /// Human-readable title for the destination (used as the page title).
    pub fn title(&self) -> &'static str {
        match self {
            Self::Profiles => "Profiles",
            Self::Authentication => "Authentication",
            Self::System => "System",
        }
    }

    /// Icon name for the destination (shown in the view switcher).
    ///
    /// Every name here MUST exist in the Adwaita icon theme, otherwise the
    /// switcher renders a broken-image placeholder. `authenticator-symbolic`
    /// (used before) does not exist; `dialog-password-symbolic` does.
    pub fn icon_name(&self) -> &'static str {
        match self {
            Self::Profiles => "system-users-symbolic",
            Self::Authentication => "dialog-password-symbolic",
            Self::System => "emblem-system-symbolic",
        }
    }

    /// Name for the view-stack page (used as the page name for navigation).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Profiles => "profiles",
            Self::Authentication => "authentication",
            Self::System => "system",
        }
    }

    /// Inverse of [`Destination::name`].
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|d| d.name() == name)
    }

    /// Iterate over all destinations in order.
    pub fn all() -> impl Iterator<Item = Self> {
        [Self::Profiles, Self::Authentication, Self::System].into_iter()
    }
}

/// Settings shell: an [`adw::ApplicationWindow`] whose content is an
/// [`adw::ViewStack`] with three destinations, plus an
/// [`adw::NavigationView`] for the enrollment sub-pages.
pub struct Shell {
    pub window: adw::ApplicationWindow,
    pub view_stack: adw::ViewStack,
    pub profiles_page: adw::PreferencesPage,
    pub auth_page: adw::PreferencesPage,
    pub system_page: adw::PreferencesPage,
    /// Navigation host for the enrollment / review sub-pages. Its root page
    /// is the settings window content itself.
    navigation_view: adw::NavigationView,
    /// Groups currently added to the profiles page. Tracked so a re-render
    /// can remove the previous set before adding the new one.
    profiles_groups: Vec<adw::PreferencesGroup>,
    auth_groups: Vec<adw::PreferencesGroup>,
    system_groups: Vec<adw::PreferencesGroup>,
    current_destination: Destination,
}

impl Shell {
    /// Construct the settings shell: an `ApplicationWindow` whose content is
    /// a `ToolbarView` with a `HeaderBar` + `ViewSwitcher` over a `ViewStack`
    /// holding the three `PreferencesPage` destinations.
    pub fn new(app: &adw::Application) -> Self {
        let profiles_page = Self::build_page(Destination::Profiles, "Profiles settings page");
        let auth_page =
            Self::build_page(Destination::Authentication, "Authentication settings page");
        let system_page = Self::build_page(Destination::System, "System settings page");

        // The three destinations as real pages of a view stack.
        let view_stack = adw::ViewStack::new();
        for (dest, page) in [
            (Destination::Profiles, &profiles_page),
            (Destination::Authentication, &auth_page),
            (Destination::System, &system_page),
        ] {
            view_stack.add_titled_with_icon(
                page,
                Some(dest.name()),
                &tr(dest.title()),
                dest.icon_name(),
            );
        }

        let switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();

        let header = adw::HeaderBar::builder().title_widget(&switcher).build();

        let switcher_bar = adw::ViewSwitcherBar::builder().stack(&view_stack).build();

        let toolbar = adw::ToolbarView::builder().content(&view_stack).build();
        toolbar.add_top_bar(&header);
        toolbar.add_bottom_bar(&switcher_bar);

        // Enrollment and review are pushed on top of the settings root.
        let root_page = adw::NavigationPage::builder()
            .title(tr("Nam-tor"))
            .tag("settings")
            .child(&toolbar)
            .build();
        let navigation_view = adw::NavigationView::new();
        navigation_view.add(&root_page);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(tr("Nam-tor"))
            .default_width(800)
            .default_height(620)
            .width_request(360)
            .height_request(294)
            .build();
        window.update_property(&[gtk4::accessible::Property::Label(&tr("Nam-tor settings"))]);
        window.set_content(Some(&navigation_view));

        // Narrow layouts move the switcher from the header bar to a bottom
        // bar, which is what keeps the window usable at phone width.
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            NARROW_BREAKPOINT_SP,
            adw::LengthUnit::Sp,
        ));
        // A GValue holding a NULL widget clears the header-bar title widget;
        // passing `None` outright trips `adw_breakpoint_add_setter`'s
        // `G_IS_VALUE (value)` assertion.
        let no_title_widget = glib::Value::from_type(gtk4::Widget::static_type());
        breakpoint.add_setter(&header, "title-widget", Some(&no_title_widget));
        breakpoint.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
        window.add_breakpoint(breakpoint);

        Self {
            window,
            view_stack,
            profiles_page,
            auth_page,
            system_page,
            navigation_view,
            profiles_groups: Vec::new(),
            auth_groups: Vec::new(),
            system_groups: Vec::new(),
            current_destination: Destination::Profiles,
        }
    }

    fn build_page(destination: Destination, accessible_label: &str) -> adw::PreferencesPage {
        let page = adw::PreferencesPage::builder()
            .title(tr(destination.title()))
            .icon_name(destination.icon_name())
            .name(destination.name())
            .build();
        page.update_property(&[gtk4::accessible::Property::Label(&tr(accessible_label))]);
        page
    }

    /// Get the current active destination. Reads the live view stack so a
    /// switcher click is reflected, falling back to the last destination
    /// set programmatically.
    pub fn active_destination(&self) -> Destination {
        self.view_stack
            .visible_child_name()
            .and_then(|name| Destination::from_name(name.as_str()))
            .unwrap_or(self.current_destination)
    }

    /// Switch to a different destination by setting the view stack's visible
    /// page to the matching `PreferencesPage`.
    pub fn set_destination(&mut self, destination: Destination) {
        self.current_destination = destination;
        self.view_stack.set_visible_child_name(destination.name());
    }

    /// Get the `PreferencesPage` for the Profiles destination.
    pub fn profiles_page(&self) -> &adw::PreferencesPage {
        &self.profiles_page
    }

    /// Get the `PreferencesPage` for the Authentication destination.
    pub fn auth_page(&self) -> &adw::PreferencesPage {
        &self.auth_page
    }

    /// Get the `PreferencesPage` for the System destination.
    pub fn system_page(&self) -> &adw::PreferencesPage {
        &self.system_page
    }

    /// Replace the Profiles page content with a single `PreferencesGroup`,
    /// removing any previously added groups.
    pub fn set_profiles_content(&mut self, group: &adw::PreferencesGroup) {
        self.clear_profiles();
        self.profiles_page.add(group);
        self.profiles_groups.push(group.clone());
    }

    /// Replace the Authentication page content with a single
    /// `PreferencesGroup`, removing any previously added groups.
    pub fn set_auth_content(&mut self, group: &adw::PreferencesGroup) {
        self.clear_auth();
        self.auth_page.add(group);
        self.auth_groups.push(group.clone());
    }

    /// Replace the System page content with a single `PreferencesGroup`,
    /// removing any previously added groups.
    pub fn set_system_content(&mut self, group: &adw::PreferencesGroup) {
        self.clear_system();
        self.system_page.add(group);
        self.system_groups.push(group.clone());
    }

    /// Add a `PreferencesGroup` to the Profiles page without clearing
    /// existing groups. Used by renderers that build multiple titled
    /// groups for one destination.
    pub fn add_profiles_group(&mut self, group: &adw::PreferencesGroup) {
        self.profiles_page.add(group);
        self.profiles_groups.push(group.clone());
    }

    /// Add a `PreferencesGroup` to the Authentication page without clearing.
    pub fn add_auth_group(&mut self, group: &adw::PreferencesGroup) {
        self.auth_page.add(group);
        self.auth_groups.push(group.clone());
    }

    /// Add a `PreferencesGroup` to the System page without clearing.
    pub fn add_system_group(&mut self, group: &adw::PreferencesGroup) {
        self.system_page.add(group);
        self.system_groups.push(group.clone());
    }

    /// Remove all tracked groups from the Profiles page.
    pub fn clear_profiles(&mut self) {
        for g in self.profiles_groups.drain(..) {
            self.profiles_page.remove(&g);
        }
    }

    /// Remove all tracked groups from the Authentication page.
    pub fn clear_auth(&mut self) {
        for g in self.auth_groups.drain(..) {
            self.auth_page.remove(&g);
        }
    }

    /// Remove all tracked groups from the System page.
    pub fn clear_system(&mut self) {
        for g in self.system_groups.drain(..) {
            self.system_page.remove(&g);
        }
    }

    /// Push an enrollment sub-page onto the window's navigation stack.
    /// The sub-page MUST be wrapped in an `adw::NavigationPage`.
    pub fn push_subpage(&self, page: &adw::NavigationPage) {
        self.navigation_view.push(page);
    }

    /// Pop the current sub-page from the window's navigation stack.
    /// Returns `true` if a sub-page was popped (the settings root never
    /// pops).
    pub fn pop_subpage(&self) -> bool {
        self.navigation_view.pop()
    }

    /// Show a loading status page in the current destination. Renders a
    /// `PreferencesGroup` with a single informational row so the user sees
    /// a loading state within the preferences idiom.
    pub fn show_loading(&mut self) {
        let group = adw::PreferencesGroup::builder()
            .title(tr("Loading…"))
            .build();
        let row = adw::ActionRow::builder()
            .title(tr("Loading Nam-tor settings"))
            .subtitle(tr("Please wait while the daemon is contacted."))
            .build();
        row.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Loading Nam-tor settings",
        ))]);
        group.add(&row);

        match self.active_destination() {
            Destination::Profiles => self.set_profiles_content(&group),
            Destination::Authentication => self.set_auth_content(&group),
            Destination::System => self.set_system_content(&group),
        }
    }

    /// Show an offline/error status page with a retry action on **every**
    /// destination — an unreachable daemon makes all three tabs useless, and
    /// leaving the other two blank looks broken.
    ///
    /// The retry action is a real `suggested-action` pill button below the
    /// group, not a list row, so it reads as the primary action.
    pub fn show_offline(&mut self, error_detail: &str, on_retry: impl Fn() + 'static) {
        let on_retry: Rc<dyn Fn()> = Rc::new(on_retry);
        let profiles = Self::build_offline_group(error_detail, Rc::clone(&on_retry));
        let auth = Self::build_offline_group(error_detail, Rc::clone(&on_retry));
        let system = Self::build_offline_group(error_detail, on_retry);
        self.set_profiles_content(&profiles);
        self.set_auth_content(&auth);
        self.set_system_content(&system);
    }

    fn build_offline_group(error_detail: &str, on_retry: Rc<dyn Fn()>) -> adw::PreferencesGroup {
        let explanation = tr("The settings app cannot communicate with the system daemon.");
        let group = adw::PreferencesGroup::builder()
            .title(tr("Cannot reach the Nam-tor daemon"))
            .description(&explanation)
            // The offline group is the only content on the page, so centre
            // it rather than leaving it stranded against the top edge.
            .vexpand(true)
            .valign(gtk4::Align::Center)
            .build();
        group.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Nam-tor daemon unreachable",
        ))]);

        let status_row = adw::ActionRow::builder()
            .title(tr("Nam-tor daemon unreachable"))
            .subtitle(if error_detail.is_empty() {
                explanation.clone()
            } else {
                tr(error_detail)
            })
            .subtitle_lines(0)
            .build();
        let warning_icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
        warning_icon.add_css_class("warning");
        status_row.add_prefix(&warning_icon);
        status_row.update_property(&[
            gtk4::accessible::Property::Label(&tr("Nam-tor daemon unreachable")),
            gtk4::accessible::Property::Description(&explanation),
        ]);
        group.add(&status_row);

        let retry_button = gtk4::Button::builder()
            .label(tr("_Retry"))
            .use_underline(true)
            .halign(gtk4::Align::Center)
            .margin_top(18)
            .tooltip_text(tr("Retry connecting to the Nam-tor daemon"))
            .css_classes(["suggested-action", "pill"])
            .build();
        retry_button.update_property(&[
            gtk4::accessible::Property::Label(&tr("Retry connection")),
            gtk4::accessible::Property::Description(&tr("Try reconnecting to the Nam-tor daemon")),
        ]);
        retry_button.connect_clicked(move |_| {
            on_retry();
        });
        group.add(&retry_button);

        group
    }

    /// Present the settings window on screen.
    pub fn present(&self) {
        self.window.present();
    }

    /// Get a reference to the settings window.
    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    /// Get a reference to the view stack holding the three destinations.
    pub fn view_stack(&self) -> &adw::ViewStack {
        &self.view_stack
    }

    /// Get a reference to the navigation view hosting the enrollment and
    /// review sub-pages.
    pub fn navigation_view(&self) -> &adw::NavigationView {
        &self.navigation_view
    }
}
