# Nam-tor packaging

Systemd units, sysusers/tmpfiles drop-ins, the setuid worker-spawn helper, and
the versioned install + model provenance manifests.

## Layout

| File | Purpose |
|---|---|
| `install-manifest.toml` | Versioned DESTDIR manifest: every installable file's path, mode, owner, group, source, and symlink flag. Schema-versioned for migrations. |
| `model-manifest.toml` | Signed model manifest: per model, the source URL, sha256, tensor contract, licence URL + sha256, redistribution policy, target arch. The daemon consults this at startup. |
| `stage.sh` | Deterministic DESTDIR staging from `install-manifest.toml` (Python 3.11+ `tomllib`; no network). |
| `systemd/namtor-daemon.socket` | Socket-activated PAM socket (`/run/namtor/auth.sock`, mode 0660, owner `root:namtor-auth`) |
| `systemd/namtor-daemon.service` | Hardened daemon unit (user `namtor`, groups `video tss`, no caps, `ProtectSystem=strict`) |
| `sysusers.d/namtor.conf` | Creates user `namtor` + group `namtor-auth`, adds `namtor` to `video`/`tss` |
| `tmpfiles.d/namtor.conf` | Creates `/var/lib/namtor` (model store) and `/run/namtor` (socket dir) |
| `uninstall.sh` | Reverses an install (removes every file the manifest declares). |
| `pam-preflight.sh` | Pre-install checks: root, target PAM file, pam_namtor.so built, daemon socket, namtor-auth group, DM user membership. Exits non-zero with a specific message if any FATAL check fails, BEFORE any PAM file is touched. |
| `pam-install.sh` | Transactional PAM integration installer. Inserts a marked `# namtor-begin`/`# namtor-end` block into the target PAM file. Never destructively edits stock lines. Backs up + checksums before editing; rolls back on any failure. Refuses unsafe ordering. |
| `pam-uninstall.sh` | Transactional PAM removal. Removes only the marked block; optionally restores from backup; verifies password login intact after removal. |
| `tests/test_pam_install.sh` | Test matrix for the PAM install/uninstall scripts. Runs without root using DESTDIR temp roots. 37 assertions across 8 scenarios. |

The setuid helper binary is `namtor-worker-spawn` (built from
`crates/namtor-worker/src/bin/namtor-worker-spawn.rs`).

## The two manifests

### `install-manifest.toml` — DESTDIR source of truth

Every file Nam-tor installs is declared here with `dest`, `src` (relative to
the repo root), `mode`, `owner`, `group`, and `symlink`. Directories are
declared separately under `[[dirs]]`. The manifest is `schema_version`-ed so
packaging migrations are explicit. `stage.sh` reads it and produces a
deterministic file tree under an empty DESTDIR; running staging twice into two
empty roots yields identical trees (same paths, modes, content sha256) — proven
in [`docs/evidence/install-manifest.json`](../docs/evidence/install-manifest.json).

### `model-manifest.toml` — model provenance source of truth

For each deployed ONNX model (YuNet, SFace, MiniFASNet PAD) the manifest
declares:

- `id` — stable model id matching the on-disk dir `<models>/<id>/`
- `licence_url` + `licence_sha256` — the upstream licence text and its hash
- `redistribution_policy` — `redistributable` | `restricted`
- `target_arch` — `x86_64` | `aarch64` | `both`
- `tensor_contract` — input/output `shape`, `dtype`, `normalisation` (lets a
  consumer verify the model matches the expected interface without loading it;
  used for INT8/FP32 dispatch and model-substitution detection)
- `[[models.variants]]` — one row per quantisation (`quant`, `source_url`,
  `sha256`); INT8 and FP32 can coexist per model_id

The daemon loads this at startup
(`namtor_daemon::models::ModelManifest::parse`) and validates every required
model through `ModelFetcher::validate_against_manifest` before the worker
loads a single weight. Three gates, all fail-closed:

1. **Tampered** — the on-disk file's sha256 must equal the manifest variant's
   sha256. A mismatch blocks startup.
2. **Unaccepted** — the upstream licence must be accepted (on-disk
   `licence.accepted` record whose sha256 matches the manifest, written under
   `NAMTOR_ACCEPT_LICENCE=1`). Unaccepted blocks startup.
3. **Restricted** — `redistribution_policy = "restricted"` models require
   `ManifestGate { allow_restricted: true }` or they block.

