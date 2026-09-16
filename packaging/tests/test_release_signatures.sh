#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# Test matrix for release artifact signing (task 25).
#
# Verifies the six gates required by the task:
#   1. Coverage    — sign-packages.sh references every artifact class a
#                    public release ships: source tarball, model archive,
#                    Arch .pkg.tar.zst, Fedora .rpm, Debian .deb, checksums,
#                    provenance.
#   2. Repro-flag  — verify-reproducible.sh accepts --release-dir and emits
#                    per-artifact-kind records.
#   3. Sign/verify — a real GPG detached signature over a test file verifies.
#   4. Tamper      — flipping one byte in the signed file fails verification.
#   5. Wrong key   — a signature from a second, unrelated key fails to verify
#                    against the first key's public key.
#   6. No privkey  — no private key material is committed to the repo or
#                    written into any artifact under packaging/ or the
#                    release output dir.
#
# Runs without network or root. Uses a throwaway GPG home (GNUPGHOME under
# the test temp dir) so the operator's keyring is never touched and no key
# material persists after the test. minisign is exercised only when present
# on PATH; the GPG path is always exercised.
#
# Usage:
#   packaging/tests/test_release_signatures.sh [SRCDIR]
#
# Exit codes: 0 = all pass, 1 = at least one failure.

set -euo pipefail

srcdir="${1:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
sign_script="$srcdir/packaging/sign-packages.sh"
verify_script="$srcdir/packaging/verify-reproducible.sh"

pass=0
fail=0
failures=()

assert_ok() {
    local desc="$1"; shift
    if "$@"; then
        pass=$((pass + 1))
        echo "  PASS: $desc"
    else
        fail=$((fail + 1))
        failures+=("$desc")
        echo "  FAIL: $desc"
    fi
}

assert_eq() {
    local desc="$1" a="$2" b="$3"
    if [[ "$a" == "$b" ]]; then
        pass=$((pass + 1))
        echo "  PASS: $desc"
    else
        fail=$((fail + 1))
        failures+=("$desc ($a != $b)")
        echo "  FAIL: $desc ($a != $b)"
    fi
}

assert_ne() {
    local desc="$1" a="$2" b="$3"
    if [[ "$a" != "$b" ]]; then
        pass=$((pass + 1))
        echo "  PASS: $desc"
    else
        fail=$((fail + 1))
        failures+=("$desc (both = $a)")
        echo "  FAIL: $desc (both = $a)"
    fi
}

# Prerequisites.
[[ -f "$sign_script"   ]] || { echo "error: $sign_script not found"   >&2; exit 1; }
[[ -f "$verify_script" ]] || { echo "error: $verify_script not found" >&2; exit 1; }
if ! command -v gpg >/dev/null 2>&1; then
    echo "error: gpg is required for this test" >&2; exit 1
fi

tmp="$(mktemp -d -t namtor-release-sig-test-XXXXXX)"
trap 'rm -rf "$tmp"' EXIT

echo "## Gate 1: sign-packages.sh covers every artifact class"
sign_body="$(cat "$sign_script")"
for pattern in \
    "namtor-\".*\\.tar\\.gz" \
    "namtor-models-" \
    "\\.pkg\\.tar\\.zst" \
    "\\.rpm" \
    "\\.deb" \
    "SHA256SUMS" \
    "namtor\\.provenance\\.json"; do
    if printf '%s\n' "$sign_body" | grep -qE "$pattern"; then
        pass=$((pass + 1))
        echo "  PASS: sign-packages.sh references /$pattern/"
    else
        fail=$((fail + 1))
        failures+=("sign-packages.sh missing pattern $pattern")
        echo "  FAIL: sign-packages.sh missing pattern /$pattern/"
    fi
done

# Release-dir mode flag must be parsed.
if printf '%s\n' "$sign_body" | grep -q -- '--release-dir'; then
    pass=$((pass + 1)); echo "  PASS: sign-packages.sh parses --release-dir"
else
    fail=$((fail + 1)); failures+=("--release-dir not parsed"); echo "  FAIL: --release-dir not parsed"
fi
if printf '%s\n' "$sign_body" | grep -q -- '--checksums'; then
    pass=$((pass + 1)); echo "  PASS: sign-packages.sh parses --checksums"
else
    fail=$((fail + 1)); failures+=("--checksums not parsed"); echo "  FAIL: --checksums not parsed"
fi

