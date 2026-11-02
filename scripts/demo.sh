#!/usr/bin/env bash
# GPU renderer stress sequence.
#
# Run it inside Prompt with the GPU renderer enabled:
#   PROMPT_RENDER=gpu PROMPT_AUTOTYPE="$PWD/scripts/demo.sh" ./target/release/prompt
#
# The README feature demo is recorded separately by driving the app through
# splits, background tabs and block view. This script is the quick render test.

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

# A truecolour animation: thousands of particles, a wireframe cube and a
# plasma band, with every cell repainted each frame. This is the workload the
# GPU renderer is for, and it is the same animation used in the README demo.
script_dir=$(cd "$(dirname "$0")" && pwd)
python3 "$script_dir/neon.py" --seconds 6.5

clear
printf '\n\n'
printf '  \033[1;38;2;122;162;247mGPU-accelerated rendering\033[0m\n\n'
printf '  \033[38;2;216;220;230mwgpu · Vulkan · instanced rectangles · glyph atlas\033[0m\n'
printf '  \033[38;2;139;146;164mSoftware fallback is automatic.\033[0m\n\n'
sleep 2.2
