#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# M1 spike, risk R1: can the reference camera stream IR and RGB at once?
#
# Both sensors sit on one USB device sharing one USB 2.0 bus. If negotiation
# refuses the second stream, dual-camera fusion must fall back to alternating
# bursts (spec §4.8).
set -uo pipefail

IR_NODE="${IR_NODE:-/dev/video2}"
RGB_NODE="${RGB_NODE:-/dev/video0}"
FRAMES="${FRAMES:-60}"

command -v v4l2-ctl >/dev/null || { echo "v4l2-ctl not found (pkg: v4l-utils)"; exit 1; }

echo "=== R1: dual-stream bandwidth ==="
echo "IR:  $IR_NODE"
echo "RGB: $RGB_NODE"
echo

echo "--- baseline: IR alone ---"
v4l2-ctl -d "$IR_NODE" --set-fmt-video=width=640,height=360,pixelformat=GREY \
    --stream-mmap --stream-count="$FRAMES" --stream-to=/dev/null 2>&1 | tail -3
ir_alone=$?
echo "IR alone exit: $ir_alone"
echo

echo "--- baseline: RGB alone ---"
v4l2-ctl -d "$RGB_NODE" --set-fmt-video=width=1280,height=720,pixelformat=MJPG \
    --stream-mmap --stream-count="$FRAMES" --stream-to=/dev/null 2>&1 | tail -3
rgb_alone=$?
echo "RGB alone exit: $rgb_alone"
echo

echo "--- simultaneous ---"
v4l2-ctl -d "$IR_NODE" --set-fmt-video=width=640,height=360,pixelformat=GREY \
    --stream-mmap --stream-count="$FRAMES" --stream-to=/dev/null >/tmp/namtor-ir.log 2>&1 &
ir_pid=$!

sleep 0.3

v4l2-ctl -d "$RGB_NODE" --set-fmt-video=width=1280,height=720,pixelformat=MJPG \
    --stream-mmap --stream-count="$FRAMES" --stream-to=/dev/null >/tmp/namtor-rgb.log 2>&1 &
rgb_pid=$!

wait "$ir_pid"; ir_both=$?
wait "$rgb_pid"; rgb_both=$?

echo "IR  concurrent exit: $ir_both"
sed 's/^/    ir:  /' /tmp/namtor-ir.log | tail -5
echo "RGB concurrent exit: $rgb_both"
sed 's/^/    rgb: /' /tmp/namtor-rgb.log | tail -5
echo

echo "=== VERDICT ==="
if [ "$ir_both" -eq 0 ] && [ "$rgb_both" -eq 0 ]; then
    echo "R1-PASS: simultaneous IR + RGB capture succeeded."
    echo "Dual-camera fusion (spec §6.5) can use concurrent streams."
else
    echo "R1-FAIL: concurrent capture refused."
    echo "Fusion must use alternating bursts (spec §4.8 fallback)."
    echo "Check logs above for ENOSPC / 'Cannot allocate memory' — the usual"
    echo "signature of UVC bandwidth negotiation refusing the second stream."
fi
