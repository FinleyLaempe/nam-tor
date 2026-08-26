// SPDX-FileCopyrightText: 2026 Nam-tor Project
// SPDX-License-Identifier: GPL-3.0-only

use namtor_client::contract;
use namtor_client::fake::FakeManagerClient;

#[test]
fn happy_path() {
    let fake = FakeManagerClient::new();
    contract::happy_path(&fake, &fake);
}

#[test]
fn error_daemon_unavailable() {
    let fake = FakeManagerClient::new();
    contract::error_daemon_unavailable(&fake, &fake);
}

#[test]
fn cancel_keeps_state_safe() {
    let fake = FakeManagerClient::new();
    contract::cancel_keeps_state_safe(&fake, &fake);
}

#[test]
fn auth_loss_preserves_profiles() {
    let fake = FakeManagerClient::new();
    contract::auth_loss_preserves_profiles(&fake, &fake);
}

#[test]
fn merge_and_replace() {
    let fake = FakeManagerClient::new();
    contract::merge_and_replace(&fake, &fake);
}