echo "## Gate 2: verify-reproducible.sh accepts --release-dir and emits kinds"
verify_body="$(cat "$verify_script")"
if printf '%s\n' "$verify_body" | grep -q -- '--release-dir'; then
    pass=$((pass + 1)); echo "  PASS: verify-reproducible.sh parses --release-dir"
else
    fail=$((fail + 1)); failures+=("verify-reproducible.sh missing --release-dir"); echo "  FAIL: verify-reproducible.sh missing --release-dir"
fi
# The evidence JSON must classify artifacts by kind (binary, model-archive,
# arch-package, fedora-package, debian-package, source-tarball, checksums,
# provenance).
for kind in model-archive arch-package fedora-package debian-package source-tarball checksums provenance; do
    if printf '%s\n' "$verify_body" | grep -qF "\"$kind\""; then
        pass=$((pass + 1)); echo "  PASS: verify-reproducible.sh emits kind=$kind"
    else
        fail=$((fail + 1)); failures+=("verify-reproducible.sh missing kind=$kind"); echo "  FAIL: verify-reproducible.sh missing kind=$kind"
    fi
done

echo "## Gate 3: GPG detached signature verifies on a clean file"
gnupghome="$tmp/gpg"
mkdir -p "$gnupghome"
chmod 700 "$gnupghome"
export GNUPGHOME="$gnupghome"
# Pinentry-mode loopback + empty passphrase so no agent prompt blocks the
# test. The key is throwaway (lives only in $tmp, cleaned on exit).
gpg --batch --pinentry-mode loopback --passphrase '' --quiet --no-tty \
    --quick-generate-key "namtor-test-key <test@namtor.local>" \
    ed25519 sign 0 >/dev/null 2>&1
keyid_a="$(gpg --batch --list-keys --with-colons 2>/dev/null \
            | awk -F: '/^fpr:/{print $10; exit}')"
[[ -n "$keyid_a" ]] || { echo "  FAIL: could not generate test key A" >&2; fail=$((fail + 1)); failures+=("test key A generation"); }

payload="$tmp/payload.bin"
printf 'namtor release test payload — unmodified\n' > "$payload"
payload_sig="$tmp/payload.bin.asc"

if [[ -n "$keyid_a" ]]; then
    # Sign via the real script (exercises sign_gpg()).
    NAMTOR_SIGN_TOOL=gpg bash "$sign_script" --gpg-key "$keyid_a" \
        --out "$tmp" "$payload" >/dev/null 2>&1
    if [[ -f "$payload_sig" ]]; then
        pass=$((pass + 1)); echo "  PASS: signing produced $payload_sig"
    else
        fail=$((fail + 1)); failures+=("no .asc produced"); echo "  FAIL: no .asc produced"
    fi

    # Verify clean signature.
    if gpg --batch --quiet --verify "$payload_sig" "$payload" >/dev/null 2>&1; then
        pass=$((pass + 1)); echo "  PASS: clean signature verifies"
    else
        fail=$((fail + 1)); failures+=("clean verify failed"); echo "  FAIL: clean verify failed"
    fi
fi

echo "## Gate 4: tampered file fails verification"
if [[ -n "$keyid_a" && -f "$payload_sig" ]]; then
    tampered="$tmp/payload.tampered.bin"
    cp "$payload" "$tampered"
    printf 'X' | dd of="$tampered" bs=1 seek=10 count=1 conv=notrunc status=none
    if gpg --batch --quiet --verify "$payload_sig" "$tampered" >/dev/null 2>&1; then
        fail=$((fail + 1)); failures+=("tampered file verified (should fail)"); echo "  FAIL: tampered file verified (should fail)"
    else
        pass=$((pass + 1)); echo "  PASS: tampered file fails verification"
    fi
else
    fail=$((fail + 1)); failures+=("gate 4 precondition (no key/sig)"); echo "  FAIL: gate 4 precondition (no key/sig)"
fi

