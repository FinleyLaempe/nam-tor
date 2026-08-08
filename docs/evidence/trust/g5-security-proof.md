# G5 Security Proof

Maps each G5 production gate to its test implementation.

| Gate | Description | Test | Status |
|---|---|---|---|
| G5.1 | PAM < 500KB, no ML deps | ci structural test | PASS |
| G5.2 | Worker sandbox (seccomp+landlock) | namtor-worker sandbox tests | PASS |
| G5.3 | SO_PEERCRED on every connection | namtor-daemon acl tests | PASS |
| G5.4 | PAM_RHOST set → refuse | namtor-pam tests | PASS |
| G5.5 | No biometric data in logs | privacy scan test | PASS |
| G5.6 | No unwrap/expect in non-test code | g5_no_unwrap.rs | PASS |

Security invariants: PAM module contains no ML code. Worker cannot open files/network.
PAD is a veto, not a weighted term. Worker sandbox never widens silently.
