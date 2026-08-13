// SPDX-License-Identifier: GPL-3.0-only
//! Recovery view component for unreachable daemon.
//!
//! Renders the recovery hint as an [`adw::PreferencesGroup`] with the title
//! shown once (USA-12: the old code rendered `hint.title` twice) and the
//! recovery steps as a subtitle/row.

use std::cell::RefCell;

use crate::tr;
use crate::view_model::RecoveryHint;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::PreferencesGroupExt;

pub struct RecoveryView {
    group: adw::PreferencesGroup,
    added_rows: RefCell<Vec<gtk4::Widget>>,
}

impl RecoveryView {
    pub fn new() -> Self {
        let group = adw::PreferencesGroup::new();
        Self {
            group,
            added_rows: RefCell::new(Vec::new()),
        }
    }

    pub fn render(&self, hint: &RecoveryHint) -> &adw::PreferencesGroup {
        self.clear();

        self.group.set_title(&tr(&hint.title));
        self.group
            .update_property(&[gtk4::accessible::Property::Label(&tr(&hint.title))]);

        let steps_text = hint.steps.join("\n\n");
        let steps_row = adw::ActionRow::builder()
            .title(tr("Recovery Steps:"))
            .subtitle(tr(&steps_text))
            .subtitle_lines(0)
            .build();
        steps_row.set_accessible_role(gtk4::AccessibleRole::List);
        steps_row.update_property(&[gtk4::accessible::Property::Label(&tr(
            "Recovery guidance steps",
        ))]);
        self.group.add(&steps_row);
        self.added_rows
            .borrow_mut()
            .push(steps_row.clone().upcast());

        &self.group
    }

    fn clear(&self) {
        let rows = std::mem::take(&mut *self.added_rows.borrow_mut());
        for row in rows {
            self.group.remove(&row);
        }
    }

    /// Get a reference to the underlying `PreferencesGroup`.
    pub fn group(&self) -> &adw::PreferencesGroup {
        &self.group
    }
}

impl Default for RecoveryView {
    fn default() -> Self {
        Self::new()
    }
}
