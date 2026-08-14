// SPDX-License-Identifier: GPL-3.0-only
//! Settings shell for the Nam-tor GNOME settings app.
//!
//! The shell owns a top-level [`adw::ApplicationWindow`] and presents an
//! [`adw::PreferencesDialog`] with three [`adw::PreferencesPage`]s: Profiles,
//! Authentication, and System. Each page hosts one or more
//! [`adw::PreferencesGroup`]s containing [`adw::ActionRow`] / [`adw::SwitchRow`]
//! rows — the modern GNOME settings idiom.
//!
//! The enrollment flow uses [`adw::PreferencesDialog::push_subpage`] /
//! [`adw::PreferencesDialog::pop_subpage`] for the capture and review
//! sub-pages. Sub-pages MUST be wrapped in [`adw::NavigationPage`] per the
//! libadwaita 1.5 migration guide (`push_subpage` takes an
//! `adw::NavigationPage`).
//!
//! Note: the Rust `libadwaita` crate drops the `Adw` prefix — the types are
//! `adw::PreferencesDialog`, `adw::PreferencesPage`, etc. The `v1_5` feature
//! is enabled in `crates/namtor-gnome/Cargo.toml`.

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::{
    ActionRowExt, AdwDialogExt, PreferencesDialogExt, PreferencesGroupExt, PreferencesPageExt,
};

use crate::tr;

/// Destination identifiers for the settings shell. Each destination maps to
/// one [`adw::PreferencesPage`] in the preferences dialog.
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

    /// Icon name for the destination (for the page icon).
    pub fn icon_name(&self) -> &'static str {
        match self {
            Self::Profiles => "system-users-symbolic",
            Self::Authentication => "authenticator-symbolic",
            Self::System => "emblem-system-symbolic",
        }
    }

    /// Name for the preferences page (used as the page name for navigation).
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

/// Settings shell presenting an [`adw::PreferencesDialog`] from an
/// [`adw::ApplicationWindow`] with three destinations.
pub struct Shell {
    pub window: adw::ApplicationWindow,
    pub dialog: adw::PreferencesDialog,
    pub profiles_page: adw::PreferencesPage,
    pub auth_page: adw::PreferencesPage,
    pub system_page: adw::PreferencesPage,
    /// Groups currently added to the profiles page. Tracked so a re-render
    /// can remove the previous set before adding the new one.
    profiles_groups: Vec<adw::PreferencesGroup>,
    auth_groups: Vec<adw::PreferencesGroup>,
    system_groups: Vec<adw::PreferencesGroup>,
    current_destination: Destination,
}

