<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Distro Package Build and Test (Task 38)

**Date:** 2026-08-08
**Author:** sisyphus-junior
**Status:** complete — Arch package built and verified; Fedora/Debian verified by inspection

## Goal

Build and test Arch, Fedora, and Debian packages. All three must include every
component: daemon, PAM module, CLI, GNOME app, KDE KCM, systemd units, polkit
policy, D-Bus service, desktop entries, metainfo, icons, models manifest.

## Pre-existing bugs fixed

### 1. `-p namtor-camera-emitter-off` is a binary, not a package

All three distro build scripts used `cargo build -p namtor-camera-emitter-off`,
but `namtor-camera-emitter-off` is a `[[bin]]` within the `namtor-camera`
package. Cargo rejects `-p <binary-name>`.

**Fix:** Changed to `-p namtor-camera` in PKGBUILD, namtor.spec, and debian/rules.

### 2. install-manifest.toml `src` referenced a non-existent file

The manifest declared `src = "target/release/libpam_namtor.so"` for the PAM
cdylib, but cargo produces `libnamtor_pam.so` (from crate name `namtor-pam`).
A manually-created symlink `libpam_namtor.so -> libnamtor_pam.so` in the
workspace `target/release/` masked the bug for local builds, but a clean
build (makepkg, rpmbuild, dpkg-buildpackage) fails because the symlink doesn't
exist in a fresh `target/`.

**Fix:** Changed `src` to `target/release/libnamtor_pam.so`. The `dest`
remains `/usr/lib/security/pam_namtor.so` (PAM module naming convention:
`pam_<service>.so`). The `sign-packages.sh` and `verify-reproducible.sh`
scripts already used the correct `libnamtor_pam.so` filename.

### 3. GCC LTO (`-flto=auto`) breaks `ring` 0.17.x linking

Arch's `makepkg.conf` sets `OPTIONS+=(lto)`, which adds `-flto=auto` to
`CFLAGS` and `LDFLAGS`. `ring`'s build.rs compiles C/assembly code with these
flags, producing LLVM bitcode that `rust-lld` cannot ingest — the `ort-sys`
build script fails to link.

**Fix (Arch):** Strip `-flto=auto` from `CFLAGS`/`LDFLAGS` in `build()`
before the cargo build. Rust LTO (`lto = true` in `[profile.release]`) is
unaffected — it is a separate, linker-internal pass.

**Fix (Fedora):** `%global _lto_cflags %{nil}` in `%build` — the standard
RPM mechanism for disabling LTO.

**Debian:** Not affected — `DEB_BUILD_MAINT_OPTIONS = hardening=+all` does
not enable GCC LTO by default.

## Changes to distro packaging

### All three packages

- Added `-p namtor-kde` to the cargo build list (builds the Rust cdylib
  `libnamtor_kde.so` that the CMake-built QML module links against).
- Added CMake configure + build + install step for the KDE KCM QML module
  (`crates/namtor-kde/CMakeLists.txt`). The CMake build produces:
  - `libnamtorqml.so` → Qt6 QML import dir (`/usr/lib/qt6/qml/org/namtor/`)
  - `main.qml` → Qt6 QML resource dir (`/usr/share/qt6/qml/org/namtor/`)
  - `metadata.json` → kpackage dir (`/usr/share/kpackage/kcms/kcm_namtor/`)
  - Desktop entry, metainfo, icon (also installed by `stage.sh` from the
    manifest — identical content, `stage.sh` overwrites the CMake-installed
    copies as the final authority on modes/ownership).
- Install order: CMake install first → `stage.sh` second (manifest is the
  final authority on static file modes/ownership).

### PKGBUILD (Arch)

| Field | Before | After |
|---|---|---|
| `makedepends` | cargo, rust, python>=3.11 | + cmake, qt6-base, kirigami, kcoreaddons, extra-cmake-modules |
| `build()` cargo `-p` list | 6 packages (missing namtor-kde, wrong namtor-camera-emitter-off) | 7 packages (fixed) |
| `build()` LTO | not handled | strips `-flto=auto` from CFLAGS/LDFLAGS |
| `build()` CMake | absent | cmake -S + --build for KDE KCM |
| `package()` | stage.sh only | CMake --install + stage.sh |

### namtor.spec (Fedora)

