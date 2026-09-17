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
#   namtor-ui    — GTK4/libadwaita enrollment wizard (.desktop, .metainfo,
#                .svg). Requires namtor = ver-rel. The single,
#                desktop-neutral UI.
#
# Two predecessors are obsoleted rather than simply dropped:
#
#   namtor-gnome — this same subpackage under its old, desktop-specific
#                name, shipped up to and including 0.1.0-3. dnf does not
#                migrate a rename on its own, so without `Obsoletes` the
#                old subpackage lingers and FILE-CONFLICTS with this one:
#                both own /usr/bin/namtor-enroll-wizard, the .desktop, the
#                metainfo and the icon.
#   namtor-kde   — the Plasma 6 KCM, shipped up to and including 0.1.0-2,
#                removed when the project consolidated on one
#                desktop-neutral GTK4 UI. Without `Obsoletes` it is left
#                behind with an unsatisfiable `Requires: namtor = ver-rel`,
#                which blocks core upgrades.
#
# An Obsoletes only fires on an upgrade to a NEWER version, which is why
# Release is bumped alongside the rename.
#
# The install manifest (schema v2) carries per-entry `package` ownership
# (core|ui); stage.sh stages all of them into one buildroot and the
# %files sections partition the tree by subpackage.
#
# Supported architectures: x86_64, aarch64 (model-manifest target_arch=both).
#
# Runtime requires (core):
#   - (no onnxruntime)   ONNX Runtime is linked into the binary by ort's
#                         download-binaries feature; nothing loads a system
#                         libonnxruntime, so requiring the package only
#                         installed an unused ~100 MB library.
#   - pam                (libpam for the auth module)
#   - systemd            (socket activation, sysusers, tmpfiles)
#
# Models are BUNDLED from the deterministic, signed model archive
# (namtor-models-%{modelver}.tar.gz, produced by
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
Version:        0.3.8
Release:        1%{?dist}
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
# The model archive version is deliberately INDEPENDENT of %%{version}: the
# ONNX weights are published once as a standing asset (models-v0.1.0) and are
# unchanged by a source-only release. Coupling them to %%{version} made the
# first version bump look for an archive that does not and should not exist.
%global modelver 0.1.0
Source1:        %{url}/releases/download/models-v%{modelver}/namtor-models-%{modelver}.tar.gz
# Source0 is verified by the release workflow via the GPG-signed checksums
# file; Source1 is verified in %prep against model-manifest.toml.

# Build requirements. python3 >= 3.11 is needed for packaging/stage.sh
# (stdlib tomllib). The compiler was declared as `rust-toolchain`, which is
# not a Fedora package name — Fedora ships `rust` and `cargo` — so rpmbuild
# refused the build on unmet BuildRequires before reaching %build.
#
# The -devel packages are what Fedora splits out and Arch does not, which is
# why the PKGBUILD's makedepends list is shorter: the GTK app, the PAM module
# and the TLS-using crates all need headers at build time. The release
# workflow's dnf step installed them while the spec did not declare them, so
# a build anywhere else — mock, COPR, a maintainer's machine — would fail.
BuildRequires:  rust
BuildRequires:  cargo
BuildRequires:  python3 >= 3.11
BuildRequires:  systemd-rpm-macros
BuildRequires:  pam-devel
BuildRequires:  openssl-devel
BuildRequires:  gtk4-devel
BuildRequires:  libadwaita-devel
# namtor-worker seals itself with seccomp + landlock, and the ONNX runtime
# needs the C++ runtime — both are link-time, and without them the build
# stops at `cannot find -lseccomp` / `cannot find -lstdc++`. `gcc-c++` is
# what carries the `libstdc++.so` the linker resolves `-lstdc++` against on
# Fedora; `libstdc++-devel` alone installs cleanly and leaves that link
# failing.
BuildRequires:  libseccomp-devel
BuildRequires:  gcc-c++
# libudev-sys builds against libudev.pc; on Fedora that comes from
# systemd-devel. Without it the build stops at "failed to run custom build
# command for `libudev-sys`".
BuildRequires:  systemd-devel

# Core runtime requirements — daemon/worker/CLI/PAM only. The GUI toolkit
# deps (gtk4/libadwaita) live in the subpackage so a headless host is not
# forced to pull GTK.
Requires:       pam
Requires:       systemd
# `libgcc`, not `%{_lib}gcc`: `%{_lib}` expands to `lib64` on x86_64, so that
# asked for a package named `lib64gcc` — a Mandriva-ism that no Fedora repo
# has ever provided, which makes the built rpm uninstallable.
Requires:       libgcc
# Optional: without tpm2-tools the TPM2 backend cannot run the `tpm2`
# dispatcher and templates are sealed with a software key instead.
Recommends:     tpm2-tools

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

