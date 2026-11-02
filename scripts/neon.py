#!/usr/bin/env python3
"""Neon Core: the 3D particle tunnel used in the README demo.

Run it in any truecolour terminal:

    python3 scripts/neon.py

Pass ``--seconds N`` to stop automatically (used by ``scripts/demo.sh``).
Press Ctrl+C to stop it early.
"""

import argparse
import math
import random
import shutil
import sys
import time


def rgb(r, g, b):
    return f"\033[38;2;{int(r) % 256};{int(g) % 256};{int(b) % 256}m"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--seconds",
        type=float,
        default=0.0,
        help="stop after N seconds (0 = run until interrupted)",
    )
    args = parser.parse_args()

    width, height = shutil.get_terminal_size((80, 24))
    width = max(20, width)
    height = max(8, height - 1)
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

    def project(x, y, z):
        f = height * 0.9 / (z + 0.1)
        return int(width / 2 + x * f * 2), int(height / 2 + y * f)

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
                x, y = project(p[0], p[1], p[2])
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
                        points.append(project(rx, ry, rz + 3))

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
            lines = ["\033[H"]
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
            lines.append(
                f"\033[1;97m  NEON CORE // FPS: {fps:.1f} // "
                f"PARTICLES: {particle_count} // TIME: {elapsed:.1f}s\033[0m"
            )
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
