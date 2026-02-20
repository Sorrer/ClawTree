#!/usr/bin/env python3
"""
Terminal screenshot capture tool for ClawTree.
Captures TUI app screens from tmux sessions and renders them to PNG images.

Approach: Uses tmux pipe-pane to capture raw PTY output during a forced redraw,
then replays through pyte (terminal emulator) to build the screen buffer,
and renders the result to a PNG with Pillow.
"""

import subprocess
import sys
import os
import time
import re
import tempfile
from pathlib import Path

import pyte
from PIL import Image, ImageDraw, ImageFont

# --- Configuration ---
FONT_PATH = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
FONT_BOLD_PATH = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"
FONT_SIZE = 14
PADDING = 16
SCALE = 2  # Render at 2x for retina-quality screenshots

# macOS-style window chrome
TITLEBAR_HEIGHT = 28
BUTTON_RADIUS = 6
BUTTON_SPACING = 20
BUTTON_START_X = 16

# Default dark theme colors
DEFAULT_BG = (30, 30, 46)    # Dark background (catppuccin-ish)
DEFAULT_FG = (205, 214, 244) # Light foreground
TITLEBAR_BG = (24, 24, 37)   # Slightly darker for titlebar

# ANSI color palette (standard 16 colors - catppuccin mocha)
COLORS_16 = {
    "black":         (69, 71, 90),
    "red":           (243, 139, 168),
    "green":         (166, 227, 161),
    "brown":         (249, 226, 175),
    "blue":          (137, 180, 250),
    "magenta":       (245, 194, 231),
    "cyan":          (148, 226, 213),
    "white":         (186, 194, 222),
    "brightblack":   (88, 91, 112),
    "brightred":     (243, 139, 168),
    "brightgreen":   (166, 227, 161),
    "brightyellow":  (249, 226, 175),
    "brightblue":    (137, 180, 250),
    "brightmagenta": (245, 194, 231),
    "brightcyan":    (148, 226, 213),
    "brightwhite":   (205, 214, 244),
    "default":       None,
}


def color_to_rgb(color_str, is_fg=True):
    """Convert a pyte color string to an RGB tuple."""
    if not color_str or color_str == "default":
        return DEFAULT_FG if is_fg else DEFAULT_BG

    # Direct hex color (6 hex digits)
    if isinstance(color_str, str) and len(color_str) == 6:
        try:
            r = int(color_str[0:2], 16)
            g = int(color_str[2:4], 16)
            b = int(color_str[4:6], 16)
            return (r, g, b)
        except ValueError:
            pass

    # Named color
    if color_str in COLORS_16:
        c = COLORS_16[color_str]
        if c is None:
            return DEFAULT_FG if is_fg else DEFAULT_BG
        return c

    # 256-color index
    try:
        idx = int(color_str)
        return index_to_rgb(idx)
    except (ValueError, TypeError):
        pass

    return DEFAULT_FG if is_fg else DEFAULT_BG


