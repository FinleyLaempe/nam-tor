# Collection Protocol — Consent, Withdrawal, Retention, and Raw Research Capture

**Spec:** §10.4 Data collection and consent
**Task:** 20 — Productize consented collection and withdrawal
**Commit:** `feat(collect): enforce consented PAD datasets`
**Date:** 2026-08-06
**Crate:** `namtor-collect`

## Protocol summary

`namtor-collect` records labelled bonafide and attack sessions with
strobe-differential frame pairs for MiniFASNet PAD training. The protocol
enforces four spec §10.4 invariants with code, not review:

1. **Local-only by default.** Nothing leaves the machine without explicit
   upload. The default `RetentionPolicy` is `LocalOnly`; a retention
   manifest is `export_eligible == false` until the contributor upgrades
   to `DerivedExport` or `FullExport`.
2. **Explicit, versioned, informed consent.** `ConsentRecord` carries a
   `purpose_version` and a `RetentionPolicy`. Bumping the purpose version
   invalidates older consents: a record granted for purpose v1 does not
   cover collection under purpose v2.
3. **Derived-first storage.** The product collection path stores **only**
   `DiffStats` (differential energy, gradient, specular, radial scores).
   Raw On/Off face frames are written exclusively by
   `ResearchCapture`, gated by `NAMTOR_RESEARCH_CAPTURE=1`, to a separate
   `raw/` subtree. The product store never writes `*_on.raw` / `*_off.raw`.
4. **Per-session withdrawal and deletion.** Every session has a stable
   `session_id`. `withdraw_session` is a soft delete (stamps
   `withdrawn_at_us`); `delete_session` is a hard delete. Withdrawn
   sessions are excluded from `export_session` and `retention_manifest`.

## On-disk layout

```text
<dir>/
  consent.json          — active ConsentRecord (purpose_version, retention_policy)
  <session_id>/
    meta.json           — SessionMeta (incl. metadata, withdrawn_at_us)
    pair_0000_d.json    — DiffStats (derived features) — product path
    ...
    raw/                — present ONLY if ResearchCapture wrote raw pairs
      pair_0000_on.raw
      pair_0000_off.raw
```

The product path writes `pair_NNNN_d.json` only. The `raw/` subtree exists
only when `NAMTOR_RESEARCH_CAPTURE=1` was set and `ResearchCapture` was
authorised. There is no code path by which the product store writes raw
face images.

## Types

### `ConsentRecord` (`consent.rs`)

```rust
pub struct ConsentRecord {
    pub contributor_id: String,
    pub state: ConsentState,            // Granted | Revoked
    pub granted_at_us: u64,
    pub revoked_at_us: Option<u64>,
    pub scope: String,
    pub purpose_version: u32,           // versioned purpose
    pub retention_policy: RetentionPolicy,
}

pub enum RetentionPolicy {
    LocalOnly,        // default; nothing leaves the machine
    DerivedExport,    // derived stats may leave; raw never
    FullExport,       // raw may leave IF research auth also granted
}
```

`grant(contributor, scope)` defaults to purpose v1, `LocalOnly`.
`grant_versioned(contributor, scope, version, policy)` is the explicit
constructor. `revoke()` marks the record `Revoked` and stamps
`revoked_at_us`.

### `CollectionMetadata` (`session.rs`)

```rust
pub struct CollectionMetadata {
    pub attack_type: Option<String>,    // None for bonafide
    pub device_model: String,
    pub environment: String,
    pub ambient_light_lux: Option<f32>,
    pub distance_cm: Option<u32>,
}
```

`SessionMeta` now carries `metadata: CollectionMetadata` and
`withdrawn_at_us: Option<u64>`. Both are `#[serde(default)]` so older
`meta.json` files still deserialize.

### Manifests (`manifest.rs`)

```rust
pub struct SessionManifest {
    pub session_id: String,
    pub contributor_id: String,
    pub kind: SessionKind,
    pub pair_count: u32,
    pub raw_pair_count: u32,            // 0 in the product path
    pub withdrawn: bool,
}

pub struct CollectionManifest {         // export_session() return
    pub session: SessionManifest,
    pub derived: Vec<DiffStats>,
    pub raw_eligible: bool,             // requires raw on disk AND FullExport
}

pub struct RetentionManifest {          // retention_manifest() return
    pub sessions: Vec<SessionManifest>,
    pub total_raw_pairs: u64,
    pub export_eligible: bool,          // all sessions DerivedExport+ and non-withdrawn
}
```

`permits_derived_export(policy)` and `permits_raw_export(policy)` are the
policy predicates used by the store.

### `ResearchCapture` (`research.rs`)

```rust
pub struct ResearchCapture { _private: () }

impl ResearchCapture {
    pub fn authorize() -> Result<Self, StoreError>;   // requires NAMTOR_RESEARCH_CAPTURE=1
    pub fn write_raw_pair(&self, session_dir: &Path, pair_index: u32,
                          on: &[u8], off: &[u8]) -> Result<(), StoreError>;
}
```

There is no public constructor: the type's existence *is* the proof of
authorisation. The env var must be literally `"1"`. Any other value (or
absence) yields `StoreError::ResearchNotAuthorised`.