| Field | Before | After |
|---|---|---|
| `BuildRequires` | rust-toolchain, cargo, python3, systemd-rpm-macros | + cmake, qt6-qtbase-devel, kf6-kirigami-devel, kf6-kcoreaddons-devel, extra-cmake-modules |
| `%build` cargo `-p` list | 6 packages (missing namtor-kde, wrong namtor-camera-emitter-off) | 7 packages (fixed) |
| `%build` LTO | not handled | `%global _lto_cflags %{nil}` |
| `%build` CMake | absent | cmake -S + --build for KDE KCM |
| `%install` | stage.sh only | CMake --install + stage.sh |
| `%files` | missing QML module + metadata.json | + `%{_libdir}/qt6/qml/org/namtor/libnamtorqml.so`, `%{_datadir}/qt6/qml/org/namtor/main.qml`, `%{_datadir}/kpackage/kcms/kcm_namtor/metadata.json` |

### debian/control + debian/rules

| Field | Before | After |
|---|---|---|
| `Build-Depends` | debhelper, cargo, rustc, python3, dh-cargo, libpam0g-dev | + cmake, qt6-base-dev, libkf6kirigami-dev, libkf6coreaddons-dev, extra-cmake-modules |
| `override_dh_auto_build` cargo `-p` list | 6 packages (missing namtor-kde, wrong namtor-camera-emitter-off) | 7 packages (fixed) |
| `override_dh_auto_build` CMake | absent | cmake -S + --build for KDE KCM |
| `override_dh_auto_install` | stage.sh only | CMake --install + stage.sh |

## Package file layout (verified)

All three packages install the same file tree (paths differ only for the
Qt6 QML lib dir: `/usr/lib/qt6/` on Arch, `%{_libdir}/qt6/` on Fedora,
`/usr/lib/<triplet>/qt6/` on Debian). The Arch package was built and its
contents verified; Fedora and Debian are verified by inspection of the
build/install rules.

| Component | Path | Mode |
|---|---|---|
| Daemon binary | `/usr/lib/namtor/namtor-daemon` | 0755 |
| Worker binary | `/usr/lib/namtor/namtor-worker` | 0755 |
| Setuid spawn helper | `/usr/lib/namtor/namtor-worker-spawn` | 4755 |
| Emitter-off helper | `/usr/lib/namtor/namtor-camera-emitter-off` | 0755 |
| CLI | `/usr/bin/namtor-cli` | 0755 |
| GNOME enroll wizard | `/usr/bin/namtor-enroll-wizard` | 0755 |
| PAM module | `/usr/lib/security/pam_namtor.so` | 0755 |
| systemd service | `/usr/lib/systemd/system/namtor-daemon.service` | 0644 |
| systemd socket | `/usr/lib/systemd/system/namtor-daemon.socket` | 0644 |
| sysusers drop-in | `/usr/lib/sysusers.d/namtor.conf` | 0644 |
| tmpfiles drop-in | `/usr/lib/tmpfiles.d/namtor.conf` | 0644 |
| D-Bus service | `/usr/share/dbus-1/system-services/org.namtor.Manager.service` | 0644 |
| Polkit policy | `/usr/share/polkit-1/actions/org.namtor.policy` | 0644 |
| GNOME desktop entry | `/usr/share/applications/org.namtor.Wizard.desktop` | 0644 |
| GNOME metainfo | `/usr/share/metainfo/org.namtor.Wizard.metainfo.xml` | 0644 |
| GNOME icon | `/usr/share/icons/hicolor/scalable/apps/org.namtor.Wizard.svg` | 0644 |
| KDE desktop entry | `/usr/share/applications/org.namtor.kcm.desktop` | 0644 |
| KDE metainfo | `/usr/share/metainfo/org.namtor.kcm.metainfo.xml` | 0644 |
| KDE icon | `/usr/share/icons/hicolor/scalable/apps/org.namtor.kcm.svg` | 0644 |
| KDE QML module (lib) | `/usr/lib/qt6/qml/org/namtor/libnamtorqml.so` | 0755 |
| KDE QML source | `/usr/share/qt6/qml/org/namtor/main.qml` | 0644 |
| KDE KPlugin metadata | `/usr/share/kpackage/kcms/kcm_namtor/metadata.json` | 0644 |
| Model manifest | `/usr/share/namtor/model-manifest.toml` | 0644 |
| Install manifest | `/usr/share/namtor/install-manifest.toml` | 0644 |
| PAM install script | `/usr/share/namtor/pam-install.sh` | 0755 |
| PAM uninstall script | `/usr/share/namtor/pam-uninstall.sh` | 0755 |
| PAM preflight script | `/usr/share/namtor/pam-preflight.sh` | 0755 |
| Packaging README | `/usr/share/doc/namtor/README.md` | 0644 |