Install namtor-ui for the GTK4/libadwaita enrollment wizard and
settings app.

# ===========================================================================
# namtor-ui — GTK4/libadwaita enrollment wizard + settings
# ===========================================================================
%package ui
Summary:        Enrollment wizard and settings for Nam-tor (GTK4 + libadwaita)
License:        GPL-3.0-only
Requires:       %{name} = %{version}-%{release}
Requires:       gtk4%{?_isa}
Requires:       libadwaita%{?_isa}
# UPGRADE BLOCKER FIX, not tidiness.
#
# namtor-gnome is this subpackage's former name. dnf migrates nothing on a
# rename: without `Obsoletes` the old subpackage stays installed and
# file-conflicts with this one on all four of its files. `Provides` keeps
# anything that required the old name satisfiable through the transaction.
#
# namtor-kde declared `Requires: namtor = %%{version}-%%{release}`. If it is
# merely dropped from the spec, a machine that still has it installed keeps
# an unsatisfiable dependency and dnf refuses to upgrade the core package.
#
# Keep both until no supported upgrade path starts from a release that
# shipped them (namtor-kde <= 0.1.0-2, namtor-gnome <= 0.1.0-3).
Provides:       namtor-gnome = %{version}-%{release}
Obsoletes:      namtor-gnome < %{version}-%{release}
Obsoletes:      namtor-kde < %{version}-%{release}

%description ui
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
archive="%{_sourcedir}/namtor-models-%{modelver}.tar.gz"
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
    -p namtor \
    -p namtor-pam \
    -p namtor-ui

%check
# --test-threads=1 serialises tests that mutate process-global env vars
# (the licence tests set/remove NAMTOR_ACCEPT_LICENCE); without it those
# races produce spurious failures.
# No debug info for the test build. %build has already produced a release
# tree, and a second full workspace with debuginfo filled the builder's disk:
# "ld: final link failed: No space left on device". Tests need neither.
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
cargo test --workspace -- --test-threads=1

%install
# Deterministic staging from packaging/install-manifest.toml — installs
# all static files for every split package (core, ui) into one
# buildroot; the %files sections partition the tree by subpackage.
./packaging/stage.sh "%{buildroot}" "%{_builddir}/nam-tor-%{version}"

# Bundle models from the verified archive (Source1). No network fetch.
archive="%{_sourcedir}/namtor-models-%{modelver}.tar.gz"
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
/usr/bin/namtor
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
# Manual pages. Arch and Debian already shipped these; the Fedora %files
# section listed none, so the RPM silently shipped no manual pages at all
# while the other two distros did (and rpmbuild with
# _unpackaged_files_terminate_build would have failed outright). rpmbuild
# compresses man pages itself, so glob the extension rather than pinning
# .8/.1 — unlike Arch, which sets !zipman to match install-manifest.toml.
%{_mandir}/man8/namtor-daemon.8*
%{_mandir}/man8/pam_namtor.8*
%{_mandir}/man1/namtor-cli.1*

# ===========================================================================
# %files — namtor-ui
# ===========================================================================
%files ui
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
    echo "   Install namtor-ui for the desktop UI."
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
# Scriptlets — namtor-ui. Desktop + icon cache refresh only; no daemon
# or PAM lifecycle (that is core's responsibility).
# ===========================================================================
%post ui
/usr/bin/update-desktop-database -q %{_datadir}/applications 2>/dev/null || :
/usr/bin/gtk-update-icon-cache -q %{_datadir}/icons/hicolor 2>/dev/null || :

%postun ui
/usr/bin/update-desktop-database -q %{_datadir}/applications 2>/dev/null || :
/usr/bin/gtk-update-icon-cache -q %{_datadir}/icons/hicolor 2>/dev/null || :

