#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Generate an in-toto/SLSA provenance attestation for a Nam-tor build.

Produces a JSON statement following the SLSA v1.0 provenance schema
(``https://slsa.dev/provenance/v1``) wrapped in an in-toto statement
(predicateType ``https://slsa.dev/provenance/v1``). The statement records:

- **subject**    — sha256 of each release artifact under ``target/release/`` AND
                   the deterministic model archive
                   (``namtor-models-<version>.tar.gz``) produced by
                   ``packaging/build-model-archive.sh``. The model archive is
                   MANDATORY: no distro recipe may build without it, so the
                   attestation refuses to emit an empty subject if the archive
                   is absent (``--allow-no-models`` overrides for CI builds
                   that intentionally skip the model bundle).
- **builder**    — id (host), the cargo/rustc toolchain versions.
- **buildType**  — a stable URI for "cargo build --release from a git checkout".
- **materials**  — the git commit URI + sha256 of ``Cargo.lock``.
- **byproducts** — the SBOM files referenced by their own sha256.

All hashes are computed locally (no network). The attestation is self-
contained: a verifier can check subject hashes against the artifacts and
material hashes against the repo without trusting the builder.

Determinism: the attestation uses a fixed epoch
(``2026-01-01T00:00:00Z``), a stable builder ID (no hostname), and sorted
keys. Running the generator twice produces byte-identical output. No host
paths or machine identifiers appear anywhere in the attestation.

The attestation also records per-model provenance in the predicate
(``modelProvenance``): for each of the three ONNX models, the sha256,
licence SPDX id, licence sha256, upstream URL, and the Nam-tor git commit
the model was built from. The MiniFASNet PAD entry references its model
card (``ml/reports/learned-pad.md``) and training recipe.

Usage::

    packaging/generate-provenance.py [SRCDIR] [--out PATH]
        [--model-archive PATH] [--version VER] [--allow-no-models]

    SRCDIR            repo root (default: git toplevel).
    --out             output file (default: <SRCDIR>/docs/evidence/packaging/
                      namtor.provenance.json).
    --model-archive   path to the deterministic model archive
                      (default: <SRCDIR>/namtor-models-<version>.tar.gz).
    --version         archive version tag (default: 0.1.0).
    --allow-no-models emit attestation even when the model archive is absent
                      (default: refuse — model archives are mandatory).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tomllib

_FIXED_EPOCH_ISO = "2026-01-01T00:00:00Z"
_BUILDER_ID = "https://namtor.dev/builders/cargo-release-v1"


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


def _subjects(srcdir: str, model_archive: str | None) -> list[dict]:
    """sha256 of each release artifact that the manifest installs, plus the
    deterministic model archive (mandatory)."""
    release = os.path.join(srcdir, "target", "release")
    subjects = []
    if os.path.isdir(release):
        names = [
            "namtor-daemon", "namtor-worker",
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
    if model_archive and os.path.isfile(model_archive):
        subjects.append({
            "name": os.path.basename(model_archive),
            "digest": {"sha256": _sha256(model_archive)},
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


def _model_provenance(srcdir: str, commit: str) -> list[dict]:
    """Per-model provenance: sha256, licence, upstream URL, build commit."""
    path = os.path.join(srcdir, "packaging", "model-manifest.toml")
    if not os.path.isfile(path):
        return []
    with open(path, "rb") as f:
        data = tomllib.load(f)
    out = []
    for m in data.get("models", []):
        mid = m["id"]
        variants = m.get("variants", [])
        sha = variants[0]["sha256"] if variants else ""
        lic_sha = m.get("licence_sha256", "")
        lic_id = _licence_id(lic_sha)
        upstream = _upstream_url(mid)
        entry = {
            "modelId": mid,
            "sha256": sha,
            "licence": lic_id,
            "licenceSha256": lic_sha,
            "upstreamUrl": upstream,
            "builtFromCommit": commit,
        }
        if mid == "minifasnet_pad":
            entry["modelCard"] = "ml/reports/learned-pad.md"
            entry["trainingRecipe"] = "ml/recipes/train_pad.py"
            entry["notice"] = "packaging/licences/minifasnet_pad.NOTICE"
        out.append(entry)
    return out


def _licence_id(licence_sha256: str) -> str:
    if licence_sha256 == ("c83b8120c50ccbd4c4f96edf53141bdd"
                          "566ebb8f8e9227e415326aa1b1aba958"):
        return "MIT"
    if licence_sha256 == ("cfc7749b96f63bd31c3c42b5c471bf756"
                          "814053e847c10f3eb003417bc523d30"):
        return "Apache-2.0"
    return "NOASSERTION"


def _upstream_url(model_id: str) -> str:
    urls = {
        "yunet_2023mar": ("https://github.com/opencv/opencv_zoo/raw/main/"
                          "models/face_detection_yunet/"
                          "face_detection_yunet_2023mar.onnx"),
        "sface_2021dec": ("https://github.com/opencv/opencv_zoo/raw/main/"
                          "models/face_recognition_sface/"
                          "face_recognition_sface_2021dec.onnx"),
        "minifasnet_pad": "self-trained",
    }
    return urls.get(model_id, "NOASSERTION")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("srcdir", nargs="?", default=None)
    ap.add_argument("--out", default=None)
    ap.add_argument("--model-archive", default=None)
    ap.add_argument("--version", default="0.1.0")
    ap.add_argument("--allow-no-models", action="store_true")
    args = ap.parse_args(argv)
    srcdir = args.srcdir or _git_toplevel()
    out_path = args.out or os.path.join(
        srcdir, "docs", "evidence", "packaging",
        "namtor.provenance.json")
    os.makedirs(os.path.dirname(out_path), exist_ok=True)

    model_archive = args.model_archive or os.path.join(
        srcdir, f"namtor-models-{args.version}.tar.gz")

    commit = _git(["rev-parse", "HEAD"], srcdir)
    if commit == "UNKNOWN":
        commit = _git(["log", "-1", "--format=%H"], srcdir)
    branch = _git(["rev-parse", "--abbrev-ref", "HEAD"], srcdir)
    remote = _git(["config", "--get", "remote.origin.url"], srcdir)
    lock_hash = _sha256(os.path.join(srcdir, "Cargo.lock"))
    tc = _toolchain()

    subjects = _subjects(srcdir, model_archive)
    has_model = any(
        s["name"].startswith("namtor-models-") for s in subjects)
    if not subjects:
        print("warning: no release artifacts found in target/release/ — "
              "run `cargo build --release` first; attestation will have "
              "an empty subject", file=sys.stderr)
    if not has_model and not args.allow_no_models:
        print(f"error: model archive not found at {model_archive} — "
              f"run `packaging/build-model-archive.sh --version {args.version}` "
              f"first; model archives are mandatory for every public build "
              f"(use --allow-no-models for CI-only builds that skip the bundle)",
              file=sys.stderr)
        return 1

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
                    "id": _BUILDER_ID,
                },
                "metadata": {
                    "invocationId": commit,
                    "startedOn": _FIXED_EPOCH_ISO,
                },
                "byproducts": _byproducts(srcdir),
            },
            "modelProvenance": _model_provenance(srcdir, commit),
        },
    }

    with open(out_path, "w") as f:
        json.dump(statement, f, indent=2, sort_keys=True)
    print(f"Provenance: {len(subjects)} subjects, commit {commit[:12]} "
          f"-> {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