If all three gates pass for every required model, the daemon starts without
network access (valid offline cache). Signature verification (GPG/cosign over
the manifest itself) is task 6 — this manifest carries the fields signing
needs (`sha256`, `licence_sha256`) so task 6 only adds verification.

## Install (Arch Linux; adapt paths for other distros)

### Option A — deterministic staging from the manifest

```sh
# 1. Build release artifacts (binaries + pam_namtor.so)
cargo build --release -p namtor-daemon -p namtor-worker -p namtor-cli \
  -p namtor-camera-emitter-off -p namtor-pam

# 2. Stage into a DESTDIR (deterministic: reads install-manifest.toml)
sudo ./packaging/stage.sh /mnt/staging

# 3. Merge the staged tree into /
sudo cp -a /mnt/staging/. /

# 4. Create users, groups, and runtime directories
sudo systemd-sysusers /usr/lib/sysusers.d/namtor.conf
sudo systemd-tmpfiles --create /usr/lib/tmpfiles.d/namtor.conf

# 5. Add display-manager user (e.g. gdm) to namtor-auth
sudo gpasswd -a gdm namtor-auth

# 6. Enable and start
sudo systemctl enable --now namtor-daemon.socket
sudo systemctl start namtor-daemon.service
```

### Option B — manual install (equivalent to the manifest)

```sh
# 1. Build and install binaries
cargo build --release -p namtor-daemon -p namtor-worker -p namtor-cli -p namtor-camera-emitter-off
install -Dm755 target/release/namtor-daemon         /usr/lib/namtor/namtor-daemon
install -Dm755 target/release/namtor-worker         /usr/lib/namtor/namtor-worker
install -Dm4755 target/release/namtor-worker-spawn  /usr/lib/namtor/namtor-worker-spawn  # setuid root
install -Dm755 target/release/namtor-cli            /usr/bin/namtor-cli
install -Dm755 target/release/namtor-camera-emitter-off /usr/lib/namtor/namtor-camera-emitter-off

# 2. System units + drop-ins
install -Dm644 packaging/systemd/namtor-daemon.service /usr/lib/systemd/system/namtor-daemon.service
install -Dm644 packaging/systemd/namtor-daemon.socket  /usr/lib/systemd/system/namtor-daemon.socket
install -Dm644 packaging/sysusers.d/namtor.conf        /usr/lib/sysusers.d/namtor.conf
install -Dm644 packaging/tmpfiles.d/namtor.conf        /usr/lib/tmpfiles.d/namtor.conf

# 3. Create users, groups, and runtime directories
sudo systemd-sysusers /usr/lib/sysusers.d/namtor.conf
sudo systemd-tmpfiles --create /usr/lib/tmpfiles.d/namtor.conf

# 4. Add display-manager user (e.g. gdm) to namtor-auth
sudo gpasswd -a gdm namtor-auth

# 5. Enable and start
sudo systemctl enable --now namtor-daemon.socket
sudo systemctl start namtor-daemon.service
```

Options A and B install the same tree — the manifest is the source of truth
for Option A, and Option B's commands are the manifest's entries unrolled.

## Verifying

```sh
systemctl status namtor-daemon.service
namtor-cli status
```

`namtor-cli status` should show `stream: closed` immediately after daemon
start (warm worker, cold camera — spec §5.5).

## Determinism proof

```sh
# Stage twice into two empty roots and compare the trees
./packaging/stage.sh /tmp/dest1 <SRCDIR>
./packaging/stage.sh /tmp/dest2 <SRCDIR>
diff <(tree_snapshot /tmp/dest1) <(tree_snapshot /tmp/dest2)  # no output = identical
```

The comparison (paths, modes, content sha256) is recorded in
[`docs/evidence/install-manifest.json`](../docs/evidence/install-manifest.json).

## Security notes

- `namtor-worker-spawn` is the **only** setuid binary. It drops to `nobody`
  and execs `namtor-worker`, which self-applies seccomp + landlock.
- The daemon (`namtor` user) is **not root** and has no capabilities
  (`CapabilityBoundingSet=` empty).
- `namtor-auth` group membership controls who may connect to the PAM socket.
  Add display-manager users (e.g. `gdm`) to it; do not add regular users.
- `SO_PEERCRED` is checked on every connection (spec §3.2); remote
  authentication (`PAM_RHOST` set) is always refused (spec §8.2).
- The model manifest gate is fail-closed: a tampered, unaccepted, or
  restricted model blocks daemon startup rather than degrading silently.

## Transactional PAM integration

