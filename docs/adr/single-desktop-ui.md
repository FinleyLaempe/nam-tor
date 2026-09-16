<!-- SPDX-License-Identifier: GPL-3.0-only -->
# ADR: One desktop-neutral UI (supersedes the three-package split)

**Date:** 2026-09-16
**Status:** Accepted
**Supersedes:** `docs/adr/turnkey-architecture.md` — §1 (package boundaries)
and §2 (file ownership matrix), for the `namtor-kde` package only. Every
other decision in that ADR — system-bus manager, default-off PAM, exactly
one owning package per path, headless-safe core — stands unchanged.
**Spec reference:** `docs/superpowers/specs/2026-08-04-nam-tor-design.md`
§9.2 (still authoritative), §9.3 (amended: not implemented)
**Plan reference:** `docs/superpowers/plans/2026-09-16-production-readiness.md`
Task 2.1

## Context

`turnkey-architecture.md` split the release into three packages: `namtor`
(core), `namtor-gnome` (GTK4/libadwaita), and `namtor-kde` (a Plasma 6
KConfigModule in QML with a C++ shim over a Rust D-Bus bridge). Both
front-ends spoke the same system-bus contract through the shared
`namtor-client` crate, so the wire surface could not drift.

Three things changed the calculus:

1. **The second front-end cost more than it delivered.** The KCM carried
   ~5,100 lines across Rust, C++, QML and CMake, plus a C ABI whose only
   purpose was to let the C++ shim reach the Rust bridge. Every UI change
   had to be made twice, in two languages, and proven twice.
2. **It was not finished.** The KCM's lock-screen integration —
   `set_face_unlock_enabled` and its lockout-safety guard — was never
   reachable from the QML or the C ABI, which exported only the version,
   bus name and object path. Plasma users had to hand-edit
   `kscreenlockerrc` regardless.
3. **It blocked a distro.** The Debian release cell was disabled because
   Debian 12 bookworm ships no KDE Frameworks 6 at all; `libkf6kirigami-dev`
   and friends only appear in trixie. That was a hard packaging wall for a
   package most users would never install.

A GTK4/libadwaita application runs on Plasma. It does not look native in
System Settings, which is what the KCM bought — at the price above.

## Decision

**Ship one desktop UI, built with GTK4 + libadwaita, and treat it as
desktop-neutral rather than as "the GNOME one".**

1. `crates/namtor-kde` is deleted, along with its packaging assets
   (`org.namtor.kcm.*`), its `po/kde` catalogue, and every Qt6/KF6/CMake
   build dependency in the PKGBUILD, spec, `debian/control` and both
   workflows.
2. The `namtor-kde` package is **obsoleted, not merely dropped**. It
   declared an exact-version dependency on the core package, so leaving it
   installed makes the core package un-upgradable on pacman, dnf and apt
   alike. The surviving desktop package claims it: Arch
   `replaces`/`conflicts`, Fedora `Obsoletes:`, Debian `Replaces:`/`Breaks:`.
   These entries stay until no supported upgrade path starts from a release
   that shipped it.
3. `namtor-layout`'s `Package` enum gains `#[serde(other)] Unknown`. The
   install manifest is written to `/usr/share/namtor/` and read back by
   `uninstall.sh` and `namtor-cli`, which routinely read a manifest older
   than themselves; a `package = "kde"` entry from a 0.1.0 install must
   degrade to "not mine", not abort the parse.
4. The KCM's one piece of non-duplicated logic — the `kscreenlockerrc`
   authenticator writer and its lockout-safety check — moves to
   `namtor-pam-config::desktop_lockscreen`, beside the `/etc/pam.d`
   provisioning it is the other half of. It has no caller yet; its module
   documentation says so rather than pretending otherwise.

The package is **not** renamed in this change. `namtor-gnome` keeps its
name for now; renaming it to `namtor-ui` is a separate change with its own
upgrade-path obligations.

## Consequences

**Good.**
- One UI to design, test, translate and accessibility-audit.
- The Debian cell's KF6 blocker is gone. (It stays disabled for a different,
  unrelated reason: it has never been exercised end to end.)
- CI's `build` job no longer needs an `archlinux:latest` container; its only
  justification was the KF6 `-dev` set.
- `cargo test --workspace` needs no `--exclude`.

**Bad, and accepted.**
- Plasma users get a GTK application in a Qt desktop. It will not match the
  System Settings look, and it will not appear in the System Settings tree —
  it is a standalone app launched from its `.desktop` entry.
- The name `namtor-gnome` now misdescribes a desktop-neutral package. That
  is confusing until it is renamed.

**Neutral.**
- The D-Bus contract, polkit action ids and bus names are untouched. They are
  ABI for installed systems and were never desktop-specific.
- `docs/evidence/kde-qa/`, `docs/evidence/ui/kde/` and
  `docs/evidence/public-release/15-kde-*.json` are frozen release artifacts.
  They remain byte-identical and correctly describe what 0.1.0 shipped; the
  release index marks them superseded.