impl Shell {
    /// Construct the settings shell: an `ApplicationWindow` hosting a
    /// `PreferencesDialog` with three `PreferencesPage`s.
    pub fn new(app: &adw::Application) -> Self {
        // Profiles page
        let profiles_page = adw::PreferencesPage::builder()
            .title(tr(Destination::Profiles.title()))
            .icon_name(Destination::Profiles.icon_name())
            .name(Destination::Profiles.name())
            .build();
        profiles_page.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Profiles settings page",
        ))]);

        // Authentication page
        let auth_page = adw::PreferencesPage::builder()
            .title(tr(Destination::Authentication.title()))
            .icon_name(Destination::Authentication.icon_name())
            .name(Destination::Authentication.name())
            .build();
        auth_page.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Authentication settings page",
        ))]);

        // System page
        let system_page = adw::PreferencesPage::builder()
            .title(tr(Destination::System.title()))
            .icon_name(Destination::System.icon_name())
            .name(Destination::System.name())
            .build();
        system_page.update_property(&[gtk4::accessible::Property::Label(&tr(
            "System settings page",
        ))]);

        // Preferences dialog hosting all three pages
        let dialog = adw::PreferencesDialog::builder()
            .title(tr("Nam-tor"))
            .content_width(720)
            .content_height(540)
            .search_enabled(true)
            .build();
        dialog.update_property(&[gtk4::accessible::Property::Label(&tr("Nam-tor settings"))]);
        dialog.add(&profiles_page);
        dialog.add(&auth_page);
        dialog.add(&system_page);

        // Application window — host for the dialog. The window itself shows
        // nothing until the dialog is presented; it exists so the app has a
        // top-level GTK window for WM/session integration.
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(tr("Nam-tor"))
            .default_width(720)
            .default_height(540)
            .build();
        window.update_property(&[gtk4::accessible::Property::Label(&tr("Nam-tor settings"))]);

        Self {
            window,
            dialog,
            profiles_page,
            auth_page,
            system_page,
            profiles_groups: Vec::new(),
            auth_groups: Vec::new(),
            system_groups: Vec::new(),
            current_destination: Destination::Profiles,
        }
    }

    /// Get the current active destination.
    pub fn active_destination(&self) -> Destination {
        self.current_destination
    }

    /// Switch to a different destination by setting the dialog's visible
    /// page to the matching `PreferencesPage`.
    pub fn set_destination(&mut self, destination: Destination) {
        self.current_destination = destination;
        let page = match destination {
            Destination::Profiles => &self.profiles_page,
            Destination::Authentication => &self.auth_page,
            Destination::System => &self.system_page,
        };
        self.dialog.set_visible_page(page);
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

    /// Push an enrollment sub-page onto the dialog's navigation stack.
    /// The sub-page MUST be wrapped in an `adw::NavigationPage` per the
    /// libadwaita 1.5 migration guide.
    pub fn push_subpage(&self, page: &adw::NavigationPage) {
        self.dialog.push_subpage(page);
    }

    /// Pop the current sub-page from the dialog's navigation stack.
    /// Returns `true` if a sub-page was popped.
    pub fn pop_subpage(&self) -> bool {
        self.dialog.pop_subpage()
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

        match self.current_destination {
            Destination::Profiles => self.set_profiles_content(&group),
            Destination::Authentication => self.set_auth_content(&group),
            Destination::System => self.set_system_content(&group),
        }
    }

    /// Show an offline/error status page with a retry action. Renders a
    /// `PreferencesGroup` containing an informational row plus a retry
    /// button row so the user can re-attempt the connection within the
    /// preferences idiom.
    pub fn show_offline(&mut self, error_detail: &str, on_retry: impl Fn() + 'static) {
        let group = adw::PreferencesGroup::builder()
            .title(tr("Cannot reach the Nam-tor daemon"))
            .description(tr(error_detail))
            .build();
        group.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Nam-tor daemon unreachable",
        ))]);

        let status_row = adw::ActionRow::builder()
            .title(tr("Nam-tor daemon unreachable"))
            .subtitle(tr(error_detail))
            .build();
        status_row.update_property(&[
            gtk4::accessible::Property::Label(&tr("Nam-tor daemon unreachable")),
            gtk4::accessible::Property::Description(&tr(
                "The settings app cannot communicate with the system daemon.",
            )),
        ]);
        group.add(&status_row);

        let retry_row = adw::ActionRow::builder()
            .title(tr("_Retry"))
            .use_underline(true)
            .activatable(true)
            .tooltip_text(tr("Retry connecting to the Nam-tor daemon"))
            .build();
        retry_row.update_property(&[
            gtk4::accessible::Property::Label(&tr("Retry connection")),
            gtk4::accessible::Property::Description(&tr("Try reconnecting to the Nam-tor daemon")),
        ]);
        retry_row.connect_activated(move |_| {
            on_retry();
        });
        group.add(&retry_row);

        match self.current_destination {
            Destination::Profiles => self.set_profiles_content(&group),
            Destination::Authentication => self.set_auth_content(&group),
            Destination::System => self.set_system_content(&group),
        }
    }

    /// Present the preferences dialog on screen.
    /// The window exists as a transient parent for session integration but
    /// is not shown separately — the dialog handles all user-facing UI.
    pub fn present(&self) {
        self.dialog.present(Some(&self.window));
    }

    /// Get a reference to the host window.
    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    /// Get a reference to the preferences dialog.
    pub fn dialog(&self) -> &adw::PreferencesDialog {
        &self.dialog
    }
}
