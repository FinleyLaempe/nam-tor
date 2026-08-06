#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Deterministic DESTDIR staging from packaging/install-manifest.toml.
#
# Usage: packaging/stage.sh <DESTDIR> [SRCDIR]
#
#   DESTDIR  — empty root to stage into (created if absent).
#   SRCDIR   — repo root containing both the build output (target/release/)
#              and the packaging/ tree. Defaults to the current git toplevel.
#
# Every file declared in install-manifest.toml is copied from
# $SRCDIR/<src> to $DESTDIR/<dest> with the declared mode. Owners/groups are
# applied only when running as root (non-root staging leaves ownership as the
# invoking user and emits a warning — modes and content are still deterministic,
# which is what the evidence gate checks).
#
# Determinism contract: running this script twice into two empty DESTDIRs
# yields identical file trees (same paths, modes, content sha256). Proven in
# docs/evidence/install-manifest.json.
#
# Requires python3 (stdlib tomllib, Python >= 3.11) for TOML parsing.

set -euo pipefail

manifest="packaging/install-manifest.toml"

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <DESTDIR> [SRCDIR]" >&2
    exit 2
fi
destdir="$1"
if [[ $# -ge 2 ]]; then
    srcdir="$2"
else
    srcdir="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
fi

manifest_path="$srcdir/$manifest"
if [[ ! -f "$manifest_path" ]]; then
    echo "error: manifest not found: $manifest_path" >&2
    exit 1
fi

if [[ "$(id -u)" -ne 0 ]]; then
    echo "warning: not running as root — owner/group will not be applied; \
modes and content remain deterministic" >&2
fi

python3 - "$manifest_path" "$destdir" "$srcdir" <<'PY'
import os
import shutil
import stat
import sys
import tomllib

manifest_path, destdir, srcdir = sys.argv[1], sys.argv[2], sys.argv[3]

with open(manifest_path, "rb") as f:
    m = tomllib.load(f)

def octmode(s: str) -> int:
    return int(s, 8)

def resolve_owner(path: str, owner: str, group: str):
    if os.geteuid() != 0:
        return
    try:
        import pwd, grp
        uid = pwd.getpwnam(owner).pw_uid
        gid = grp.getgrnam(group).gr_gid
        os.chown(path, uid, gid, follow_symlinks=False)
    except KeyError:
        # In a DESTDIR sandbox the named users may not exist; leave numeric.
        pass

# Directories first (sorted for determinism).
for d in sorted(m.get("dirs", []), key=lambda x: x["path"]):
    target = os.path.join(destdir, d["path"].lstrip("/"))
    os.makedirs(target, exist_ok=True)
    os.chmod(target, octmode(d["mode"]))
    resolve_owner(target, d["owner"], d["group"])

# Then files (sorted by dest path).
for f in sorted(m.get("files", []), key=lambda x: x["dest"]):
    dest = os.path.join(destdir, f["dest"].lstrip("/"))
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    src = os.path.join(srcdir, f["src"])
    if f.get("symlink", False):
        if os.path.lexists(dest):
            os.remove(dest)
        os.symlink(f["src"], dest)
    else:
        if not os.path.exists(src):
            raise SystemExit(f"error: staging source missing: {src} "
                             f"(for {f['dest']})")
        # Copy without preserving source metadata; set explicitly below.
        shutil.copyfile(src, dest)
        os.chmod(dest, octmode(f["mode"]))
        resolve_owner(dest, f["owner"], f["group"])

print(f"staged {len(m.get('dirs', []))} dirs + {len(m.get('files', []))} files "
      f"into {destdir}")
PY
