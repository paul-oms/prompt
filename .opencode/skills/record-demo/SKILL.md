---
name: Record Prompt demo
description: Re-record docs/demo.gif for Prompt with the Neon Core particle tunnel and the split/background/block-view flow. Use when the README demo needs updating, when asked how the demo is made, or when the demo recording breaks after a UI change.
---

# Record the Prompt demo

`docs/demo.gif` is a live recording of the real app, not a mockup or an
animation composited by hand. The flow is:

1. The welcome composer is visible.
2. Type `run neon.py` and press `Ctrl+G`. The on-device Qwen model expands it to
   `python /home/.../scripts/neon.py`.
3. Review it, append ` --seconds 30`, then run it.
4. The composer docks; the particle tunnel fills the pane.
5. Split right while the tunnel keeps running.
6. Run `while true; do date +%T; sleep 1; done` in the new pane and move it to
   the background panel.
7. Focus the left pane and open block view.
8. Encode the capture as a looping GIF.

The title strip is part of the recording. It is a static 1080x34 image
(`references/title.png`) laid over the top of the 1080x700 window capture.

## Automated recording

Build the release binary first, then run the recorder:

```sh
cargo build --release
.opencode/skills/record-demo/scripts/record-demo.sh
```

It writes `docs/demo.gif` by default. Useful overrides:

```sh
DEMO_DISPLAY=:97 .opencode/skills/record-demo/scripts/record-demo.sh
DEMO_OUT=/tmp/demo.gif .opencode/skills/record-demo/scripts/record-demo.sh
```

The recorder requires `Xvfb`, `xdotool`, `ffmpeg`, `python3`, and the release
binary. It also needs the Qwen demo model: it reuses
`target/demo-model.gguf` or the normal Prompt model cache, and downloads it to
`target/demo-model.gguf` if neither exists. Override it with `DEMO_MODEL=...`.
The recorder sets `PROMPT_CWD` to `scripts/`, so the generated
`python .../neon.py` command resolves correctly while the shell still sees the
normal home directory. It starts a private Xvfb at 1080x734, so the window lands
at 0,0 with no window manager. If the display is already in use, pick another
one.

## What the recorder does

- Uses the real `HOME` so the shell sources its normal rc files and the Prompt
  prompt looks right, but points `XDG_DATA_HOME`, `XDG_STATE_HOME` and
  `XDG_CACHE_HOME` at a temp directory so the recording does not pollute the
  developer's Prompt history.
- Forces X11 with `GDK_BACKEND=x11` and unsets `WAYLAND_DISPLAY`; otherwise
  winit connects to the desktop Wayland compositor instead of Xvfb.
- Runs with `PROMPT_RENDER=gpu`. On this machine Vulkan works under Xvfb; check
  the app log for `prompt: GPU renderer: ... (Vulkan, ...)`. If it falls back to
  software, do not describe the recording as a GPU capture.
- Captures the 1080x700 window with ffmpeg at 20fps, drives the UI with
  `xdotool`, then pads the capture to 1080x734 and overlays the title strip.
- Encodes at 900px wide, 12fps, 64 colours, `flags=area`, `dither=none`,
  `diff_mode=rectangle`. The particle field is dense; 1080px at 20fps produces
  a 7MB+ GIF, while these settings keep it around 2.7MB and still readable.

## Manual equivalent

If the recorder needs adjusting, these are the important commands.

Start an isolated display and app:

```sh
Xvfb :98 -screen 0 1080x734x24 -nolisten tcp &
mkdir -p /tmp/prompt-demo-data
HOME=$HOME \
XDG_DATA_HOME=/tmp/prompt-demo-data \
XDG_STATE_HOME=/tmp/prompt-demo-data/state \
XDG_CACHE_HOME=/tmp/prompt-demo-data/cache \
PROMPT_MODEL_PATH="$HOME/Source/prompt/target/demo-model.gguf" \
PROMPT_CWD="$HOME/Source/prompt/scripts" \
DISPLAY=:98 GDK_BACKEND=x11 PROMPT_RENDER=gpu \
  target/release/prompt
```

Find the window and capture it:

```sh
wid=$(DISPLAY=:98 xdotool search --class '^prompt$' | head -1)
DISPLAY=:98 xdotool windowfocus "$wid"
ffmpeg -y -f x11grab -draw_mouse 0 -framerate 20 \
  -video_size 1080x700 -i :98.0+0,0 -t 17 \
  -c:v libx264 -preset ultrafast -crf 10 /tmp/demo-raw.mp4
```

Drive the sequence (timings are in the recorder; keep the typing delays so the
composer animation is visible):

```sh
xdotool type --window "$wid" --delay 14 'run neon.py'
sleep 0.35
xdotool key --window "$wid" ctrl+g
sleep 3
xdotool type --window "$wid" --delay 14 ' --seconds 30'
sleep 0.35
xdotool key --window "$wid" Return
sleep 3
xdotool key --window "$wid" ctrl+shift+d
sleep 0.9
xdotool type --window "$wid" --delay 10 'while true; do date +%T; sleep 1; done'
xdotool key --window "$wid" Return
sleep 1.25
xdotool key --window "$wid" ctrl+shift+m
sleep 1.9
xdotool key --window "$wid" ctrl+shift+Left
sleep 0.5
xdotool key --window "$wid" ctrl+shift+b
sleep 2.4
```

Add the title strip and encode:

```sh
ffmpeg -y -i /tmp/demo-raw.mp4 -i .opencode/skills/record-demo/references/title.png \
  -filter_complex "[0:v]pad=1080:734:0:34:color=0x11131a[bg];[bg][1:v]overlay=0:0" \
  -c:v libx264 -preset ultrafast -crf 10 -pix_fmt yuv420p /tmp/demo-titled.mp4

ffmpeg -y -i /tmp/demo-titled.mp4 \
  -vf "fps=12,scale=900:-1:flags=area,split[s0][s1];[s0]palettegen=max_colors=64:stats_mode=diff[p];[s1][p]paletteuse=dither=none:diff_mode=rectangle" \
  -loop 0 docs/demo.gif
```

## Verify

- `ffprobe -v error -show_entries stream=width,height,nb_frames,r_frame_rate -of default=nw=1 docs/demo.gif`
  should report 900x612, 12fps, and a full frame count.
- Check the first frame (welcome), the AI-generated command and the edit, the
  split transition, the background panel, and the final block view.
- The tunnel status line should show about 60fps while it fills the pane.
- Keep the GIF under roughly 3MB. If it grows, lower the width or frame rate
  before raising the colour count.
- Do not commit the temp captures or the Xvfb logs.
