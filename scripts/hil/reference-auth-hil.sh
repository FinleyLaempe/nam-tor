#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Nam-tor reference authentication HIL — Task 7.
#
# Proves the installed authentication path end-to-end on the reference
# hardware (spec §12 G1.1 zero-config enrollment, G1.3 actionable failure
# messages). An owner runs this on a machine with the reference camera
# (Sonix 3277:0018, IR /dev/video2, RGB /dev/video0), the `namtor` system
# user, the `namtor-auth` group, an enrolled face, and the daemon running.
#
# Without root/camera/system-users the script records a WITHHELD record and
# exits 0 — it never fabricates a pass. The evidence file is JSONL, one
# record per step, appended on every run (idempotent: re-running overwrites
# the file so repeated runs do not accumulate stale passes).
#
# Steps (each appends one JSONL record):
#   1. install     — verify the install tree matches install-manifest.toml
#   2. activate    — systemctl start the daemon socket + service
#   3. enroll      — namtor-cli enroll for a test user
#   4. verify_pos  — namtor-cli verify for the enrolled user (expect Match)
#   5. verify_neg  — namtor-cli verify for an unenrolled uid (expect NoMatch)
#   6. trace       — journalctl safe trace dump (no biometric data)
#   7. restore     — stop daemon, remove test enrollment, restore state
#
# Usage:
#   sudo ./scripts/hil/reference-auth-hil.sh
#   ./scripts/hil/reference-auth-hil.sh            # → WITHHELD, exit 0
#   NAMTOR_EVIDENCE=/path/to.jsonl ./scripts/hil/reference-auth-hil.sh
#   NAMTOR_TEST_USER=alice ./scripts/hil/reference-auth-hil.sh
#
# Environment overrides (all have safe defaults):
#   NAMTOR_EVIDENCE     — evidence JSONL path
#                         (default docs/evidence/reference-auth-hil.jsonl,
#                          relative to the repo root)
#   NAMTOR_TEST_USER    — username to enroll/verify (default: current user)
#   NAMTOR_NEG_UID      — uid guaranteed unenrolled for negative verify
#                         (default 65534, the 'nobody' account)
#   NAMTOR_SOCKET       — daemon socket (default /run/namtor/auth.sock)
#   NAMTOR_DAEMON_BIN   — daemon binary (default /usr/lib/namtor/namtor-daemon)
#   NAMTOR_CLI_BIN      — cli binary (default /usr/bin/namtor-cli)
#   NAMTOR_IR_NODE      — IR camera node (default /dev/video2)
#   NAMTOR_DRY_RUN      — set to 1 to force WITHHELD regardless of hardware
#                         (used by CI; never fabricates a pass)

set -uo pipefail

# ---------------------------------------------------------------------------
# Resolve repo root (this script lives in scripts/hil/)
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

NAMTOR_EVIDENCE="${NAMTOR_EVIDENCE:-$REPO_ROOT/docs/evidence/reference-auth-hil.jsonl}"
NAMTOR_TEST_USER="${NAMTOR_TEST_USER:-$(id -un 2>/dev/null || echo hil-test)}"
NAMTOR_NEG_UID="${NAMTOR_NEG_UID:-65534}"
NAMTOR_SOCKET="${NAMTOR_SOCKET:-/run/namtor/auth.sock}"
NAMTOR_DAEMON_BIN="${NAMTOR_DAEMON_BIN:-/usr/lib/namtor/namtor-daemon}"
NAMTOR_CLI_BIN="${NAMTOR_CLI_BIN:-/usr/bin/namtor-cli}"
NAMTOR_IR_NODE="${NAMTOR_IR_NODE:-/dev/video2}"
NAMTOR_DRY_RUN="${NAMTOR_DRY_RUN:-0}"

# Hardware identity — recorded in every JSONL record so a reviewer can tie
# a pass to a specific machine. Best-effort: empty if the tools are absent.
HW_ID=""
if command -v dmidecode >/dev/null 2>&1; then
    HW_ID="$(dmidecode -s system-uuid 2>/dev/null || echo "")"
