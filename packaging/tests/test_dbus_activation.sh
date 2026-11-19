#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# D-Bus and PAM activation must converge on the hardened socket-owning unit.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
python3 - "$ROOT" <<'PY'
import configparser
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1])
service = configparser.ConfigParser(interpolation=None)
service.read(root / 'packaging/dbus-1/org.namtor.Manager.service')
activation = service['D-BUS Service']
assert activation.get('SystemdService') == 'namtor-daemon.service', (
    'D-Bus must start the hardened service, not a second transient daemon')
unit = (root / 'packaging/systemd/namtor-daemon.service').read_text()
socket = (root / 'packaging/systemd/namtor-daemon.socket').read_text()
assert 'Requires=namtor-daemon.socket' in unit
assert 'SocketUser=root' in socket and 'SocketGroup=namtor-auth' in socket
assert 'SocketMode=0660' in socket

# pacman extracts D-Bus policy files before post_install. A broker can notice
# them immediately, before the transaction's later sysusers hook is reached.
# Exercise pre_install with a fake sysusers command: identities must already
# exist by the time pacman makes the policy visible.
with tempfile.TemporaryDirectory(prefix='namtor-activation-') as tmp:
    tmp = pathlib.Path(tmp)
    sysusers = tmp / 'systemd-sysusers'
    sysusers.write_text('#!/bin/sh\n[ "$1" = "-" ] || exit 42\ncat > "$NAMTOR_TEST_IDENTITIES"\n')
    sysusers.chmod(0o755)
    import os
    env = os.environ.copy()
    env['PATH'] = str(tmp) + os.pathsep + env['PATH']
    identities = tmp / 'identities'
    env['NAMTOR_TEST_IDENTITIES'] = str(identities)
    subprocess.run(['bash', '-c', 'source "$1"; pre_install', '_',
                    str(root / 'packaging/arch/namtor.install')], env=env, check=True)
    declarations = lambda text: [line.strip() for line in text.splitlines()
                                  if line.strip() and not line.lstrip().startswith('#')]
    assert declarations(identities.read_text()) == declarations(
        (root / 'packaging/sysusers.d/namtor.conf').read_text()), (
            'pre-install identities must match the shipped sysusers config')
    # A sysusers error must be visible instead of continuing with no identity.
    sysusers.write_text('#!/bin/sh\nexit 42\n')
    result = subprocess.run(['bash', '-c', 'source "$1"; pre_install', '_',
                             str(root / 'packaging/arch/namtor.install')], env=env)
    assert result.returncode != 0, 'identity creation failure must abort pre-install'
print('PASS: unified D-Bus/socket activation and pre-extraction identities')
PY