%changelog
* Thu Sep 17 2026 Nam-tor Project <namtor@example.invalid> - 0.3.8-1
- Every page of the settings app was rendered and looked at, which is not
  something the test suite can do for you. Four defects that the design
  contract, the behavioural tests and the accessibility and privacy scans had
  all passed: the capture page showed "Adjust your pose" under a success-green
  silhouette, the same green as "Enrollment complete", because the silhouette
  was coloured from image quality alone; the Profiles page carried the heading
  "Enrolled Profiles" twice, over the list and over the button that adds to it;
  an error on that page was one red line with the title and detail glued
  together and the recovery steps thrown away; and the review screen's detail
  was the daemon's capture-time guidance, so "Ready to save" sat above "keep
  going — Keep turning your head slowly".
- The System page reported "IR Camera — Not detected" for a missing device
  database and for unreadable sysfs as well as for an absent camera. Only the
  last is about the hardware; the first sends someone with a broken install to
  check their webcam. Each condition now reads differently and the fixable one
  says to reinstall.
- The setup page's Cancel was added straight to a PreferencesGroup, which
  renders a bare button as a full-width row, so the one control that leaves the
  page looked like another field in the form.
- A daemon too old to export org.namtor.Manager3 came back as a transport
  failure — "the daemon could not be reached", about a daemon that was running
  and answering — rather than as a version mismatch with a remedy.
- The adaptive layout was checked at 360 px rather than assumed: the view
  switcher moves to a bottom bar with all three labels untruncated and every
  page still fits.

* Thu Sep 17 2026 Nam-tor Project <namtor@example.invalid> - 0.3.7-1
- A camera held by another program reaches every surface as "camera is busy"
  with a remedy, instead of "ioctl failed: VIDIOC_S_FMT: EBUSY". EBUSY is
  typed from the ioctl outwards; nothing string-matches to recover it.
- The Settings app called org.namtor.Manager3.NegotiateVersion, a method no
  interface has ever exported, so version negotiation could not work against
  a real daemon. It reads the ApiVersion property now, and connect negotiates,
  so a half-applied package update says so once rather than failing one method
  at a time.
- The support bundle recorded an empty log section when journalctl exited
  non-zero, which reads as "the daemon logged nothing".
- The daemon bound yunet/sface/minifasnet by position in an array; bound by id
  now, so reordering the required-model list cannot load the PAD network as the
  face detector.
- namtor cli enroll parsed --models-dir, --templates-dir, --key-path and
  --device on the daemon-driven path and then discarded them.
- The busy-camera remedy told the user to stop a systemd unit that is not
  installed.
- IR strobe labelling follows the driver's frame sequence, so a dropped frame
  no longer inverts emitter-on/emitter-off for the rest of the stream.
- The shipped ONNX weights are now loaded and run by the release pipeline
  before packaging; weights that do not infer block the release.

* Thu Sep 17 2026 Nam-tor Project <namtor@example.invalid> - 0.3.6-1
- A suspend arriving during an authentication no longer waits out logind's
  delay window. The dispatcher mutex is held across the blocking verify, and a
  verify can stall as long as the camera withholds a frame — which is what the
  start of a suspend causes — so the machine could sleep with the IR emitter
  still lit. The handler now gives up after 750 ms and darkens the emitter
  directly from the record the daemon writes before arming.
- Hardened the second half of the V4L2 buffer leak: mmap_buffers could orphan
  a mapping the same way request_buffers did, which pins the driver queue and
  makes every later camera open fail with EBUSY.
- The PAM module accepts a timeout_ms= argument beside socket=, bounding the
  daemon round trip and the spec 8.4 keypress window.
- UI messages with runtime parts (profile names, error details) were
  permanently untranslatable, because the interpolated value went into the
  gettext msgid.
- A test asserted a build artifact that its own build never produces.

* Thu Sep 17 2026 Nam-tor Project <namtor@example.invalid> - 0.3.5-1
- SECURITY: the PAM module read its daemon socket path from the
  NAMTOR_SOCKET environment variable. Since it returns PAM_SUCCESS on a match,
  anything able to set that variable for a PAM-using process could point it at
  a socket it controls and authenticate. sudo resets the environment by
  default; su inherits the caller's. The socket now comes from the module's
  own PAM argument (socket=), which lives in root-owned /etc/pam.d. Nothing
  shipped set the variable, so no deployed configuration changes.
- The spec 8.3 failure messages are now shown to the user through the PAM
  conversation function: "No infrared camera found", "Camera is busy",
  "Camera privacy shutter is closed", "Face unlock unavailable", "Face not
  recognised", "Too many attempts". A presentation-attack veto renders exactly
  like a plain non-match, so the system never reveals which signal caught it;
  the journal keeps them distinct. A user with no enrolment is told nothing.