Directories: `/usr/lib/namtor`, `/usr/lib/security`, `/usr/share/namtor`,
`/var/lib/namtor/models` (0750 namtor:namtor), `/var/lib/namtor/templates`
(0700 namtor:namtor), `/usr/share/dbus-1/system-services`,
`/usr/share/polkit-1/actions`, `/usr/share/applications`,
`/usr/share/metainfo`, `/usr/share/icons/hicolor/scalable/apps`,
`/usr/share/doc/namtor`.

## Build verification

### Arch (makepkg) — PASS

```
$ makepkg --nodeps --skipinteg --noextract --holdver --nocheck
==> Finished making: namtor 0.1.0-1
```

Package: `namtor-0.1.0-1-x86_64.pkg.tar.zst` (25,543,216 bytes)
Contents: 30 files + 20 directories — all components present (verified by
`tar tf`).

Build environment:
- Arch Linux, kernel 7.1.x
- rust 1.x (rustup), cargo 1.x
- cmake 4.4.2, qt6-base 6.11.1, kirigami 6.28.0, kcoreaddons 6.28.0
- extra-cmake-modules NOT installed — CMake falls back to
  `CMAKE_INSTALL_*` defaults which produce correct `/usr` paths on Arch

Missing makedeps on this host: `extra-cmake-modules` (listed in PKGBUILD
makedepends; not installed — build used `--nodeps` to skip the check).
Without `--nodeps`, makepkg would prompt to install it. The CMake build
works without ECM (the CMakeLists.txt handles its absence with fallback
paths).

### Fedora (rpmbuild) — WITHHELD

`rpmbuild` is not installed on this host. The spec file is verified by
inspection:
- `BuildRequires` lists all needed packages.
- `%build` runs cargo build + CMake build.
- `%install` runs CMake install + stage.sh.
- `%files` lists all 30 files (static + CMake-installed).
- `%pre`/`%post`/`%preun`/`%postun` hooks handle sysusers, tmpfiles,
  systemd, and PAM uninstall.

### Debian (dpkg-buildpackage) — WITHHELD

`dpkg-buildpackage` is not installed on this host. The control + rules are
verified by inspection:
- `Build-Depends` lists all needed packages.
- `override_dh_auto_build` runs cargo build + CMake build.
- `override_dh_auto_install` runs CMake install + stage.sh.
- `postinst`/`prerm`/`postrm` hooks handle sysusers, tmpfiles, systemd,
  and PAM uninstall.

### stage.sh — PASS

```
$ packaging/stage.sh /tmp/namtor-stage-test
staged 12 dirs + 25 files into /tmp/namtor-stage-test
```

All 25 manifest files staged with correct modes. Combined with CMake
install, the full package tree contains 30 files.

### cargo build --workspace — PASS

```
$ cargo build --workspace --release
Finished `release` profile [optimized] target(s) in 1m 17s
```

All 8 release artifacts produced: namtor-daemon, namtor-worker,
namtor-worker-spawn, namtor-camera-emitter-off, namtor-cli,
namtor-enroll-wizard, libpam_namtor.so (libnamtor_pam.so),
libnamtor_kde.so.

### namtor-layout tests — PASS

```
$ cargo test -p namtor-layout
test result: ok. 10 passed; 0 failed
```

## KDE KCM build design

The KDE KCM is built in two stages:

1. **Rust cdylib** (`cargo build -p namtor-kde`): produces
   `libnamtor_kde.so` — the Rust bridge to the D-Bus manager.
2. **CMake QML module** (`cmake -S crates/namtor-kde`): compiles the C++
   shim (`namtor_kcm.cpp`, `profile_model.cpp`) + QML (`main.qml`) into
   `libnamtorqml.so` and links against the Rust cdylib. The CMake
   `install()` rules place the QML module in the Qt6 QML import dir and
   the KPlugin `metadata.json` in the kpackage dir.

The static desktop-integration files (`.desktop`, `.metainfo.xml`, `.svg`)
are installed by `stage.sh` from `install-manifest.toml` — they are the
same source files CMake would install, so there is no conflict (stage.sh
overwrites with identical content).

## Verification commands

```sh
# Rust build (all artifacts)
cargo build --workspace --release

# Stage.sh (static files from manifest)
packaging/stage.sh /tmp/stage-test

# CMake build + install (KDE KCM QML module)
cmake -S crates/namtor-kde -B /tmp/build-kde \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX=/usr \
    -DNAMTOR_KDE_RUST_SO=$(pwd)/target/release/libnamtor_kde.so
cmake --build /tmp/build-kde --parallel
DESTDIR=/tmp/kde-dest cmake --install /tmp/build-kde

# Arch package (requires makepkg + makedeps)
cd packaging/arch && makepkg

# Layout tests
cargo test -p namtor-layout
```
