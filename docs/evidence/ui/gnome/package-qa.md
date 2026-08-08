# GNOME App Packaging QA — Task 20

**Date:** 2026-08-08
**Commit:** (this commit)
**Scope:** Package and system-test the GNOME management UI (`namtor-gnome`).

## What was packaged

| Asset | Install path | Manifest source |
|---|---|---|
| Binary | `/usr/bin/namtor-enroll-wizard` | `target/release/namtor-enroll-wizard` |
| Desktop entry | `/usr/share/applications/org.namtor.Wizard.desktop` | `packaging/applications/org.namtor.Wizard.desktop` |
| AppStream metainfo | `/usr/share/metainfo/org.namtor.Wizard.metainfo.xml` | `packaging/metainfo/org.namtor.Wizard.metainfo.xml` |
| Icon (SVG) | `/usr/share/icons/hicolor/scalable/apps/org.namtor.Wizard.svg` | `packaging/icons/hicolor/scalable/apps/org.namtor.Wizard.svg` |
| Translations (structure) | `packaging/po/` (POTFILES.in, .pot template, README) | not in install-manifest (distro packaging concern) |

## Application identity

- **App ID:** `org.namtor.Wizard` — matches `gtk4::Application::builder().application_id("org.namtor.Wizard")` in `wizard.rs`.
- **Binary:** `namtor-enroll-wizard` (the `[[bin]]` target in `crates/namtor-gnome/Cargo.toml`).
- **StartupWMClass:** `org.namtor.Wizard` — ensures the window maps to the desktop entry under Wayland.

## Settings search metadata

GNOME Shell and Settings search discover the app via:
1. **Desktop entry `Keywords` field:** `face;authentication;biometric;iris;infrared;login;security;namtor;`
2. **AppStream `<keywords>` section:** same set as structured XML.
3. **Categories:** `Settings;Security;GTK;GNOME;` — `Settings` is the single main category (freedesktop hint compliance).

## Validators

### desktop-file-validate

```
$ desktop-file-validate packaging/applications/org.namtor.Wizard.desktop
EXIT: 0
```

Result: **PASS** — no errors, no hints.

### appstreamcli validate

```
$ appstreamcli validate --no-net packaging/metainfo/org.namtor.Wizard.metainfo.xml
✔ Validation was successful: pedantic: 1
EXIT: 0
```

Result: **PASS** — no errors, no warnings. One pedantic hint
(`cid-contains-uppercase-letter`) is expected: GNOME app IDs conventionally
use mixed case (e.g. `org.gnome.Calculator`). The `--no-net` flag skips URL
reachability checks since the project repository is not yet public.

`appstream-util` is not installed on this system; `appstreamcli` is the
modern replacement and covers the same validation surface.

## Source-tree path assumptions removed

### wizard.rs recovery message

The daemon-unreachable recovery page in `crates/namtor-gnome/src/bin/wizard.rs`
hardcoded `systemctl --user start namtor-daemon` and
`journalctl --user -u namtor-daemon`. The daemon is a **system** service
(`namtor-daemon.service` under `/usr/lib/systemd/system/`), not a user
service. Fixed to:

```
• Start the daemon: sudo systemctl start namtor-daemon
• Verify the D-Bus service file is installed
• Check journalctl -u namtor-daemon
```

No other source-tree path assumptions exist in the GNOME crate. The D-Bus
client (`dbus_client.rs`) connects via `zbus::blocking::Connection::session()`
with no hardcoded paths; the bus name, object path, and interface are
compile-time constants, not filesystem paths.

## Build verification

```
$ cargo build -p namtor-gnome
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.35s

$ cargo clippy -p namtor-gnome -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.33s

$ cargo fmt -p namtor-gnome -- --check
EXIT: 0
```

Result: **PASS** — build, clippy, and fmt all clean.

## Staging verification

```
$ ./packaging/stage.sh /tmp/namtor-gnome-stage-test
staged 11 dirs + 18 files into /tmp/namtor-gnome-stage-test
EXIT: 0
```

Staged GNOME assets verified present:
```
/tmp/namtor-gnome-stage-test/usr/bin/namtor-enroll-wizard
/tmp/namtor-gnome-stage-test/usr/share/applications/org.namtor.Wizard.desktop
/tmp/namtor-gnome-stage-test/usr/share/icons/hicolor/scalable/apps/org.namtor.Wizard.svg
/tmp/namtor-gnome-stage-test/usr/share/metainfo/org.namtor.Wizard.metainfo.xml
```

## Distro packaging updates

### Arch (PKGBUILD)
- Added `-p namtor-gnome` to `cargo build --release --frozen`.
- Added `gtk4` and `libadwaita` to `depends`.

### Fedora (namtor.spec)
- Added `-p namtor-gnome` to `cargo build --release --frozen`.
- Added `Requires: gtk4` and `Requires: libadwaita`.
- Added GNOME file entries to `%files`.

### Debian (control + rules)
- Added `-p namtor-gnome` to `override_dh_auto_build`.
- Added `libgtk-4-1` and `libadwaita-1-0` to `Depends`.

## Pre-existing test failures (out of scope)

Two test failures exist on HEAD before this task's changes, confirmed via
`git stash` comparison:

1. `tests/privacy_scan.rs` — `missing field always_on_emitter in
   initializer of StatusInfo`. The `StatusInfo` struct gained an
   `always_on_emitter` field but the test initializer was not updated.

2. `tests/presenter_flow.rs::service_not_registered_has_recovery_hint` —
   `assertion failed: hint.title.to_lowercase().contains("bus")`.

Both are pre-existing and unrelated to packaging. They are left for the
GNOME task owners to fix.

## WITHHELD: Clean GNOME VM launch test

Launching the app in a clean GNOME VM and driving the fake-service
lifecycle (discover, launch, enroll with fake D-Bus, verify recovery on
daemon-down) requires a GNOME VM with GTK4/libadwaita installed. This is
**WITHHELD** — it needs a graphical VM environment that is not available
in this session. The validators, build, and staging proof above cover the
packaging correctness gate. The VM launch test is the next HIL step.

## Privacy

No biometric data appears in any packaging metadata:
- Desktop entry: no faces, no thresholds, no model identifiers.
- AppStream metainfo: descriptive text only; no biometric payloads.
- Icon: abstract shield outline — no face or biometric imagery.
- Translations: POTFILES.in lists only UI string sources; no biometric
  data is translatable.