- Verified through real libpam in a temporary PAM stack — no root, nothing
  installed — including that every condition still fails closed.

* Thu Sep 17 2026 Nam-tor Project <namtor@example.invalid> - 0.3.4-1
- Pressing "Save Profile" aborted the settings app. AdwNavigationView emits
  `popped` synchronously, so the back-navigation handler added in 0.3.2 ran
  inside a render that already held the controller's RefCell borrow; with
  panic=abort the process died. Anyone who reached the review screen and
  pressed Save in 0.3.3 lost the app.
- Every PAM service toggle applied twice, producing two polkit prompts for one
  switch, and let the second result overwrite the first.
- A stuck enrollment worker kept the IR emitter armed through suspend while
  the journal reported the session cancelled.
- A profile save completing after the user pressed Back was applied anyway,
  stranding the UI on a page whose buttons did nothing.
- A failed emitter force-off silently disabled the watchdog; the emitter was
  lit before the stream started rather than after.
- The emitter-state record path came from the environment, which could
  silently disarm the SIGKILL backstop.
- The screen-reader throttle never throttled (250 ms interval against a 500 ms
  render loop); it now suppresses repeats of unchanged guidance.
- lifecycle.sh could report "no ONNX models" on a machine that has them.
- Spec 8.3 failure conditions are now a typed set on the wire with a
  fault-injection matrix; PAM logs the specific condition rather than a
  verdict word.

* Thu Sep 17 2026 Nam-tor Project <namtor@example.invalid> - 0.3.3-1
- Stopping an enrollment scan left the IR emitter lit for as long as the
  Review screen stayed open. The capture source is kept across Stop so Resume
  can restart it, stop() only halted the stream, and the per-frame watchdog
  cannot fire on a source that pulls no frames. Stop now darkens the emitter
  and Resume re-arms it.
- Pausing and resuming an enrollment broke face unlock until the daemon was
  restarted. Re-requesting V4L2 buffers orphaned the first set of mappings,
  which kept the driver queue busy, so every later capture failed with EBUSY
  and PAM silently fell back to the password.
- The daemon did nothing on suspend. Captures now close on logind
  PrepareForSleep, under a delay inhibitor so logind waits for the write.
- A refused remote-host request after a clean Prepare skipped the connection
  cleanup and left the capture open for the warm window.
- The SIGKILL backstop no longer hardcodes /dev/video2 or the reference
  camera's payload; it re-resolves the device from its USB identity.
- Saving a profile no longer freezes the window; the commit runs on a worker
  thread behind a real saving state. Saving a scan the daemon calls unfinished
  now asks first.
- Gate G1.5 measured on all four teardown paths (end, stop, suspend, SIGKILL):
  control-plane teardown 4.8-49.5 ms median against a 200 ms budget.

* Thu Sep 17 2026 Nam-tor Project <namtor@example.invalid> - 0.3.2-1
- The enrollment scan page destroyed itself. The 500 ms progress poll was
  registered inside the render it triggers, with its source id discarded, so
  the timer count doubled on every tick that produced progress - 1024 timers
  after five seconds, each issuing its own blocking D-Bus call on the GTK main
  thread. The scan page froze the app within seconds of a real scan.
- Every error route dead-ended. The Error state built a Dismiss button and
  never added it to a container, discarded the message, and re-rendered the
  capture page with Stop and Cancel wired to empty closures - the destination
  for a missing camera, a polkit denial, a busy session and a review timeout
  alike. It now shows what failed, the recovery steps, and two buttons that
  work.
- Pressing Back permanently broke Enroll. Nothing observed AdwNavigationView's
  back button, Escape or swipe gesture, so the controller kept a page that was
  no longer on the stack: Enroll then rendered into a detached page and did
  nothing for the rest of the process, while the daemon session stayed open
  with the camera and IR emitter running.
- A polkit denial on a PAM toggle was invisible: the result was discarded, the
  switch flipped and quietly flipped back 50 ms later.
- Three screens presented invented values as measurements: the scan announced
  "0% - Poor" before the camera returned a frame, the review screen derived
  its quality verdict from readiness rather than quality, and the System page
  reported "IR Camera - Detected" from nothing but daemon reachability.
- A daemon that died mid-scan was indistinguishable from a scan that stopped
  improving.
- An unrecognised guidance word was printed verbatim as the page headline.
- The screen-reader announcement throttle was wired up; it had been written,
  tested and never called while two widgets re-announced on every poll.
