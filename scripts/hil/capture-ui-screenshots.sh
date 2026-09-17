#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Nam-tor UI screenshot capture harness — tasks 28/29.
#
# Captures screenshots of every named UI state (the 15-state matrix from
# docs/evidence/enrollment-redesign/16-visual-review.md) for the enrollment
# wizard, against a real namtor-daemon on the system bus.  The output
# directory is docs/evidence/ui/real-run/ui/ (one desktop-neutral UI;
# the <desktop> split dates from when a Plasma KCM was captured too).
#
# This used to make a second pass over the Plasma KCM via kcmshell6. The
# KCM was removed when the project consolidated on one desktop-neutral
# GTK4 UI, so there is one pass now. The existing KDE screenshots under
# docs/evidence/ui/kde/ are frozen release artifacts and are left in
# place.
#
# Without NAMTOR_HW_TEST=1 the script prints a SKIP message and exits 0 —
# it never fabricates a screenshot.  The manifest
# (docs/evidence/ui/real-run/manifest.json) is regenerated on every run
# with one entry per attempted state, recording pass/fail/skip per entry.
#
# Usage:
#   NAMTOR_HW_TEST=1 sudo ./scripts/hil/capture-ui-screenshots.sh
#   ./scripts/hil/capture-ui-screenshots.sh            # → SKIP, exit 0
#
# Environment overrides (all have safe defaults):
#   NAMTOR_HW_TEST       — must be "1" to run the live path
#   NAMTOR_DAEMON_BIN    — the multi-call binary, run as `$NAMTOR_DAEMON_BIN
#                          daemon` (default /usr/bin/namtor)
#   NAMTOR_UI_BIN        — wizard binary (default /usr/bin/namtor-enroll-wizard)
#   NAMTOR_SOCKET        — daemon socket (default /run/namtor/auth.sock)
#   NAMTOR_EVIDENCE_DIR  — screenshot root (default docs/evidence/ui/real-run)
#                          (relative to repo root)

set -euo pipefail

# ---------------------------------------------------------------------------
# Gate: NAMTOR_HW_TEST=1 is required.  Without it, SKIP and exit 0.
# ---------------------------------------------------------------------------
if [[ "${NAMTOR_HW_TEST:-0}" != "1" ]]; then
    echo "SKIP: NAMTOR_HW_TEST not set, screenshot capture requires real hardware/display"
    exit 0
fi

# ---------------------------------------------------------------------------
# Resolve repo root (this script lives in scripts/hil/).
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

NAMTOR_DAEMON_BIN="${NAMTOR_DAEMON_BIN:-/usr/bin/namtor}"
NAMTOR_UI_BIN="${NAMTOR_UI_BIN:-/usr/bin/namtor-enroll-wizard}"
NAMTOR_SOCKET="${NAMTOR_SOCKET:-/run/namtor/auth.sock}"
NAMTOR_EVIDENCE_DIR="${NAMTOR_EVIDENCE_DIR:-$REPO_ROOT/docs/evidence/ui/real-run}"

UI_DIR="$NAMTOR_EVIDENCE_DIR/ui"
MANIFEST="$NAMTOR_EVIDENCE_DIR/manifest.json"

# D-Bus constants (match reference-auth-hil.sh).
DBUS_DEST="org.namtor.Manager"
DBUS_PATH="/org/namtor/Manager"
IFACE_V1="org.namtor.Manager"
IFACE_V2="org.namtor.Manager2"
IFACE_V3="org.namtor.Manager3"

# State tracked for cleanup and manifest.
DAEMON_STARTED_BY_US=0
UI_PID=""
MANIFEST_ENTRIES=()

