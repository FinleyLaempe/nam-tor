#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Generate an in-toto/SLSA provenance attestation for a Nam-tor build.

Produces a JSON statement following the SLSA v1.0 provenance schema
(``https://slsa.dev/provenance/v1``) wrapped in an in-toto statement
(predicateType ``https://slsa.dev/provenance/v1``). The statement records:

- **subject**    — sha256 of each release artifact under ``target/release/``.
- **builder**    — id (host), the cargo/rustc toolchain versions.
- **buildType**  — a stable URI for "cargo build --release from a git checkout".
- **materials**  — the git commit URI + sha256 of ``Cargo.lock``.
- **byproducts** — the SBOM files referenced by their own sha256.

All hashes are computed locally (no network). The attestation is self-
contained: a verifier can check subject hashes against the artifacts and
material hashes against the repo without trusting the builder.

Usage::

    packaging/generate-provenance.py [SRCDIR] [--out PATH]

    SRCDIR  repo root (default: git toplevel).
    --out   output file (default: <SRCDIR>/docs/evidence/packaging/
            namtor.provenance.json).
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import subprocess
import sys


def _git_toplevel() -> str:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=False)
    return out.stdout.strip() if out.returncode == 0 else os.getcwd()


def _git(args: list[str], srcdir: str) -> str:
    out = subprocess.run(
        ["git", *args], cwd=srcdir, capture_output=True, text=True,
        check=False)
    return out.stdout.strip() if out.returncode == 0 else "UNKNOWN"


def _sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def _toolchain() -> dict[str, str]:
    info: dict[str, str] = {}
    for tool in ("cargo", "rustc"):
        out = subprocess.run(
            [tool, "--version"], capture_output=True, text=True, check=False)
        info[tool] = out.stdout.splitlines()[0] if out.stdout else "UNKNOWN"
    return info


def _subjects(srcdir: str) -> list[dict]:
    """sha256 of each release artifact that the manifest installs."""
    release = os.path.join(srcdir, "target", "release")
    subjects = []
    if not os.path.isdir(release):
        return subjects
    names = [
        "namtor-daemon", "namtor-worker", "namtor-worker-spawn",
        "namtor-camera-emitter-off", "namtor-cli", "namtor-enroll-wizard",
        "libnamtor_pam.so",
    ]
    for name in names:
        path = os.path.join(release, name)
        if os.path.isfile(path):
            subjects.append({
                "name": name,
                "digest": {"sha256": _sha256(path)},
            })
    return subjects


def _byproducts(srcdir: str) -> list[dict]:
    """Reference the generated SBOM files by their own sha256."""
    evidence = os.path.join(srcdir, "docs", "evidence", "packaging")
    out = []
    for fname in ("namtor.cyclonedx.json", "namtor.spdx.json"):
        path = os.path.join(evidence, fname)
        if os.path.isfile(path):
            out.append({
                "name": fname,
                "digest": {"sha256": _sha256(path)},
            })
    return out


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("srcdir", nargs="?", default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args(argv)
    srcdir = args.srcdir or _git_toplevel()
    out_path = args.out or os.path.join(
        srcdir, "docs", "evidence", "packaging",
        "namtor.provenance.json")
    os.makedirs(os.path.dirname(out_path), exist_ok=True)

    commit = _git(["rev-parse", "HEAD"], srcdir)
    if commit == "UNKNOWN":
        commit = _git(["log", "-1", "--format=%H"], srcdir)
    branch = _git(["rev-parse", "--abbrev-ref", "HEAD"], srcdir)
    remote = _git(["config", "--get", "remote.origin.url"], srcdir)
    lock_hash = _sha256(os.path.join(srcdir, "Cargo.lock"))
    tc = _toolchain()

    subjects = _subjects(srcdir)
    if not subjects:
        print("warning: no release artifacts found in target/release/ — "
              "run `cargo build --release` first; attestation will have "
              "an empty subject", file=sys.stderr)

    statement = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": subjects,
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {
            "buildDefinition": {
                "buildType":
                    "https://namtor.dev/buildtypes/cargo-release-v1",
                "externalParameters": {
                    "profile": "release",
                    "target": "workspace",
                },
                "internalParameters": {
                    "cargo": tc.get("cargo", "UNKNOWN"),
                    "rustc": tc.get("rustc", "UNKNOWN"),
                    "panic": "abort",
                    "lto": True,
                    "codegen-units": 1,
                    "strip": True,
                },
                "resolvedDependencies": [
                    {
                        "uri": (f"git+{remote}"
                                if remote and remote != "UNKNOWN"
                                else "git+local"),
                        "digest": {"gitCommit": commit},
                    },
                    {
                        "uri": "file://Cargo.lock",
                        "digest": {"sha256": lock_hash},
                    },
                ],
            },
            "runDetails": {
                "builder": {
                    "id": f"host://{os.uname().nodename}",
                },
                "metadata": {
                    "invocationId": commit,
                    "startedOn": _dt.datetime.now(
                        _dt.timezone.utc).isoformat(),
                },
                "byproducts": _byproducts(srcdir),
            },
        },
    }

    with open(out_path, "w") as f:
        json.dump(statement, f, indent=2, sort_keys=True)
    print(f"Provenance: {len(subjects)} subjects, commit {commit[:12]} "
          f"-> {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
