# SPDX-License-Identifier: GPL-3.0-only
# Translation files for namtor-ui.
#
# Per-locale .po files live here (e.g. de.po, fr.po). The .pot template
# is generated from packaging/po/POTFILES.in via:
#
#   xgettext --package-name=namtor-ui --package-version=0.1.0 \
#     -f packaging/po/POTFILES.in -o packaging/po/namtor-ui.pot
#
# To start a new locale:
#
#   msginit -i packaging/po/namtor-ui.pot -l <locale> -o packaging/po/<locale>.po
#
# Compiled .mo files are installed to /usr/share/locale/<locale>/LC_MESSAGES/
# by the distro packaging (PKGBUILD / spec / debian rules), not by
# install-manifest.toml — locale files are a distro-packaging concern,
# not a per-file manifest entry.
