#!/usr/bin/env bash
# Scripted sequence used to record docs/demo.gif.
#
# Run it inside Prompt with the GPU renderer enabled:
#   PROMPT_RENDER=gpu PROMPT_AUTOTYPE="$PWD/scripts/demo.sh" ./target/release/prompt
#
# It stays deliberately short and readable so the demo can be re-recorded when
# the UI changes.

set -u

clear
printf '\033[1;38;2;122;162;247m Prompt \033[0m  GPU renderer · wgpu / Vulkan\n'
printf '\033[38;2;139;146;164m Instanced rectangles and glyphs, painted on the GPU.\033[0m\n\n'
sleep 0.7

# Colour-heavy scrollback. Every line is a new frame for the grid renderer.
for i in $(seq 1 320); do
  printf '\033[38;5;%dm%4d\033[0m  glyph atlas · instanced quads · wgpu\n' \
    "$((17 + i % 200))" "$i"
  sleep 0.005
done
sleep 0.3
clear

# A truecolour animation: thousands of changing cells every frame. This is the
# workload the GPU renderer is for.
python3 - <<'PY'
import math
import shutil
import sys
import time

cols, rows = shutil.get_terminal_size((128, 30))
cols = max(20, cols)
rows = max(8, rows - 1)
out = sys.stdout
out.write("\033[?25l\033[2J")
start = time.monotonic()
try:
    while time.monotonic() - start < 6.5:
        t = time.monotonic() - start
        out.write("\033[H")
        for row in range(rows):
            y = row / rows
            line = []
            for col in range(cols):
                x = col / cols
                v = (math.sin(x * 9.0 + t * 2.0)
                     + math.sin(y * 7.0 - t * 1.5)
                     + math.sin((x + y) * 5.0 + t)) / 3.0
                r = int(92 + 92 * math.cos(6.28318 * (v * 0.5)))
                g = int(108 + 90 * math.cos(6.28318 * (v * 0.5 + 0.33)))
                b = int(158 + 78 * math.cos(6.28318 * (v * 0.5 + 0.66)))
                r = max(0, min(255, r))
                g = max(0, min(255, g))
                b = max(0, min(255, b))
                line.append(f"\033[48;2;{r};{g};{b}m ")
            out.write("".join(line) + "\033[0m\r\n")
        out.flush()
        time.sleep(0.05)
finally:
    out.write("\033[0m\033[?25h")
    out.flush()
PY

clear
printf '\n\n'
printf '  \033[1;38;2;122;162;247mGPU-accelerated rendering\033[0m\n\n'
printf '  \033[38;2;216;220;230mwgpu · Vulkan · instanced rectangles · glyph atlas\033[0m\n'
printf '  \033[38;2;139;146;164mSoftware fallback is automatic.\033[0m\n\n'
sleep 2.2
