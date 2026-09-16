#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
# allow: SIZE_OK — single-purpose SBOM generator emitting two formats
# (CycloneDX 1.5 + SPDX 2.3) from three input sources (Cargo.lock,
# model-manifest.toml, release binaries). Splitting would scatter one
# cohesive transform across files for no reviewer benefit.
"""Generate SBOM (CycloneDX 1.5 + SPDX 2.3) for the Nam-tor workspace.

Parses ``Cargo.lock`` with the stdlib ``tomllib`` (Python >= 3.11) for
name / version / checksum / dependency edges, and cross-references
``cargo metadata --format-version 1`` for the licence field of each
package. ``cargo metadata`` runs offline against the local registry
cache; if it is unavailable the licence is recorded as ``UNKNOWN`` and
generation proceeds (the SBOM is still complete in every other field).

In addition to Cargo dependencies, the SBOM includes:

- **Release binaries** — namtor-daemon, namtor-worker, namtor-cli,
  namtor-camera-emitter-off, pam_namtor.so (libnamtor_pam.so),
  and namtor-enroll-wizard when
  built. Each is a component with a sha256 hash.
- **ONNX models** — yunet_2023mar, sface_2021dec, minifasnet_pad, read
  from ``packaging/model-manifest.toml``. Each carries its sha256,
  licence, upstream URL, and the Nam-tor git commit it was built from.

Determinism: the SBOM uses a fixed epoch (2026-01-01T00:00:00Z),
deterministic UUIDs (uuid5 from a stable namespace), sorted entries,
and no host paths or machine identifiers. Running the generator twice
produces byte-identical output.

No network access is required at any point.

Usage::

    packaging/generate-sbom.py [SRCDIR] [--out DIR]

    SRCDIR  repo root containing Cargo.lock (default: git toplevel).
    --out   output directory (default: <SRCDIR>/docs/evidence/packaging).

Writes ``namtor.cyclonedx.json`` and ``namtor.spdx.json``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tomllib
import uuid

# Deterministic build epoch — all timestamps in the SBOM use this value.
_FIXED_EPOCH_ISO = "2026-01-01T00:00:00Z"
_SBOM_NAMESPACE = uuid.UUID("6ba7b810-9dad-11d1-80b4-00c04fd430c8")


def _git_toplevel() -> str:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=False,
    )
    return out.stdout.strip() if out.returncode == 0 else os.getcwd()


def _git(args: list[str], srcdir: str) -> str:
    out = subprocess.run(
        ["git", *args], cwd=srcdir, capture_output=True, text=True,
        check=False)
    return out.stdout.strip() if out.returncode == 0 else "UNKNOWN"


def _load_lock(srcdir: str) -> list[dict]:
    with open(os.path.join(srcdir, "Cargo.lock"), "rb") as f:
        return tomllib.load(f)["package"]


def _load_model_manifest(srcdir: str) -> list[dict]:
    """Read packaging/model-manifest.toml and return per-model provenance."""
    path = os.path.join(srcdir, "packaging", "model-manifest.toml")
    if not os.path.isfile(path):
        return []
    with open(path, "rb") as f:
        data = tomllib.load(f)
    return data.get("models", [])


def _binary_components(srcdir: str) -> list[dict]:
    """sha256 of each release binary the manifest installs."""
    release = os.path.join(srcdir, "target", "release")
    names = [
        ("namtor-daemon", "namtor-daemon"),
        ("namtor-worker", "namtor-worker"),
        ("namtor-cli", "namtor-cli"),
        ("namtor-camera-emitter-off", "namtor-camera-emitter-off"),
        ("libnamtor_pam.so", "pam_namtor.so"),
        ("namtor-enroll-wizard", "namtor-enroll-wizard"),
    ]
    out = []
    for src_name, display_name in names:
        path = os.path.join(release, src_name)
        if os.path.isfile(path):
            out.append({
                "name": display_name,
                "version": "0.1.0",
                "sha256": _sha256_file(path),
                "licence": "GPL-3.0-only",
                "kind": "binary",
            })
    return out


def _model_components(srcdir: str, manifest_models: list[dict]) -> list[dict]:
    """ONNX model components with sha256, licence, upstream URL."""
    commit = _git(["rev-parse", "HEAD"], srcdir)
    out = []
    for m in manifest_models:
        mid = m["id"]
        licence_id = _model_licence_id(m)
        variants = m.get("variants", [])
        sha = variants[0]["sha256"] if variants else ""
        upstream = _model_upstream_url(mid)
        out.append({
            "name": mid,
            "version": "0.1.0",
            "sha256": sha,
            "licence": licence_id,
            "licence_sha256": m.get("licence_sha256", ""),
            "upstream_url": upstream,
            "built_from_commit": commit,
            "kind": "model",
            "model_card": _model_card_ref(srcdir, mid),
        })
    return out


def _model_licence_id(model: dict) -> str:
    sha = model.get("licence_sha256", "")
    if sha == "c83b8120c50ccbd4c4f96edf53141bdd566ebb8f8e9227e415326aa1b1aba958":
        return "MIT"
    if sha == "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30":
        return "Apache-2.0"
    return "NOASSERTION"


def _model_upstream_url(model_id: str) -> str:
    urls = {
        "yunet_2023mar": "https://github.com/opencv/opencv_zoo/raw/main/"
                         "models/face_detection_yunet/"
                         "face_detection_yunet_2023mar.onnx",
        "sface_2021dec": "https://github.com/opencv/opencv_zoo/raw/main/"
                         "models/face_recognition_sface/"
                         "face_recognition_sface_2021dec.onnx",
        "minifasnet_pad": "self-trained",
    }
    return urls.get(model_id, "NOASSERTION")


def _model_card_ref(srcdir: str, model_id: str) -> str:
    if model_id == "minifasnet_pad":
        return "ml/reports/learned-pad.md"
    return ""


def _sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def _cargo_metadata(srcdir: str) -> dict[str, str]:
    """Return name|version -> licence string (offline). Empty on failure."""
    try:
        out = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--manifest-path",
             os.path.join(srcdir, "Cargo.toml")],
            capture_output=True,
            text=True,
            check=True,
            cwd=srcdir,
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return {}
    try:
        data = json.loads(out.stdout)
    except json.JSONDecodeError:
        return {}
    result: dict[str, str] = {}
    for pkg in data.get("packages", []):
        key = f"{pkg['name']}|{pkg['version']}"
        result[key] = pkg.get("license") or "UNKNOWN"
    return result


def _purl(name: str, version: str, source: str | None) -> str:
    if source and "crates.io" in source:
        return f"pkg:cargo/{name}@{version}"
    return f"pkg:generic/{name}@{version}"


def _spdx_id(name: str, version: str) -> str:
    safe = "".join(c if c.isalnum() else "-" for c in f"{name}-{version}")
    return f"SPDXRef-{safe}"


def _cyclonedx(entries: list[dict], srcdir: str,
               binaries: list[dict], models: list[dict]) -> dict:
    components = []
    deps = []
    for e in sorted(entries, key=lambda x: x["name"]):
        name = e["name"]
        version = e["version"]
        source = e.get("source")
        lic = e["_license"]
        comp = {
            "type": "library",
            "bom-ref": f"{name}@{version}",
            "name": name,
            "version": version,
            "purl": _purl(name, version, source),
            "licenses": [{"license": {"id": lic}}] if lic != "UNKNOWN" else [],
            "externalRefs": [],
        }
        if e.get("checksum"):
            comp["hashes"] = [{"alg": "SHA-256", "content": e["checksum"]}]
        if source and "crates.io" in source:
            comp["externalRefs"].append({
                "type": "distribution",
                "url": f"https://crates.io/crates/{name}/{version}",
            })
        components.append(comp)
        deps.append({
            "ref": f"{name}@{version}",
            "dependsOn": [f"{d['name']}@{d['version']}" for d in e["_deps"]],
        })
    for b in sorted(binaries, key=lambda x: x["name"]):
        ref = f"binary-{b['name']}@{b['version']}"
        components.append({
            "type": "application",
            "bom-ref": ref,
            "name": b["name"],
            "version": b["version"],
            "hashes": [{"alg": "SHA-256", "content": b["sha256"]}],
            "licenses": [{"license": {"id": b["licence"]}}],
        })
        deps.append({"ref": ref, "dependsOn": []})
    for m in sorted(models, key=lambda x: x["name"]):
        ref = f"model-{m['name']}@{m['version']}"
        ext_refs = []
        if m.get("upstream_url") and m["upstream_url"] != "self-trained":
            ext_refs.append({"type": "distribution", "url": m["upstream_url"]})
        comp = {
            "type": "machine-learning-model",
            "bom-ref": ref,
            "name": m["name"],
            "version": m["version"],
            "hashes": [{"alg": "SHA-256", "content": m["sha256"]}],
            "licenses": [{"license": {"id": m["licence"]}}],
            "externalRefs": ext_refs,
            "properties": [
                {"name": "namtor:builtFromCommit", "value": m["built_from_commit"]},
                {"name": "namtor:licenceSha256", "value": m["licence_sha256"]},
            ],
        }
        if m.get("model_card"):
            comp["properties"].append(
                {"name": "namtor:modelCard", "value": m["model_card"]})
        components.append(comp)
        deps.append({"ref": ref, "dependsOn": []})
    top_ref = "namtor-workspace@0.1.0"
    deps.append({"ref": top_ref, "dependsOn": [c["bom-ref"] for c in components]})
    serial = uuid.uuid5(_SBOM_NAMESPACE, "namtor-cyclonedx-v1")
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{serial}",
        "version": 1,
        "metadata": {
            "timestamp": _FIXED_EPOCH_ISO,
            "tools": [{"vendor": "Nam-tor", "name": "generate-sbom.py",
                        "version": "1.0"}],
            "component": {
                "type": "application",
                "bom-ref": top_ref,
                "name": "namtor",
                "version": "0.1.0",
                "licenses": [{"license": {"id": "GPL-3.0-only"}}],
            },
        },
        "components": components,
        "dependencies": deps,
    }


def _spdx(entries: list[dict], srcdir: str,
          binaries: list[dict], models: list[dict]) -> dict:
    packages = []
    for e in sorted(entries, key=lambda x: x["name"]):
        name = e["name"]
        version = e["version"]
        spdx_id = _spdx_id(name, version)
        pkg = {
            "name": name,
            "SPDXID": spdx_id,
            "versionInfo": version,
            "downloadLocation": (
                f"https://crates.io/crates/{name}/{version}"
                if e.get("source") and "crates.io" in e["source"]
                else "NOASSERTION"
            ),
            "filesAnalyzed": False,
            "licenseConcluded": e["_license"] if e["_license"] != "UNKNOWN"
            else "NOASSERTION",
            "licenseDeclared": e["_license"] if e["_license"] != "UNKNOWN"
            else "NOASSERTION",
            "copyrightText": "NOASSERTION",
        }
        if e.get("checksum"):
            pkg["packageVerificationCode"] = e["checksum"]
        packages.append(pkg)
    for b in sorted(binaries, key=lambda x: x["name"]):
        sid = _spdx_id(f"binary-{b['name']}", b["version"])
        packages.append({
            "name": b["name"],
            "SPDXID": sid,
            "versionInfo": b["version"],
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": b["licence"],
            "licenseDeclared": b["licence"],
            "copyrightText": "NOASSERTION",
            "packageVerificationCode": b["sha256"],
            "sourceInfo": "Built from Nam-tor source (cargo build --release)",
        })
    for m in sorted(models, key=lambda x: x["name"]):
        sid = _spdx_id(f"model-{m['name']}", m["version"])
        dl = m["upstream_url"] if m["upstream_url"] != "self-trained" else "NOASSERTION"
        src_info = (f"Built from Nam-tor commit {m['built_from_commit']}; "
                     f"licence_sha256={m['licence_sha256']}")
        if m.get("model_card"):
            src_info += f"; model_card={m['model_card']}"
        packages.append({
            "name": m["name"],
            "SPDXID": sid,
            "versionInfo": m["version"],
            "downloadLocation": dl,
            "filesAnalyzed": False,
            "licenseConcluded": m["licence"],
            "licenseDeclared": m["licence"],
            "copyrightText": "NOASSERTION",
            "packageVerificationCode": m["sha256"],
            "sourceInfo": src_info,
        })
    rels = []
    for e in sorted(entries, key=lambda x: x["name"]):
        src = _spdx_id(e["name"], e["version"])
        for d in e["_deps"]:
            rels.append({
                "spdxElementId": src,
                "relationshipType": "DEPENDS_ON",
                "relatedSpdxElement": _spdx_id(d["name"], d["version"]),
            })
    ns = f"https://namtor.dev/spdx/{uuid.uuid5(_SBOM_NAMESPACE, 'namtor-spdx-v1')}"
    return {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": "namtor",
        "documentNamespace": ns,
        "creationInfo": {
            "creators": ["Tool: Nam-tor generate-sbom.py (1.0)"],
            "created": _FIXED_EPOCH_ISO,
        },
        "packages": packages,
        "relationships": rels,
    }


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("srcdir", nargs="?", default=None)
    ap.add_argument("--out", default=None)
    args = ap.parse_args(argv)
    srcdir = args.srcdir or _git_toplevel()
    outdir = args.out or os.path.join(srcdir, "docs", "evidence", "packaging")
    os.makedirs(outdir, exist_ok=True)

    lock_pkgs = _load_lock(srcdir)
    licences = _cargo_metadata(srcdir)
    manifest_models = _load_model_manifest(srcdir)
    binaries = _binary_components(srcdir)
    models = _model_components(srcdir, manifest_models)
    by_key = {f"{p['name']}|{p['version']}": p for p in lock_pkgs}
    by_name: dict[str, dict] = {}
    for p in lock_pkgs:
        by_name.setdefault(p["name"], p)
    entries = []
    for p in lock_pkgs:
        key = f"{p['name']}|{p['version']}"
        dep_names = p.get("dependencies", [])
        dep_entries: list[dict] = []
        for dep in dep_names:
            if " " in dep:
                dname, dver = dep.split(" ", 1)
                if f"{dname}|{dver}" in by_key:
                    dep_entries.append({"name": dname, "version": dver})
            elif dep in by_name:
                dep_entries.append(
                    {"name": dep, "version": by_name[dep]["version"]})
        entries.append({
            "name": p["name"],
            "version": p["version"],
            "source": p.get("source"),
            "checksum": p.get("checksum"),
            "_license": licences.get(key, "UNKNOWN"),
            "_deps": dep_entries,
        })

    cdx = _cyclonedx(entries, srcdir, binaries, models)
    spdx = _spdx(entries, srcdir, binaries, models)
    cdx_path = os.path.join(outdir, "namtor.cyclonedx.json")
    spdx_path = os.path.join(outdir, "namtor.spdx.json")
    with open(cdx_path, "w") as f:
        json.dump(cdx, f, indent=2, sort_keys=True)
    with open(spdx_path, "w") as f:
        json.dump(spdx, f, indent=2, sort_keys=True)
    ext = sum(1 for e in entries if e["source"] and "crates.io" in e["source"])
    loc = len(entries) - ext
    unknown = sum(1 for e in entries if e["_license"] == "UNKNOWN")
    print(f"SBOM: {len(entries)} cargo packages "
          f"({ext} external, {loc} workspace), "
          f"{len(binaries)} binaries, {len(models)} models "
          f"-> {cdx_path}, {spdx_path}")
    if unknown:
        print(f"  warning: {unknown} packages with UNKNOWN licence "
              "(cargo metadata unavailable)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