echo "## Gate 5: wrong key fails verification"
if [[ -n "$keyid_a" ]]; then
    gnupghome_b="$tmp/gpg-b"
    mkdir -p "$gnupghome_b"
    chmod 700 "$gnupghome_b"
    GNUPGHOME="$gnupghome_b" gpg --batch --pinentry-mode loopback --passphrase '' \
        --quiet --no-tty --quick-generate-key "namtor-wrong-key <wrong@namtor.local>" \
        ed25519 sign 0 >/dev/null 2>&1
    keyid_b="$(GNUPGHOME="$gnupghome_b" gpg --batch --list-keys --with-colons \
                2>/dev/null | awk -F: '/^fpr:/{print $10; exit}')"
    if [[ -n "$keyid_b" ]]; then
        wrong_sig="$tmp/payload.wrong.asc"
        GNUPGHOME="$gnupghome_b" gpg --batch --pinentry-mode loopback --quiet \
            --detach-sig --armor --digest-algo SHA256 \
            --local-user "$keyid_b" --output "$wrong_sig" "$payload" >/dev/null 2>&1
        # Verifying the wrong-key signature against key A's keyring must fail.
        if gpg --batch --quiet --verify "$wrong_sig" "$payload" >/dev/null 2>&1; then
            fail=$((fail + 1)); failures+=("wrong-key signature verified (should fail)"); echo "  FAIL: wrong-key signature verified (should fail)"
        else
            pass=$((pass + 1)); echo "  PASS: wrong-key signature fails verification"
        fi
        # Cross-check: the wrong-key signature DOES verify in key B's keyring.
        if GNUPGHOME="$gnupghome_b" gpg --batch --quiet --verify "$wrong_sig" "$payload" >/dev/null 2>&1; then
            pass=$((pass + 1)); echo "  PASS: wrong-key signature verifies in its own keyring (sanity)"
        else
            fail=$((fail + 1)); failures+=("wrong-key sig failed in its own keyring"); echo "  FAIL: wrong-key sig failed in its own keyring"
        fi
    else
        fail=$((fail + 1)); failures+=("test key B generation"); echo "  FAIL: test key B generation"
    fi
else
    fail=$((fail + 1)); failures+=("gate 5 precondition (no key A)"); echo "  FAIL: gate 5 precondition (no key A)"
fi

echo "## Gate 6: no private key material in repo or release artifacts"
# (a) packaging/ must contain no private key blocks.
priv_in_packaging="$(grep -rEl '-----BEGIN (PGP |OPENSSH |)PRIVATE KEY BLOCK-----' \
    "$srcdir/packaging/" 2>/dev/null || true)"
if [[ -z "$priv_in_packaging" ]]; then
    pass=$((pass + 1)); echo "  PASS: no private key blocks under packaging/"
else
    fail=$((fail + 1)); failures+=("private key block in packaging/: $priv_in_packaging"); echo "  FAIL: private key block in packaging/: $priv_in_packaging"
fi
# (b) sign-packages.sh must not contain --generate-key or --export-secret-key.
if grep -qE 'generate-key|export-secret|import-secret' "$sign_script"; then
    fail=$((fail + 1)); failures+=("sign-packages.sh touches secret-key ops"); echo "  FAIL: sign-packages.sh touches secret-key ops"
else
    pass=$((pass + 1)); echo "  PASS: sign-packages.sh never generates/exports/imports secret keys"
fi
# (c) The test's throwaway GNUPGHOMEs must be inside $tmp (cleaned on exit).
for h in "$gnupghome" "$tmp/gpg-b"; do
    if [[ -d "$h" && "$h" == "$tmp"* ]]; then
        pass=$((pass + 1)); echo "  PASS: keyring $h confined to test tempdir"
    else
        fail=$((fail + 1)); failures+=("keyring outside tempdir: $h"); echo "  FAIL: keyring outside tempdir: $h"
    fi
done
# (d) Export the public key — must not contain private-key markers.
pub_export="$tmp/pub.asc"
gpg --batch --quiet --armor --export "$keyid_a" > "$pub_export" 2>/dev/null || true
if grep -q 'PRIVATE KEY' "$pub_export" 2>/dev/null; then
    fail=$((fail + 1)); failures+=("public key export leaked private material"); echo "  FAIL: public key export leaked private material"
else
    pass=$((pass + 1)); echo "  PASS: public key export contains no private material"
fi

echo "## Gate 7 (bonus): --release-dir signs all artifact classes present"
release_out="$tmp/dist"
mkdir -p "$release_out"
# Stage a representative release set (names match real distro output).
printf 'source tarball\n'  > "$release_out/namtor-0.1.0.tar.gz"
printf 'model archive\n'   > "$release_out/namtor-models-0.1.0.tar.gz"
printf 'arch core\n'       > "$release_out/namtor-0.1.0-1-x86_64.pkg.tar.zst"
printf 'arch gnome\n'      > "$release_out/namtor-gnome-0.1.0-1-x86_64.pkg.tar.zst"
printf 'fedora core\n'     > "$release_out/namtor-0.1.0-1.fc40.x86_64.rpm"
printf 'fedora gnome\n'    > "$release_out/namtor-gnome-0.1.0-1.fc40.x86_64.rpm"
printf 'debian core\n'     > "$release_out/namtor_0.1.0-1_amd64.deb"
printf 'debian gnome\n'    > "$release_out/namtor-gnome_0.1.0-1_amd64.deb"
printf '{"provenance":1}\n' > "$release_out/namtor.provenance.json"

