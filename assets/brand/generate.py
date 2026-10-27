#!/usr/bin/env python3
"""Generates the Mostalgia brand artwork (SVG) in this folder.

Needs fontTools (text is converted to paths so it renders the same
everywhere). PNG/ICO exports are made with the `resvg` CLI and Pillow,
see BRANDING.md.
"""
import os
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = os.path.join(HERE, "..", "..", "crates", "mostalgia-app", "assets", "fonts")

# Palette
AMBER = "#F2A541"
AMBER_LIGHT = "#F7C26B"
TERRACOTTA = "#D9653B"
EMBER = "#B8472A"
CREAM = "#FBF3E4"
CREAM_DEEP = "#F0DFC2"
PLUM = "#5B2A4E"
NIGHT = "#2B1730"
SAGE = "#9DB8A0"

_fonts = {}


def font(name):
    if name not in _fonts:
        _fonts[name] = TTFont(os.path.join(FONTS, name))
    return _fonts[name]


def text_path(text, fontfile, size, x, y, letter_spacing=0.0):
    """SVG path data for `text` with its baseline at (x, y); returns (d, width)."""
    f = font(fontfile)
    gs = f.getGlyphSet()
    cmap = f.getBestCmap()
    upm = f["head"].unitsPerEm
    scale = size / upm
    hmtx = f["hmtx"]
    d = []
    cursor = 0.0
    for ch in text:
        gname = cmap.get(ord(ch))
        if gname is None:
            continue
        pen = SVGPathPen(gs)
        tpen = TransformPen(pen, (scale, 0, 0, -scale, x + cursor, y))
        gs[gname].draw(tpen)
        d.append(pen.getCommands())
        cursor += hmtx[gname][0] * scale + letter_spacing * size
    return " ".join(p for p in d if p), cursor - letter_spacing * size


def icon_body(detail=True):
    """The icon's artwork in a 512×512 box (no <svg> wrapper)."""
    bars = ""
    if detail:
        # A little waveform playing between the reels.
        heights = [18, 34, 52, 30, 44, 22, 14]
        x0, w, gap = 220, 7, 5
        for i, h in enumerate(heights):
            bx = x0 + i * (w + gap)
            bars += f'<rect x="{bx}" y="{276 - h / 2}" width="{w}" height="{h}" rx="3.5" fill="{AMBER}"/>'
    reel_teeth = ""
    if detail:
        for cx in (178, 334):
            reel_teeth += f'<circle cx="{cx}" cy="276" r="11" fill="{PLUM}"/>'
    return f'''
  <defs>
    <linearGradient id="sunset" x1="0" y1="0" x2="0.25" y2="1">
      <stop offset="0" stop-color="{AMBER_LIGHT}"/>
      <stop offset="0.55" stop-color="{TERRACOTTA}"/>
      <stop offset="1" stop-color="{EMBER}"/>
    </linearGradient>
    <radialGradient id="glow" cx="0.3" cy="0.18" r="0.7">
      <stop offset="0" stop-color="#FFF4D6" stop-opacity="0.55"/>
      <stop offset="1" stop-color="#FFF4D6" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect x="16" y="16" width="480" height="480" rx="112" fill="url(#sunset)"/>
  <rect x="16" y="16" width="480" height="480" rx="112" fill="url(#glow)"/>
  <!-- cassette shadow -->
  <rect x="80" y="166" width="352" height="214" rx="36" fill="{NIGHT}" opacity="0.22"/>
  <!-- cassette -->
  <rect x="76" y="150" width="360" height="218" rx="36" fill="{CREAM}"/>
  <rect x="104" y="172" width="304" height="30" rx="10" fill="{TERRACOTTA}" opacity="0.9"/>
  <rect x="124" y="226" width="264" height="100" rx="50" fill="{NIGHT}"/>
  <circle cx="178" cy="276" r="30" fill="{CREAM}"/>
  <circle cx="334" cy="276" r="30" fill="{CREAM}"/>
  {reel_teeth}
  {bars}
  <path d="M152 368 L172 338 H340 L360 368 Z" fill="{CREAM_DEEP}"/>
'''


def icon_svg(detail=True):
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">{icon_body(detail)}</svg>\n'


def icon_small_svg():
    """Bolder variant for 16–32 px: bigger cassette, big reels, no waveform."""
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
  <defs>
    <linearGradient id="sunset" x1="0" y1="0" x2="0.25" y2="1">
      <stop offset="0" stop-color="{AMBER_LIGHT}"/>
      <stop offset="0.55" stop-color="{TERRACOTTA}"/>
      <stop offset="1" stop-color="{EMBER}"/>
    </linearGradient>
  </defs>
  <rect x="8" y="8" width="496" height="496" rx="116" fill="url(#sunset)"/>
  <rect x="44" y="120" width="424" height="272" rx="44" fill="{CREAM}"/>
  <rect x="84" y="152" width="344" height="40" rx="14" fill="{TERRACOTTA}"/>
  <rect x="92" y="220" width="328" height="128" rx="64" fill="{NIGHT}"/>
  <circle cx="164" cy="284" r="42" fill="{CREAM}"/>
  <circle cx="348" cy="284" r="42" fill="{CREAM}"/>
