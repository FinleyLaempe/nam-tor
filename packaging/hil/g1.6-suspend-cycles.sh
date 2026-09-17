#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Gate G1.6 — "first authentication after S3 succeeds 20/20".
#
# Suspends the machine to RAM N times with an RTC wake alarm, and after every
# resume runs the FIRST face authentication through the installed daemon,
# recording its verdict and latency. This is a hardware-in-the-loop run: it
# really suspends the machine, and it needs you sitting in front of the camera
# each time it wakes.
#
# What it exercises that no unit test can: the kernel re-enumerating the USB
# camera across S3 (the /dev/videoN node may renumber), the daemon closing its
# captures on logind PrepareForSleep, and the capture factory re-discovering
# the camera from sysfs on the first request after resume.
#
# Usage:
#   sudo packaging/hil/g1.6-suspend-cycles.sh [--cycles N] [--wake-after S] [--out FILE]
#
# Requirements:
#   - run as root (rtcwake), from a desktop session, with SUDO_USER set
#   - namtor-daemon active, and a face enrolled for SUDO_USER
#   - suspend-to-RAM supported ("mem" in /sys/power/state)
#
# Exit: 0 only if every cycle's first authentication is a MATCH.

set -uo pipefail

CYCLES=20
WAKE_AFTER=20      # seconds asleep per cycle
SETTLE=4           # seconds after resume before authenticating (USB re-enumeration)
VERIFY_TIMEOUT=15  # seconds; a first auth slower than this counts as a failure
OUT=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --cycles) CYCLES="$2"; shift 2 ;;
        --wake-after) WAKE_AFTER="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \?//'; exit 0 ;;
        *) echo "g1.6: unknown option: $1" >&2; exit 2 ;;
    esac
done

die() { echo "g1.6: $*" >&2; exit 2; }

# --- preflight: refuse to start a run whose result would mean nothing -------
[[ $EUID -eq 0 ]] || die "must run as root (rtcwake needs it): sudo $0"
[[ -n "${SUDO_USER:-}" && "$SUDO_USER" != "root" ]] \
    || die "run via sudo from your own session, so SUDO_USER names the enrolled user"
command -v rtcwake >/dev/null || die "rtcwake not found (util-linux)"
grep -qw mem /sys/power/state || die "suspend-to-RAM not supported: /sys/power/state = $(cat /sys/power/state)"
NAMTOR="$(command -v namtor || true)"
[[ -n "$NAMTOR" ]] || die "namtor not on PATH; install the package first"
systemctl is-active --quiet namtor-daemon \
    || die "namtor-daemon is not active: systemctl start namtor-daemon"
if ! sudo -u "$SUDO_USER" "$NAMTOR" cli list >/dev/null 2>&1; then
    die "could not list profiles for $SUDO_USER — is the daemon reachable?"
fi

# A baseline before any suspend. If this fails, every post-resume failure would
# be blamed on S3 when the real fault is enrollment, lighting or position.
echo "g1.6: baseline authentication before any suspend — look at the camera"
baseline="$(timeout "$VERIFY_TIMEOUT" sudo -u "$SUDO_USER" "$NAMTOR" cli verify 2>/dev/null)"
if [[ "$baseline" != MATCH* ]]; then
    die "baseline authentication did not match (${baseline:-no output}); fix that before measuring S3"
fi
echo "g1.6: baseline OK: $baseline"

[[ -n "$OUT" ]] || OUT="g1.6-suspend-cycles-$(date -u +%Y%m%dT%H%M%SZ).tsv"
printf 'cycle\tresumed_utc\tverdict\texit\tlatency_ms\tvideo_nodes\n' >"$OUT"

pass=0
for ((i = 1; i <= CYCLES; i++)); do
    echo "g1.6: cycle $i/$CYCLES — suspending for ${WAKE_AFTER}s"
    if ! rtcwake -m mem -s "$WAKE_AFTER" >/dev/null 2>&1; then
        echo "g1.6: rtcwake failed on cycle $i; stopping" >&2
        break
    fi
    resumed="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    sleep "$SETTLE"
    # Recorded so a renumbered node is visible in the evidence, not assumed.
    nodes="$(ls /dev/video* 2>/dev/null | tr '\n' ' ' | sed 's/ $//')"

    echo "g1.6: cycle $i resumed — look at the camera"
    t0=$(date +%s%N)
    result="$(timeout "$VERIFY_TIMEOUT" sudo -u "$SUDO_USER" "$NAMTOR" cli verify 2>/dev/null)"
    rc=$?
    t1=$(date +%s%N)
    ms=$(( (t1 - t0) / 1000000 ))
    verdict="${result%% *}"
    [[ $rc -eq 124 ]] && verdict="TIMEOUT"
    [[ -z "$verdict" ]] && verdict="NO_OUTPUT"

    printf '%d\t%s\t%s\t%d\t%d\t%s\n' "$i" "$resumed" "$verdict" "$rc" "$ms" "$nodes" >>"$OUT"
    if [[ "$verdict" == "MATCH" ]]; then
        pass=$((pass + 1))
        echo "g1.6: cycle $i: MATCH in ${ms} ms"
    else
        echo "g1.6: cycle $i: $verdict (exit $rc) in ${ms} ms" >&2
    fi
done

echo "============================================================"
echo "G1.6 first auth after S3: $pass/$CYCLES matched"
echo "evidence: $OUT"
echo "============================================================"
[[ $pass -eq $CYCLES ]]
