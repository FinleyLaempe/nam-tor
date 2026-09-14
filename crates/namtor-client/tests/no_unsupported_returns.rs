// SPDX-FileCopyrightText: 2026 Nam-tor Project
// SPDX-License-Identifier: GPL-3.0-only

//! Asserts every `ManagerClient` v3 method returns a real result (not
//! `ManagerError::Unsupported`) for at least one implementation
//! (`FakeManagerClient`). This guards against the pre-refactor pattern
//! where v3 methods had default trait bodies returning `Unsupported`.

use namtor_client::client::ManagerClient;
use namtor_client::fake::FakeManagerClient;
use namtor_client::types::ManagerError;

fn assert_not_unsupported<T>(result: Result<T, ManagerError>) {
    if let Err(ManagerError::Unsupported(msg)) = result {
        panic!("v3 method returned Unsupported: {msg}");
    }
}

#[test]
fn v3_methods_never_return_unsupported() {
    let fake = FakeManagerClient::new();

    let session = fake.start_enrollment_v3(1000, "Alice");
    assert_not_unsupported(session.as_ref().map(|_| ()).map_err(|e| e.clone()));
    let session = session.expect("start_enrollment_v3 should succeed on fresh fake");
    let session_str = session.0.clone();

    assert_not_unsupported(fake.poll_progress_v3(&session_str));
    assert_not_unsupported(fake.review_enrollment_v3(&session_str));
    assert_not_unsupported(fake.commit_profile_v3(&session_str));

    let session2 = fake
        .start_enrollment_v3(1000, "Bob")
        .expect("second start_enrollment_v3");
    assert_not_unsupported(fake.stop_enrollment_v3(&session2.0));
    assert_not_unsupported(fake.resume_enrollment_v3(&session2.0));
    assert_not_unsupported(fake.cancel_enrollment_v3(&session2.0));
    assert_not_unsupported(fake.negotiate_v3(3));
}
