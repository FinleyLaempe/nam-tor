# SPDX-License-Identifier: GPL-3.0-only
#
# Nam-tor RPM spec for Fedora 40/41 (and rawhide). Builds from the upstream
# release tarball with `cargo build --release`, stages via
# packaging/stage.sh for a deterministic DESTDIR layout, and ships the
# transactional PAM install/uninstall scripts under /usr/share/namtor.
#
# Split into two subpackages from one spec:
#   namtor       (base/core) — daemon, worker, CLI, PAM module, systemd
#                units, sysusers, tmpfiles, D-Bus, polkit, manifests,
#                bundled models + licence notices, lifecycle helper, SBOM.
#   namtor-gnome — GTK4/libadwaita enrollment wizard (.desktop, .metainfo,
#                .svg). Requires namtor = ver-rel. The single, desktop-
#                neutral UI despite the name.
#
# A third subpackage, namtor-kde (the Plasma 6 KCM), shipped up to and
# including 0.1.0-2 and is gone — the project consolidated on one
# desktop-neutral GTK4 UI. namtor-gnome carries `Obsoletes: namtor-kde`
# so dnf removes the stale subpackage during the upgrade instead of
# leaving it behind with an unsatisfiable `Requires: namtor = ver-rel`.
#
# The install manifest (schema v2) carries per-entry `package` ownership
# (core|gnome); stage.sh stages all of them into one buildroot and the
# %files sections partition the tree by subpackage.
#
# Supported architectures: x86_64, aarch64 (model-manifest target_arch=both).
#
# Runtime requires (core):
#   - onnxruntime        (CPU EP for model inference; the daemon refuses to
#                         start without a working onnxruntime in the worker)
#   - pam                (libpam for the auth module)
#   - systemd            (socket activation, sysusers, tmpfiles)
#
# Models are BUNDLED from the deterministic, signed model archive
# (namtor-models-%{version}.tar.gz, produced by
# packaging/build-model-archive.sh). No network fetch occurs at build or
# install time. The archive is MANDATORY: %prep verifies its MANIFEST.json
# + sha256 sidecar + per-model hashes against model-manifest.toml before
# any compilation. Weights carry their own permissive licences (YuNet=MIT,
# SFace=Apache-2.0, MiniFASNet=Apache-2.0 self-trained) and are
# redistributable. The licence notice texts under packaging/licences/ are
# bundled into the core package via %license so operators can review every
# upstream model licence offline.

%global debug_package %{nil}

Name:           namtor
Version:        0.1.0
Release:        3%{?dist}
Summary:        IR face authentication for Linux — daemon, PAM module, CLI

# Combined SPDX expression: the Rust core is GPL-3.0-only; the bundled ONNX
# models carry their own permissive licences (MIT for YuNet, Apache-2.0 for
# SFace and the self-trained MiniFASNet PAD). The notice texts are shipped
# under %_licensedir/%name/ via the %license directives in %%files.
License:        GPL-3.0-only AND MIT AND Apache-2.0
URL:            https://github.com/namtor/nam-tor
Source0:        %{url}/archive/refs/tags/v%{version}/%{name}-%{version}.tar.gz
# Source1 is the deterministic, reproducible model archive. Its sha256 is
# pinned (no SKIP); the release workflow regenerates it via
# packaging/build-model-archive.sh and updates the hash below. Repeated
# builds produce an identical tarball (fixed epoch, sorted entries, gzip -n).
Source1:        %{url}/releases/download/v%{version}/namtor-models-%{version}.tar.gz
# Source0 is verified by the release workflow via the GPG-signed checksums
# file; Source1 is verified in %prep against model-manifest.toml.

# Build requirements — Fedora ships rust-toolchain + cargo in the standard
# repos. python3 >= 3.11 is needed for packaging/stage.sh (stdlib tomllib).
BuildRequires:  rust-toolchain
BuildRequires:  cargo
BuildRequires:  python3 >= 3.11
BuildRequires:  systemd-rpm-macros

# Core runtime requirements — daemon/worker/CLI/PAM only. The GUI toolkit
# deps (gtk4/libadwaita) live in the subpackage so a headless host is not
# forced to pull GTK.
Requires:       onnxruntime
Requires:       pam
Requires:       systemd
Requires:       %{_lib}gcc%{?_isa}

# The core package provides a PAM module; conflict with any other package
# that ships pam_namtor.so.
Provides:       pam_namtor%{?_isa} = %{version}-%{release}
Conflicts:      pam_namtor

