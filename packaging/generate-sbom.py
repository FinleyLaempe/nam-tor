#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Generate SBOM (CycloneDX 1.5 + SPDX 2.3) for the Nam-tor workspace.

Parses ``Cargo.lock`` with the stdlib ``tomllib`` (Python >= 3.11) for
name / version / checksum / dependency edges, and cross-references
``cargo metadata --format-version 1`` for the licence field of each
package. ``cargo metadata`` runs offline against the local registry
cache; if it is unavailable the licence is recorded as ``UNKNOWN`` and
generation proceeds (the SBOM is still complete in every other field).

No network access is required at any point.

Usage::

    packaging/generate-sbom.py [SRCDIR] [--out DIR]

    SRCDIR  repo root containing Cargo.lock (default: git toplevel).
    --out   output directory (default: <SRCDIR>/docs/evidence/packaging).

Writes ``namtor.cyclonedx.json`` and ``namtor.spdx.json``.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import subprocess
import sys
import tomllib
import uuid


def _git_toplevel() -> str:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=False,
    )
    return out.stdout.strip() if out.returncode == 0 else os.getcwd()


def _load_lock(srcdir: str) -> list[dict]:
    with open(os.path.join(srcdir, "Cargo.lock"), "rb") as f:
        return tomllib.load(f)["package"]


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


def _cyclonedx(entries: list[dict], srcdir: str) -> dict:
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
    top_ref = "namtor-workspace@0.1.0"
    deps.append({"ref": top_ref, "dependsOn": [c["bom-ref"] for c in components]})
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{uuid.uuid4()}",
        "version": 1,
        "metadata": {
            "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
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


def _spdx(entries: list[dict], srcdir: str) -> dict:
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
    rels = []
    for e in sorted(entries, key=lambda x: x["name"]):
        src = _spdx_id(e["name"], e["version"])
        for d in e["_deps"]:
            rels.append({
                "spdxElementId": src,
                "relationshipType": "DEPENDS_ON",
                "relatedSpdxElement": _spdx_id(d["name"], d["version"]),
            })
    ns = f"https://namtor.dev/spdx/{uuid.uuid4()}"
    return {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": "namtor",
        "documentNamespace": ns,
        "creationInfo": {
            "creators": ["Tool: Nam-tor generate-sbom.py (1.0)"],
            "created": _dt.datetime.now(_dt.timezone.utc)
            .strftime("%Y-%m-%dT%H:%M:%SZ"),
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

    cdx = _cyclonedx(entries, srcdir)
    spdx = _spdx(entries, srcdir)
    cdx_path = os.path.join(outdir, "namtor.cyclonedx.json")
    spdx_path = os.path.join(outdir, "namtor.spdx.json")
    with open(cdx_path, "w") as f:
        json.dump(cdx, f, indent=2, sort_keys=True)
    with open(spdx_path, "w") as f:
        json.dump(spdx, f, indent=2, sort_keys=True)
    ext = sum(1 for e in entries if e["source"] and "crates.io" in e["source"])
    loc = len(entries) - ext
    unknown = sum(1 for e in entries if e["_license"] == "UNKNOWN")
    print(f"SBOM: {len(entries)} packages "
          f"({ext} external, {loc} workspace) -> {cdx_path}, {spdx_path}")
    if unknown:
        print(f"  warning: {unknown} packages with UNKNOWN licence "
              "(cargo metadata unavailable)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