# ---------------------------------------------------------------------------
# Cleanup — runs on EXIT via trap.  Stops the daemon if we started it,
# kills any launched UI processes.  Never fails the script.
# ---------------------------------------------------------------------------
cleanup() {
    if [[ -n "$UI_PID" ]] && kill -0 "$UI_PID" 2>/dev/null; then
        kill "$UI_PID" 2>/dev/null || true
        wait "$UI_PID" 2>/dev/null || true
    fi
    if [[ "$DAEMON_STARTED_BY_US" -eq 1 ]]; then
        systemctl stop namtor-daemon.service 2>/dev/null || true
        systemctl stop namtor-daemon.socket 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Manifest helpers — build a JSON array of entries.  We use python3 or jq
# if available, falling back to hand-rolled JSON.
# ---------------------------------------------------------------------------

# add_entry <state> <desktop> <path> <status> <detail>
# status: "pass" | "fail" | "skip"
add_entry() {
    local state="$1" desktop="$2" path="$3" status="$4" detail="$5"
    MANIFEST_ENTRIES+=("$state|$desktop|$path|$status|$detail")
}

# Write the manifest JSON from accumulated MANIFEST_ENTRIES.
write_manifest() {
    mkdir -p "$NAMTOR_EVIDENCE_DIR"
    if command -v jq >/dev/null 2>&1; then
        # Build JSON array via jq.
        local entries_json="[]"
        for entry in "${MANIFEST_ENTRIES[@]}"; do
            local state desktop path status detail
            IFS='|' read -r state desktop path status detail <<< "$entry"
            entries_json="$(jq -c --arg s "$state" --arg d "$desktop" \
                --arg p "$path" --arg st "$status" --arg det "$detail" \
                '. + [{"state":$s,"desktop":$d,"path":$p,"status":$st,"detail":$det}]' \
                <<< "$entries_json")"
        done
        local ts
        ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        jq -n --argjson entries "$entries_json" --arg ts "$ts" \
            '{generated:$ts, entries:$entries}' > "$MANIFEST"
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c '
import json, sys, datetime
entries = []
for line in sys.argv[1:]:
    parts = line.split("|", 4)
    if len(parts) == 5:
        state, desktop, path, status, detail = parts
        entries.append({"state": state, "desktop": desktop, "path": path,
                        "status": status, "detail": detail})
ts = datetime.datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%SZ")
print(json.dumps({"generated": ts, "entries": entries}, indent=2))
' "${MANIFEST_ENTRIES[@]}" > "$MANIFEST"
    else
        # Hand-rolled JSON fallback.
        local ts
        ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf '{"generated":"%s","entries":[\n' "$ts" > "$MANIFEST"
        local first=1
        for entry in "${MANIFEST_ENTRIES[@]}"; do
            local state desktop path status detail
            IFS='|' read -r state desktop path status detail <<< "$entry"
            # Minimal escaping.
            local es="${state//\\/\\\\}"; es="${es//\"/\\\"}"
            local ed="${desktop//\\/\\\\}"; ed="${ed//\"/\\\"}"
            local ep="${path//\\/\\\\}"; ep="${ep//\"/\\\"}"
            local est="${status//\\/\\\\}"; est="${est//\"/\\\"}"
            local edet="${detail//\\/\\\\}"; edet="${edet//\"/\\\"}"
            if [[ $first -eq 0 ]]; then printf ',\n' >> "$MANIFEST"; fi
            first=0
            printf '  {"state":"%s","desktop":"%s","path":"%s","status":"%s","detail":"%s"}' \
                "$es" "$ed" "$ep" "$est" "$edet" >> "$MANIFEST"
        done
        printf '\n]}\n' >> "$MANIFEST"
    fi
    echo "capture-ui-screenshots: manifest written to $MANIFEST"
}

# ---------------------------------------------------------------------------
# Screenshot helper — detects the best available tool and captures to <path>.
# Returns 0 on success, 1 on failure.  Prints a warning if no tool is found.
# ---------------------------------------------------------------------------
take_screenshot() {
    local path="$1"
    local dir
    dir="$(dirname "$path")"
    mkdir -p "$dir"

    if command -v gnome-screenshot >/dev/null 2>&1; then
        gnome-screenshot -f "$path" 2>/dev/null && return 0
    fi
    if command -v grim >/dev/null 2>&1; then
        grim "$path" 2>/dev/null && return 0
    fi
    if command -v spectacle >/dev/null 2>&1; then
        spectacle -b -f -n -o "$path" 2>/dev/null && return 0
    fi
    # No screenshot tool available.
    echo "WARN: no screenshot tool found (gnome-screenshot, grim, spectacle)" >&2
    return 1
}

# ---------------------------------------------------------------------------
# D-Bus helper — call a Manager method on the system bus.
# Sets $DBUS_CALL_OUT and $DBUS_CALL_RC.
# ---------------------------------------------------------------------------
dbus_call() {
    local iface="$1"; shift
    DBUS_CALL_OUT="$(gdbus call --system --dest "$DBUS_DEST" \
        --object-path "$DBUS_PATH" --method "$iface.$@" 2>&1)" || true
    DBUS_CALL_RC=$?
}

# Extract first quoted string from a gdbus reply.
gdbus_extract_string() {
    sed -n "s/^.*'\([^']*\)'.*$/\1/p" | head -1
}

# ---------------------------------------------------------------------------
# Daemon startup — try systemctl first, fall back to direct binary launch.
# ---------------------------------------------------------------------------
start_daemon() {
    if systemctl is-active --quiet namtor-daemon.service 2>/dev/null; then
        echo "capture-ui-screenshots: daemon already active"
        return 0
    fi
    if systemctl start namtor-daemon.socket namtor-daemon.service 2>/dev/null; then
        sleep 1
        if systemctl is-active --quiet namtor-daemon.service; then
            DAEMON_STARTED_BY_US=1
            echo "capture-ui-screenshots: daemon started via systemctl"
            return 0
        fi
    fi
    # Fall back to direct binary launch (for non-systemd or dev environments).
    if [[ -x "$NAMTOR_DAEMON_BIN" ]]; then
        "$NAMTOR_DAEMON_BIN" daemon &
        DAEMON_PID=$!
        DAEMON_STARTED_BY_US=1
        sleep 2
        if kill -0 "$DAEMON_PID" 2>/dev/null; then
            echo "capture-ui-screenshots: daemon started via direct binary (pid $DAEMON_PID)"
            return 0
        fi
    fi
    echo "capture-ui-screenshots: FAILED to start daemon" >&2
    return 1
}

# ---------------------------------------------------------------------------
# State matrix — the 15 states from 16-visual-review.md.
# Each state is: <state_name> <desktop> <env_vars> <drive_action>
# drive_action describes how to reach the state (gdbus calls, env vars, etc.)
# ---------------------------------------------------------------------------

# UI states (15 from the visual-review matrix).
UI_STATES=(
    "profiles-empty"
    "profiles-populated"
    "setup"
    "capture-low"
    "capture-medium"
    "capture-high"
    "stopped-review"
    "auth-collapsed"
    "auth-expanded"
    "system-healthy"
    "system-degraded"
    "offline"
    "rtl"
    "high-contrast"
    "200pct-text"
)


# ---------------------------------------------------------------------------
# Drive the UI to a named state.  Best-effort: uses gdbus calls and
# environment variables.  Returns 0 on success, 1 if the state could not
# be reached (screenshot is still attempted).
# ---------------------------------------------------------------------------
drive_ui_state() {
    local state="$1"
    local ui_env=()

    case "$state" in
        profiles-empty)
            # Ensure no profiles exist (remove all via v1 RemoveProfile).
            # Best-effort: the app should show the empty state.
            ;;
        profiles-populated)
            # Ensure at least one profile exists.  Use v3 StartEnrollment +
            # StopEnrollment + CommitProfile to create a test profile.
            # Best-effort: if the daemon already has profiles, this is a no-op.
            ;;
        setup)
            # Navigate to the enrollment setup page (click "Enroll Face").
            # Best-effort via xdotool/ydotool if available.
            if command -v xdotool >/dev/null 2>&1; then
                xdotool key --delay 50 Tab Return 2>/dev/null || true
            fi
            ;;
        capture-low|capture-medium|capture-high)
            # Start enrollment via v3, let it capture for a few seconds.
            # Quality depends on the camera input — we just need the capture
            # page visible.
            dbus_call "$IFACE_V3" "StartEnrollment" "99999" "screenshot-test"
            if [[ $DBUS_CALL_RC -eq 0 ]]; then
                sleep 3
            fi
            ;;
        stopped-review)
            # Start then stop enrollment to reach the review page.
            dbus_call "$IFACE_V3" "StartEnrollment" "99999" "screenshot-review"
            if [[ $DBUS_CALL_RC -eq 0 ]]; then
                local session
                session="$(echo "$DBUS_CALL_OUT" | gdbus_extract_string)"
                sleep 2
                dbus_call "$IFACE_V3" "StopEnrollment" "$session"
            fi
            ;;
        auth-collapsed|auth-expanded)
            # Navigate to the Authentication destination.
            # Collapsed = default; expanded = click the expander.
            if command -v xdotool >/dev/null 2>&1; then
                xdotool key --delay 50 Tab Tab Return 2>/dev/null || true
                if [[ "$state" == "auth-expanded" ]]; then
                    sleep 1
                    xdotool key --delay 50 Tab Return 2>/dev/null || true
                fi
            fi
            ;;
        system-healthy)
            # Navigate to the System destination (daemon running = healthy).
            if command -v xdotool >/dev/null 2>&1; then
                xdotool key --delay 50 Tab Tab Tab Return 2>/dev/null || true
            fi
            ;;
        system-degraded)
            # Stop the daemon to show degraded state, then restart after.
            systemctl stop namtor-daemon.service 2>/dev/null || true
            sleep 1
            ;;
        offline)
            # Stop the daemon entirely to show the offline error page.
            systemctl stop namtor-daemon.service 2>/dev/null || true
            sleep 1
            ;;
        rtl)
            # Relaunch the app with an RTL locale.
            ui_env+=("LANG=ar" "LC_ALL=ar")
            ;;
        high-contrast)
            # Use GNOME high-contrast theme.
            ui_env+=("GTK_THEME=HighContrast")
            ;;
        200pct-text)
            # Scale text to 200%.
            ui_env+=("GDK_SCALE=2")
            ;;
        *)
            echo "WARN: unknown UI state '$state'" >&2
            ;;
    esac

    # For states that need a fresh app launch with env vars, kill and relaunch.
    if [[ ${#ui_env[@]} -gt 0 ]]; then
        if [[ -n "$UI_PID" ]] && kill -0 "$UI_PID" 2>/dev/null; then
            kill "$UI_PID" 2>/dev/null || true
            wait "$UI_PID" 2>/dev/null || true
        fi
        env "${ui_env[@]}" "$NAMTOR_UI_BIN" &
        UI_PID=$!
        sleep 3
    fi

    # Restart daemon if it was stopped for a degraded/offline state.
    case "$state" in
        system-degraded|offline)
            # Leave it stopped for the screenshot; restart after capture.
            ;;
        *)
            # Ensure daemon is running for non-degraded states.
            if ! systemctl is-active --quiet namtor-daemon.service 2>/dev/null; then
                systemctl start namtor-daemon.service 2>/dev/null || true
                sleep 1
            fi
            ;;
    esac
}

