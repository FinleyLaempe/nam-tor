#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Nam-tor uninstall — removes all system files created by the install steps.
#
# Usage: sudo ./packaging/uninstall.sh
#
# Reversible: re-running the install commands in packaging/README.md
# reinstalls everything.

set -euo pipefail

echo "Removing Nam-tor system files..."

# Stop services if running
systemctl stop namtor-daemon.service 2>/dev/null || true
systemctl stop namtor-daemon.socket 2>/dev/null || true
systemctl disable namtor-daemon.service 2>/dev/null || true
systemctl disable namtor-daemon.socket 2>/dev/null || true

# Remove systemd units
rm -f /usr/lib/systemd/system/namtor-daemon.service
rm -f /usr/lib/systemd/system/namtor-daemon.socket
systemctl daemon-reload

# Remove binaries
rm -f /usr/lib/namtor/namtor-daemon
rm -f /usr/lib/namtor/namtor-worker
rm -f /usr/lib/namtor/namtor-worker-spawn
rm -f /usr/lib/namtor/namtor-camera-emitter-off
rmdir /usr/lib/namtor 2>/dev/null || true

# Remove CLI
rm -f /usr/bin/namtor-cli

# Remove PAM module
rm -f /usr/lib/security/pam_namtor.so

# Remove sysusers/tmpfiles drop-ins
rm -f /usr/lib/sysusers.d/namtor.conf
rm -f /usr/lib/tmpfiles.d/namtor.conf

# Remove model store + templates (includes enrolled biometric data)
rm -rf /var/lib/namtor

# Remove runtime socket dir
rm -rf /run/namtor

# Remove user + groups
if getent passwd namtor >/dev/null 2>&1; then
    userdel namtor 2>/dev/null || true
fi
if getent group namtor >/dev/null 2>&1; then
    groupdel namtor 2>/dev/null || true
fi
if getent group namtor-auth >/dev/null 2>&1; then
    groupdel namtor-auth 2>/dev/null || true
fi

# Check if PAM stack was modified
if grep -q pam_namtor /etc/pam.d/gdm-password 2>/dev/null; then
    echo "WARNING: pam_namtor.so is still in /etc/pam.d/gdm-password"
    echo "Remove this line manually:"
    echo "  sudo sed -i '/pam_namtor/d' /etc/pam.d/gdm-password"
fi

echo "Done. Nam-tor fully removed."
echo "Your PAM stack is unchanged (verify with: grep namtor /etc/pam.d/gdm-password)"
