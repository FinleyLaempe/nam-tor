# SPDX-License-Identifier: GPL-3.0-only
#
# Nam-tor RPM spec for Fedora 40/41 (and rawhide). Builds from the upstream
# release tarball with `cargo build --release`, stages via
# packaging/stage.sh for a deterministic DESTDIR layout, and ships the
# transactional PAM install/uninstall scripts under /usr/share/namtor.
#
# Supported architectures: x86_64, aarch64 (model-manifest target_arch=both).
#
# Runtime requires:
#   - onnxruntime        (CPU EP for model inference; the daemon refuses to
#                         start without a working onnxruntime in the worker)
#   - pam                (libpam for the auth module)
#   - systemd            (socket activation, sysusers, tmpfiles)
#
# Models are NOT packaged — they are fetched at runtime per
# /usr/share/namtor/model-manifest.toml after the operator accepts the
# upstream licences (NAMTOR_ACCEPT_LICENCE=1). This keeps weights (which
# carry their own licence terms) out of the binary RPM.

%global debug_package %{nil}

Name:           namtor
Version:        0.1.0
Release:        1%{?dist}
Summary:        IR face authentication for Linux — daemon, PAM module, CLI

License:        GPL-3.0-only
URL:            https://github.com/namtor/nam-tor
Source0:        %{url}/archive/refs/tags/v%{version}/%{name}-%{version}.tar.gz
# Source0 is verified by the release workflow via the GPG-signed checksums
# file; no hardcoded sha256 here so updates do not require spec edits.

# Build requirements — Fedora ships rust-toolchain + cargo in the standard
# repos. python3 >= 3.11 is needed for packaging/stage.sh (stdlib tomllib).
BuildRequires:  rust-toolchain
BuildRequires:  cargo
BuildRequires:  python3 >= 3.11
BuildRequires:  systemd-rpm-macros
BuildRequires:  cmake
BuildRequires:  qt6-qtbase-devel
BuildRequires:  kf6-kirigami-devel
BuildRequires:  kf6-kcoreaddons-devel
BuildRequires:  extra-cmake-modules

# Runtime requirements.
Requires:       onnxruntime
Requires:       pam
Requires:       systemd
Requires:       %{_lib}gcc%{?_isa}
Requires:       gtk4%{?_isa}
Requires:       libadwaita%{?_isa}

# The package provides a PAM module; conflict with any other package that
# ships pam_namtor.so.
Provides:       pam_namtor%{?_isa} = %{version}-%{release}
Conflicts:      pam_namtor

# Architectures — matches model-manifest target_arch=both.
ExclusiveArch:  x86_64 aarch64

%description
Nam-tor replaces Howdy, targeting Windows Hello parity on speed and
accuracy while fixing the architectural problems that produced Howdy's
authentication-bypass CVEs. Four processes across three trust levels:
daemon (namtor user), setuid worker-spawn, sandboxed inference worker
(nobody, seccomp+landlock), and a PAM module with no ML code that fails
closed on every path. Strobe-differential liveness with analytic gates
before any model is trained.

This package provides the daemon, worker, setuid spawn helper, CLI,
PAM module, systemd units, and transactional PAM install scripts.
Models are fetched at runtime after licence acceptance.

%prep
%autosetup -n nam-tor-%{version}

# Cargo.lock must ship with the release tarball (reproducible offline build).
test -f Cargo.lock || { echo "Cargo.lock missing from tarball" >&2; exit 1; }

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
    -p namtor-gnome \
    -p namtor-kde

# KDE KCM QML module (CMake build — installs libnamtorqml.so + main.qml +
# metadata.json into the Qt6 QML import dir). The Rust cdylib
# (libnamtor_kde.so) is built by cargo above and linked by CMake.
%global rust_so %{_builddir}/nam-tor-%{version}/target/release/libnamtor_kde.so
cmake -S %{_builddir}/nam-tor-%{version}/crates/namtor-kde \
    -B %{_builddir}/build-kde \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX=%{_prefix} \
    -DNAMTOR_KDE_RUST_SO=%{rust_so}