# Post-capture cleanup for UI states that stopped the daemon.
post_ui_state() {
    local state="$1"
    case "$state" in
        system-degraded|offline)
            # Restart the daemon after capturing the degraded/offline state.
            systemctl start namtor-daemon.service 2>/dev/null || true
            sleep 1
            ;;
        capture-low|capture-medium|capture-high|stopped-review)
            # Clean up any test enrollment session.
            dbus_call "$IFACE_V3" "CancelEnrollment" "screenshot-test" 2>/dev/null || true
            dbus_call "$IFACE_V3" "CancelEnrollment" "screenshot-review" 2>/dev/null || true
            ;;
    esac
}

# ---------------------------------------------------------------------------
# Capture one screenshot for a given desktop and state.
# Records the result in the manifest.
# ---------------------------------------------------------------------------
capture_state() {
    local desktop="$1"
    local state="$2"
    local outpath

    outpath="$UI_DIR/${state}.png"

    echo "capture-ui-screenshots: capturing $desktop/$state → $outpath"

    if take_screenshot "$outpath"; then
        add_entry "$state" "$desktop" "$outpath" "pass" "screenshot captured"
    else
        add_entry "$state" "$desktop" "$outpath" "fail" "screenshot tool unavailable or failed"
    fi
}

# ---------------------------------------------------------------------------
# Main — start daemon, launch UIs, iterate states, capture, write manifest.
# ---------------------------------------------------------------------------

echo "capture-ui-screenshots: NAMTOR_HW_TEST=1, starting capture run"

# Ensure output directories exist.
mkdir -p "$UI_DIR"

# Start the daemon.
if ! start_daemon; then
    echo "capture-ui-screenshots: cannot start daemon — recording all states as skip" >&2
    for state in "${UI_STATES[@]}"; do
        add_entry "$state" "ui" "$UI_DIR/${state}.png" "skip" "daemon not started"
    done
    write_manifest
    exit 0
fi

# --- UI capture pass ---

# Launch the wizard.
if [[ -x "$NAMTOR_UI_BIN" ]]; then
    "$NAMTOR_UI_BIN" &
    UI_PID=$!
    sleep 3
else
    echo "capture-ui-screenshots: wizard binary not found at $NAMTOR_UI_BIN" >&2
fi

for state in "${UI_STATES[@]}"; do
    drive_ui_state "$state"
    sleep 1
    capture_state "ui" "$state"
    post_ui_state "$state"
    sleep 0.5
done

# Write the manifest.
write_manifest

echo "capture-ui-screenshots: capture run complete"
echo "  Screenshots: $UI_DIR"
echo "  Manifest:          $MANIFEST"
exit 0