`raw_pair_count(session_dir)` counts `*_on.raw` files under `raw/`,
returning 0 when the subtree does not exist.

## Store API (`store.rs`)

| Method | Effect |
|---|---|
| `write_consent` / `read_consent` | persist / load `ConsentRecord` |
| `require_consent(contributor)` | fail closed if consent missing, revoked, or mismatched |
| `create_session(meta)` | create `<session_id>/meta.json` |
| `write_derived(session_dir, stats)` | **product path**: write `pair_NNNN_d.json` only |
| `write_pair(session_dir, idx, on, off, stats, &ResearchCapture)` | **research path**: raw pair under `raw/` + derived |
| `withdraw_session(id)` | soft delete: stamp `withdrawn_at_us`. Idempotent. |
| `delete_session(id)` | hard delete: remove session directory |
| `delete_contributor_sessions(id)` | hard delete all sessions for a contributor (revocation) |
| `export_session(id)` | `CollectionManifest` of non-withdrawn session; `SessionWithdrawn` if withdrawn |
| `retention_manifest()` | aggregate `RetentionManifest` over all non-withdrawn sessions |
| `read_session_meta(id)` / `list_sessions()` | introspection |

`export_session` sets `raw_eligible = true` only when raw pairs exist on
disk *and* the contributor's active consent is `FullExport`. `DerivedExport`
consent with raw on disk still reports `raw_eligible == false`.

`retention_manifest` sets `export_eligible = true` only when every
non-withdrawn session's contributor has active consent with at least
`DerivedExport`. A revoked or `LocalOnly` consent, or a missing consent
record, makes the manifest not export eligible.

## Test results

`cargo test -p namtor-collect -- --test-threads=1` — **33 passed, 0 failed,
0 ignored**. `cargo clippy -p namtor-collect -- -D warnings` — clean.
`cargo fmt -p namtor-collect -- --check` — clean.

### Tests locking the productization invariants (Task 20)

| Test | Invariant locked |
|---|---|
| `grant_withdraw_delete_export_lifecycle` | grant → export → withdraw (export refuses) → delete (gone) |
| `withdrawn_session_excluded_from_retention_manifest` | withdrawn sessions do not appear in retention manifest |
| `revoked_consent_makes_manifest_not_export_eligible` | revoked consent → `export_eligible == false` |
| `local_only_consent_is_not_export_eligible` | default `LocalOnly` consent → not export eligible; `export_session.raw_eligible == false` |
| `product_enrollment_stores_no_raw_images` | product path writes no `raw/`, no `*_on.raw`/`*_off.raw`; export reports `raw_pair_count == 0` and `raw_eligible == false` |
| `versioned_consent_round_trips_and_persists` | `purpose_version` and `retention_policy` survive serialize → disk → deserialize |
| `retention_manifest_aggregates_raw_pairs_and_eligibility` | `total_raw_pairs` counts research raw pairs; `DerivedExport` keeps manifest eligible while `export_session.raw_eligible` stays false |
| `withdraw_is_idempotent` | withdrawing an already-withdrawn session is a no-op |
| `product_store_writes_no_raw_files` (research.rs) | `write_derived` creates no raw subtree or raw files |
| `write_pair_creates_files_under_raw_subdir` (store.rs) | research `write_pair` writes raw under `raw/` and derived at top level |
| `authorize_fails_without_env_var` / `authorize_succeeds_with_env_var` (research.rs) | `NAMTOR_RESEARCH_CAPTURE=1` gate enforced |

### Pre-existing tests (unchanged behaviour)

`consent_write_read_roundtrip`, `require_consent_fails_without_record`,
`require_consent_passes_with_grant`, `require_consent_fails_after_revoke`,
`create_session_writes_meta`, `delete_session_removes_directory`,
`delete_contributor_sessions_removes_all`, `list_sessions_returns_all`,
`session_id_is_unique`, `meta_carries_kind_and_contributor`,
`session_kind_serializes_snake_case`, `grant_creates_active_consent`,
`revoke_makes_inactive`, `consent_round_trip`,
`retention_policy_serializes_snake_case`,
`versioned_grant_records_policy_and_version`,
`permits_derived_export_table`, `permits_raw_export_table`,
`session_manifest_marks_withdrawn`,
`write_derived_only_creates_no_raw_files`.

## What this does NOT claim

- **No production cohort is selected.** This protocol governs how data is
  collected and retained; it does not establish a z-norm cohort. R5 remains
  WITHHELD per `docs/evidence/r5-cohort.md`.
- **No raw images are committed.** Tests use synthetic byte arrays
  (`vec![100u8; 64]`, `[1,2,3]`), never biometric data. The
  no-biometric-data-in-the-repo rule is unchanged.
- **No upload is implemented.** "Export" here means producing a manifest
  that *could* leave the machine. Actual network upload is out of scope for
  this task; the protocol is deliberately local-first and the default is
  `LocalOnly`.
- **No GUI.** This is the storage and consent layer; the GNOME wizard
  (`namtor-gnome`) is a separate crate.
