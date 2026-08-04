#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# M1 spike, risk R9: is the emitter energised by streaming alone, and does the
# firmware strobe it? Must run BEFORE any emitter probing, because both cases
# change what probing means (spec §4.10.5).
set -uo pipefail

IR_NODE="${IR_NODE:-/dev/video2}"
FRAMES="${FRAMES:-60}"
WIDTH="${WIDTH:-640}"
HEIGHT="${HEIGHT:-360}"
RAW="${RAW:-/tmp/namtor-idle.raw}"

command -v v4l2-ctl >/dev/null || { echo "v4l2-ctl not found (pkg: v4l-utils)"; exit 1; }
command -v python3 >/dev/null || { echo "python3 not found"; exit 1; }

echo "=== R9: emitter idle baseline ==="
echo "Capturing $FRAMES frames from $IR_NODE with no emitter control applied."
echo "Leave the scene normally lit; do not cover the camera."
echo

v4l2-ctl -d "$IR_NODE" \
    --set-fmt-video=width="$WIDTH",height="$HEIGHT",pixelformat=GREY \
    --stream-mmap --stream-count="$FRAMES" --stream-to="$RAW" 2>&1 | tail -2

python3 - "$RAW" "$WIDTH" "$HEIGHT" <<'PY'
import sys, pathlib

raw = pathlib.Path(sys.argv[1])
w, h = int(sys.argv[2]), int(sys.argv[3])
size = w * h

data = raw.read_bytes()
n = len(data) // size
if n == 0:
    print("no frames captured")
    sys.exit(1)

means = [sum(data[i * size:(i + 1) * size]) / size for i in range(n)]
even = [m for i, m in enumerate(means) if i % 2 == 0]
odd = [m for i, m in enumerate(means) if i % 2 == 1]

even_m = sum(even) / len(even)
odd_m = sum(odd) / len(odd)
alt = abs(even_m - odd_m)
overall = sum(means) / len(means)
ramp = means[-1] - means[0]

print(f"\nframes={n}")
print("means:", " ".join(f"{m:.1f}" for m in means))
print(f"\neven-idx mean={even_m:.2f}  odd-idx mean={odd_m:.2f}  alternation={alt:.2f}")
print(f"overall mean={overall:.2f}  min={min(means):.1f}  max={max(means):.1f}")
print(f"first-to-last ramp={ramp:+.2f}")

print("\n=== VERDICT ===")
if alt > 15.0:
    print("firmware_strobed: frames alternate in brightness.")
    print("The device strobes the emitter itself. Pair frames from metadata;")
    print("no emitter control may be needed at all (spec §4.10.5).")
elif overall > 30.0:
    print("always_on (PROVISIONAL): the image is bright with no control applied.")
    print("Strobe differential would be impossible unless an off control exists.")
    print("")
    print("CONFIRM BEFORE RECORDING THIS. Strong ambient infrared -- direct")
    print("sunlight, halogen or incandescent lamps, a space heater -- produces")
    print("the same signature. Re-run in a darkened room with no window and no")
    print("incandescent source. If the image goes dark, the verdict is")
    print("idle_dark and the brightness was ambient, not the emitter.")
else:
    print("idle_dark: the emitter is not energised by streaming.")
    print("Emitter control is REQUIRED; the sensor is unusable without it.")
    print("Emitter control is mandatory. On the reference camera this is\n"
          "supplied by MSXU mode 2 (Tier 0) -- see quirks/devices/3277-0018.toml.")

if abs(ramp) > 0.2 and overall < 5.0:
    print(f"\nNote: a slow monotonic ramp ({ramp:+.2f} over {n} frames) is")
    print("auto-exposure searching for light. Slow AE is favourable for R2:")
    print("it cannot track a strobe toggling in tens of milliseconds.")
PY
