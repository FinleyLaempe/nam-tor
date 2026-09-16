#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Deterministic DESTDIR staging from packaging/install-manifest.toml.
#
# Usage: packaging/stage.sh <DESTDIR> [SRCDIR] [--package core|gnome|all]
#
#   DESTDIR  — empty root to stage into (created if absent).
#   SRCDIR   — repo root containing both the build output (target/release/)
#              and the packaging/ tree. Defaults to the current git toplevel.
#   --package — stage only entries owned by the given split package
#              (core, gnome). Defaults to "all" (stage every entry).
#
# Every file declared in install-manifest.toml is copied from
# $SRCDIR/<src> to $DESTDIR/<dest> with the declared mode. Owners/groups are
# applied only when running as root (non-root staging leaves ownership as the
# invoking user and emits a warning — modes and content are still deterministic,
# which is what the evidence gate checks).
#
# Split-package filtering: entries with `package = "core"` belong to the
# `namtor` package and `package = "gnome"` to `namtor-gnome`. When
# `--package` is specified, only that package's entries (dirs + files) are
# staged. Shared directories owned by core (e.g. /usr/share/applications)
# are staged under core; the desktop package stages only its own files and
# relies on core having created the parent directories.
#
# Determinism contract: running this script twice into two empty DESTDIRs
# yields identical file trees (same paths, modes, content sha256). Proven in
# docs/evidence/install-manifest.json.
#
# Requires python3 (stdlib tomllib, Python >= 3.11) for TOML parsing.

set -euo pipefail

manifest="packaging/install-manifest.toml"
package_filter="all"

args=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --package)
            package_filter="$2"
            shift 2
            ;;
        --package=*)
            package_filter="${1#--package=}"
            shift
            ;;
        *)
            args+=("$1")
            shift
            ;;
    esac
done
set -- "${args[@]}"

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <DESTDIR> [SRCDIR] [--package core|gnome|all]" >&2
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

case "$package_filter" in
    core|gnome|all) ;;
    *)
        echo "error: --package must be one of: core, gnome, all (got: $package_filter)" >&2
        exit 2
        ;;
esac

if [[ "$(id -u)" -ne 0 ]]; then
    echo "warning: not running as root — owner/group will not be applied; \
modes and content remain deterministic" >&2
fi

python3 - "$manifest_path" "$destdir" "$srcdir" "$package_filter" <<'PY'
import os
import shutil
import stat
import sys
import tomllib

manifest_path, destdir, srcdir, package_filter = (
    sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
)

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

def entry_package(entry: dict) -> str:
    return entry.get("package", "core")

def matches_filter(entry: dict) -> bool:
    if package_filter == "all":
        return True
    return entry_package(entry) == package_filter

# Directories first (sorted for determinism).
dirs = [d for d in m.get("dirs", []) if matches_filter(d)]
for d in sorted(dirs, key=lambda x: x["path"]):
    target = os.path.join(destdir, d["path"].lstrip("/"))
    os.makedirs(target, exist_ok=True)
    os.chmod(target, octmode(d["mode"]))
    resolve_owner(target, d["owner"], d["group"])

# Then files (sorted by dest path).
files = [f for f in m.get("files", []) if matches_filter(f)]
for f in sorted(files, key=lambda x: x["dest"]):
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

print(f"staged {len(dirs)} dirs + {len(files)} files "
      f"into {destdir}"
      + (f" (package: {package_filter})" if package_filter != "all" else ""))
PY
