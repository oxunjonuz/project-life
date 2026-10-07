#!/usr/bin/env python3
"""Build the app icon for the macOS bundle.

The mark is the design's own shield-check glyph (the shape Figma uses for "protected"), taken from
the exported asset rather than redrawn, on the design's own accent colour. Nothing is invented: the
path comes from design/figma-PL/assets, the colour from the palette measured in the JSX.

    python3 tools/make_icon.py [--out app/macos/ProjectLife.icns]
"""
import argparse
import re
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
ROOT = Path(__file__).resolve().parent.parent
ASSET = ROOT / "design/figma-PL/assets/6b937241-819d-4441-8296-ac2d4fff40a2.svg"
ACCENT = "#245FC4"          # the design's accent blue
GLYPH = "#FFFFFF"

ICNS_TYPES = [("ic11", 32), ("ic12", 64), ("ic07", 128), ("ic13", 256), ("ic09", 512), ("ic14", 512), ("ic10", 1024)]


def glyph_path():
    text = ASSET.read_text()
    m = re.search(r'<path[^>]*d="([^"]+)"', text)
    if not m:
        raise SystemExit(f"no path in {ASSET}")
    return m.group(1)


def icon_svg(size=1024):
    d = glyph_path()
    inner = 560
    scale = inner / 18.0
    pad = (size - inner) / 2.0
    radius = size * 0.2237          # macOS squircle-ish corner
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
  <rect x="0" y="0" width="{size}" height="{size}" rx="{radius:.0f}" ry="{radius:.0f}" fill="{ACCENT}"/>
  <g transform="translate({pad:.2f},{pad:.2f}) scale({scale:.4f})">
    <path d="{d}" fill="none" stroke="{GLYPH}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
  </g>
</svg>
'''


def render(svg_path: Path, png_path: Path, size: int, chromium: str):
    cmd = [chromium, "--headless=new", "--no-sandbox", "--hide-scrollbars",
           "--force-device-scale-factor=1", f"--window-size={size},{size}",
           "--default-background-color=00000000", f"--screenshot={png_path}", svg_path.as_uri()]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=180)
    if not png_path.is_file():
        raise SystemExit(f"chromium did not render the icon: {r.stderr[-400:]}")


def pack_icns(pngs, out: Path):
    entries = []
    for kind, size in ICNS_TYPES:
        data = pngs[size]
        entries.append(kind.encode("ascii") + struct.pack(">I", len(data) + 8) + data)
    body = b"".join(entries)
    out.write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(ROOT / "app/macos/ProjectLife.icns"))
    ap.add_argument("--chromium", default="/usr/bin/chromium")
    # Round 301: the same mark, the same asset and the same colour, for the Linux icon theme and for
    # the Windows shell's tray and executable. One drawing, three platforms — not three drawings that
    # drift apart.
    ap.add_argument("--png-dir", default=None, help="also write icon-<size>.png files here (Linux)")
    ap.add_argument("--ico", default=None, help="also write a multi-size .ico here (Windows)")
    args = ap.parse_args()

    from PIL import Image

    with tempfile.TemporaryDirectory() as td:
        td = Path(td)
        svg = td / "icon.svg"
        svg.write_text(icon_svg(1024))
        master = td / "icon1024.png"
        render(svg, master, 1024, args.chromium)
        img = Image.open(master).convert("RGBA")
        pngs = {}
        for _kind, size in ICNS_TYPES:
            buf = td / f"icon{size}.png"
            img.resize((size, size), Image.LANCZOS).save(buf, "PNG")
            pngs[size] = buf.read_bytes()
        pack_icns(pngs, Path(args.out))
        if args.png_dir:
            outdir = Path(args.png_dir)
            outdir.mkdir(parents=True, exist_ok=True)
            for size in [16, 24, 32, 48, 64, 128, 256, 512]:
                img.resize((size, size), Image.LANCZOS).save(outdir / f"icon-{size}.png", "PNG")
            print(f"wrote {len([16,24,32,48,64,128,256,512])} PNGs to {outdir}")
        if args.ico:
            ico = Path(args.ico)
            ico.parent.mkdir(parents=True, exist_ok=True)
            img.save(ico, format="ICO", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
            print(f"wrote {ico} ({ico.stat().st_size} bytes)")
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
