# SPDX-License-Identifier: GPL-3.0-only
#
# KDE KCM Package QA Evidence — Task 25
#
# Records the build, validator, accessibility, i18n, and packaging results
# for the Nam-tor KDE Plasma 6 KCM (crates/namtor-kde).

# KDE KCM Package QA Evidence

**Date:** 2026-08-08  
**Task:** 25 — Package and finish KDE KCM  
**Commit:** (this commit)  
**Status:** PASS (structural); WITHHELD (runtime HIL)

## 1. Build

| Gate | Command | Result |
|---|---|---|
| Rust build | `cargo build -p namtor-kde` | PASS |
| Rust clippy | `cargo clippy -p namtor-kde -- -D warnings` | PASS (0 warnings) |
| Rust fmt | `cargo fmt -p namtor-kde -- --check` | PASS |
| Rust tests | `cargo test -p namtor-kde` | PASS (18 unit + 3 integration = 21) |
| CMake configure | `cmake .. -DCMAKE_INSTALL_PREFIX=/tmp/namtor-kde-install` | PASS (ECM not found; fallback paths used) |
| CMake build | `cmake --build .` | PASS (libnamtorqml.so + namtor-abi-smoke) |
| CTest | `ctest --output-on-failure` | PASS (1/1 abi-smoke) |
| CMake install | `cmake --install .` | PASS (6 files installed) |
| SPDX headers | `scripts/check-spdx.sh <all changed files>` | PASS |

### CMake install tree (no-ECM fallback)

```
/tmp/namtor-kde-install/
├── lib/qt6/qml/org/namtor/libnamtorqml.so
├── share/applications/org.namtor.kcm.desktop
├── share/icons/hicolor/scalable/apps/org.namtor.kcm.svg
├── share/kpackage/kcms/kcm_namtor/metadata.json
├── share/metainfo/org.namtor.kcm.metainfo.xml
└── share/qt6/qml/org/namtor/main.qml
```

With ECM installed, the KDE_INSTALL_* paths resolve to the distro-standard
locations (`/usr/lib/qt6/qml/`, `/usr/share/applications/`, etc.).

## 2. Validators

| Validator | Command | Result |
|---|---|---|
| desktop-file-validate | `desktop-file-validate packaging/applications/org.namtor.kcm.desktop` | PASS (exit 0, no hints) |
| appstreamcli | `appstreamcli validate --no-net packaging/metainfo/org.namtor.kcm.metainfo.xml` | PASS (exit 0, "Validation was successful") |

### Desktop entry details

- `Type=Application` with `Exec=kcmshell6 kcm_namtor` for standalone launch.
- `NoDisplay=true` — the KCM is discovered inside System Settings via
  `metadata.json` (KPlugin); the desktop entry only covers krunner/menu
  search and kcmshell6 invocation.
- `Categories=Settings;DesktopSettings;KDE;Qt;` — single main category
  (`Settings`) to avoid the "more than one main category" hint that
  Task 20 hit with the GNOME desktop entry.
- German translations (`Name[de]`, `Comment[de]`, `Keywords[de]`).

### AppStream metainfo details

- OARS 1.1 content rating: all attributes `none` (no violence, sex,
  language, social, money content).
- Keywords: face, authentication, biometric, infrared, login, security,
  namtor.
- `--no-net` flag used to skip URL reachability checks (pre-release).
- No biometric data, model identifiers, or threshold values in the file.

## 3. Accessibility (a11y) audit

All interactive controls and informational labels in `qml/main.qml` have
`Accessible.role` and `Accessible.name` set:

| Control type | Count | Accessible.role | Accessible.name |
|---|---|---|---|
| Kirigami.Heading | 2 | `Header` | `text` (i18n) |
| QQC2.Label (status) | 1 | `StaticText` | `text` (i18n) |
| Kirigami.Card | per-profile | `ListItem` | i18n composed name |
| Kirigami.Title | per-profile | `StaticText` | `text` (model data) |
| QQC2.Label (enabled/disabled) | per-profile | `StaticText` | `text` (i18n) |
| QQC2.Label (last used) | per-profile | `StaticText` | `text` (i18n) |
| QQC2.ToolButton (Rename/Replace/Delete) | 3 per-profile | `Button` | i18n with profile name |
| QQC2.Button (Add Profile) | 1 | `Button` | i18n name + description |
| QQC2.ProgressBar | 1 | `ProgressBar` | i18n with coverage % + announcements |
| QQC2.Label (wizard instructions/quality) | 2 | `StaticText` | `text` (i18n) |
| QQC2.Button (Finish/Cancel in wizard) | 2 | `Button` | i18n name |
| QQC2.TextField | 1 | `EditableText` | i18n name |
| QQC2.Button (Rename confirm) | 1 | `Button` | `text` (i18n) |
| QQC2.Label (replace/delete messages) | 2 | `StaticText` | `text` (set via i18n) |
| QQC2.Button (Replace/Delete/Cancel in dialogs) | 4 | `Button` | `text` (i18n) |

All keyboard-reachable: Kirigami cards have `focusPolicy: Qt.StrongFocus`;
buttons and text fields are standard QQC2 controls with default focus
support.

