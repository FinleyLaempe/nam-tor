// SPDX-License-Identifier: GPL-3.0-//
//! Navigation and page switching logic for the Nam-tor UI.
//!
//! This module manages the transition between different views (Main, Enrollment,
//! Recovery) based on the state of the [`PresenterModel`].

use crate::components::EnrollmentViewComponent;
use crate::components::{ProfileListView, RecoveryView};
use crate::shell::Shell;
use crate::view_model::PresenterModel;

pub enum Page {
    Main,
    Enrollment,
    Recovery,
}

pub struct Navigation {
    shell: Shell,
    main_page: ProfileListView,
    enrollment_page: EnrollmentViewComponent,
    recovery_page: RecoveryView,
}

impl Navigation {
    pub fn new(shell: Shell) -> Self {
        Self {
            shell,
            main_page: ProfileListView::new(),
            enrollment_page: EnrollmentViewComponent::new(),
            recovery_page: RecoveryView::new(),
        }
    }

    /// Transition the shell to the page appropriate for the current model state.
    pub fn update(&self, model: &PresenterModel) {
        if let Some(error) = &model.error {
            if let Some(hint) = &error.recovery {
                self.shell.set_content(self.recovery_page.render(hint));
                return;
            }
        }

        if let Some(enrollment) = &model.enrollment {
            self.shell
                .set_content(self.enrollment_page.render(
                    Some(enrollment),
                    |_| {}, // TODO: wire to presenter
                    || {}, // TODO: wire to presenter
                ));
            return;
        }

        self.shell.set_content(self.main_page.render(model));
    }
}
