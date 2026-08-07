# Failure Contract — Auth Outcome Privacy and Actionability

**Status: unit-evidenced (CI).** The failure contract is enforced by tests in
`namtor-daemon` (`verify.rs`) and `namtor-pam` (`lib.rs`). No hardware is
required; the contract is a pure data-flow property of the verdict → external
reason → PAM/CLI mapping.

## Scope

Task 3 retains typed safe external failure codes for CLI recovery while PAM
remains generic/fail-closed, and guarantees a PAD veto is indistinguishable
from a regular `NoMatch` (spec §8.3). The trust boundary is
`external_reason()` in `crates/namtor-daemon/src/verify.rs`: the daemon's
internal `VerifyOutcome.reason` keeps the detail (journal/audit log at debug
level); the `AuthReply.reason` sent to PAM/CLI carries only the generic
external reason.

## The contract

### Daemon → client (`AuthReply`)

`external_reason(outcome)` maps the internal `VerifyOutcome` to the external
`Option<String>` that crosses the socket:

| Internal outcome | Verdict | External reason |
|---|---|---|
| Match | `Match` | `None` |
| PAD veto (`pad veto: energy/gradient/specular/radial (…)`) | `NoMatch` | `None` — **stripped** |
| `NoMatch` with empty reason | `NoMatch` | `None` — collapsed (indistinguishable from PAD veto) |
| `NoMatch` "below threshold" | `NoMatch` | `"score too low"` — normalised (no threshold word/value) |
| `NoMatch` "no face" / "no enrollment" / "empty enrollment" / "timeout" | `NoMatch` | passed through (no biometric data) |
| `Unavailable` (no camera / store error / inference failed / pad failed / differential failed) | `Unavailable` | passed through (infrastructure, no biometric data) |
| `RateLimited` | `RateLimited` | passed through |
| `Refused` (rhost set) | `Refused` | passed through |

### CLI (`namtor-cli verify`)

Distinct exit codes per verdict retain typed recovery:

| Verdict | Exit code | Surface |
|---|---|---|
| `Match` | 0 | `MATCH (score=…)` |
| `NoMatch` | 1 | `NO MATCH (score=…)` — reason NOT surfaced (§8.3) |
| `Unavailable` | 2 | `UNAVAILABLE: {reason}` |
| `RateLimited` | 3 | `RATE LIMITED: {reason}` |
| `Refused` | 4 | `REFUSED: {reason}` |
| transport error | 5 | `verify failed: {error}` |

### PAM (`pam_namtor.so`)

Generic, fail-closed. PAM never surfaces `AuthReply.reason`; `log_fail` logs
only a hardcoded reason code literal:

| Verdict | PAM code | Journal reason code |
|---|---|---|
| `Match` | `PAM_SUCCESS` | (none) |
| `NoMatch` | `PAM_AUTH_ERR` | `no_match` |
| `Unavailable` | `PAM_AUTHINFO_UNAVAIL` | `unavailable` |
| `RateLimited` | `PAM_MAXTRIES` | `rate_limited` |
| `Refused` | `PAM_AUTHINFO_UNAVAIL` | `refused` |
| (any error/timeout/unreachable) | `PAM_AUTHINFO_UNAVAIL` | `daemon_unreachable` / `daemon_error` / `keypress_fallback` |

Only `Verdict::Match` → `PAM_SUCCESS` (fail-closed invariant, spec §8.2).

## Unit evidence (CI)

`cargo test -p namtor-daemon --lib verify -- --test-threads=1` — 21 tests green:

- `external_reason_strips_pad_veto_detail_for_nomatch` — internal keeps gate
  detail; external is `None`.
- `external_reason_strips_all_pad_veto_variants` — energy/gradient/specular/
  radial all → `None`.
- `external_reason_passes_through_non_pad_nomatch` — "no face" passes through;
  "below threshold" → "score too low"; empty reason → `None`.
- `external_reason_passes_through_unavailable_and_refused` — infrastructure
  reasons pass through.
- `external_reason_is_none_for_match` — Match carries no reason.
- `external_reason_rejects_gate_names_embeddings_and_thresholds` —
  **comprehensive privacy scan**: for every outcome variant (Match, all four
  PAD vetoes, six non-PAD NoMatch reasons, five Unavailable reasons, Refused,
  RateLimited) the external reason is scanned for forbidden tokens:
  `energy`, `gradient`, `specular`, `radial`, `pad veto`, `threshold`,
  `embedding`, `vec=`, `0x`, `template`. Any hit fails the test.
- `failure_modes_map_to_intended_verdicts` — **failure-contract table**:
  missing camera → `Unavailable`, missing store → `Unavailable`, missing
  worker → `Unavailable`, timeout → `NoMatch`, rate limit → `RateLimited`,
  PAD veto → `NoMatch` with `None` external reason. The PAD veto is asserted
  indistinguishable from a reason-less `NoMatch` (same verdict, same `None`
  external reason).
- `non_pad_reasons_carry_no_biometric_data` — non-PAD reasons carry no
  `embedding`/`template`/`pixel`/`vec=`/`0x`.

`cargo test -p namtor-pam --lib -- --test-threads=1` — 23 tests green:

- `log_line_carries_only_hardcoded_reason_codes` — for every PAM reason code
  (`no_match`, `unavailable`, `rate_limited`, `refused`, `daemon_unreachable`,
  `daemon_error`, `keypress_fallback`) the journal line is scanned for
  forbidden tokens (`pad veto`, gate names, `threshold`, `embedding`, `vec=`,
  `0x`, and the daemon's own reason strings like `no capture source` /
  `store error` / `below threshold`). Any hit fails. The line must contain
  the hardcoded reason code and the PAM code name.
- `only_match_maps_to_pam_success` — fail-closed: only `Match` → `PAM_SUCCESS`;
  every other verdict → error code.
- `verdict_to_pam_code_mapping` — the full Verdict → PAM-code table.

## Residual risk

- The contract is a static data-flow property; it cannot be broken at runtime
  without changing `external_reason` or the PAM `log_fail` reason literals.
  Both are covered by the tests above.
- The CLI surfaces `Unavailable`/`RateLimited`/`Refused` reasons verbatim.
  These are infrastructure/config strings ("no capture source", "backoff
  active", "rhost set") that carry no biometric data; the privacy scan
  asserts this. If a future reason string ever embeds a forbidden token, the
  scan fails.
- The daemon journal logs the *internal* reason at debug level (gate name +
  scalar). This is the audit trail (spec §8.3); it never crosses the socket.
  The debug-level journal is operator-only, not user-facing.