- "last used: epoch 1726550400" is now a readable relative time, and the two
  troubleshooting rows that did nothing now show a copyable command.
- tr() actually loads a gettext catalog; nothing had ever called into gettext,
  so no translation could be loaded even if one were shipped.

* Thu Sep 17 2026 Nam-tor Project <namtor@example.invalid> - 0.3.1-1
- Ship the PAM editor. pam-install.sh, pam-uninstall.sh and lifecycle.sh are
  all installed to /usr/share/namtor/ and all three drove a `namtor-pam-edit`
  binary that the install manifest has never shipped. lifecycle.sh warns and
  skips when it cannot find it, so every PAM lifecycle phase was a silent
  no-op: pre-upgrade snapshot, post-upgrade migrate, pre-remove disable-all and
  post-remove purge never ran, and removing the package left pam_namtor.so
  entries in /etc/pam.d pointing at a module that had just been deleted. The
  editor is now `namtor pam-edit`, a fifth role of the multi-call binary, and
  adds no bytes because namtor-daemon already linked the crate.
- Refuse to enable a PAM service that has no password fallback. The check
  classified the candidate stack, which already contained the namtor block,
  and the classifier short-circuits on that block and returns Safe before it
  evaluates the fallback rule — so the check was defeated by the very block it
  exists to validate. It classifies the stock stack now.
- Refuse to write a PAM block referencing a pam_namtor.so that is not
  installed. EditError::ModuleNotFound existed and was mapped to a D-Bus error
  but nothing ever produced it. The preflight runs before the backup, so a
  refusal leaves no trace.
- `namtor pam-edit restore` reverts a service to its pre-Nam-tor bytes from the
  verified backup, undoing damage that `disable` would carry forward.
- Group resolution no longer reads its path from NAMTOR_GROUP_FILE, which let
  anything able to set the daemon's environment redirect the group-membership
  lookup that decides who may authenticate.
- Cover the no-TPM software-fallback path in the settings UI, which had no test
  at all, and run the three CI gates that no workflow ran.

* Thu Sep 17 2026 Nam-tor Project <namtor@example.invalid> - 0.3.0-1
- One `namtor` multi-call binary replaces namtor-daemon, namtor-worker,
  namtor-cli and namtor-camera-emitter-off: 73.7 MB of separate executables
  become 26 MB. The worker is still a separate PROCESS, re-exec'd through
  /proc/self/exe, so the seccomp+Landlock privilege boundary is unchanged.
- The worker sandbox is now actually applied. The daemon had never passed
  --apply-sandbox, so inference ran unconfined despite every document saying
  otherwise. Models are preloaded before the process seals itself.
- The analytic PAD radial gate no longer vetoes what it cannot assess. It
  had refused 23 of 24 genuine faces; measured after the fix, 23 of 24
  authenticate. Spoof resistance remains unmeasured (G4 WITHHELD).
* Thu Sep 17 2026 Nam-tor Project <namtor@example.invalid> - 0.2.0-1
- Release 0.2.0. The shipped package set changed, so this is a minor version
  bump rather than another revision: namtor-kde is gone and namtor-gnome is
  now namtor-ui. namtor-ui keeps Provides/Obsoletes for both former names so
  dnf migrates an existing install instead of leaving it stranded.
- Enrollment guidance is derived from real per-frame measurements (blur,
  illumination, pose, face size, saturated-sample fraction) rather than one
  fabricated scalar copied across four fields; all nine guidance messages
  are reachable and quality is continuous instead of three fixed values.
- The core subpackage now ships manual pages (namtor-daemon.8, namtor-cli.1,
  pam_namtor.8); the %files section previously listed none at all.
* Wed Sep 16 2026 Nam-tor Project <namtor@example.invalid> - 0.1.0-4
- Rename the namtor-gnome subpackage to namtor-ui: it is the single,
  desktop-neutral GTK4/libadwaita UI and has no GNOME-only runtime
  dependency, so the old name was the last desktop-specific artifact.
- namtor-ui carries `Provides: namtor-gnome = ver-rel` and
  `Obsoletes: namtor-gnome < ver-rel`. dnf does not migrate a rename on
  its own, and the two subpackages own exactly the same four files, so
  without this the upgrade fails on a file conflict.
- The install manifest now says `package = "ui"`; `namtor-layout` aliases
  the old `"gnome"` value so uninstall and diagnostics keep working
  against a manifest installed before the rename.

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
