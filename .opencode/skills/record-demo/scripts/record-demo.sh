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
    -video_size 1080x700 -i "$display.0+0,0" -t 13 \
    -c:v libx264 -preset ultrafast -crf 10 "$tmp/raw.mp4" &
ff_pid=$!

cmd="python3 $repo/scripts/neon.py"
if [[ "$repo" == "$HOME/"* ]]; then
    cmd="python3 ~/${repo#"$HOME"/}/scripts/neon.py"
fi

sleep 0.45
xdotool type --window "$wid" --delay 14 "$cmd"
sleep 0.3
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
