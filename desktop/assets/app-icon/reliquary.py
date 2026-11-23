#!/usr/bin/env python3
"""The Boxen app icon: Reliquary, a dark casket split by a seam of cold light.

This scene is the icon's artwork source. Running it regenerates everything derived from it:

  AppIcon.icon/Assets/1-casket.png  the casket and its glow (Liquid Glass off in Icon Composer)
  AppIcon.icon/Assets/2-seam.png    the seam alone (Liquid Glass on, so its highlight catches the light)
  Boxen.icns                        a flattened legacy icon for the app bundle the desktop build makes
  preview.png                       the flattened icon at 1024 px, for reviewing changes

AppIcon.icon/icon.json holds the composition (background, groups, glass, shadows, appearances) and is
edited by hand or in Icon Composer; this script never rewrites it.

Requires Python 3 and Pillow. Building Boxen.icns also requires macOS `iconutil`.
"""
import argparse
import math
import pathlib
import shutil
import subprocess
import sys
import tempfile

from PIL import Image, ImageChops, ImageDraw, ImageFilter

import ps1_render as ps1
from ps1_render import Scene, box

HERE = pathlib.Path(__file__).resolve().parent

# ------------------------------------------------------------------ scene
KEY = {"type": "dir", "dir": (0.55, -0.9, -0.7), "color": (165, 175, 210)}
RIM = {"type": "dir", "dir": (-0.8, -0.2, 0.55), "color": (90, 120, 190)}
CASKET = (64, 58, 74)
LIGHT = (110, 225, 255)
SEAM = (210, 250, 255)
GAP = 0.16  # height of the lit seam between the two halves
BACKGROUND = (35, 42, 78)  # Icon Composer's automatic gradient is built from this colour


def scene():
    height = (1.68 - GAP) / 2  # the two halves and the seam make a 1.6 × 1.68 × 1.6 casket
    centre = GAP / 2 + height / 2
    mesh = box(0, centre, 0, 1.6, height, 1.6, "casket", sub_div=3)
    mesh.extend(box(0, -centre, 0, 1.6, height, 1.6, "casket", sub_div=3))
    mesh.extend(box(0, 0, 0, 1.5, GAP, 1.5, "seam"))
    return Scene(mesh, {
        "casket": {"color": CASKET, "ambient": 0.32, "spec": 0.6, "shine": 34},
        "seam": {"emit": SEAM},
    }, [KEY, RIM,
        {"type": "point", "pos": (0.3, 0, 0.9), "color": LIGHT, "power": 2.6, "falloff": 1.4},
        {"type": "point", "pos": (-0.9, 0, 0.3), "color": LIGHT, "power": 2.0, "falloff": 1.4}],
        {"dist": 7.0, "focal": 3.6, "yaw": 0.72, "pitch": 0.48},
        glow=[(LIGHT, 13, 0.5)])


# ------------------------------------------------------------------ outputs
def layers():
    casket = ps1.render(scene(), keep={"casket"})
    seam = ps1.render(scene(), keep={"seam"}, with_glow=False)
    return casket, seam


def background(size):
    """An approximation of Icon Composer's automatic gradient: lighter at the top, deeper below."""
    top = tuple(min(255, int(c * 1.25 + 10)) for c in BACKGROUND)
    bottom = tuple(int(c * 0.22) for c in BACKGROUND)
    img = Image.new("RGBA", (size, size))
    draw = ImageDraw.Draw(img)
    for y in range(size):
        t = y / (size - 1)
        draw.line([(0, y), (size, y)], fill=tuple(int(top[k] * (1 - t) + bottom[k] * t) for k in range(3)) + (255,))
    return img


def squircle_mask(size, inset, radius):
    """macOS's continuous-corner rounded rectangle, approximated with a superellipse."""
    scale = 4
    big = size * scale
    mask = Image.new("L", (big, big), 0)
    px = mask.load()
    lo, hi = inset * scale, (size - inset) * scale
    r = radius * scale
    n = 5.0
    for y in range(lo, hi):
        dy = max(lo + r - y, y - (hi - r), 0) / r
        if dy > 1:
            continue
        dx = (1 - dy ** n) ** (1 / n) * r
        x0 = int(lo + r - dx) if dy > 0 else lo
        x1 = int(hi - r + dx) if dy > 0 else hi
        for x in range(max(lo, x0), min(hi, x1)):
            px[x, y] = 255
    return mask.resize((size, size), Image.LANCZOS)


def flatten(casket, seam, size=1024, inset=0, radius=None):
    """The composed icon without system effects: background, casket, seam, masked to the icon shape."""
    radius = radius if radius is not None else round((size - 2 * inset) * 0.225)
    body = size - 2 * inset
    art = background(body)
    for layer in (casket, seam):
        art.alpha_composite(layer.resize((body, body), Image.NEAREST))
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    canvas.paste(art, (inset, inset))
    mask = squircle_mask(size, inset, radius)
    canvas.putalpha(ImageChops.multiply(canvas.getchannel("A"), mask))
    return canvas


def legacy_icon(casket, seam):
    """macOS's pre-Liquid-Glass icon grid: an 824 px body centred on a 1024 canvas with a soft shadow."""
    icon = flatten(casket, seam, size=1024, inset=100, radius=185)
    shadow = Image.new("RGBA", icon.size, (0, 0, 0, 0))
    shadow.putalpha(icon.getchannel("A").point(lambda a: int(a * 0.45)))
    shadow = shadow.filter(ImageFilter.GaussianBlur(14))
    out = Image.new("RGBA", icon.size, (0, 0, 0, 0))
    out.alpha_composite(shadow, (0, 12))
    out.alpha_composite(icon)
    return out


def write_icns(image, path):
    if not shutil.which("iconutil"):
        sys.exit("iconutil is unavailable; Boxen.icns is built on macOS")
    with tempfile.TemporaryDirectory() as tmp:
        iconset = pathlib.Path(tmp) / "Boxen.iconset"
        iconset.mkdir()
        for points in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                pixels = points * scale
                name = f"icon_{points}x{points}{'@2x' if scale == 2 else ''}.png"
                image.resize((pixels, pixels), Image.LANCZOS).save(iconset / name)
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(path)], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--no-icns", action="store_true", help="skip Boxen.icns (for non-macOS hosts)")
    args = parser.parse_args()
    casket, seam = layers()
    assets = HERE / "AppIcon.icon" / "Assets"
    assets.mkdir(parents=True, exist_ok=True)
    casket.save(assets / "1-casket.png")
    seam.save(assets / "2-seam.png")
    flatten(casket, seam).save(HERE / "preview.png")
    if not args.no_icns:
        write_icns(legacy_icon(casket, seam), HERE / "Boxen.icns")
    print("wrote", ", ".join(str(p.relative_to(HERE)) for p in [assets / "1-casket.png", assets / "2-seam.png",
                                                                  HERE / "preview.png"] +
          ([] if args.no_icns else [HERE / "Boxen.icns"])))


if __name__ == "__main__":
    main()
