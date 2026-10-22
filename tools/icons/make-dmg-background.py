#!/usr/bin/env python3
"""Draws the background of the .dmg installer window: an arrow from the app
to the Applications folder and short instructions. Writes a two-resolution
TIFF (1x and Retina) to apps/annex-mac-app/dmg/background.tiff.

Needs Pillow and the Inter font (or point --font-dir at it). The
icon positions must match bundle.macOS.dmg in tauri.conf.json, whose window
height is this image's plus Finder's 32 pt title bar.
"""
import argparse
import os
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
OUT = os.path.join(ROOT, "apps", "annex-mac-app", "dmg", "background.tiff")
W, H = 660, 400
APP = (180, 190)
APPS = (480, 190)
ICON = 128

INK = (29, 29, 31)
MUTED = (110, 110, 115)
BLUE_A = (59, 130, 246)
BLUE_B = (29, 78, 216)


def font(font_dir, weight, size):
    candidates = [os.path.join(font_dir, f"Inter-{weight}.otf")] if font_dir else []
    candidates += [
        f"/usr/share/fonts/opentype/inter/Inter-{weight}.otf",
        os.path.expanduser(f"~/Library/Fonts/Inter-{weight}.otf"),
        f"/Library/Fonts/Inter-{weight}.otf",
    ]
    for c in candidates:
        if c and os.path.exists(c):
            return ImageFont.truetype(c, size)
    raise SystemExit("Inter not found; pass --font-dir <folder with Inter-*.otf>")


def draw(k, font_dir):
    img = Image.new("RGB", (W * k, H * k))
    d = ImageDraw.Draw(img)
    top, bottom = (248, 250, 253), (232, 238, 247)
    for y in range(H * k):
        t = y / (H * k - 1)
        d.line([(0, y), (W * k, y)], fill=tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)))

    def text(xy, s, weight, size, fill):
        f = font(font_dir, weight, size * k)
        d.text((xy[0] * k, xy[1] * k), s, font=f, fill=fill, anchor="mm")

    text((W / 2, 52), "Drag Annex to Applications", "SemiBold", 22, INK)
    text((W / 2, 80), "Drop the Annex icon on the folder to install it.", "Regular", 13, MUTED)
    text((W / 2, 352), "Then open Annex from Applications. It lives in the menu bar.", "Regular", 12, MUTED)

    # Arrow between the icons, drawn on a supersampled layer for smooth edges.
    s = 4 * k
    layer = Image.new("L", (W * s, H * s), 0)
    a = ImageDraw.Draw(layer)
    y = APP[1] * s
    x0 = (APP[0] + ICON / 2 + 26) * s
    x1 = (APPS[0] - ICON / 2 - 26) * s
    shaft = 9 * s
    head_l, head_w = 26 * s, 30 * s
    a.rounded_rectangle([x0, y - shaft / 2, x1 - head_l + 4 * s, y + shaft / 2], radius=shaft / 2, fill=255)
    a.polygon([(x1 - head_l, y - head_w / 2), (x1, y), (x1 - head_l, y + head_w / 2)], fill=255)
    mask = layer.resize((W * k, H * k), Image.LANCZOS)
    grad = Image.new("RGB", (W * k, H * k))
    g = ImageDraw.Draw(grad)
    for x in range(W * k):
        t = min(1, max(0, (x / k - x0 / s) / ((x1 - x0) / s)))
        g.line([(x, 0), (x, H * k)], fill=tuple(round(p + (q - p) * t) for p, q in zip(BLUE_A, BLUE_B)))
    img.paste(grad, (0, 0), mask)
    return img


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--font-dir", help="folder with Inter-Regular.otf and Inter-SemiBold.otf")
    args = ap.parse_args()
    one, two = draw(1, args.font_dir), draw(2, args.font_dir)
    # Finder picks the representation that matches the screen by its DPI.
    two.encoderinfo = {"dpi": (144, 144)}
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    one.save(OUT, save_all=True, append_images=[two], dpi=(72, 72), compression="tiff_lzw")
    print("wrote", OUT)


if __name__ == "__main__":
    main()