def index_to_rgb(idx):
    """Convert 256-color index to RGB."""
    if idx < 16:
        names = [
            "black", "red", "green", "brown", "blue", "magenta", "cyan", "white",
            "brightblack", "brightred", "brightgreen", "brightyellow",
            "brightblue", "brightmagenta", "brightcyan", "brightwhite",
        ]
        return COLORS_16.get(names[idx], DEFAULT_FG)

    if idx < 232:
        idx -= 16
        r = (idx // 36) % 6
        g = (idx // 6) % 6
        b = idx % 6
        return (
            0 if r == 0 else 55 + r * 40,
            0 if g == 0 else 55 + g * 40,
            0 if b == 0 else 55 + b * 40,
        )

    v = 8 + (idx - 232) * 10
    return (v, v, v)


def replay_typescript(filepath, cols=140, rows=40):
    """Replay a `script` typescript file through pyte to get screen state."""
    screen = pyte.Screen(cols, rows)
    stream = pyte.Stream(screen)

    with open(filepath, "rb") as f:
        data = f.read()

    # script adds a header line like "Script started on ..."
    # Find the first ESC or newline after header
    text = data.decode("utf-8", errors="replace")
    # Skip the first line (header)
    first_nl = text.find("\n")
    if first_nl >= 0:
        text = text[first_nl + 1:]

    # Also remove trailing "Script done on ..." line if present
    done_idx = text.rfind("\nScript done on")
    if done_idx >= 0:
        text = text[:done_idx]

    stream.feed(text)
    return screen


def capture_tmux_session(session_name, cols=140, rows=40, wait_secs=3):
    """
    Capture a tmux session's screen by:
    1. Starting pipe-pane to record raw PTY output
    2. Resizing the pane to force a full redraw
    3. Stopping pipe-pane
    4. Replaying through pyte
    """
    logfile = tempfile.mktemp(suffix=".log")

    try:
        # Ensure pipe is stopped
        subprocess.run(["tmux", "pipe-pane", "-t", session_name], check=False)

        # Clear and start pipe
        with open(logfile, "w") as f:
            f.truncate(0)
        subprocess.run(
            ["tmux", "pipe-pane", "-t", session_name, "-o", f"cat >> {logfile}"],
            check=True
        )

        # Force full redraw by resizing to different dims then back
        subprocess.run(["tmux", "resize-pane", "-t", session_name, "-x", str(cols - 10), "-y", str(rows - 5)], check=False)
        time.sleep(1)

        # Clear the log (we want just the final frame)
        with open(logfile, "w") as f:
            f.truncate(0)

        # Resize back to target
        subprocess.run(["tmux", "resize-pane", "-t", session_name, "-x", str(cols), "-y", str(rows)], check=True)
        time.sleep(wait_secs)

        # Stop pipe
        subprocess.run(["tmux", "pipe-pane", "-t", session_name], check=True)

        # Read the captured data
        with open(logfile, "r", errors="replace") as f:
            data = f.read()

        if not data.strip():
            print("Warning: No data captured from pipe-pane. The session may not be rendering.")
            return None

        # Replay through pyte
        screen = pyte.Screen(cols, rows)
        stream = pyte.Stream(screen)
        stream.feed(data)
        return screen

    finally:
        try:
            os.unlink(logfile)
        except OSError:
            pass


def render_screen_to_image(screen, title="ClawTree", output_path="screenshot.png"):
    """Render a pyte Screen to a PNG image with window chrome."""
    font = ImageFont.truetype(FONT_PATH, FONT_SIZE * SCALE)
    font_bold = ImageFont.truetype(FONT_BOLD_PATH, FONT_SIZE * SCALE)

    # Calculate cell dimensions
    bbox = font.getbbox("M")
    cell_w = bbox[2] - bbox[0]
    cell_h = int((bbox[3] - bbox[1]) * 1.4)

    # Image dimensions
    content_w = cell_w * screen.columns
    content_h = cell_h * screen.lines
    titlebar_h = TITLEBAR_HEIGHT * SCALE
    pad = PADDING * SCALE
    corner_radius = 10 * SCALE

    img_w = content_w + pad * 2
    img_h = content_h + titlebar_h + pad

    # Create image
    img = Image.new("RGBA", (img_w, img_h), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    # Rounded rectangle background
    draw.rounded_rectangle(
        [0, 0, img_w - 1, img_h - 1],
        radius=corner_radius,
        fill=DEFAULT_BG + (255,),
    )

    # Titlebar
    draw.rounded_rectangle(
        [0, 0, img_w - 1, titlebar_h + corner_radius],
        radius=corner_radius,
        fill=TITLEBAR_BG + (255,),
    )
    draw.rectangle(
        [0, corner_radius, img_w - 1, titlebar_h],
        fill=TITLEBAR_BG + (255,),
    )

    # Window buttons
    btn_colors = [(255, 95, 86), (255, 189, 46), (39, 201, 63)]
    btn_r = BUTTON_RADIUS * SCALE
    btn_y = titlebar_h // 2
    btn_start = BUTTON_START_X * SCALE
    btn_spacing = BUTTON_SPACING * SCALE
    for i, color in enumerate(btn_colors):
        cx = btn_start + i * btn_spacing
        draw.ellipse(
            [cx - btn_r, btn_y - btn_r, cx + btn_r, btn_y + btn_r],
            fill=color + (255,),
        )

    # Title text
    if title:
        title_font = ImageFont.truetype(FONT_PATH, 12 * SCALE)
        title_bbox = title_font.getbbox(title)
        title_w = title_bbox[2] - title_bbox[0]
        title_x = (img_w - title_w) // 2
        title_y = (titlebar_h - (title_bbox[3] - title_bbox[1])) // 2
        draw.text((title_x, title_y), title, fill=(150, 150, 170, 255), font=title_font)

    # Render each cell
    y_offset = titlebar_h
    x_offset = pad

    for row in range(screen.lines):
        for col in range(screen.columns):
            char = screen.buffer[row][col]
            x = x_offset + col * cell_w
            y = y_offset + row * cell_h

            fg = color_to_rgb(char.fg, is_fg=True)
            bg = color_to_rgb(char.bg, is_fg=False)

            if char.reverse:
                fg, bg = bg, fg

            # Always draw background (fills the entire cell area)
            draw.rectangle(
                [x, y, x + cell_w - 1, y + cell_h - 1],
                fill=bg + (255,),
            )

            # Draw character
            ch = char.data
            if ch and ch.strip():
                use_font = font_bold if char.bold else font
                draw.text((x, y), ch, fill=fg + (255,), font=use_font)

    # Save
    img.save(output_path, "PNG")
    print(f"Saved: {output_path} ({img_w}x{img_h})")
    return output_path


def capture_screenshot(source, output_path, title="ClawTree", cols=140, rows=40, mode="typescript"):
    """
    Full pipeline: capture terminal → render to image.

    mode="typescript" : source is a path to a `script` typescript file
    mode="tmux"       : source is a tmux session name
    """
    if mode == "typescript":
        screen = replay_typescript(source, cols, rows)
    elif mode == "tmux":
        screen = capture_tmux_session(source, cols, rows)
    else:
        raise ValueError(f"Unknown mode: {mode}")

    if screen is None:
        print("Failed to capture screen")
        return None

    return render_screen_to_image(screen, title=title, output_path=output_path)


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description="Capture terminal screenshots")
    parser.add_argument("source", help="Typescript file path or tmux session name")
    parser.add_argument("output", help="Output PNG path")
    parser.add_argument("--mode", choices=["typescript", "tmux"], default="typescript",
                        help="Capture mode (default: typescript)")
    parser.add_argument("--title", default="ClawTree", help="Window title text")
    parser.add_argument("--cols", type=int, default=140, help="Terminal columns")
    parser.add_argument("--rows", type=int, default=40, help="Terminal rows")
    args = parser.parse_args()

    capture_screenshot(args.source, args.output, title=args.title,
                       cols=args.cols, rows=args.rows, mode=args.mode)