## 4. Internationalisation (i18n) audit

All user-visible literal strings in `qml/main.qml` are wrapped in `i18n()`:

- Section headings: `i18n("Face Authentication")`, `i18n("Profiles")`
- Status messages: `i18n("Daemon reachable; camera available.")`, etc.
- Profile state: `i18n("Enabled")`, `i18n("Disabled")`, `i18n("Never used")`,
  `i18n("Last used: %1")`
- Button labels: `i18n("Rename")`, `i18n("Replace")`, `i18n("Delete")`,
  `i18n("Add Profile")`, `i18n("Finish")`, `i18n("Cancel")`
- Dialog messages: `i18n("Replace the face profile for ...")`,
  `i18n("Delete the face profile for ...")`
- Accessibility names: all i18n-wrapped with `%1` interpolation for
  dynamic data (profile names)
- Placeholder text: `i18n("Profile name")`

Dynamic model data (`modelData.name`, `modelData.lastUsed`) is NOT wrapped
in i18n — these are user-entered values, not translatable strings.

### Translation infrastructure

- `packaging/po/kde/POTFILES.in` — lists `crates/namtor-kde/qml/main.qml`
- `packaging/po/kde/namtor-kde.pot` — placeholder template (xgettext with
  `--language=Qt --qt` parser regenerates from POTFILES.in)
- `packaging/po/kde/README.md` — instructions for generating .pot and
  starting new locales
- KDE uses `ki18n` for `i18n()` in QML; compiled `.mo` files are a distro
  packaging concern (not install-manifest entries)

## 5. Packaging

### Install manifest entries (packaging/install-manifest.toml)

3 new entries for static desktop-integration files:

| dest | src | mode |
|---|---|---|
| `/usr/share/applications/org.namtor.kcm.desktop` | `packaging/applications/org.namtor.kcm.desktop` | 0644 |
| `/usr/share/metainfo/org.namtor.kcm.metainfo.xml` | `packaging/metainfo/org.namtor.kcm.metainfo.xml` | 0644 |
| `/usr/share/icons/hicolor/scalable/apps/org.namtor.kcm.svg` | `packaging/icons/hicolor/scalable/apps/org.namtor.kcm.svg` | 0644 |

The QML module (`libnamtorqml.so`), `main.qml`, and `metadata.json` are
installed by CMake's `install()` rules, not the manifest — CMake handles
the Qt6 QML import dir and kpackage/kcms directory.

### Staging verification

`packaging/stage.sh` successfully stages all 3 new KDE entries into a
DESTDIR (11 dirs + 21 files total, up from 11 dirs + 18 files before).

### CMake install rules

`crates/namtor-kde/CMakeLists.txt` install rules now cover:
- QML module library + main.qml + metadata.json (existing)
- Desktop entry (new)
- AppStream metainfo (new)
- KCM icon (new)

Uses `ECM_FOUND` conditional for `KDE_INSTALL_*` vs `CMAKE_INSTALL_*`
fallback paths (ECM defines `KDE_INSTALL_*`; without ECM, GNUInstallDirs
defaults are used).

## 6. Icon

`packaging/icons/hicolor/scalable/apps/org.namtor.kcm.svg` — 128×128 SVG
shield outline with a person silhouette. Uses KDE blue (`#1d99f3`). No
face or biometric imagery — same shield design as the GNOME icon with
KDE's accent color. Production artwork should replace this placeholder.

## 7. KCM discovery

Two discovery paths:

1. **System Settings** — `metadata.json` (KPlugin JSON, Plasma 6 format)
   installed to `/usr/share/kpackage/kcms/kcm_namtor/`. System Settings
   reads the `KPlugin.Id`, `Name`, `Category`, `Keywords`, and `Icon`
   fields. `X-KDE-System-Settings-Parent-Category: user` places it under
   the Users category.

2. **Application menu / krunner search** — `org.namtor.kcm.desktop`
   installed to `/usr/share/applications/`. `Exec=kcmshell6 kcm_namtor`
   launches the KCM standalone. `NoDisplay=true` prevents duplicate
   entries in the application menu (the KCM is already in System Settings).

3. **Software center** — `org.namtor.kcm.metainfo.xml` installed to
   `/usr/share/metainfo/`. KDE Discover and AppStream-compatible software
   centers use this for display and search.

## 8. WITHHELD (requires runtime environment)

| Item | Reason |
|---|---|
| `kcmshell6 kcm_namtor` runtime load | No kcmshell6 / plasma-systemsettings installed on this host |
| System Settings discovery | No running Plasma 6 session |
| Screen reader (orca) a11y verification | No orca / no running Plasma session |
| KDE Discover metainfo display | No Discover installed |
| Translation compilation (.po → .mo) | No locales started; distro packaging concern |
| ECM-based install paths | ECM not installed; fallback paths verified instead |

The Rust bridge tests (21) cover the D-Bus communication logic. The CMake
build + install + CTest cover the structural packaging. Runtime KCM
loading in a real Plasma 6 session is the next HIL step.