# Architectures — matches model-manifest target_arch=both.
ExclusiveArch:  x86_64 aarch64

%description
Nam-tor replaces Howdy, targeting Windows Hello parity on speed and
accuracy while fixing the architectural problems that produced Howdy's
authentication-bypass CVEs. Four processes across three trust levels:
daemon (namtor user), sandboxed inference worker (namtor user,
seccomp+landlock), and a PAM module with no ML code that fails
closed on every path. Strobe-differential liveness with analytic gates
before any model is trained.

This core package provides the daemon, worker, CLI, PAM module, systemd
units, sysusers/tmpfiles drop-ins, D-Bus service, polkit policy, model +
install manifests, the shared lifecycle helper, the transactional PAM
scripts, the bundled ONNX models with their licence notices, and the SBOM.
Models are bundled from the signed deterministic archive (no network fetch
at install time). PAM is OFF by default.

Install namtor-gnome for the GTK4/libadwaita enrollment wizard and
settings app.

# ===========================================================================
# namtor-gnome — GTK4/libadwaita enrollment wizard + settings
# ===========================================================================
%package gnome
Summary:        Enrollment wizard and settings for Nam-tor (GTK4 + libadwaita)
License:        GPL-3.0-only
Requires:       %{name} = %{version}-%{release}
Requires:       gtk4%{?_isa}
Requires:       libadwaita%{?_isa}
# UPGRADE BLOCKER FIX, not tidiness: namtor-kde declared
# `Requires: namtor = %%{version}-%%{release}`. If it is merely dropped from
# the spec, a machine that still has it installed keeps an unsatisfiable
# dependency and dnf refuses to upgrade the core package. `Obsoletes` makes
# dnf remove it as part of the transaction. Keep this until no supported
# upgrade path starts from a release that shipped namtor-kde (<= 0.1.0-2).
Obsoletes:      namtor-kde < %{version}-%{release}

%description gnome
The desktop integration for Nam-tor: a GTK4 + libadwaita enrollment wizard
and settings app. Install this alongside the core namtor package to manage
enrollment, review protection status, and toggle PAM services from the
desktop. Requires the core namtor package at the same version.

%prep
%autosetup -n nam-tor-%{version}

# Cargo.lock must ship with the release tarball (reproducible offline build).
test -f Cargo.lock || { echo "Cargo.lock missing from tarball" >&2; exit 1; }

# The model archive (Source1) is MANDATORY. build-model-archive.sh --verify
# checks MANIFEST.json + sha256 sidecar + per-model hashes against
# model-manifest.toml. Fails closed if absent or tampered.
archive="%{_sourcedir}/namtor-models-%{version}.tar.gz"
test -f "$archive" || { echo "error: model archive missing: $archive" >&2; exit 1; }
./packaging/build-model-archive.sh --verify "$archive" "%{_builddir}/nam-tor-%{version}"

%build
# ring 0.17.x C/asm code fails to link when GCC LTO (-flto=auto, set by
# Fedora's default RPM LTO flags) produces LLVM bitcode that rust-lld cannot
# ingest. Disable LTO for the C compilation; Rust LTO (lto=true in
# Cargo.toml [profile.release]) is unaffected.
%global _lto_cflags %{nil}
# Build every artifact the install manifest references. -p pins avoid
# pulling unrelated workspace members (e.g. namtor-eval, namtor-probe).
cargo build --release --frozen \
    -p namtor-daemon \
    -p namtor-worker \
    -p namtor-cli \
    -p namtor-camera \
    -p namtor-pam \
    -p namtor-gnome

%check
# --test-threads=1 serialises tests that mutate process-global env vars
# (the licence tests set/remove NAMTOR_ACCEPT_LICENCE); without it those
# races produce spurious failures.
cargo test --workspace -- --test-threads=1

%install
# Deterministic staging from packaging/install-manifest.toml — installs
# all static files for every split package (core, gnome) into one
# buildroot; the %files sections partition the tree by subpackage.
./packaging/stage.sh "%{buildroot}" "%{_builddir}/nam-tor-%{version}"

# Bundle models from the verified archive (Source1). No network fetch.
archive="%{_sourcedir}/namtor-models-%{version}.tar.gz"
mkdir -p "%{buildroot}/var/lib/namtor/models"
tar -xzf "$archive" -C "%{buildroot}/var/lib/namtor/models" \
    --strip-components=1 models/yunet_2023mar.onnx \
    models/sface_2021dec.onnx models/minifasnet_pad.onnx
