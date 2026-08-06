// SPDX-License-Identifier: GPL-3.0-only
//! Shared helpers for the integration tests.

#![allow(dead_code)]

use namtor_gnome::presenter::ProfilePresenter;
use namtor_gnome::ManagerClient;

/// Run a presenter action and return the resulting model clone, so tests can
/// snapshot state without borrowing the presenter.
pub fn after<C: ManagerClient>(
    presenter: &mut ProfilePresenter<C>,
    action: impl FnOnce(&mut ProfilePresenter<C>),
) -> namtor_gnome::PresenterModel {
    action(presenter);
    presenter.model().clone()
}
