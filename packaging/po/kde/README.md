# SPDX-License-Identifier: GPL-3.0-only
# Translation files for namtor-kde (KDE Plasma 6 KCM).
#
# Per-locale .po files live here (e.g. de.po, fr.po). The .pot template
# is generated from packaging/po/kde/POTFILES.in via:
#
#   xgettext --package-name=namtor-kde --package-version=0.1.0 \
#     --language=Qt --qt --from-code=UTF-8 \
#     -f packaging/po/kde/POTFILES.in -o packaging/po/kde/namtor-kde.pot
#
# To start a new locale:
#
#   msginit -i packaging/po/kde/namtor-kde.pot -l <locale> \
#     -o packaging/po/kde/<locale>.po
#
# KDE uses ki18n for i18n() in QML. Compiled .mo files are installed to
# /usr/share/locale/<locale>/LC_MESSAGES/ by the distro packaging
# (PKGBUILD / spec / debian rules), not by install-manifest.toml — locale
# files are a distro-packaging concern, not a per-file manifest entry.
#
# If ECM (extra-cmake-modules) is available, the CMake build can also use
# ecm_process_po_files_as_qm to compile .po → .qm at build time. The
# CMakeLists.txt does not invoke this automatically (ECM is optional);
# distro packagers with ECM should wire it in their build rules.
