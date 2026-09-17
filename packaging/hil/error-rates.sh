#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Gates G3 (FAR/FRR) and G4 (presentation-attack APCER) — measurement runner.
#
# Conducts a live error-rate session against the installed daemon: it prompts
# for each presentation, records what the system answered, and writes a JSONL
# of attempts. `namtor-eval` turns that into rates with an N and a 95% upper
# bound, which is what G3.2 and G4.2 require — per attack instrument, never
# averaged.
#
# This does not manufacture the inputs those gates need. It means that once a
# subject or an attack artefact is in front of the camera, measuring is one
# command instead of a project.
#
# Usage:
#   packaging/hil/error-rates.sh genuine  --n 50  [--conditions "desk lamp, no glasses"]
#   packaging/hil/error-rates.sh impostor --n 50  --subject bob
#   packaging/hil/error-rates.sh attack   --n 20  --instrument print
#   packaging/hil/error-rates.sh report   --in attempts.jsonl
#
# `attack` instruments are free text and are kept distinct in the report:
# print, screen_replay, ir_print, mask, ... A system that resists one and
# falls to another is not "half safe", so they are never merged.
#
# Requirements: an enrolled face for the presenting subject, a running daemon,
# and a person at the camera. Exit 0 when the run completes.

set -uo pipefail

grep_all() { grep "$@" >/dev/null; }

MODE="${1:-}"; shift || true
N=20
CONDITIONS="unspecified"
INSTRUMENT=""
SUBJECT=""
OUT=""
IN=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --n) N="$2"; shift 2 ;;
        --conditions) CONDITIONS="$2"; shift 2 ;;
        --instrument) INSTRUMENT="$2"; shift 2 ;;
        --subject) SUBJECT="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --in) IN="$2"; shift 2 ;;
        -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \?//'; exit 0 ;;
        *) echo "error-rates: unknown option: $1" >&2; exit 2 ;;
    esac
done

die() { echo "error-rates: $*" >&2; exit 2; }

NAMTOR="$(command -v namtor || echo ./target/release/namtor)"
[[ -x "$NAMTOR" ]] || die "namtor not found; install the package or build it"

case "$MODE" in
    genuine|impostor|attack)
        [[ "$MODE" == "attack" && -z "$INSTRUMENT" ]] && die "attack needs --instrument <name>"
        [[ -n "$OUT" ]] || OUT="error-rates-$MODE-$(date -u +%Y%m%dT%H%M%SZ).jsonl"
        : >"$OUT"
        start=$(date +%s)
        echo "error-rates: $N x $MODE${INSTRUMENT:+ ($INSTRUMENT)} — conditions: $CONDITIONS"
        echo "error-rates: press Enter before each presentation; Ctrl-C to stop early"
        for ((i = 1; i <= N; i++)); do
            case "$MODE" in
                genuine)  printf '  [%d/%d] present YOUR face, then Enter: ' "$i" "$N" ;;
                impostor) printf '  [%d/%d] have %s present their face, then Enter: ' "$i" "$N" "${SUBJECT:-the other person}" ;;
                attack)   printf '  [%d/%d] present the %s artefact, then Enter: ' "$i" "$N" "$INSTRUMENT" ;;
            esac
            read -r _ || break

            t0=$(date +%s%N)
            out="$(timeout 20 "$NAMTOR" cli verify 2>/dev/null)"
            rc=$?
            t1=$(date +%s%N)
            ms=$(( (t1 - t0) / 1000000 ))

            # Accepted only on MATCH. Anything that never reached a decision is
            # inconclusive and is excluded from every rate rather than being
            # counted as a convenient rejection.
            case "$out" in
                MATCH*)      outcome=accepted ;;
                "NO MATCH"*) outcome=rejected ;;
                *)           outcome=inconclusive ;;
            esac
            [[ $rc -eq 124 ]] && outcome=inconclusive

            case "$MODE" in
                genuine)  kind='"genuine"' ;;
                impostor) kind='"zero_effort_impostor"' ;;
                attack)   kind="{\"attack\":{\"instrument\":\"$INSTRUMENT\"}}" ;;
            esac
            printf '{"index":%d,"at_s":%d,"kind":%s,"outcome":"%s","latency_ms":%d}\n' \
                "$i" "$(( $(date +%s) - start ))" "$kind" "$outcome" "$ms" >>"$OUT"
            echo "      -> $outcome (${ms} ms)"
        done
        echo "error-rates: wrote $OUT"
        echo "error-rates: score it with: $0 report --in $OUT"
        ;;
    report)
        [[ -n "$IN" ]] || die "report needs --in <attempts.jsonl>"
        [[ -f "$IN" ]] || die "no such file: $IN"
        # The statistics live in namtor-eval, not in this script.
        cargo run -q -p namtor-eval --bin namtor-eval-report -- --in "$IN" --conditions "$CONDITIONS"
        ;;
    *)
        die "usage: $0 <genuine|impostor|attack|report> [options]  (try --help)"
        ;;
esac
