# ADR: Turnkey Release Architecture

**Date:** 2026-08-08  
**Status:** Accepted; **partially superseded 2026-09-16** by
`docs/adr/single-desktop-ui.md`, which removes the `namtor-kde` package and,
in its 2026-09-16 follow-up, renames `namtor-gnome` to `namtor-ui`. Read
every `namtor-gnome` below as today's `namtor-ui`. Everything else here
still holds. The three-package text below is left unedited as the record of
what 0.1.0 shipped.  
**Spec reference:** `docs/superpowers/specs/2026-08-04-nam-tor-design.md` §9  
**Plan reference:** `model-licensing-strategy` Task 1  

## Context

Nam-tor ships as three split packages — `namtor` (core), `namtor-gnome`, and
`namtor-kde` — for Arch, Fedora, and Debian. Every runtime and generated file
must have exactly one owning package so that upgrades preserve state, removal
purges state, and no two packages claim the same path.

The manager D-Bus service runs on the **system bus** (`org.namtor.Manager`).
GNOME and KDE clients connect to the system bus (not session bus) and
generate bindings from the frozen introspection XML.

PAM authentication is **default-off at install time**. The user must
independently opt into each detected PAM service through the native settings
surface. No package install script edits PAM files.

## Decision

### 1. Package boundaries

| Package | Owns | Depends on |
|---|---|---|
| `namtor` (core) | Daemon, worker, PAM module, CLI, models, licences, systemd units, D-Bus service, polkit policy, sysusers/tmpfiles, PAM scripts | — |
| `namtor-gnome` | GTK4/libadwaita wizard binary, .desktop, metainfo, icon, gettext | `namtor` (= version) |
| `namtor-kde` | KCM QML module, C++ shim, .desktop, metainfo, icon, gettext | `namtor` (= version) |

Desktop packages depend on the exact core version. Core works headless
without either desktop package.

### 2. File ownership matrix

Every installed path maps to exactly one owning package. "Generated" marks
files created at runtime (not staged from the manifest).

#### Core (`namtor`)

| Path | Type | Owner |
|---|---|---|
| `/usr/lib/namtor/namtor-daemon` | binary | core |
| `/usr/lib/namtor/namtor-worker` | binary | core |
| `/usr/lib/namtor/namtor-camera-emitter-off` | binary | core |
| `/usr/bin/namtor-cli` | binary | core |
| `/usr/lib/security/pam_namtor.so` | PAM module (cdylib) | core |
| `/usr/lib/systemd/system/namtor-daemon.service` | systemd unit | core |
| `/usr/lib/systemd/system/namtor-daemon.socket` | systemd socket unit | core |
| `/usr/lib/sysusers.d/namtor.conf` | sysusers drop-in | core |
| `/usr/lib/tmpfiles.d/namtor.conf` | tmpfiles drop-in | core |
| `/usr/share/dbus-1/system-services/org.namtor.Manager.service` | D-Bus service activation | core |
| `/usr/share/polkit-1/actions/org.namtor.policy` | polkit policy | core |
| `/usr/share/namtor/model-manifest.toml` | model manifest | core |
| `/usr/share/namtor/install-manifest.toml` | install manifest | core |
| `/usr/share/namtor/pam-install.sh` | PAM install script | core |
| `/usr/share/namtor/pam-uninstall.sh` | PAM uninstall script | core |
| `/usr/share/namtor/pam-preflight.sh` | PAM preflight script | core |
| `/usr/share/doc/namtor/README.md` | packaging docs | core |
| `/var/lib/namtor/models/<id>/` | model weights + licence records | core (generated) |
| `/var/lib/namtor/templates/<uid>/` | encrypted face templates | core (generated) |
| `/var/lib/namtor/pam-registry.json` | enabled-PAM-service registry | core (generated) |
| `/run/namtor/auth.sock` | PAM auth socket | core (generated, tmpfiles) |

#### GNOME (`namtor-gnome`)

| Path | Type | Owner |
|---|---|---|
| `/usr/bin/namtor-enroll-wizard` | GTK4 wizard binary | gnome |
| `/usr/share/applications/org.namtor.Wizard.desktop` | .desktop entry | gnome |
| `/usr/share/metainfo/org.namtor.Wizard.metainfo.xml` | AppStream metainfo | gnome |
| `/usr/share/icons/hicolor/scalable/apps/org.namtor.Wizard.svg` | icon | gnome |

#### KDE (`namtor-kde`)

| Path | Type | Owner |
|---|---|---|
| `/usr/lib/qt6/qml/org/namtor/kcm/` (CMake install) | QML module + C++ shim | kde |
| `/usr/share/applications/org.namtor.kcm.desktop` | .desktop entry | kde |
| `/usr/share/metainfo/org.namtor.kcm.metainfo.xml` | AppStream metainfo | kde |
| `/usr/share/icons/hicolor/scalable/apps/org.namtor.kcm.svg` | icon | kde |