</svg>
'''


SYMBOLIC = '''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16">
  <path fill="#bebebe" fill-rule="evenodd" d="M3 3h10a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2h-1.6l-.9-1.6H4.5L3.6 13H3a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2zm2.6 3.6a1.4 1.4 0 1 0 0 2.8h4.8a1.4 1.4 0 1 0 0-2.8z"/>
  <circle fill="#bebebe" cx="5.7" cy="8" r="0.95"/>
  <circle fill="#bebebe" cx="10.3" cy="8" r="0.95"/>
</svg>
'''


def wordmark(dark=False):
    most, alg = (CREAM, AMBER) if dark else (PLUM, TERRACOTTA)
    size = 112
    d1, w1 = text_path("Most", "Inter-Bold.ttf", size, 0, 0, -0.02)
    d2, w2 = text_path("algia", "Inter-Bold.ttf", size, 0, 0, -0.02)
    gap = 36
    icon_px = 136
    width = icon_px + gap + w1 + w2 - 0.02 * size + 8
    height = 160
    base = 112
    tx = icon_px + gap
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width:.0f} {height}" width="{width:.0f}" height="{height}">
  <g transform="translate(0 {(height - icon_px) / 2}) scale({icon_px / 512})">{icon_body(True)}</g>
  <path transform="translate({tx} {base})" d="{d1}" fill="{most}"/>
  <path transform="translate({tx + w1 - 0.02 * size} {base})" d="{d2}" fill="{alg}"/>
</svg>
'''


def banner():
    W, H = 1280, 640
    title, tw = text_path("Most", "Inter-Bold.ttf", 132, 0, 0, -0.025)
    title2, tw2 = text_path("algia", "Inter-Bold.ttf", 132, 0, 0, -0.025)
    tag1, t1w = text_path("Nostalgia, but for meetings", "Inter-Medium.ttf", 40, 0, 0)
    tag2, t2w = text_path("you can actually recall in full.", "Inter-Medium.ttf", 40, 0, 0)
    feat, fw = text_path(
        "Local Whisper transcription  ·  Claude summaries  ·  Ask your meetings",
        "Inter-Medium.ttf", 22, 0, 0, 0.01)
    # Faint waveform along the bottom.
    wave = ""
    import math
    for i in range(0, 128):
        x = 20 + i * 10
        h = 18 + 70 * abs(math.sin(i * 0.37) * math.sin(i * 0.11 + 1.3))
        wave += f'<rect x="{x}" y="{560 - h / 2}" width="5" height="{h:.1f}" rx="2.5" fill="{AMBER}" opacity="0.18"/>'
    ix, iy, isz = 112, 176, 288
    tx = ix + isz + 72
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="{NIGHT}"/>
      <stop offset="1" stop-color="{PLUM}"/>
    </linearGradient>
    <radialGradient id="sun" cx="0.82" cy="0.05" r="0.55">
      <stop offset="0" stop-color="{AMBER}" stop-opacity="0.55"/>
      <stop offset="0.6" stop-color="{TERRACOTTA}" stop-opacity="0.12"/>
      <stop offset="1" stop-color="{TERRACOTTA}" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="{W}" height="{H}" fill="url(#bg)"/>
  <rect width="{W}" height="{H}" fill="url(#sun)"/>
  {wave}
  <g transform="translate({ix} {iy}) scale({isz / 512})">{icon_body(True)}</g>
  <path transform="translate({tx} 300)" d="{title}" fill="{CREAM}"/>
  <path transform="translate({tx + tw - 0.025 * 132} 300)" d="{title2}" fill="{AMBER}"/>
  <path transform="translate({tx + 4} 370)" d="{tag1}" fill="{CREAM}" opacity="0.92"/>
  <path transform="translate({tx + 4} 420)" d="{tag2}" fill="{CREAM}" opacity="0.92"/>
  <path transform="translate({tx + 4} 476)" d="{feat}" fill="{SAGE}"/>
</svg>
'''


def main():
    out = lambda name, s: open(os.path.join(HERE, name), "w").write(s)
    out("icon.svg", icon_svg(True))
    out("icon-small.svg", icon_small_svg())
    out("icon-symbolic.svg", SYMBOLIC)
    out("favicon.svg", icon_small_svg())
    out("wordmark.svg", wordmark(False))
    out("wordmark-dark.svg", wordmark(True))
    out("banner.svg", banner())


if __name__ == "__main__":
    main()