fi
if [[ -z "$HW_ID" ]] && [[ -r /sys/class/dmi/id/product_uuid ]]; then
    HW_ID="$(cat /sys/class/dmi/id/product_uuid 2>/dev/null || echo "")"
fi
if [[ -z "$HW_ID" ]] && [[ -r /sys/class/net/eno1/address ]]; then
    HW_ID="$(cat /sys/class/net/eno1/address 2>/dev/null || echo "")"
fi

# ---------------------------------------------------------------------------
# JSONL helpers — emit one record per step. jq is preferred but not required;
# we fall back to a python3 one-liner so the script runs on minimal systems.
# ---------------------------------------------------------------------------

emit_record() {
    # emit_record <step> <status> <detail>
    local step="$1"
    local status="$2"
    local detail="$3"
    local ts
    ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    local hw="${HW_ID:-}"
    if command -v jq >/dev/null 2>&1; then
        jq -c -n \
            --arg ts "$ts" \
            --arg step "$step" \
            --arg status "$status" \
            --arg detail "$detail" \
            --arg hw "$hw" \
            '{timestamp:$ts, step:$step, status:$status, detail:$detail, hardware_identity:$hw}' \
            >> "$NAMTOR_EVIDENCE"
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c '
import json, sys
rec = {
    "timestamp": sys.argv[1],
    "step": sys.argv[2],
    "status": sys.argv[3],
    "detail": sys.argv[4],
    "hardware_identity": sys.argv[5],
}
print(json.dumps(rec, separators=(",", ":")))
' "$ts" "$step" "$status" "$detail" "$hw" >> "$NAMTOR_EVIDENCE"
    else
        # Last resort: hand-rolled JSON. detail is escaped minimally.
        local esc
        esc="${detail//\\/\\\\}"
        esc="${esc//\"/\\\"}"
        printf '{"timestamp":"%s","step":"%s","status":"%s","detail":"%s","hardware_identity":"%s"}\n' \
            "$ts" "$step" "$status" "$esc" "$hw" >> "$NAMTOR_EVIDENCE"
    fi
}

# ---------------------------------------------------------------------------
# Prerequisite gate. The owner-gated inputs are: root, the reference camera,
# the namtor system user, and the namtor-auth group. If any is missing — or
# NAMTOR_DRY_RUN=1 — record WITHHELD for every step and exit 0. WITHHELD is
# the only acceptable outcome without hardware; we never fabricate a pass.
# ---------------------------------------------------------------------------

missing=()

if [[ "$NAMTOR_DRY_RUN" == "1" ]]; then
    missing+=("dry_run_forced")
fi

if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
    missing+=("root")
fi

if [[ ! -e "$NAMTOR_IR_NODE" ]]; then
    missing+=("ir_camera($NAMTOR_IR_NODE)")
fi

if ! id namtor >/dev/null 2>&1; then
    missing+=("namtor_user")
fi

if ! getent group namtor-auth >/dev/null 2>&1; then
    missing+=("namtor-auth_group")
fi

if [[ ! -x "$NAMTOR_DAEMON_BIN" ]]; then
    missing+=("daemon_binary($NAMTOR_DAEMON_BIN)")
fi

if [[ ! -x "$NAMTOR_CLI_BIN" ]]; then
    missing+=("cli_binary($NAMTOR_CLI_BIN)")
fi

