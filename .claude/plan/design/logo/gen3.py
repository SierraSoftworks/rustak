#!/usr/bin/env python3
"""Round 3: inner rim × {base, gradient, oversized, gradient+oversized}, on light and dark."""
import os
from gen import *
from gen2 import bend, bend_poly

DARK_BG = "#1A1C20"

def mark(u, rim="inset", gradient=False, oversized=False):
    top, bot = (f"url(#{u}ink)", f"url(#{u}rust)") if gradient else (INK, RUST)
    out = ""
    if rim == "keyline":
        out += f'<path d="{SHIELD_B}" fill="none" stroke="{WHITE}" stroke-width="12" stroke-linejoin="round"/>'
    out += bend(u, top, bot)
    if rim == "inset":
        out += (f'<path d="{SHIELD_B}" fill="none" stroke="{WHITE}" stroke-width="8" stroke-linejoin="round" '
                f'transform="translate(128,128) scale(0.9) translate(-128,-128)"/>')
    if rim == "both":
        out += f'<path d="{SHIELD_B}" fill="none" stroke="{WHITE}" stroke-width="12" stroke-linejoin="round"/>'
        out += (f'<path d="{SHIELD_B}" fill="none" stroke="{WHITE}" stroke-width="6" stroke-linejoin="round" '
                f'transform="translate(128,128) scale(0.82) translate(-128,-128)"/>')
    s = 0.88 if oversized else 0.6
    y = 128 if oversized else 132
    k = 0.79 if rim == "both" else 0.855
    inner = f'<clipPath id="{u}in"><path d="{SHIELD_B}" transform="translate(128,128) scale({k}) translate(-128,-128)"/></clipPath>'
    body = ""
    if gradient:
        body += f'<g filter="url(#{u}soft)" opacity="0.35">' + eagle(128, y + 6, s, "#000") + '</g>'
    body += eagle(128, y, s, WHITE)
    if oversized:
        out += inner + f'<g clip-path="url(#{u}in)">{body}</g>'
    else:
        out += body
    return out

COMBOS = [
    ("base",               dict()),
    ("gradient",           dict(gradient=True)),
    ("oversized eagle",    dict(oversized=True)),
    ("gradient + oversized", dict(gradient=True, oversized=True)),
]
RIMS = [("inset rim (A6)", "inset"), ("outer keyline", "keyline"), ("both rims", "both")]

def cell(u, rim, kw):
    """mark on light and on dark, side by side, 200px each"""
    m = mark(u, rim=rim, **kw)
    return (f'<rect x="0" y="0" width="200" height="200" rx="12" fill="{PAPER}"/>'
            f'<rect x="212" y="0" width="200" height="200" rx="12" fill="{DARK_BG}"/>'
            f'<svg x="0" y="0" width="200" height="200" viewBox="0 0 256 256">{defs(u)}{m}</svg>'
            f'<svg x="212" y="0" width="200" height="200" viewBox="0 0 256 256">{defs(u+"d")}{mark(u+"d", rim=rim, **kw)}</svg>')

def board():
    cw, ch, pad = 440, 250, 24
    W = pad*2 + 160 + cw*len(RIMS)
    H = pad*2 + 90 + ch*len(COMBOS)
    out = [f'<svg viewBox="0 0 {W} {H}" width="{W}" height="{H}" xmlns="http://www.w3.org/2000/svg" font-family="{FONT_SANS}">',
           f'<rect width="{W}" height="{H}" fill="#EDEDEF"/>',
           text(pad, 44, "rustak — round 3: rim × finish × eagle size, on light and dark", 26, INK, weight=700, anchor="start")]
    for j, (rname, _) in enumerate(RIMS):
        out.append(text(pad + 160 + j*cw + 206, 84, rname, 16, INK, weight=700))
    for i, (cname, kw) in enumerate(COMBOS):
        y = pad + 100 + i*ch
        out.append(text(pad, y + 100, f"B{i+1}", 20, INK, weight=700, anchor="start"))
        out.append(text(pad, y + 122, cname, 13, "#6B6E75", weight=400, anchor="start"))
        for j, (_, rim) in enumerate(RIMS):
            x = pad + 160 + j*cw
            out.append(f'<g transform="translate({x},{y})">{cell(f"c{i}{j}", rim, kw)}</g>')
    out.append('</svg>')
    return "\n".join(out)

if __name__ == "__main__":
    os.makedirs("round3", exist_ok=True)
    for i, (cname, kw) in enumerate(COMBOS):
        for _, rim in RIMS:
            with open(f"round3/B{i+1}-{rim}.svg", "w") as f:
                f.write(f'<svg viewBox="0 0 256 256" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="rustak logo">\n  {defs("r")}\n  {mark("r", rim=rim, **kw)}\n</svg>\n')
    open("board3.svg", "w").write(board())
    print("ok")
