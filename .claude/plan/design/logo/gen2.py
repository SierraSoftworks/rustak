#!/usr/bin/env python3
"""Round 2: variants on concept 04 (party per bend)."""
import os, textwrap
from gen import *  # palette, shapes, eagle(), defs(), text()

def bend_poly(sinister=False, angle=45):
    """Polygon covering the lower side of a diagonal through (128,128)."""
    import math
    a = math.radians(angle)
    dx, dy = (-math.cos(a) if sinister else math.cos(a)), -math.sin(a)   # line direction (rising)
    nx, ny = (-math.sin(a) if sinister else math.sin(a)), math.cos(a)     # normal pointing down
    x1, y1 = 128 - dx*600, 128 - dy*600
    x2, y2 = 128 + dx*600, 128 + dy*600
    return (f"M{x1:.1f},{y1:.1f} L{x2:.1f},{y2:.1f} L{x2+nx*800:.1f},{y2+ny*800:.1f} "
            f"L{x1+nx*800:.1f},{y1+ny*800:.1f} Z")

def bend(u, top, bottom, sinister=False, angle=45):
    return f'''<g clip-path="url(#{u}clipB)">
      <rect width="256" height="256" fill="{top}"/>
      <path d="{bend_poly(sinister, angle)}" fill="{bottom}"/>
    </g>'''

E = dict(x=128, y=132, s=0.66)

def v01(u):  # baseline
    return bend(u, INK, RUST) + eagle(128, 132, 0.66, WHITE)
def v02(u):  # bend sinister
    return bend(u, INK, RUST, sinister=True) + eagle(128, 132, 0.66, WHITE)
def v03(u):  # colours swapped
    return bend(u, RUST, INK) + eagle(128, 132, 0.66, WHITE)
def v04(u):  # steeper bend
    return bend(u, INK, RUST, angle=62) + eagle(128, 132, 0.66, WHITE)
def v05(u):  # flatter bend (closer to per fess)
    return bend(u, INK, RUST, angle=28) + eagle(128, 132, 0.66, WHITE)
def v06(u):  # white inner rim (TAK double rim)
    return bend(u, INK, RUST) + f'<path d="{SHIELD_B}" fill="none" stroke="{WHITE}" stroke-width="8" transform="translate(128,128) scale(0.9) translate(-128,-128)"/>' + eagle(128, 132, 0.6, WHITE)
def v07(u):  # ink frame like mail-backup
    return bend(u, INK2, RUST) + f'<path d="{SHIELD_B}" fill="none" stroke="{INK}" stroke-width="14" stroke-linejoin="round"/>' + eagle(128, 132, 0.62, WHITE)
def v08(u):  # paper + rust, ink eagle
    return bend(u, PAPER2, RUST) + f'<path d="{SHIELD_B}" fill="none" stroke="{INK}" stroke-width="12" stroke-linejoin="round"/>' + eagle(128, 132, 0.62, INK)
def v09(u):  # counterchanged eagle: rust on ink, ink on rust
    return (bend(u, INK, RUST)
            + f'<clipPath id="{u}dn"><path d="{bend_poly()}"/></clipPath>'
            + eagle(128, 132, 0.66, RUST)
            + f'<g clip-path="url(#{u}dn)">' + eagle(128, 132, 0.66, INK) + '</g>')
def v10(u):  # rust bend band on ink
    return (f'<g clip-path="url(#{u}clipB)"><rect width="256" height="256" fill="{INK}"/>'
            f'<path d="M-40,296 L296,-40 L340,4 L4,340 Z" fill="{RUST}"/></g>' + eagle(128, 132, 0.66, WHITE))
def v11(u):  # gradients + white eagle with soft shadow (mail-backup finish)
    return (bend(u, f"url(#{u}ink)", f"url(#{u}rust)")
            + f'<g filter="url(#{u}soft)" opacity="0.35">' + eagle(128, 138, 0.66, "#000") + '</g>'
            + eagle(128, 132, 0.66, WHITE))
def v12(u):  # oversized eagle cropped by the shield
    return bend(u, INK, RUST) + f'<g clip-path="url(#{u}clipB)">' + eagle(128, 128, 0.92, WHITE) + '</g>'

VARIANTS = [
    ("A1", "Baseline (04)",        "Ink over rust, bend dexter, white eagle", v01),
    ("A2", "Bend sinister",        "Diagonal mirrored",                       v02),
    ("A3", "Colours swapped",      "Rust over ink",                           v03),
    ("A4", "Steeper bend",         "62° split",                               v04),
    ("A5", "Flatter bend",         "28° split, closer to horizontal",         v05),
    ("A6", "Inner rim",            "White inset rim, TAK-style double edge",  v06),
    ("A7", "Ink frame",            "Heavy ink frame like mail-backup",        v07),
    ("A8", "Paper + rust",         "Light body, ink eagle and frame",         v08),
    ("A9", "Counterchanged",       "Eagle swaps colour across the bend",      v09),
    ("A10","Bend band",            "Rust band on ink instead of halves",      v10),
    ("A11","Gradient finish",      "Subtle gradients + soft shadow",          v11),
    ("A12","Oversized eagle",      "Wings cropped by the shield edge",        v12),
]

