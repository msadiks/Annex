#!/usr/bin/env python3
"""Draws the Annex icon and writes every size both apps need. Needs Pillow."""
import os
from PIL import Image, ImageDraw

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
S = 1024


def master():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # Background: rounded square, vertical blue gradient.
    bg = Image.new("RGBA", (S, S))
    g = ImageDraw.Draw(bg)
    for y in range(S):
        t = y / S
        g.line([(0, y), (S, y)], fill=(int(30 + 20 * t), int(110 - 40 * t), int(240 - 60 * t), 255))
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle([40, 40, S - 40, S - 40], radius=200, fill=255)
    img.paste(bg, (0, 0), mask)
    # Two displays: the Mac (back) and the PC monitor (front), joined.
    white = (255, 255, 255, 255)
    soft = (255, 255, 255, 120)
    d.rounded_rectangle([170, 230, 640, 540], radius=36, outline=(190, 210, 255, 255), width=36)
    d.rounded_rectangle([380, 400, 860, 720], radius=36, fill=(28, 72, 200, 255), outline=white, width=40)
    d.rectangle([590, 720, 650, 800], fill=white)
    d.rounded_rectangle([500, 790, 740, 830], radius=20, fill=white)
    return img


def template(size):
    """macOS menu bar template: black + alpha."""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    k = size / 44
    d.rounded_rectangle([3 * k, 6 * k, 27 * k, 24 * k], radius=3 * k, outline=(0, 0, 0, 140), width=max(1, round(3 * k)))
    d.rounded_rectangle([14 * k, 14 * k, 41 * k, 33 * k], radius=3 * k, fill=(0, 0, 0, 255))
    d.rectangle([26 * k, 33 * k, 29 * k, 37 * k], fill=(0, 0, 0, 255))
    d.rounded_rectangle([21 * k, 37 * k, 34 * k, 40 * k], radius=1 * k, fill=(0, 0, 0, 255))
    return img


def main():
    m = master()
    os.makedirs(os.path.join(ROOT, "assets"), exist_ok=True)
    m.save(os.path.join(ROOT, "assets", "icon-1024.png"))
    m.save(os.path.join(ROOT, "crates", "annex-win", "resources", "annex.ico"),
           sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    icons = os.path.join(ROOT, "apps", "annex-mac-app", "icons")
    os.makedirs(icons, exist_ok=True)
    for name, size in [("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256), ("icon.png", 512)]:
        m.resize((size, size), Image.LANCZOS).save(os.path.join(icons, name))
    m.save(os.path.join(icons, "icon.icns"))
    m.save(os.path.join(icons, "icon.ico"), sizes=[(32, 32), (256, 256)])
    template(44).save(os.path.join(icons, "tray.png"))


if __name__ == "__main__":
    main()
