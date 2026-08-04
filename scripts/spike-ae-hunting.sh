#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# M1 spike, risk R2: does firmware auto-exposure hunt against the strobe?
#
# Captures a run of IR frames while the emitter is held on, then held off,
# and reports per-frame mean luminance. A stable emitter-on plateau means AE
# is not fighting us. A decaying ramp after the emitter turns on is AE hunting.
#
# Requires R3-PASS: emitter unit/selector/payloads from `namtor-spike emitter`.
set -uo pipefail

IR_NODE="${IR_NODE:-/dev/video2}"
FRAMES="${FRAMES:-90}"
OUT="${OUT:-/tmp/namtor-ae}"

command -v v4l2-ctl >/dev/null || { echo "v4l2-ctl not found (pkg: v4l-utils)"; exit 1; }
command -v python3 >/dev/null || { echo "python3 not found"; exit 1; }

mkdir -p "$OUT"

echo "=== R2: auto-exposure behaviour ==="
echo "Capturing $FRAMES IR frames to $OUT"
echo
echo "Hold the emitter ON using the unit/selector recorded by:"
echo "  namtor-spike emitter --device $IR_NODE --descriptors <sysfs descriptors>"
echo "then press Enter to capture."
read -r _

v4l2-ctl -d "$IR_NODE" \
    --set-fmt-video=width=640,height=360,pixelformat=GREY \
    --stream-mmap --stream-count="$FRAMES" \
    --stream-to="$OUT/on.raw"

echo "Now turn the emitter OFF and press Enter."
read -r _

v4l2-ctl -d "$IR_NODE" \
    --set-fmt-video=width=640,height=360,pixelformat=GREY \
    --stream-mmap --stream-count="$FRAMES" \
    --stream-to="$OUT/off.raw"

python3 - "$OUT" "$FRAMES" <<'PY'
import sys, pathlib

out = pathlib.Path(sys.argv[1])
n = int(sys.argv[2])
size = 640 * 360

def means(path):
    data = path.read_bytes()
    frames = len(data) // size
    return [
        sum(data[i * size:(i + 1) * size]) / size
        for i in range(min(frames, n))
    ]

on = means(out / "on.raw")
off = means(out / "off.raw")

print(f"\nemitter ON  frames: {len(on)}")
print("  first 10:", " ".join(f"{m:.1f}" for m in on[:10]))
print("  last 10: ", " ".join(f"{m:.1f}" for m in on[-10:]))
print(f"emitter OFF frames: {len(off)}")
print("  first 10:", " ".join(f"{m:.1f}" for m in off[:10]))
print("  last 10: ", " ".join(f"{m:.1f}" for m in off[-10:]))

if not on or not off:
    print("\nNo frames captured; cannot judge R2.")
    sys.exit(1)

# AE hunting shows up as the emitter-on mean drifting back toward the
# emitter-off mean as the firmware compensates.
on_early = sum(on[:10]) / len(on[:10])
on_late = sum(on[-10:]) / len(on[-10:])
off_mean = sum(off) / len(off)

drift = on_early - on_late
separation = on_late - off_mean

print(f"\non_early={on_early:.1f} on_late={on_late:.1f} off_mean={off_mean:.1f}")
print(f"drift={drift:.1f}  separation_at_end={separation:.1f}")

print("\n=== VERDICT ===")
if drift > 15.0:
    print("R2-FAIL: emitter-on luminance decays; firmware AE is hunting.")
    print("Mitigations, in order (spec §4.5):")
    print("  1. find an exposure control via MSXU or vendor XU and pin it")
    print("  2. shorten strobe periods below AE's tracking rate")
    print("  3. normalise the differential in software, accepting noise")
elif separation < 10.0:
    print("R2-INCONCLUSIVE: emitter-on and emitter-off are barely separable.")
    print("Re-check that the emitter was actually held on.")
else:
    print("R2-PASS: emitter-on luminance is stable and well separated.")
    print("Strobe differential is viable as designed.")
PY