def lockup(u, mark_fn, name_ink="rus", name_rust="tak", mono=True, stacked=False):
    fam = FONT_MONO if mono else FONT_SANS
    if stacked:
        return (f'<svg x="0" y="0" width="256" height="200" viewBox="0 0 256 256">{defs(u)}{mark_fn(u)}</svg>'
                + f'<text x="128" y="250" font-family="{fam}" font-size="44" font-weight="700" text-anchor="middle">'
                  f'<tspan fill="{INK}">{name_ink}</tspan><tspan fill="{RUST}">{name_rust}</tspan></text>')
    return (f'<svg x="0" y="10" width="130" height="130" viewBox="0 0 256 256">{defs(u)}{mark_fn(u)}</svg>'
            + f'<text x="146" y="92" font-family="{fam}" font-size="48" font-weight="700">'
              f'<tspan fill="{INK}">{name_ink}</tspan><tspan fill="{RUST}">{name_rust}</tspan></text>')

def board():
    cols, tw, th, pad = 4, 300, 380, 24
    rows = (len(VARIANTS) + cols - 1) // cols
    W = cols * tw + pad * 2
    H = rows * th + pad * 2 + 60 + 340
    out = [f'<svg viewBox="0 0 {W} {H}" width="{W}" height="{H}" xmlns="http://www.w3.org/2000/svg" font-family="{FONT_SANS}">',
           f'<rect width="{W}" height="{H}" fill="#EDEDEF"/>',
           text(pad, 44, "rustak — round 2: variants on 04 (party per bend)", 26, INK, weight=700, anchor="start")]
    for i, (num, title, desc, fn) in enumerate(VARIANTS):
        x = pad + (i % cols) * tw
        y = pad + 60 + (i // cols) * th
        u = f"v{num}"
        out.append(f'<g transform="translate({x},{y})">')
        out.append(f'<rect x="8" y="8" width="{tw-16}" height="{th-16}" rx="14" fill="{WHITE}"/>')
        out.append(f'<svg x="{(tw-256)/2}" y="24" width="256" height="256" viewBox="0 0 256 256">{defs(u)}{fn(u)}</svg>')
        out.append(text(24, 314, f"{num}  {title}", 17, INK, weight=700, anchor="start"))
        for j, l in enumerate(textwrap.wrap(desc, 38)[:2]):
            out.append(text(24, 336 + j*18, l, 13, "#6B6E75", weight=400, anchor="start"))
        out.append('</g>')
    # lockups + size test row
    y = pad + 60 + rows * th
    out.append(f'<g transform="translate({pad},{y})">')
    out.append(f'<rect x="8" y="8" width="{cols*tw-16}" height="316" rx="14" fill="{WHITE}"/>')
    out.append(text(24, 40, "Lockups and size check (baseline mark)", 17, INK, weight=700, anchor="start"))
    out.append(f'<g transform="translate(24,60)">{lockup("l1", v01)}</g>')
    out.append(f'<g transform="translate(24,180)">{lockup("l2", v01, mono=False)}</g>')
    out.append(f'<g transform="translate(420,60)">{lockup("l3", v01, name_ink="rus", name_rust="tak", stacked=True)}</g>')
    # dark background lockup
    out.append(f'<rect x="700" y="60" width="440" height="120" rx="12" fill="{INK}"/>')
    out.append(f'<g transform="translate(720,60)"><svg x="0" y="10" width="100" height="100" viewBox="0 0 256 256">{defs("l4")}{v01("l4")}</svg>'
               f'<text x="116" y="76" font-family="{FONT_MONO}" font-size="44" font-weight="700"><tspan fill="{WHITE}">rus</tspan><tspan fill="{RUST2}">tak</tspan></text></g>')
    # size check: 128, 64, 32, 16
    sx = 720
    for sz in (128, 64, 32, 16):
        out.append(f'<svg x="{sx}" y="{200 + (128-sz)}" width="{sz}" height="{sz}" viewBox="0 0 256 256">{defs(f"s{sz}")}{v01(f"s{sz}")}</svg>')
        out.append(text(sx + sz/2, 200 + 128 + 16, f"{sz}px", 11, "#6B6E75", weight=400))
        sx += sz + 24
    out.append('</g>')
    out.append('</svg>')
    return "\n".join(out)

if __name__ == "__main__":
    os.makedirs("round2", exist_ok=True)
    for num, title, desc, fn in VARIANTS:
        with open(f"round2/{num}.svg", "w") as f:
            f.write(f'<svg viewBox="0 0 256 256" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="rustak logo {num}">\n  {defs("r")}\n  {fn("r")}\n</svg>\n')
    with open("board2.svg", "w") as f:
        f.write(board())
    print("ok")
