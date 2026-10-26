"""Generates the NopePDF icon: a rounded gradient square with a white page.

Usage:
    python scripts/make-icon.py [COLOR]     # write assets/icon.png, icon.ico, icon256.png
    python scripts/make-icon.py --variants  # write a 256 px preview of every color to assets/variants/
    python scripts/make-icon.py --list      # list color names

Requires Pillow (pip install pillow). The default color is the current app icon.
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw

DEFAULT = "burgundy"

# name: (gradient top, gradient bottom); the bottom color is also the accent line.
PALETTES = {
    "burgundy": ((160, 40, 70), (105, 15, 40)),
    "red": ((239, 83, 80), (198, 40, 40)),
    "blue": ((66, 133, 244), (25, 80, 190)),
    "sky-blue": ((100, 200, 245), (30, 145, 215)),
    "navy": ((52, 78, 140), (20, 38, 85)),
    "green": ((76, 175, 80), (30, 120, 45)),
    "bottle-green": ((40, 120, 90), (15, 75, 55)),
    "teal": ((38, 198, 180), (0, 128, 128)),
    "purple": ((149, 117, 205), (94, 53, 177)),
    "orange": ((255, 167, 38), (230, 100, 20)),
    "graphite": ((110, 115, 125), (55, 60, 70)),
    "pink": ((240, 98, 146), (194, 24, 91)),
}

SIZE = 1024
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
ASSETS = Path(__file__).resolve().parent.parent / "assets"


def render(top, bottom):
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    grad = Image.new("RGBA", (SIZE, SIZE))
    gd = ImageDraw.Draw(grad)
    for y in range(SIZE):
        t = y / (SIZE - 1)
        gd.line([(0, y), (SIZE, y)], fill=tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3)) + (255,))
    mask = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(mask).rounded_rectangle([40, 40, SIZE - 40, SIZE - 40], radius=200, fill=255)
    img.paste(grad, (0, 0), mask)

    d = ImageDraw.Draw(img)
    # Page with a folded corner tinted in the accent color.
    left, top_y, right, bottom_y, fold = 270, 190, 754, 834, 150
    d.polygon([(left, top_y), (right - fold, top_y), (right, top_y + fold), (right, bottom_y), (left, bottom_y)],
              fill=(255, 255, 255, 255))
    tint = tuple(int(255 - (255 - c) * 0.25) for c in bottom) + (255,)
    d.polygon([(right - fold, top_y), (right - fold, top_y + fold), (right, top_y + fold)], fill=tint)
    # Text lines: the first in the accent color.
    for i, (y, w) in enumerate([(430, 380), (510, 380), (590, 380), (670, 250)]):
        color = bottom + (255,) if i == 0 else (120, 120, 128, 255)
        d.rounded_rectangle([left + 52, y, left + 52 + w, y + 36], radius=18, fill=color)
    return img


def main(args):
    if args and args[0] == "--list":
        for name in PALETTES:
            print(name + (" (default)" if name == DEFAULT else ""))
        return
    if args and args[0] == "--variants":
        out = ASSETS / "variants"
        out.mkdir(exist_ok=True)
        for name, (top, bottom) in PALETTES.items():
            render(top, bottom).resize((256, 256), Image.LANCZOS).save(out / f"icon-{name}.png")
        print(f"Wrote {len(PALETTES)} variants to {out}")
        return
    name = args[0] if args else DEFAULT
    if name not in PALETTES:
        sys.exit(f"Unknown color '{name}'. Available: {', '.join(PALETTES)}")
    img = render(*PALETTES[name])
    img.save(ASSETS / "icon.png")
    img.save(ASSETS / "icon.ico", sizes=[(s, s) for s in ICO_SIZES])
    img.resize((256, 256), Image.LANCZOS).save(ASSETS / "icon256.png")
    print(f"Icon set to '{name}' in {ASSETS}")


if __name__ == "__main__":
    main(sys.argv[1:])