# Worker expects <root>/<id>/<quant>.onnx
mkdir -p "%{buildroot}/var/lib/namtor/models/yunet_2023mar"
ln -s ../yunet_2023mar.onnx "%{buildroot}/var/lib/namtor/models/yunet_2023mar/fp32.onnx"
mkdir -p "%{buildroot}/var/lib/namtor/models/sface_2021dec"
ln -s ../sface_2021dec.onnx "%{buildroot}/var/lib/namtor/models/sface_2021dec/fp32.onnx"

# Pre-write licence acceptance records (permissive, auto-accepted).
echo '{"model_id":"yunet_2023mar","licence_url":"https://raw.githubusercontent.com/opencv/opencv_zoo/main/models/face_detection_yunet/LICENSE","licence_sha256":[200,59,129,32,197,12,203,212,196,249,110,223,83,20,27,221,86,110,187,143,142,146,39,228,21,50,106,161,177,171,169,88],"accepted_at_us":0,"accepted_by":"package"}' \
    > "%{buildroot}/var/lib/namtor/models/yunet_2023mar/licence.accepted"
echo '{"model_id":"sface_2021dec","licence_url":"https://www.apache.org/licenses/LICENSE-2.0.txt","licence_sha256":[207,199,116,155,150,246,59,211,28,60,66,181,196,113,191,117,104,20,5,62,132,124,16,243,235,0,52,23,188,82,61,48],"accepted_at_us":0,"accepted_by":"package"}' \
    > "%{buildroot}/var/lib/namtor/models/sface_2021dec/licence.accepted"

# ===========================================================================
# %files — core (namtor)
# ===========================================================================
%files
%license COPYING.md
%license packaging/licences/GPL-3.0.txt
%license packaging/licences/APACHE-2.0.txt
%license packaging/licences/MIT.txt
%license packaging/licences/yunet_2023mar.LICENSE
%license packaging/licences/sface_2021dec.LICENSE
%license packaging/licences/minifasnet_pad.NOTICE
%doc %{_defaultdocdir}/%{name}/README.md
%doc %{_defaultdocdir}/%{name}/COPYING.md
%doc %{_defaultdocdir}/%{name}/namtor.spdx.json
%doc %{_defaultdocdir}/%{name}/namtor.cyclonedx.json
/usr/lib/namtor/namtor-daemon
/usr/lib/namtor/namtor-worker
/usr/lib/namtor/namtor-camera-emitter-off
/usr/bin/namtor-cli
/usr/lib/security/pam_namtor.so
/usr/lib/systemd/system/namtor-daemon.service
/usr/lib/systemd/system/namtor-daemon.socket
/usr/lib/sysusers.d/namtor.conf
/usr/lib/tmpfiles.d/namtor.conf
%{_datadir}/namtor/model-manifest.toml
%{_datadir}/namtor/install-manifest.toml
%{_datadir}/namtor/pam-install.sh
%{_datadir}/namtor/pam-uninstall.sh
%{_datadir}/namtor/pam-preflight.sh
%{_datadir}/namtor/lifecycle.sh
%{_datadir}/dbus-1/system-services/org.namtor.Manager.service
%{_datadir}/polkit-1/actions/org.namtor.policy
# Bundled models (MIT/Apache-2.0, redistributable) + pre-written licence
# acceptance records. No network fetch at install.
%dir /var/lib/namtor/models
/var/lib/namtor/models/yunet_2023mar.onnx
/var/lib/namtor/models/sface_2021dec.onnx
/var/lib/namtor/models/minifasnet_pad.onnx
/var/lib/namtor/models/yunet_2023mar
/var/lib/namtor/models/sface_2021dec

# ===========================================================================
# %files — namtor-gnome
# ===========================================================================
%files gnome
%license COPYING.md
/usr/bin/namtor-enroll-wizard
%{_datadir}/applications/org.namtor.Wizard.desktop
%{_datadir}/metainfo/org.namtor.Wizard.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/org.namtor.Wizard.svg

# ===========================================================================
# Scriptlets — core (namtor). The daemon + PAM lifecycle is core's job;
# the subpackages only refresh desktop/icon caches.
# ===========================================================================
%pre
# Register the system user + runtime dirs before the unit files are
# installed so the daemon can start cleanly on enable.
%sysusers_create_package namtor %{_sysusersdir}/namtor.conf
%tmpfiles_create_package namtor %{_tmpfilesdir}/namtor.conf

