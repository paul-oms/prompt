#!/usr/bin/env python3
"""Neon Core: the 3D particle tunnel used in the README demo.

Run it in any truecolour terminal:

    python3 scripts/neon.py

Pass ``--seconds N`` to stop automatically (used by ``scripts/demo.sh``).
Press Ctrl+C to stop it early.
"""

import argparse
import fcntl
import math
import os
import random
import struct
import sys
import termios
import time

# Fallback when the terminal does not report pixel dimensions. Monospace cells
# are roughly twice as tall as they are wide.
DEFAULT_CELL_ASPECT = 2.0


def rgb(r, g, b):
    return f"\033[38;2;{int(r) % 256};{int(g) % 256};{int(b) % 256}m"


def terminal_size():
    """Current terminal size in cells, re-read every frame.

    Terminals resize their PTY when a window or split changes, but a program
    that only asks once keeps drawing the old grid and its output wraps.
    """
    try:
        size = os.get_terminal_size(sys.stdout.fileno())
        return size.columns, size.lines
    except OSError:
        return 80, 24


def cell_aspect():
    """Cell height divided by cell width, from the terminal's pixel size.

    Prompt reports the PTY's pixel dimensions, so this stays correct when the
    font, scale or split geometry changes. Returns a 2:1 fallback when the
    terminal does not expose pixel sizes.
    """
    try:
        packed = fcntl.ioctl(sys.stdout.fileno(), termios.TIOCGWINSZ, b"\0" * 8)
        rows, cols, xpixel, ypixel = struct.unpack("HHHH", packed)
        if rows and cols and xpixel and ypixel:
            return (ypixel / rows) / (xpixel / cols)
    except (OSError, struct.error):
        pass
    return DEFAULT_CELL_ASPECT


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--seconds",
        type=float,
        default=0.0,
        help="stop after N seconds (0 = run until interrupted)",
    )
    args = parser.parse_args()

    width, height = terminal_size()
    width = max(1, width)
    height = max(1, height - 1)
    aspect = cell_aspect()
    particle_count = 2500
    particles = [
        [
            random.uniform(-1, 1),
            random.uniform(-1, 1),
            random.uniform(1, 15),
            random.uniform(0.5, 2),
        ]
        for _ in range(particle_count)
    ]

    def project(x, y, z, width, height, aspect):
        f = height * 0.9 / (z + 0.1)
        return int(width / 2 + x * f * aspect), int(height / 2 + y * f)

    start = time.monotonic()
    frames = 0
    out = sys.stdout
    out.write("\033[2J\033[?25l")
    out.flush()
    try:
        while True:
            frame_start = time.monotonic()
            t = frame_start - start
            if args.seconds and t >= args.seconds:
                break

            # A split or window resize changes the PTY's grid underneath us.
            # Follow it instead of drawing the old width into the new pane,
            # which would wrap every line and scroll the frame.
            new_width, new_height = terminal_size()
            new_width = max(1, new_width)
            new_height = max(1, new_height - 1)
            if (new_width, new_height) != (width, height):
                width, height = new_width, new_height
                aspect = cell_aspect()
                # Clear the reflowed/scrolled remains of the old frame.
                out.write("\033[2J\033[H")
            else:
                out.write("\033[H")

            screen = [[" "] * width for _ in range(height)]
            colors = [[None] * width for _ in range(height)]

            # 3D particle tunnel
            for p in particles:
                p[2] -= 0.14 * p[3]
                if p[2] < 0.2:
                    p[:] = [
                        random.uniform(-1, 1),
                        random.uniform(-1, 1),
                        15,
                        random.uniform(0.5, 2),
                    ]
                x, y = project(p[0], p[1], p[2], width, height, aspect)
                if 0 <= x < width and 0 <= y < height:
                    screen[y][x] = random.choice("✦◆◇*+×")
                    v = 1 - p[2] / 15
                    colors[y][x] = (255 * v, 60 + 180 * v, 255)

            # Rotating 3D wireframe cube
            points = []
            for x in [-1, 1]:
                for y in [-1, 1]:
                    for z in [-1, 1]:
                        a = t * 0.8
                        b = t * 0.6
                        rx = x * math.cos(a) - z * math.sin(a)
                        rz = x * math.sin(a) + z * math.cos(a)
                        ry = y * math.cos(b) - rz * math.sin(b)
                        rz = y * math.sin(b) + rz * math.cos(b)
                        points.append(project(rx, ry, rz + 3, width, height, aspect))

            edges = [
                (0, 1),
                (0, 2),
                (0, 4),
                (1, 3),
                (1, 5),
                (2, 3),
                (2, 6),
                (3, 7),
                (4, 5),
                (4, 6),
                (5, 7),
                (6, 7),
            ]
            for i, j in edges:
                x1, y1 = points[i]
                x2, y2 = points[j]
                steps = max(abs(x2 - x1), abs(y2 - y1), 1)
                for k in range(steps + 1):
                    x = int(x1 + (x2 - x1) * k / steps)
                    y = int(y1 + (y2 - y1) * k / steps)
                    if 0 <= x < width and 0 <= y < height:
                        screen[y][x] = "█"
                        colors[y][x] = (0, 255, 200)

            # Plasma overlay
            for x in range(0, width, 3):
                y = int(
                    height / 2
                    + math.sin(x * 0.08 + t * 2) * height * 0.2
                    + math.sin(x * 0.025 - t) * height * 0.12
                )
                if 0 <= y < height:
                    screen[y][x] = "▓"
                    colors[y][x] = (
                        255,
                        30 + int(100 * math.sin(t + x * 0.03) ** 2),
                        220,
                    )

            # Render
            lines = []
            for y in range(height):
                for x in range(width):
                    color = colors[y][x]
                    if color:
                        lines.append(rgb(*color) + screen[y][x])
                    else:
                        lines.append(" ")
                lines.append("\033[0m\n")

            frames += 1
            elapsed = time.monotonic() - start
            fps = frames / max(elapsed, 0.001)
            status = (
                f"  NEON CORE // FPS: {fps:.1f} // "
                f"PARTICLES: {particle_count} // TIME: {elapsed:.1f}s"
            )
            # Keep the status line on one row; a wrapped status line would
            # scroll the whole frame in a narrow split pane.
            if len(status) > width:
                status = f" NEON {fps:.0f}fps {particle_count}p {elapsed:.0f}s"
            lines.append(f"\033[1;97m{status[:width]}\033[0m")
            out.write("".join(lines))
            out.flush()

            delay = 1 / 60 - (time.monotonic() - frame_start)
            if delay > 0:
                time.sleep(delay)
    except (KeyboardInterrupt, BrokenPipeError):
        pass
    finally:
        out.write("\033[?25h\033[0m\n")
        out.flush()


if __name__ == "__main__":
    main()