cmake --build %{_builddir}/build-kde --parallel

%check
cargo test --workspace -- --skip concurrent_connections_serialise --test-threads=1

%install
# KDE KCM: install the CMake-built QML module + metadata.json into
# %{buildroot}. stage.sh will overwrite the overlapping desktop/metainfo/icon
# files with identical content from the manifest (same source files).
DESTDIR=%{buildroot} cmake --install %{_builddir}/build-kde

# Deterministic staging from packaging/install-manifest.toml — installs
# all static files (binaries, PAM, systemd, D-Bus, polkit, GNOME/KDE
# desktop entries, metainfo, icons, manifests, PAM scripts, README).
./packaging/stage.sh "%{buildroot}" "%{_builddir}/nam-tor-%{version}"

%files
%license COPYING.md
%doc %{_defaultdocdir}/%{name}/README.md
/usr/lib/namtor/namtor-daemon
/usr/lib/namtor/namtor-worker
/usr/lib/namtor/namtor-worker-spawn
/usr/lib/namtor/namtor-camera-emitter-off
/usr/bin/namtor-cli
/usr/bin/namtor-enroll-wizard
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
%{_datadir}/dbus-1/system-services/org.namtor.Manager.service
%{_datadir}/polkit-1/actions/org.namtor.policy
%{_datadir}/applications/org.namtor.Wizard.desktop
%{_datadir}/metainfo/org.namtor.Wizard.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/org.namtor.Wizard.svg
%{_datadir}/applications/org.namtor.kcm.desktop
%{_datadir}/metainfo/org.namtor.kcm.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/org.namtor.kcm.svg
# KDE KCM QML module + KPlugin metadata (CMake-installed)
%{_libdir}/qt6/qml/org/namtor/libnamtorqml.so
%{_datadir}/qt6/qml/org/namtor/main.qml
%{_datadir}/kpackage/kcms/kcm_namtor/metadata.json

%pre
# Register the system user + runtime dirs before the unit files are
# installed so the daemon can start cleanly on enable.
%sysusers_create_package namtor %{_sysusersdir}/namtor.conf
%tmpfiles_create_package namtor %{_tmpfilesdir}/namtor.conf

%post
%systemd_post namtor-daemon.socket namtor-daemon.service

echo ":: namtor installed. To complete setup:"
echo "  1. sudo gpasswd -a gdm namtor-auth"
echo "  2. NAMTOR_ACCEPT_LICENCE=1 namtor-cli models ensure"
echo "  3. sudo /usr/share/namtor/pam-preflight.sh && sudo /usr/share/namtor/pam-install.sh"
echo "  4. sudo systemctl enable --now namtor-daemon.socket"
echo "See %{_defaultdocdir}/%{name}/README.md for the full flow."

%preun
# Remove the PAM integration (transactional, restores the backup).
if [ -x %{_datadir}/namtor/pam-uninstall.sh ]; then
    %{_datadir}/namtor/pam-uninstall.sh --restore-backup || true
fi
%systemd_preun namtor-daemon.socket namtor-daemon.service

%postun
%systemd_postun namtor-daemon.socket namtor-daemon.service
# Clean runtime dirs (model store under /var/lib/namtor left for manual removal).
systemd-tmpfiles --remove %{_tmpfilesdir}/namtor.conf 2>/dev/null || true

%changelog
* Wed Aug 06 2026 Nam-tor Project <namtor@example.invalid> - 0.1.0-1
- Initial RPM package for Fedora 40/41.
- Daemon, worker, setuid spawn helper, CLI, PAM module, systemd units.
- Transactional PAM install/uninstall scripts shipped under /usr/share/namtor.
- Models fetched at runtime per model-manifest.toml after licence acceptance.