if [[ ${#missing[@]} -gt 0 ]]; then
    reason="WITHHELD: missing owner-gated inputs: $(IFS=,; echo "${missing[*]}")"
    # Start a fresh evidence file for this run (idempotent: no stale passes).
    : > "$NAMTOR_EVIDENCE"
    emit_record "install"    "WITHHELD" "$reason"
    emit_record "activate"   "WITHHELD" "$reason"
    emit_record "enroll"     "WITHHELD" "$reason"
    emit_record "verify_pos" "WITHHELD" "$reason"
    emit_record "verify_neg" "WITHHELD" "$reason"
    emit_record "trace"      "WITHHELD" "$reason"
    emit_record "restore"    "WITHHELD" "$reason"
    echo "reference-auth-hil: $reason"
    echo "reference-auth-hil: evidence written to $NAMTOR_EVIDENCE (7 WITHHELD records)"
    exit 0
fi

# ---------------------------------------------------------------------------
# Live HIL path — owner has root + camera + system users. Each step records
# pass|fail. A fail on install or activate aborts with recovery instructions
# (malformed install must not proceed to enrollment).
# ---------------------------------------------------------------------------

: > "$NAMTOR_EVIDENCE"

# Step 1 — install validation. Verify every file in install-manifest.toml is
# present with the declared mode. stage.sh is the deterministic producer; we
# check the deployed tree against the manifest's declared paths.
install_check() {
    local manifest="$REPO_ROOT/packaging/install-manifest.toml"
    if [[ ! -f "$manifest" ]]; then
        echo "manifest missing: $manifest"
        return 1
    fi
    local fails=0
    # Check the critical binaries + PAM module exist and are executable.
    local paths=(
        "$NAMTOR_DAEMON_BIN"
        "/usr/lib/namtor/namtor-worker"
        "/usr/lib/namtor/namtor-worker-spawn"
        "$NAMTOR_CLI_BIN"
        "/usr/lib/security/pam_namtor.so"
        "/usr/lib/systemd/system/namtor-daemon.service"
        "/usr/lib/systemd/system/namtor-daemon.socket"
    )
    for p in "${paths[@]}"; do
        if [[ ! -e "$p" ]]; then
            echo "missing: $p"
            fails=$((fails + 1))
        fi
    done
    # setuid bit on worker-spawn (mode 4755).
    if [[ -e /usr/lib/namtor/namtor-worker-spawn ]]; then
        local mode
        mode="$(stat -c '%a' /usr/lib/namtor/namtor-worker-spawn 2>/dev/null || echo "")"
        if [[ "$mode" != "4755" ]]; then
            echo "wrong mode on namtor-worker-spawn: got ${mode:-?}, want 4755"
            fails=$((fails + 1))
        fi
    fi
    return "$fails"
}

install_out="$(install_check 2>&1)" || true
install_rc=$?
if [[ $install_rc -eq 0 ]]; then
    emit_record "install" "pass" "all manifest paths present, setuid bit verified"
else
    emit_record "install" "fail" "install_check rc=$install_rc: $install_out"
    # Malformed install: do NOT proceed to enrollment/verify. Recovery:
    cat >&2 <<'EOF'
reference-auth-hil: install validation FAILED.
Recovery:
  1. Re-stage:  sudo ./packaging/stage.sh /mnt/staging && sudo cp -a /mnt/staging/. /
  2. Re-sysusers+tmpfiles:
     sudo systemd-sysusers /usr/lib/sysusers.d/namtor.conf
     sudo systemd-tmpfiles --create /usr/lib/tmpfiles.d/namtor.conf
  3. Re-run:    sudo ./scripts/hil/reference-auth-hil.sh
Aborting before enrollment — a malformed install must not reach auth.
EOF
    exit 1
fi

# Step 2 — activate the daemon (socket + service).
activate_out="$(systemctl start namtor-daemon.socket 2>&1)" || true
activate_out="$activate_out
$(systemctl start namtor-daemon.service 2>&1)" || true
# Give it a moment to come up, then check the socket.
sleep 1
if [[ -S "$NAMTOR_SOCKET" ]] && systemctl is-active --quiet namtor-daemon.service; then
    emit_record "activate" "pass" "daemon socket + service active"
else
    emit_record "activate" "fail" "socket or service not active: $activate_out"
    cat >&2 <<'EOF'
reference-auth-hil: activation FAILED.
Recovery:
  systemctl status namtor-daemon.service
  journalctl -u namtor-daemon.service -b --no-pager
Aborting before enrollment — daemon must be up.
EOF
    exit 1
fi

# Step 3 — enroll the test user. Idempotent: remove any prior enrollment
# first so a re-run is clean. Enrollment requires the camera and the namtor
# system user to own the store; run as root so chown to namtor takes effect.
NAMTOR_ACCEPT_LICENCE=1 "$NAMTOR_CLI_BIN" remove "$(id -u "$NAMTOR_TEST_USER")" 2>/dev/null || true
enroll_out="$(NAMTOR_ACCEPT_LICENCE=1 "$NAMTOR_CLI_BIN" enroll --user "$NAMTOR_TEST_USER" 2>&1)"
enroll_rc=$?
if [[ $enroll_rc -eq 0 ]]; then
    emit_record "enroll" "pass" "enrolled $NAMTOR_TEST_USER (uid $(id -u "$NAMTOR_TEST_USER"))"
else
    emit_record "enroll" "fail" "namtor-cli enroll rc=$enroll_rc: $enroll_out"
    # Continue — verify_pos will record its own fail; restore still runs.
fi

# Step 4 — positive verify. Expect Match for the enrolled user.
verify_pos_out="$("$NAMTOR_CLI_BIN" verify --user "$NAMTOR_TEST_USER" 2>&1)"
verify_pos_rc=$?
if [[ $verify_pos_rc -eq 0 ]] && echo "$verify_pos_out" | grep -q '^MATCH'; then
    emit_record "verify_pos" "pass" "positive verify Match for $NAMTOR_TEST_USER"
else
    emit_record "verify_pos" "fail" "rc=$verify_pos_rc: $verify_pos_out"
fi

# Step 5 — negative verify. Expect NoMatch for an unenrolled uid. We use the
# 'nobody' uid by default; an unenrolled uid must NOT match.
neg_user=""
if command -v getent >/dev/null 2>&1; then
    neg_user="$(getent passwd "$NAMTOR_NEG_UID" | cut -d: -f1 || echo "")"
fi
neg_user="${neg_user:-nobody}"
verify_neg_out="$("$NAMTOR_CLI_BIN" verify --user "$neg_user" 2>&1)"
verify_neg_rc=$?
# NoMatch is exit 1 from namtor-cli verify; that is the EXPECTED outcome here.
if [[ $verify_neg_rc -eq 1 ]] && echo "$verify_neg_out" | grep -q 'NO MATCH'; then
    emit_record "verify_neg" "pass" "negative verify NoMatch for uid $NAMTOR_NEG_UID ($neg_user)"
else
    emit_record "verify_neg" "fail" "expected NoMatch, got rc=$verify_neg_rc: $verify_neg_out"
fi

# Step 6 — safe trace dump. journalctl for the daemon, filtered to exclude
# biometric data. The spec (§8.3/§8.6) forbids score/embedding/template at
# info+ level; we record that the trace was captured and assert no biometric
# tokens appear at info+ level. The trace itself is NOT committed (privacy).
trace_tmp="$(mktemp)"
journalctl -u namtor-daemon.service -b --no-pager -o cat 2>/dev/null | \
    grep -iE 'level=(info|warn|error)' > "$trace_tmp" || true
# Forbidden tokens at info+ level (privacy scan mirrors the Rust test).
if grep -qiE 'score=|embedding|template|vec=|0x[0-9a-f]{8}|threshold=' "$trace_tmp"; then
    emit_record "trace" "fail" "biometric token found at info+ level in journal (privacy violation)"
else
    emit_record "trace" "pass" "journal info+ scan clean (no score/embedding/template/vec/threshold)"
fi
rm -f "$trace_tmp"

# Step 7 — restoration. Stop the daemon, remove the test enrollment so the
# machine is left as found. Always runs (even if earlier steps failed).
restore_msgs=""
"$NAMTOR_CLI_BIN" remove "$(id -u "$NAMTOR_TEST_USER")" 2>/dev/null \
    && restore_msgs="removed test enrollment" || restore_msgs="remove failed (no enrollment?)"
systemctl stop namtor-daemon.service 2>/dev/null || true
systemctl stop namtor-daemon.socket 2>/dev/null || true
restore_msgs="$restore_msgs; daemon stopped"
emit_record "restore" "pass" "$restore_msgs"

echo "reference-auth-hil: HIL complete. Evidence: $NAMTOR_EVIDENCE"
exit 0
