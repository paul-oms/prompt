#!/usr/bin/env bash
# Re-record the README demo GIF.
#
# This drives the real Prompt UI under Xvfb, captures it with ffmpeg, adds the
# title strip, and encodes the result as docs/demo.gif. See ../SKILL.md for the
# flow and tuning notes.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$script_dir/../../../.." && pwd)
title="$script_dir/../references/title.png"
out=${DEMO_OUT:-"$repo/docs/demo.gif"}
display=${DEMO_DISPLAY:-:98}
tmp=$(mktemp -d /tmp/prompt-demo.XXXXXX)
app_pid=
xvfb_pid=
ff_pid=

cleanup() {
    if [ -n "$ff_pid" ]; then
        kill "$ff_pid" 2>/dev/null || true
        wait "$ff_pid" 2>/dev/null || true
    fi
    if [ -n "$app_pid" ]; then
        kill "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
    if [ -n "$xvfb_pid" ]; then
        kill "$xvfb_pid" 2>/dev/null || true
        wait "$xvfb_pid" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT

for tool in ffmpeg xdotool Xvfb python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "missing required tool: $tool" >&2
        exit 1
    fi
done

# The demo uses the real on-device model so the generation is genuine. Reuse a
# cached copy when possible; otherwise fetch it once into target/.
model=${DEMO_MODEL:-}
if [ -z "$model" ]; then
    for candidate in \
        "$repo/target/demo-model.gguf" \
        "$HOME/.local/share/prompt/models/qwen2.5-coder-0.5b-instruct-q4_k_m.gguf"; do
        if [ -f "$candidate" ]; then
            model=$candidate
            break
        fi
    done
fi
if [ -z "$model" ]; then
    if ! command -v curl >/dev/null 2>&1; then
        echo "missing required tool: curl (needed for the demo model)" >&2
        exit 1
    fi
    model="$repo/target/demo-model.gguf"
    mkdir -p "$(dirname "$model")"
    echo "downloading the demo model to $model" >&2
    curl -L --fail --retry 3 -o "$model" \
        "https://huggingface.co/Qwen/Qwen2.5-Coder-0.5B-Instruct-GGUF/resolve/main/qwen2.5-coder-0.5b-instruct-q4_k_m.gguf"
fi
if [ ! -f "$model" ]; then
    echo "missing demo model: $model" >&2
    exit 1
fi
# The app runs from scripts/ so the generated command is relative to it.
model="$(cd "$(dirname "$model")" && pwd)/$(basename "$model")"
export PROMPT_MODEL_PATH="$model"
prompt=${DEMO_PROMPT:-"run neon.py"}
edit=${DEMO_EDIT:-" --seconds 30"}

if [ ! -x "$repo/target/release/prompt" ]; then
    echo "missing $repo/target/release/prompt; run: cargo build --release" >&2
    exit 1
fi
if [ ! -f "$repo/scripts/neon.py" ] || [ ! -f "$title" ]; then
    echo "missing scripts/neon.py or the title strip" >&2
    exit 1
fi
if [ -e "/tmp/.X${display#:}-lock" ]; then
    echo "display $display is already in use; set DEMO_DISPLAY=:97 (or another free display)" >&2
    exit 1
fi

Xvfb "$display" -screen 0 1080x734x24 -nolisten tcp >"$tmp/xvfb.log" 2>&1 &
xvfb_pid=$!
sleep 0.6

# Keep the developer's real HOME so the shell starts with its normal prompt and
# rc files, but isolate the app's history/data directories.
mkdir -p "$tmp/data" "$tmp/state" "$tmp/cache"
export HOME
export XDG_DATA_HOME="$tmp/data"
export XDG_STATE_HOME="$tmp/state"
export XDG_CACHE_HOME="$tmp/cache"
export DISPLAY="$display"
export GDK_BACKEND=x11
export PROMPT_RENDER=gpu
unset WAYLAND_DISPLAY WAYLAND_SOCKET XDG_SESSION_TYPE GDK_SCALE GDK_DPI_SCALE QT_SCALE_FACTOR

# Start the shell in scripts/ so "run neon.py" resolves to the demo script.
export PROMPT_CWD="$repo/scripts"
"$repo/target/release/prompt" >"$tmp/app.log" 2>&1 &
app_pid=$!

wid=
for _ in $(seq 1 100); do
    wid=$(xdotool search --class '^prompt$' 2>/dev/null | head -1 || true)
    [ -n "$wid" ] && break
    sleep 0.1
done
if [ -z "$wid" ]; then
    echo "Prompt window did not appear" >&2
    cat "$tmp/app.log" >&2
    exit 1
fi
for _ in $(seq 1 20); do
    grep -q "GPU renderer:" "$tmp/app.log" && break
    sleep 0.1
done
if ! grep -q "GPU renderer:" "$tmp/app.log"; then
    echo "warning: the app did not report a GPU renderer; the recording will use the software path" >&2
fi

xdotool windowfocus "$wid"
sleep 0.8

ffmpeg -y -loglevel error -f x11grab -draw_mouse 0 -framerate 20 \
    -video_size 1080x700 -i "$display.0+0,0" -t 17 \
    -c:v libx264 -preset ultrafast -crf 10 "$tmp/raw.mp4" &
ff_pid=$!

sleep 0.45
xdotool type --window "$wid" --delay 14 "$prompt"
sleep 0.35
# Ask the on-device model to turn the shorthand into a runnable command.
xdotool key --window "$wid" ctrl+g
sleep 3.0
# Review the result and add a runtime limit before running it.
xdotool type --window "$wid" --delay 14 "$edit"
sleep 0.35
xdotool key --window "$wid" Return
sleep 3.0
xdotool key --window "$wid" ctrl+shift+d
sleep 0.9
xdotool type --window "$wid" --delay 10 'while true; do date +%T; sleep 1; done'
sleep 0.3
xdotool key --window "$wid" Return
sleep 1.25
xdotool key --window "$wid" ctrl+shift+m
sleep 1.9
xdotool key --window "$wid" ctrl+shift+Left
sleep 0.5
xdotool key --window "$wid" ctrl+shift+b
sleep 2.4

wait "$ff_pid"
ff_pid=

ffmpeg -y -loglevel error -i "$tmp/raw.mp4" -i "$title" \
    -filter_complex "[0:v]pad=1080:734:0:34:color=0x11131a[bg];[bg][1:v]overlay=0:0" \
    -c:v libx264 -preset ultrafast -crf 10 -pix_fmt yuv420p "$tmp/titled.mp4"

ffmpeg -y -loglevel error -i "$tmp/titled.mp4" \
    -vf "fps=12,scale=900:-1:flags=area,split[s0][s1];[s0]palettegen=max_colors=64:stats_mode=diff[p];[s1][p]paletteuse=dither=none:diff_mode=rectangle" \
    -loop 0 "$tmp/demo.gif"

mkdir -p "$(dirname "$out")"
mv "$tmp/demo.gif" "$out"
echo "wrote $out ($(du -h "$out" | cut -f1))"