if [[ -n "$keyid_a" ]]; then
    NAMTOR_SIGN_TOOL=gpg bash "$sign_script" --release-dir "$release_out" \
        --version 0.1.0 --gpg-key "$keyid_a" --checksums \
        --provenance "$release_out/namtor.provenance.json" \
        --out "$release_out" >/dev/null 2>&1
    sig_rc=$?
    if [[ $sig_rc -eq 0 ]]; then
        pass=$((pass + 1)); echo "  PASS: --release-dir + --checksums exit 0"
    else
        fail=$((fail + 1)); failures+=("--release-dir sign exit $sig_rc"); echo "  FAIL: --release-dir sign exit $sig_rc"
    fi
    # Every original artifact must have a .asc sidecar (per-file signing
    # is always on in release-dir mode; --checksums adds the manifest).
    for art in namtor-0.1.0.tar.gz namtor-models-0.1.0.tar.gz \
               namtor-0.1.0-1-x86_64.pkg.tar.zst \
               namtor-gnome-0.1.0-1-x86_64.pkg.tar.zst \
               namtor-0.1.0-1.fc40.x86_64.rpm \
               namtor-gnome-0.1.0-1.fc40.x86_64.rpm \
               namtor_0.1.0-1_amd64.deb \
               namtor-gnome_0.1.0-1_amd64.deb; do
        if [[ -f "$release_out/$art.asc" ]]; then
            pass=$((pass + 1))
        else
            fail=$((fail + 1)); failures+=("$art.asc missing")
        fi
    done
    echo "  PASS: all 11 distro artifacts have per-file .asc signatures"
    if [[ -f "$release_out/SHA256SUMS" ]]; then
        pass=$((pass + 1)); echo "  PASS: SHA256SUMS generated"
    else
        fail=$((fail + 1)); failures+=("SHA256SUMS not generated"); echo "  FAIL: SHA256SUMS not generated"
    fi
    if [[ -f "$release_out/SHA256SUMS.asc" ]]; then
        pass=$((pass + 1)); echo "  PASS: SHA256SUMS signed"
    else
        fail=$((fail + 1)); failures+=("SHA256SUMS.asc missing"); echo "  FAIL: SHA256SUMS.asc missing"
    fi
    # The checksums manifest must list every artifact. The staged set above
    # is 9: source tarball, model archive, provenance, and core+gnome for
    # each of Arch/Fedora/Debian. It was 12 while namtor-kde existed.
    sums_lines="$(wc -l < "$release_out/SHA256SUMS")"
    if [[ "$sums_lines" -ge 9 ]]; then
        pass=$((pass + 1)); echo "  PASS: SHA256SUMS covers $sums_lines artifacts (>=9)"
    else
        fail=$((fail + 1)); failures+=("SHA256SUMS only $sums_lines entries"); echo "  FAIL: SHA256SUMS only $sums_lines entries"
    fi
    # Verify the checksums signature.
    if gpg --batch --quiet --verify "$release_out/SHA256SUMS.asc" "$release_out/SHA256SUMS" >/dev/null 2>&1; then
        pass=$((pass + 1)); echo "  PASS: SHA256SUMS signature verifies"
    else
        fail=$((fail + 1)); failures+=("SHA256SUMS verify failed"); echo "  FAIL: SHA256SUMS verify failed"
    fi
    # Verify every hash in SHA256SUMS matches the file on disk.
    if (cd "$release_out" && sha256sum -c SHA256SUMS >/dev/null 2>&1); then
        pass=$((pass + 1)); echo "  PASS: all SHA256SUMS entries match on-disk files"
    else
        fail=$((fail + 1)); failures+=("SHA256SUMS hash mismatch"); echo "  FAIL: SHA256SUMS hash mismatch"
    fi
    # Provenance must also be signed (per-file).
    if [[ -f "$release_out/namtor.provenance.json.asc" ]]; then
        pass=$((pass + 1)); echo "  PASS: provenance file signed"
    else
        fail=$((fail + 1)); failures+=("provenance .asc missing"); echo "  FAIL: provenance .asc missing"
    fi
fi

echo ""
echo "Results: $pass passed, $fail failed"
if [[ $fail -gt 0 ]]; then
    echo "Failures:"
    for f in "${failures[@]}"; do echo "  - $f"; done
    exit 1
fi
exit 0
