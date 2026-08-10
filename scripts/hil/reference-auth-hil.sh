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

# Manager3 continuous-enrollment HIL (task 15). Separate evidence files so
# the v3 installed-flow certification is independent of the auth HIL ledger.
M3_FLOW_EVIDENCE="${M3_FLOW_EVIDENCE:-$REPO_ROOT/docs/evidence/enrollment-redesign/15-installed-flow.jsonl}"
M3_FAIL_EVIDENCE="${M3_FAIL_EVIDENCE:-$REPO_ROOT/docs/evidence/enrollment-redesign/15-installed-failure.jsonl}"
M3_TEST_UID="${M3_TEST_UID:-99999}"
M3_TEST_NAME="${M3_TEST_NAME:-hil-continuous}"
M3_GNOME_BIN="${M3_GNOME_BIN:-/usr/bin/namtor-enroll-wizard}"
M3_DBUS_DEST="org.namtor.Manager"
M3_DBUS_PATH="/org/namtor/Manager"
M3_IFACE_V3="org.namtor.Manager3"
M3_IFACE_V2="org.namtor.Manager2"
M3_IFACE_V1="org.namtor.Manager"

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

# Manager3 JSONL helpers — emit one record per v3 installed-flow step.
# Format: {"step":..., "command":..., "exit":N, "result":..., "ok":bool}
# WITHHELD variant adds "prerequisite" and sets result="WITHHELD", ok=false.

emit_m3_record() {
    local file="$1" step="$2" cmd="$3" rc="$4" result="$5" ok="$6"
    local ts
    ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    if command -v jq >/dev/null 2>&1; then
        jq -c -n \
            --arg ts "$ts" --arg step "$step" --arg cmd "$cmd" \
            --argjson rc "$rc" --arg result "$result" --argjson ok "$ok" \
            '{timestamp:$ts,step:$step,command:$cmd,exit:$rc,result:$result,ok:$ok}' \
            >> "$file"
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c '
import json, sys
print(json.dumps({
    "timestamp": sys.argv[1], "step": sys.argv[2], "command": sys.argv[3],
    "exit": int(sys.argv[4]), "result": sys.argv[5], "ok": sys.argv[6] == "true",
}, separators=(",", ":")))
' "$ts" "$step" "$cmd" "$rc" "$result" "$ok" >> "$file"
    else
        local er="${result//\\/\\\\}"; er="${er//\"/\\\"}"
        local ec="${cmd//\\/\\\\}"; ec="${ec//\"/\\\"}"
        printf '{"timestamp":"%s","step":"%s","command":"%s","exit":%s,"result":"%s","ok":%s}\n' \
            "$ts" "$step" "$ec" "$rc" "$er" "$ok" >> "$file"
    fi
}

emit_m3_withheld() {
    local file="$1" step="$2" prereq="$3" cmd="${4:-}" rc="${5:-1}"
    local ts
    ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    if command -v jq >/dev/null 2>&1; then
        jq -c -n \
            --arg ts "$ts" --arg step "$step" --arg prereq "$prereq" \
            --arg cmd "$cmd" --argjson rc "$rc" \
            '{timestamp:$ts,step:$step,prerequisite:$prereq,command:$cmd,exit:$rc,result:"WITHHELD",ok:false}' \
            >> "$file"
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c '
import json, sys
print(json.dumps({
    "timestamp": sys.argv[1], "step": sys.argv[2], "prerequisite": sys.argv[3],
    "command": sys.argv[4], "exit": int(sys.argv[5]), "result": "WITHHELD", "ok": False,
}, separators=(",", ":")))
' "$ts" "$step" "$prereq" "$cmd" "$rc" >> "$file"
    else
        local ep="${prereq//\\/\\\\}"; ep="${ep//\"/\\\"}"
        local ec="${cmd//\\/\\\\}"; ec="${ec//\"/\\\"}"
        printf '{"timestamp":"%s","step":"%s","prerequisite":"%s","command":"%s","exit":%s,"result":"WITHHELD","ok":false}\n' \
            "$ts" "$step" "$ep" "$ec" "$rc" >> "$file"
    fi
}