The PAM stack is the single most safety-critical integration surface: a bad
edit can lock the user out of their own machine. `pam-install.sh`,
`pam-uninstall.sh`, and `pam-preflight.sh` implement a transactional install
protocol with backup, rollback, and refusal of unsafe ordering. The full
test matrix and evidence is in
[`docs/evidence/pam-install-matrix.md`](../docs/evidence/pam-install-matrix.md).

### Safety guarantees

1. **Never destructively edit stock PAM files.** The installer inserts a
   marked block (`# namtor-begin` ... `# namtor-end`) containing
   `auth sufficient pam_namtor.so`. Stock lines are never deleted or
   reordered. The uninstaller removes only the marked block.
2. **Password preservation.** After install, the PAM file must still have a
   password-fallback auth line (pam_unix / pam_systemd_home / common-auth /
   system-auth / password-auth) BELOW the namtor block. The installer
   verifies this and refuses if the fallback would be lost. If
   `pam_namtor.so` returns `PAM_AUTHINFO_UNAVAIL` (daemon down, timeout,
   any error), the `sufficient` control means "this module is unavailable,
   fall through" — and password auth proceeds.
3. **Backup/rollback.** Before any edit, the target PAM file is backed up
   to `<file>.namtor.bak` with a sha256 checksum. If any step after the
   backup fails, the backup is restored. The checksum guards against a
   tampered backup being trusted for restore.
4. **Upgrade migration.** Re-running the installer on an already-installed
   system removes the old block and inserts the new one in place. The
   original backup is preserved (not overwritten) so the pre-namtor state
   is always recoverable. The result is byte-identical to the first install.
5. **Uninstall restoration.** The uninstaller removes only the marked block
   and verifies password login is intact. With `--restore-backup` it
   restores the file from the checksum-verified backup. With
   `--purge-backup` it cleans up the backup files after a successful
   removal.
6. **Unsafe ordering refused.** The installer refuses:
   - `auth required` / `auth requisite` / `auth [default=bad]` /
     `auth [default=die]` control fields — a daemon outage with `required`
     would lock users out (spec §8.3). Only `sufficient` or
     `[default=ignore]` are accepted.
   - Insertion after `pam_deny.so` — the block would be unreachable.
   - `pam_deny.so` between the block and the password fallback — the
     fall-through would hit `deny` before password.

### Preflight checks

`pam-preflight.sh` runs BEFORE any PAM file is touched. FATAL checks (exit
non-zero): not root (unless `DESTDIR` is set for testing), target PAM file
missing, `pam_namtor.so` not built/installed, no password-fallback auth line
in the target. SOFT checks (warn only): daemon socket not present, namtor-auth
group not created, display-manager user not in namtor-auth — these may
legitimately not hold yet during a first install.

### Install

```sh
# 1. Build and install pam_namtor.so (see install-manifest.toml / stage.sh)
cargo build --release -p namtor-pam
sudo install -Dm755 target/release/libpam_namtor.so /usr/lib/security/pam_namtor.so

# 2. Run preflight (optional — pam-install.sh runs it automatically)
sudo ./packaging/pam-preflight.sh

# 3. Install the PAM integration (default target: /etc/pam.d/gdm-password)
sudo ./packaging/pam-install.sh

# For a different target (e.g. Arch system-auth, Fedora password-auth):
sudo ./packaging/pam-install.sh --target /etc/pam.d/system-auth
```

The installer inserts:
```
# namtor-begin
auth sufficient pam_namtor.so
# namtor-end
```
before the first `auth` line in the target file.

### Uninstall

```sh
# Remove the marked block only (least destructive)
sudo ./packaging/pam-uninstall.sh

# Restore the original file from the checksum-verified backup
sudo ./packaging/pam-uninstall.sh --restore-backup

# Remove the block and purge the backup files
sudo ./packaging/pam-uninstall.sh --purge-backup

# Different target
sudo ./packaging/pam-uninstall.sh --target /etc/pam.d/system-auth
```

### Testing without root

All three scripts honour `DESTDIR` for testing against a fake root:

```sh
# Run the full test matrix (no root required)
bash packaging/tests/test_pam_install.sh
```

The test script builds fake PAM files (Arch-style, Fedora-style, pathological
deny-first, no-fallback) in `mktemp -d` roots, runs the installer/uninstaller
with `DESTDIR`, and verifies the file tree. 37 assertions across 8 scenarios:
install preserves fallback, uninstall restores original, broken module caught
by preflight, daemon failure leaves PAM intact (stack shape + source-level
fail-closed contract), upgrade idempotent, unsafe ordering refused, rollback
restores backup.