### 3. System-bus ownership

The manager service owns `org.namtor.Manager` on the **system bus** at object
path `/org/namtor/Manager`. This is the only bus the daemon connects to. GNOME
and KDE clients connect to the system bus to call manager methods and receive
signals. No session-bus usage is permitted in the manager path.

D-Bus service activation is via
`/usr/share/dbus-1/system-services/org.namtor.Manager.service` (core-owned).

### 4. PAM-default-off policy

At package install time, **zero PAM services are enabled**. The PAM module
(`pam_namtor.so`) is installed but no PAM stack file contains a namtor block.
The user must:

1. Open the native settings surface (GNOME or KDE).
2. Enroll a face.
3. Independently toggle each detected PAM service (gdm-password, sddm, sudo,
   su, polkit, etc.) via the `SetPamServiceEnabled` D-Bus method.

Package install scripts (`postinst`, `namtor.install`, etc.) must NOT call
`pam-install.sh` or edit any PAM file. The PAM scripts are available for
manual/admin use only.

### 5. Upgrade and purge semantics

**Upgrade** preserves all state:
- Profiles (`/var/lib/namtor/templates/`) — preserved.
- PAM registry (`/var/lib/namtor/pam-registry.json`) — preserved; enabled
  services remain enabled.
- Models (`/var/lib/namtor/models/`) — preserved; hashes re-validated at
  daemon startup.
- Desktop files, icons, metainfo — replaced by new versions.

**Removal** (`apt remove`, `dnf remove`, `pacman -R`) purges biometric state:
- PAM blocks removed from every enabled service (via registry).
- `/var/lib/namtor/templates/` — purged.
- `/var/lib/namtor/pam-registry.json` — purged.
- `/var/lib/namtor/models/` — purged.
- `/run/namtor/` — purged.
- Sysusers/tmpfiles entries — removed.
- Desktop integration files — removed.

**Debian conffile policy:** `apt remove` preserves conffiles (configs);
`apt purge` removes them. Both paths remove biometric/key/model state.

### 6. TPM fallback

Template storage uses TPM2 2.0 sealing when a usable TPM is present
(`/dev/tpmrm0`). When TPM is unavailable, the daemon atomically creates a
software AES-256-GCM key (file fallback, mode 0600, owned by root:namtor).

The `ProtectionStatus` v2 D-Bus type exposes:
- `sealing`: `"tpm2"` or `"software"` (never downgrades silently).
- `has_tpm`: whether a TPM2 device exists.
- `warning`: human-readable warning when using software fallback.

An existing TPM-bound store is **never silently downgraded** to software. If
TPM is lost, the daemon reports the mismatch and requires re-enrollment.

### 7. Model ownership

Models are core-owned. The three deployed models live under
`/var/lib/namtor/models/`:

| Model | Path | Licence | Owner |
|---|---|---|---|
| YuNet 2023mar | `/var/lib/namtor/models/yunet_2023mar/` | MIT | core |
| SFace 2021dec | `/var/lib/namtor/models/sface_2021dec/` | Apache-2.0 | core |
| MiniFASNet PAD | `/var/lib/namtor/models/minifasnet_pad/` | self-trained (GPL-3.0) | core |

The model manifest (`/usr/share/namtor/model-manifest.toml`) declares each
model's source URL, sha256, tensor contract, licence URL + sha256,
redistribution policy, and target arch. The daemon validates every model
before worker spawn (fail-closed on tamper, unaccepted, or restricted).

### 8. D-Bus contract versioning

The v1 interface (`org.namtor.Manager`, `API_VERSION = 1`) is frozen
byte-for-byte in `docs/evidence/dbus-contract.xml`. v1 tests assert the
exact method/signal/property/error sets and argument signatures. v1 is
never modified.

The v2 interface (`org.namtor.Manager2`, `API_VERSION_V2 = 2`) is frozen in
`docs/evidence/dbus-contract-v2.xml`. The v2 file contains the v1 interface
(preserved byte-for-byte) plus the v2 interface with PAM service management
and protection status. v2 tests assert the exact v2 method/signal/property/
error sets, argument signatures, typed struct round-trips, and mutation
guards.

A v2-capable server serves both v1 and v2 clients. Version negotiation via
`negotiate_version_v2` accepts v1 and v2; any other version fails safely.

## Consequences

- No two packages claim the same path; collision is a CI gate (Task 3).
- PAM is never silently enabled; the user explicitly opts in per service.
- Upgrade preserves user state; removal purges all biometric data.
- TPM fallback is visible, not silent; the user sees a warning.
- The v1 contract is immutable; v2 additions are additive only.
- GNOME and KDE clients generate from the same introspection XML, ensuring
  wire-level parity.