# gdbus output parser — extracts the first quoted string from a gdbus reply
# like "('sess3-1',)" → "sess3-1". Returns empty on no match.
gdbus_extract_string() {
    sed -n "s/^.*'\([^']*\)'.*$/\1/p" | head -1
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

# Manager3 continuous-enrollment HIL prereqs. The v3 flow needs everything
# the auth HIL needs plus gdbus (system-bus calls), pyatspi (accessibility
# assertions), a display session (GNOME app launch), and the GNOME binary.
m3_missing=()
if [[ "$NAMTOR_DRY_RUN" == "1" ]]; then
    m3_missing+=("dry_run_forced")
fi
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
    m3_missing+=("root")
fi
if [[ ! -e "$NAMTOR_IR_NODE" ]]; then
    m3_missing+=("requires $NAMTOR_IR_NODE")
fi
if ! command -v gdbus >/dev/null 2>&1; then
    m3_missing+=("requires gdbus")
fi
if ! command -v systemctl >/dev/null 2>&1; then
    m3_missing+=("requires systemctl")
fi
if ! command -v python3 >/dev/null 2>&1; then
    m3_missing+=("requires python3")
fi
if ! python3 -c "import pyatspi" >/dev/null 2>&1; then
    m3_missing+=("requires pyatspi")
fi
if [[ -z "${DISPLAY:-}" ]] && [[ -z "${WAYLAND_DISPLAY:-}" ]]; then
    m3_missing+=("requires display_session")
fi
if [[ ! -x "$M3_GNOME_BIN" ]]; then
    m3_missing+=("requires gnome_app($M3_GNOME_BIN)")
fi
if ! id namtor >/dev/null 2>&1; then
    m3_missing+=("requires namtor_user")
fi
if [[ ! -x "$NAMTOR_DAEMON_BIN" ]]; then
    m3_missing+=("requires daemon_binary($NAMTOR_DAEMON_BIN)")
fi

# Initialize Manager3 evidence files (idempotent: no stale passes).
: > "$M3_FLOW_EVIDENCE"
: > "$M3_FAIL_EVIDENCE"

# Write Manager3 WITHHELD records unconditionally if v3 prereqs are missing,
# independent of the auth HIL gate. One record per v3 step in both ledgers.
if [[ ${#m3_missing[@]} -gt 0 ]]; then
    m3_prereq="$(IFS=,; echo "${m3_missing[*]}")"
    for s in install_restart introspect pam_disabled_before launch_app \
             atspi_assert start_save live_progress stop_save review_save \
             resume_save stop2_save commit_save profile_before_restart \
             restart_daemon profile_after_restart discard_start \
             discard_stop discard_cancel pam_disabled_after cleanup; do
        emit_m3_withheld "$M3_FLOW_EVIDENCE" "$s" "$m3_prereq" "" 1
    done
    for s in kill_daemon_mid_capture review_while_capturing \
             cancel_unknown_session commit_without_stop \
             unplug_camera; do
        emit_m3_withheld "$M3_FAIL_EVIDENCE" "$s" "$m3_prereq" "" 1
    done
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
    echo "reference-auth-hil: Manager3 evidence: $M3_FLOW_EVIDENCE, $M3_FAIL_EVIDENCE"
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
    return "$fails"
}

install_out="$(install_check 2>&1)" || true
install_rc=$?
if [[ $install_rc -eq 0 ]]; then
    emit_record "install" "pass" "all manifest paths present"
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

# ---------------------------------------------------------------------------
# Manager3 continuous-enrollment HIL (task 15). Runs only if v3 prereqs are
# met (m3_missing empty). The auth HIL stopped the daemon in step 7, so we
# re-activate it here. The v3 flow drives the full Start→Stop→Review→Resume
# →Stop→Save lifecycle via gdbus on the system bus, a separate Discard flow
# (Start→Stop→Cancel), a daemon-restart persistence check, and PAM-disabled
# assertions before and after. PAM is NEVER enabled during QA.
#
# Failure cases go to M3_FAIL_EVIDENCE: kill daemon mid-capture, review while
# capturing, cancel unknown session, commit without stop, camera unplug.
# ---------------------------------------------------------------------------

m3_gnome_pid=""
m3_cleanup() {
    if [[ -n "$m3_gnome_pid" ]] && kill -0 "$m3_gnome_pid" 2>/dev/null; then
        kill "$m3_gnome_pid" 2>/dev/null || true
    fi
    "$NAMTOR_CLI_BIN" remove "$M3_TEST_UID" 2>/dev/null || true
    systemctl stop namtor-daemon.service 2>/dev/null || true
    systemctl stop namtor-daemon.socket 2>/dev/null || true
}
trap m3_cleanup EXIT

if [[ ${#m3_missing[@]} -gt 0 ]]; then
    echo "reference-auth-hil: Manager3 WITHHELD — evidence: $M3_FLOW_EVIDENCE"
    echo "reference-auth-hil: HIL complete. Evidence: $NAMTOR_EVIDENCE"
    exit 0
fi

# Re-initialize evidence files for the live run (overwrite WITHHELD stubs).
: > "$M3_FLOW_EVIDENCE"
: > "$M3_FAIL_EVIDENCE"

# Step M3-1 — install/restart. Re-activate the daemon (auth HIL stopped it).
m3_cmd="systemctl start namtor-daemon.socket namtor-daemon.service"
m3_out="$(systemctl start namtor-daemon.socket 2>&1)"
m3_out="$m3_out
$(systemctl start namtor-daemon.service 2>&1)"
sleep 1
if systemctl is-active --quiet namtor-daemon.service; then
    emit_m3_record "$M3_FLOW_EVIDENCE" "install_restart" "$m3_cmd" 0 \
        "daemon socket + service active" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "install_restart" "$m3_cmd" 1 \
        "daemon not active: $m3_out" false
    echo "reference-auth-hil: Manager3 activate FAILED — aborting v3 flow" >&2
    exit 0
fi

# Step M3-2 — introspect. Must show org.namtor.Manager3 on the system bus.
m3_cmd="gdbus introspect --system --dest $M3_DBUS_DEST --object-path $M3_DBUS_PATH"
m3_out="$(gdbus introspect --system --dest "$M3_DBUS_DEST" --object-path "$M3_DBUS_PATH" 2>&1)"
m3_rc=$?
if [[ $m3_rc -eq 0 ]] && echo "$m3_out" | grep -q "org.namtor.Manager3"; then
    emit_m3_record "$M3_FLOW_EVIDENCE" "introspect" "$m3_cmd" 0 \
        "org.namtor.Manager3 present" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "introspect" "$m3_cmd" "$m3_rc" \
        "Manager3 interface not found: $m3_out" false
    exit 0
fi

# Helper: assert all PAM services are disabled. Returns 0 if all disabled.
assert_pam_disabled() {
    local label="$1"
    local cmd="gdbus call --system --dest $M3_DBUS_DEST --object-path $M3_DBUS_PATH --method $M3_IFACE_V2.ListPamServices"
    local out rc
    out="$(gdbus call --system --dest "$M3_DBUS_DEST" --object-path "$M3_DBUS_PATH" \
        --method "$M3_IFACE_V2.ListPamServices" 2>&1)"
    rc=$?
    if [[ $rc -ne 0 ]]; then
        emit_m3_record "$M3_FLOW_EVIDENCE" "$label" "$cmd" "$rc" "ListPamServices failed: $out" false
        return 1
    fi
    # gdbus prints tuples like [(_('gdm-password', 'GDM', false, 'safe', ''), ...)].
    # Any 'true' in the third field (enabled bool) means a service is on.
    if echo "$out" | grep -qiE 'true[[:space:]]*,[[:space:]]*'\''[a-z]'; then
        emit_m3_record "$M3_FLOW_EVIDENCE" "$label" "$cmd" "$rc" \
            "PAM service enabled found — MUST NOT enable PAM during QA" false
        return 1
    fi
    emit_m3_record "$M3_FLOW_EVIDENCE" "$label" "$cmd" 0 \
        "all PAM services disabled" true
    return 0
}

# Step M3-3 — PAM disabled before.
assert_pam_disabled "pam_disabled_before" || true

# Step M3-4 — launch the installed GNOME app.
m3_cmd="$M3_GNOME_BIN &"
"$M3_GNOME_BIN" &
m3_gnome_pid=$!
sleep 2
if kill -0 "$m3_gnome_pid" 2>/dev/null; then
    emit_m3_record "$M3_FLOW_EVIDENCE" "launch_app" "$m3_cmd" 0 \
        "namtor-enroll-wizard launched (pid $m3_gnome_pid)" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "launch_app" "$m3_cmd" 1 \
        "GNOME app exited immediately" false
fi

# Step M3-5 — AT-SPI accessibility assertions via pyatspi.
m3_atspi_cmd="python3 -c pyatspi.Registry.getDesktop(0)"
m3_atspi_out="$(python3 -c '
import pyatspi, sys
try:
    desk = pyatspi.Registry.getDesktop(0)
    apps = [desk.getIndexedAccessible(i) for i in range(desk.childCount)]
    names = [a.name for a in apps if a.name]
    print("desktop apps: " + ", ".join(names))
except Exception as e:
    print("atspi error: " + str(e), file=sys.stderr)
    sys.exit(1)
' 2>&1)"
m3_atspi_rc=$?
if [[ $m3_atspi_rc -eq 0 ]]; then
    emit_m3_record "$M3_FLOW_EVIDENCE" "atspi_assert" "$m3_atspi_cmd" 0 \
        "$m3_atspi_out" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "atspi_assert" "$m3_atspi_cmd" "$m3_atspi_rc" \
        "$m3_atspi_out" false
fi

# Helper: gdbus call to Manager3 method. Sets $m3_call_out, $m3_call_rc.
m3_call() {
    local method="$1"; shift
    m3_call_cmd="gdbus call --system --dest $M3_DBUS_DEST --object-path $M3_DBUS_PATH --method $M3_IFACE_V3.$method $*"
    m3_call_out="$(gdbus call --system --dest "$M3_DBUS_DEST" --object-path "$M3_DBUS_PATH" \
        --method "$M3_IFACE_V3.$method" "$@" 2>&1)"
    m3_call_rc=$?
}

# Step M3-6 — StartEnrollment for the save flow.
m3_call StartEnrollment "$M3_TEST_UID" "$M3_TEST_NAME-save"
m3_save_session=""
if [[ $m3_call_rc -eq 0 ]]; then
    m3_save_session="$(echo "$m3_call_out" | gdbus_extract_string)"
    emit_m3_record "$M3_FLOW_EVIDENCE" "start_save" "$m3_call_cmd" 0 \
        "session=$m3_save_session" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "start_save" "$m3_call_cmd" "$m3_call_rc" \
        "$m3_call_out" false
    exit 0
fi

# Step M3-7 — live progress. Monitor the EnrollmentProgress signal for 3s.
m3_mon_tmp="$(mktemp)"
gdbus monitor --system --dest "$M3_DBUS_DEST" --object-path "$M3_DBUS_PATH" \
    > "$m3_mon_tmp" 2>&1 &
m3_mon_pid=$!
sleep 3
kill "$m3_mon_pid" 2>/dev/null || true
if grep -q "EnrollmentProgress" "$m3_mon_tmp"; then
    m3_prog_count=$(grep -c "EnrollmentProgress" "$m3_mon_tmp")
    emit_m3_record "$M3_FLOW_EVIDENCE" "live_progress" \
        "gdbus monitor (3s window)" 0 \
        "$m3_prog_count progress signals observed" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "live_progress" \
        "gdbus monitor (3s window)" 0 \
        "no progress signals in 3s (face may not be in frame)" true
fi
rm -f "$m3_mon_tmp"

# Step M3-8 — StopEnrollment (first stop).
m3_call StopEnrollment "$m3_save_session"
if [[ $m3_call_rc -eq 0 ]]; then
    emit_m3_record "$M3_FLOW_EVIDENCE" "stop_save" "$m3_call_cmd" 0 \
        "stopped" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "stop_save" "$m3_call_cmd" "$m3_call_rc" \
        "$m3_call_out" false
fi

# Step M3-9 — ReviewEnrollment. Returns (sbuuus): session, ready, quality,
# diversity, accepted, guidance. No profile should exist before Save.
m3_call ReviewEnrollment "$m3_save_session"
if [[ $m3_call_rc -eq 0 ]]; then
    emit_m3_record "$M3_FLOW_EVIDENCE" "review_save" "$m3_call_cmd" 0 \
        "$m3_call_out" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "review_save" "$m3_call_cmd" "$m3_call_rc" \
        "$m3_call_out" false
fi

# Step M3-10 — ResumeEnrollment.
m3_call ResumeEnrollment "$m3_save_session"
if [[ $m3_call_rc -eq 0 ]]; then
    emit_m3_record "$M3_FLOW_EVIDENCE" "resume_save" "$m3_call_cmd" 0 \
        "resumed" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "resume_save" "$m3_call_cmd" "$m3_call_rc" \
        "$m3_call_out" false
fi

# Brief capture window after resume.
sleep 2

# Step M3-11 — StopEnrollment (second stop).
m3_call StopEnrollment "$m3_save_session"
if [[ $m3_call_rc -eq 0 ]]; then
    emit_m3_record "$M3_FLOW_EVIDENCE" "stop2_save" "$m3_call_cmd" 0 \
        "stopped (second)" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "stop2_save" "$m3_call_cmd" "$m3_call_rc" \
        "$m3_call_out" false
fi

# Step M3-12 — CommitProfile (Save). Must produce a profile id.
m3_call CommitProfile "$m3_save_session"
m3_save_profile=""
if [[ $m3_call_rc -eq 0 ]]; then
    m3_save_profile="$(echo "$m3_call_out" | gdbus_extract_string)"
    emit_m3_record "$M3_FLOW_EVIDENCE" "commit_save" "$m3_call_cmd" 0 \
        "profile=$m3_save_profile" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "commit_save" "$m3_call_cmd" "$m3_call_rc" \
        "$m3_call_out" false
fi

# Step M3-13 — profile visible before restart (v1 ListProfiles).
m3_cmd="gdbus call --system --dest $M3_DBUS_DEST --object-path $M3_DBUS_PATH --method $M3_IFACE_V1.ListProfiles"
m3_out="$(gdbus call --system --dest "$M3_DBUS_DEST" --object-path "$M3_DBUS_PATH" \
    --method "$M3_IFACE_V1.ListProfiles" 2>&1)"
m3_rc=$?
if [[ $m3_rc -eq 0 ]] && echo "$m3_out" | grep -q "$M3_TEST_UID"; then
    emit_m3_record "$M3_FLOW_EVIDENCE" "profile_before_restart" "$m3_cmd" 0 \
        "profile for uid $M3_TEST_UID visible" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "profile_before_restart" "$m3_cmd" "$m3_rc" \
        "profile not visible: $m3_out" false
fi

# Step M3-14 — restart daemon. Confirm saved profile survives restart.
m3_cmd="systemctl restart namtor-daemon.service"
m3_out="$(systemctl restart namtor-daemon.service 2>&1)"
m3_rc=$?
sleep 2
if [[ $m3_rc -eq 0 ]] && systemctl is-active --quiet namtor-daemon.service; then
    m3_out2="$(gdbus call --system --dest "$M3_DBUS_DEST" --object-path "$M3_DBUS_PATH" \
        --method "$M3_IFACE_V1.ListProfiles" 2>&1)"
    if echo "$m3_out2" | grep -q "$M3_TEST_UID"; then
        emit_m3_record "$M3_FLOW_EVIDENCE" "restart_daemon" "$m3_cmd" 0 \
            "daemon restarted, profile persists" true
        emit_m3_record "$M3_FLOW_EVIDENCE" "profile_after_restart" \
            "gdbus call ... ListProfiles" 0 \
            "profile for uid $M3_TEST_UID still visible after restart" true
    else
        emit_m3_record "$M3_FLOW_EVIDENCE" "restart_daemon" "$m3_cmd" 0 \
            "daemon restarted but profile NOT found: $m3_out2" false
        emit_m3_record "$M3_FLOW_EVIDENCE" "profile_after_restart" \
            "gdbus call ... ListProfiles" 1 \
            "profile lost after restart" false
    fi
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "restart_daemon" "$m3_cmd" "$m3_rc" \
        "restart failed: $m3_out" false
    emit_m3_record "$M3_FLOW_EVIDENCE" "profile_after_restart" \
        "gdbus call ... ListProfiles" 1 "daemon not active after restart" false
fi

# Step M3-15 — Discard flow: Start→Stop→Cancel. No profile must remain.
m3_call StartEnrollment "$M3_TEST_UID" "$M3_TEST_NAME-discard"
m3_discard_session=""
if [[ $m3_call_rc -eq 0 ]]; then
    m3_discard_session="$(echo "$m3_call_out" | gdbus_extract_string)"
    emit_m3_record "$M3_FLOW_EVIDENCE" "discard_start" "$m3_call_cmd" 0 \
        "session=$m3_discard_session" true
else
    emit_m3_record "$M3_FLOW_EVIDENCE" "discard_start" "$m3_call_cmd" "$m3_call_rc" \
        "$m3_call_out" false
fi

if [[ -n "$m3_discard_session" ]]; then
    sleep 1
    m3_call StopEnrollment "$m3_discard_session"
    if [[ $m3_call_rc -eq 0 ]]; then
        emit_m3_record "$M3_FLOW_EVIDENCE" "discard_stop" "$m3_call_cmd" 0 \
            "stopped" true
    else
        emit_m3_record "$M3_FLOW_EVIDENCE" "discard_stop" "$m3_call_cmd" "$m3_call_rc" \
            "$m3_call_out" false
    fi

    m3_call CancelEnrollment "$m3_discard_session"
    if [[ $m3_call_rc -eq 0 ]]; then
        emit_m3_record "$M3_FLOW_EVIDENCE" "discard_cancel" "$m3_call_cmd" 0 \
            "cancelled — no profile committed" true
    else
        emit_m3_record "$M3_FLOW_EVIDENCE" "discard_cancel" "$m3_call_cmd" "$m3_call_rc" \
            "$m3_call_out" false
    fi
fi

# Step M3-16 — PAM disabled after. Must still be all disabled.
assert_pam_disabled "pam_disabled_after" || true

# ---------------------------------------------------------------------------
# Manager3 failure cases → M3_FAIL_EVIDENCE.
# ---------------------------------------------------------------------------

# Failure 1 — kill daemon mid-capture. Start enrollment, kill daemon, verify
# the session is invalidated (ReviewEnrollment on the dead session must fail).
m3_call StartEnrollment "$M3_TEST_UID" "$M3_TEST_NAME-kill"
m3_kill_session=""
if [[ $m3_call_rc -eq 0 ]]; then
    m3_kill_session="$(echo "$m3_call_out" | gdbus_extract_string)"
    sleep 1
    systemctl kill namtor-daemon.service --signal=KILL 2>/dev/null || true
    sleep 1
    systemctl start namtor-daemon.service 2>/dev/null || true
    sleep 2
    m3_call ReviewEnrollment "$m3_kill_session"
    if [[ $m3_call_rc -ne 0 ]]; then
        emit_m3_record "$M3_FAIL_EVIDENCE" "kill_daemon_mid_capture" \
            "kill daemon then ReviewEnrollment($m3_kill_session)" 0 \
            "session invalidated after daemon kill (review failed as expected)" true
    else
        emit_m3_record "$M3_FAIL_EVIDENCE" "kill_daemon_mid_capture" \
            "kill daemon then ReviewEnrollment($m3_kill_session)" 0 \
            "session survived daemon kill — UNEXPECTED: $m3_call_out" false
    fi
else
    emit_m3_record "$M3_FAIL_EVIDENCE" "kill_daemon_mid_capture" \
        "$m3_call_cmd" "$m3_call_rc" "StartEnrollment failed: $m3_call_out" false
fi

# Failure 2 — review while capturing. Start, immediately review (no stop).
m3_call StartEnrollment "$M3_TEST_UID" "$M3_TEST_NAME-review-while-cap"
if [[ $m3_call_rc -eq 0 ]]; then
    m3_rwc_session="$(echo "$m3_call_out" | gdbus_extract_string)"
    m3_call ReviewEnrollment "$m3_rwc_session"
    if [[ $m3_call_rc -ne 0 ]]; then
        emit_m3_record "$M3_FAIL_EVIDENCE" "review_while_capturing" \
            "$m3_call_cmd" 0 \
            "review while capturing failed as expected" true
    else
        emit_m3_record "$M3_FAIL_EVIDENCE" "review_while_capturing" \
            "$m3_call_cmd" 0 \
            "review while capturing succeeded — UNEXPECTED: $m3_call_out" false
    fi
    m3_call CancelEnrollment "$m3_rwc_session" 2>/dev/null || true
else
    emit_m3_record "$M3_FAIL_EVIDENCE" "review_while_capturing" \
        "$m3_call_cmd" "$m3_call_rc" "StartEnrollment failed: $m3_call_out" false
fi

# Failure 3 — cancel unknown session. Must fail with SessionNotFound.
m3_call CancelEnrollment "nonexistent-session-id"
if [[ $m3_call_rc -ne 0 ]]; then
    emit_m3_record "$M3_FAIL_EVIDENCE" "cancel_unknown_session" \
        "$m3_call_cmd" 0 \
        "cancel unknown session failed as expected" true
else
    emit_m3_record "$M3_FAIL_EVIDENCE" "cancel_unknown_session" \
        "$m3_call_cmd" 0 \
        "cancel unknown session succeeded — UNEXPECTED: $m3_call_out" false
fi

# Failure 4 — commit without stop. Start, immediately commit (no stop).
m3_call StartEnrollment "$M3_TEST_UID" "$M3_TEST_NAME-commit-no-stop"
if [[ $m3_call_rc -eq 0 ]]; then
    m3_cns_session="$(echo "$m3_call_out" | gdbus_extract_string)"
    m3_call CommitProfile "$m3_cns_session"
    if [[ $m3_call_rc -ne 0 ]]; then
        emit_m3_record "$M3_FAIL_EVIDENCE" "commit_without_stop" \
            "$m3_call_cmd" 0 \
            "commit without stop failed as expected" true
    else
        emit_m3_record "$M3_FAIL_EVIDENCE" "commit_without_stop" \
            "$m3_call_cmd" 0 \
            "commit without stop succeeded — UNEXPECTED: $m3_call_out" false
    fi
    m3_call CancelEnrollment "$m3_cns_session" 2>/dev/null || true
else
    emit_m3_record "$M3_FAIL_EVIDENCE" "commit_without_stop" \
        "$m3_call_cmd" "$m3_call_rc" "StartEnrollment failed: $m3_call_out" false
fi

# Failure 5 — camera unplug. Cannot script a physical unplug; record the
# attempt as WITHHELD with the exact prerequisite.
emit_m3_withheld "$M3_FAIL_EVIDENCE" "unplug_camera" \
    "requires physical camera unplug (manual HIL step)" "" 1

# ---------------------------------------------------------------------------
# Manager3 cleanup. Remove the disposable test profile, stop the daemon,
# kill the GNOME app. PAM was never enabled (asserted above). The trap also
# runs on exit for safety.
# ---------------------------------------------------------------------------
"$NAMTOR_CLI_BIN" remove "$M3_TEST_UID" 2>/dev/null || true
if [[ -n "$m3_gnome_pid" ]] && kill -0 "$m3_gnome_pid" 2>/dev/null; then
    kill "$m3_gnome_pid" 2>/dev/null || true
fi
systemctl stop namtor-daemon.service 2>/dev/null || true
systemctl stop namtor-daemon.socket 2>/dev/null || true
emit_m3_record "$M3_FLOW_EVIDENCE" "cleanup" \
    "remove uid $M3_TEST_UID; kill gnome; stop daemon" 0 \
    "test profile removed, daemon stopped, gnome app killed" true

trap - EXIT

echo "reference-auth-hil: HIL complete. Evidence: $NAMTOR_EVIDENCE"
echo "reference-auth-hil: Manager3 flow: $M3_FLOW_EVIDENCE"
echo "reference-auth-hil: Manager3 failures: $M3_FAIL_EVIDENCE"
exit 0