%post
%systemd_post namtor-daemon.socket namtor-daemon.service
# Delegate validation + service enablement to the shared lifecycle helper
# (Task 19). $1 = 1 fresh install, 2 upgrade. PAM is default-off — the
# helper never edits /etc/pam.d in post-install.
if [ $1 -eq 1 ]; then
    /usr/share/namtor/lifecycle.sh post-install --version %{version}-%{release} || {
        echo ":: namtor: post-install mandatory checks failed" >&2
        exit 1
    }
    echo ":: namtor installed. PAM services are OFF by default."
    echo "   To complete setup:"
    echo "     1. Open the Nam-tor Settings app."
    echo "     2. Enroll your face."
    echo "     3. Choose which PAM services to enable (e.g. gdm-password)."
    echo "   Install namtor-gnome for the desktop UI."
    echo "   No CLI or root shell needed for setup."
fi

%preun
# $1 = 0 removal, 1 upgrade. On removal, disable PAM + stop the daemon via
# the lifecycle helper, then cache it so %postun can still run after RPM
# deletes the payload.
%systemd_preun namtor-daemon.socket namtor-daemon.service
if [ $1 -eq 0 ]; then
    /usr/share/namtor/lifecycle.sh pre-remove || true
    cp -f /usr/share/namtor/lifecycle.sh /var/tmp/namtor-lifecycle.sh 2>/dev/null || true
fi

%postun
%systemd_postun namtor-daemon.socket namtor-daemon.service
# $1 = 0 removal. The payload (incl. lifecycle.sh) is gone by now — call
# the cached copy from %preun, then remove the cache. The cached helper
# purges all approved PAM state and removes the runtime dirs, so removing
# the core package cleans up every Nam-tor PAM integration.
if [ $1 -eq 0 ]; then
    /var/tmp/namtor-lifecycle.sh post-remove || true
    rm -f /var/tmp/namtor-lifecycle.sh 2>/dev/null || true
fi

%posttrans
# $1 = 2 upgrade. After the new payload is settled, migrate PAM entries
# and re-validate models/services via the lifecycle helper.
if [ $1 -eq 2 ]; then
    /usr/share/namtor/lifecycle.sh post-upgrade --version %{version}-%{release} || true
fi

# ===========================================================================
# Scriptlets — namtor-gnome. Desktop + icon cache refresh only; no daemon
# or PAM lifecycle (that is core's responsibility).
# ===========================================================================
%post gnome
/usr/bin/update-desktop-database -q %{_datadir}/applications 2>/dev/null || :
/usr/bin/gtk-update-icon-cache -q %{_datadir}/icons/hicolor 2>/dev/null || :

%postun gnome
/usr/bin/update-desktop-database -q %{_datadir}/applications 2>/dev/null || :
/usr/bin/gtk-update-icon-cache -q %{_datadir}/icons/hicolor 2>/dev/null || :

%changelog
* Wed Sep 16 2026 Nam-tor Project <namtor@example.invalid> - 0.1.0-3
- Drop the namtor-kde subpackage: the project consolidated on a single
  desktop-neutral GTK4/libadwaita UI, so the Plasma 6 KCM is gone along
  with the Qt6/KF6/CMake build requirements.
- namtor-gnome now carries `Obsoletes: namtor-kde < ver-rel` so an
  existing namtor-kde install is removed during the upgrade rather than
  blocking it with an unsatisfiable `Requires: namtor = ver-rel`.

* Sun Aug 09 2026 Nam-tor Project <namtor@example.invalid> - 0.1.0-2
- Split into namtor (core), namtor-gnome, namtor-kde subpackages.
- Core: daemon, worker, CLI, PAM, systemd, sysusers, tmpfiles, D-Bus,
  polkit, manifests, bundled models + licence notices, lifecycle helper,
  SBOM. No GTK/Qt runtime deps.
- namtor-gnome: GTK4/libadwaita wizard; Requires namtor = ver-rel.
- namtor-kde: Plasma 6 KCM (Kirigami/Qt6); Requires namtor = ver-rel.
- License field is now the combined SPDX expression GPL-3.0-only AND MIT
  AND Apache-2.0 to cover the bundled model weights.
- PAM remains OFF by default; removal purges approved state via the
  lifecycle helper.

* Wed Aug 06 2026 Nam-tor Project <namtor@example.invalid> - 0.1.0-1
- Initial RPM package for Fedora 40/41.
- Daemon, worker, CLI, PAM module, systemd units.
- Transactional PAM install/uninstall scripts shipped under /usr/share/namtor.
- Models bundled from the deterministic signed archive (no network fetch).
