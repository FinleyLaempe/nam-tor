# Activation and installation investigation (#29, #33)

## Findings

- The D-Bus activation file omitted `SystemdService`. A D-Bus client therefore
  started a transient daemon without the systemd socket FD or hardening.
- The shipped daemon unit already requires `namtor-daemon.socket`; delegating
  bus activation to that unit preserves the root-owned socket and avoids an
  unprivileged bind. No socket-directory permission or group widening is needed.
- The reference host's broker warnings occurred at **22:43:13** on 2026-11-19,
  during package extraction / the start of `post_install`, not the later reload.
  `/var/log/pacman.log` records `20-systemd-sysusers.hook` at **22:43:14** and
  `dbus-reload.hook` at **22:43:16**. The journal records a successful D-Bus reload
  at **22:43:16**, with no further namtor parse warnings in that boot at inspection.
- The issue's proposed persistent negative-NSS-cache explanation is unproven.
  A broker directory-watch parse before account creation fits the timestamps.
  Arch now creates the identities in `pre_install`, before files become visible.

## Verification and limits

`packaging/tests/test_dbus_activation.sh` failed first on the absent activation
delegation, then on the absent pre-install function. It passes after the fixes.
It exercises the real Arch scriptlet with an isolated sysusers command, asserts
the exact identity declarations match the shipped config, checks failure
propagation, and checks the systemd socket/service contract. It never modifies
the host's users or services.

No reboot, system bus restart, installed package replacement, or real fresh
package installation was performed for this investigation. A clean fresh-boot
broker journal (issue #33 task 1) remains unverified; do not describe the
packaging regression as that hardware/host evidence.
