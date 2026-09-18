#!/usr/bin/env python3
"""Generate a board of 15 rustak logo concepts as SVG (plus standalone tiles)."""
import os, textwrap

INK, INK2 = "#2C2E33", "#484B51"
PAPER, PAPER2 = "#F7F7F7", "#E2E2E2"
RUST, RUST2 = "#E5401C", "#F2643F"
WHITE = "#FFFFFF"

# ---------- shared shapes (256x256 space, centred at 128) ----------
SHIELD_A = "M48,40 H208 V130 C208,180 172,214 128,228 C84,214 48,180 48,130 Z"        # heater (TAK-like)
SHIELD_B = "M72,36 H184 Q200,36 200,52 V128 Q200,192 128,226 Q56,192 56,128 V52 Q56,36 72,36 Z"  # rounded modern
SHIELD_C = "M128,28 L208,62 V142 L128,228 L48,142 V62 Z"                              # angular / hex
SHIELD_D = "M128,236 C68,176 44,146 44,110 A84,84 0 0 1 212,110 C212,146 188,176 128,236 Z"  # pin

# Eagle: spread wings, facing left. Local space roughly x∈[-100,100], y∈[-52,58]
EAGLE_BODY = ("M0,-22 C36,-52 74,-70 100,-64 "
              "L84,-50 L90,-38 L70,-30 L76,-18 L54,-12 L58,0 L36,2 L38,12 L18,12 L18,26 "
              "L26,50 L14,46 L8,58 L0,50 L-8,58 L-14,46 L-26,50 L-18,26 L-18,12 "
              "L-38,12 L-36,2 L-58,0 L-54,-12 L-76,-18 L-70,-30 L-90,-38 L-84,-50 L-100,-64 "
              "C-74,-70 -36,-52 0,-22 Z")
# pointed head: seen from below, beak leads in the direction of flight
EAGLE_HEAD = ("M0,-52 C6,-44 11,-36 11,-29 C11,-23 8,-18 5,-15 L0,-10 L-5,-15 "
              "C-8,-18 -11,-23 -11,-29 C-11,-36 -6,-44 0,-52 Z")
EAGLE_BEAK = "M0,-52 C3,-48 5,-44 6,-40 L-6,-40 C-5,-44 -3,-48 0,-52 Z"
EAGLE = EAGLE_BODY + " " + EAGLE_HEAD
EAGLE_EYES = ((-10, -34), (10, -34))
WING_R = ("M0,-22 C36,-52 74,-70 100,-64 L84,-50 L90,-38 L70,-30 L76,-18 L54,-12 L58,0 L36,2 L38,12 L18,12 L10,4 Z")

# Eagle head profile, larger, facing left. Local space x∈[-64,30], y∈[-56,60]
HEAD = ("M0,-56 C24,-56 42,-44 46,-26 L48,-6 L40,4 C40,16 32,26 20,32 L12,36 L0,58 L-12,36 L-20,32 "
        "C-32,26 -40,16 -40,4 L-48,-6 L-46,-26 C-42,-44 -24,-56 0,-56 Z")
HEAD_BEAK = "M-15,28 C-8,20 8,20 15,28 L0,58 Z"
HEAD_BROW = "M-42,-12 L-6,-2 L-36,6 Z M42,-12 L6,-2 L36,6 Z"

FEATHER = ("M0,-78 C34,-46 40,10 8,66 L0,74 L-8,66 C-40,10 -34,-46 0,-78 Z")

def eagle(x, y, s, fill, eye=None, extra=""):
    out = f'<g transform="translate({x},{y}) scale({s})"><path d="{EAGLE}" fill="{fill}" {extra}/>'
    if eye:  # contrast beak instead of eyes (eyes read as an owl)
        out += f'<path d="{EAGLE_BEAK}" fill="{eye}"/>'
    return out + "</g>"

def head(x, y, s, fill, eye=None, beak=None):
    out = f'<g transform="translate({x},{y}) scale({s})"><path d="{HEAD}" fill="{fill}"/>'
    if beak:
        out += f'<path d="{HEAD_BEAK}" fill="{beak}"/>'
    if eye:
        out += f'<path d="{HEAD_BROW}" fill="{eye}"/>'
    return out + "</g>"

def defs(uid):
    return f'''<defs>
    <linearGradient id="{uid}ink" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{INK2}"/><stop offset="1" stop-color="{INK}"/></linearGradient>
    <linearGradient id="{uid}paper" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{PAPER}"/><stop offset="1" stop-color="{PAPER2}"/></linearGradient>
    <linearGradient id="{uid}rust" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{RUST2}"/><stop offset="1" stop-color="{RUST}"/></linearGradient>
    <clipPath id="{uid}clipA"><path d="{SHIELD_A}"/></clipPath>
    <clipPath id="{uid}clipB"><path d="{SHIELD_B}"/></clipPath>
    <clipPath id="{uid}clipC"><path d="{SHIELD_C}"/></clipPath>
    <filter id="{uid}soft" x="-60%" y="-60%" width="220%" height="220%"><feGaussianBlur stdDeviation="3.6"/></filter>
  </defs>'''

FONT_SANS = "'Inter','Helvetica Neue',Arial,sans-serif"
FONT_MONO = "'SF Mono','JetBrains Mono',Menlo,Consolas,monospace"

def text(x, y, s, size, fill, weight=800, family=FONT_SANS, anchor="middle", spacing=0, extra=""):
    return (f'<text x="{x}" y="{y}" font-family="{family}" font-size="{size}" font-weight="{weight}" '
            f'fill="{fill}" text-anchor="{anchor}" letter-spacing="{spacing}" {extra}>{s}</text>')

# ---------- the 15 concepts ----------
def c01(u):  # Heraldic homage
    return f'''
    <path d="{SHIELD_A}" fill="{INK}" stroke="{WHITE}" stroke-width="10" stroke-linejoin="round"/>
    <path d="{SHIELD_A}" fill="none" stroke="{INK}" stroke-width="4" stroke-linejoin="round" transform="translate(128,128) scale(1.085) translate(-128,-128)"/>
    {eagle(128, 104, 0.72, WHITE, eye=RUST)}
    <g clip-path="url(#{u}clipA)"><rect x="40" y="150" width="176" height="42" fill="{RUST}"/></g>
    {text(128, 182, "RUSTAK", 30, WHITE, spacing=2)}'''

def c02(u):  # Rust gear ring
    return f'''
    <circle cx="128" cy="132" r="104" fill="none" stroke="{RUST}" stroke-width="16" stroke-dasharray="13 8.9"/>
    <circle cx="128" cy="132" r="94" fill="none" stroke="{RUST}" stroke-width="10"/>
    <g transform="translate(128,132) scale(0.62) translate(-128,-128)">
      <path d="{SHIELD_A}" fill="{INK}" stroke="{WHITE}" stroke-width="12" stroke-linejoin="round"/>
      {eagle(128, 132, 0.86, WHITE, eye=RUST)}
    </g>'''

def c03(u):  # Flat minimal
    return f'''
    <path d="{SHIELD_B}" fill="{RUST}"/>
    {eagle(128, 132, 0.66, WHITE)}'''

def c04(u):  # Split per bend
    return f'''
    <g clip-path="url(#{u}clipB)">
      <rect x="0" y="0" width="256" height="256" fill="{INK}"/>
      <path d="M0,256 L256,0 V256 Z" fill="{RUST}"/>
    </g>
    {eagle(128, 132, 0.66, WHITE)}'''

def c05(u):  # Monoline
    return f'''
    <path d="{SHIELD_B}" fill="none" stroke="{INK}" stroke-width="9" stroke-linejoin="round"/>
    <g transform="translate(128,128) scale(1.05)">
      <path d="{HEAD}" fill="none" stroke="{RUST}" stroke-width="7" stroke-linejoin="round"/>
      <path d="{HEAD_BEAK}" fill="{RUST}"/>
      <path d="{HEAD_BROW}" fill="{RUST}"/>
    </g>'''

def c06(u):  # mail-backup sibling: light shield, dark frame, rust diamond badge
    return f'''
    <path d="{SHIELD_B}" fill="url(#{u}paper)" stroke="url(#{u}ink)" stroke-width="15" stroke-linejoin="round"/>
    {eagle(122, 120, 0.6, INK, eye=RUST)}
    <g transform="translate(196,178)">
      <g transform="rotate(45)"><rect x="-38" y="-32" width="76" height="76" rx="17" fill="#000" opacity="0.22" filter="url(#{u}soft)"/></g>
      <g transform="rotate(45)"><rect x="-38" y="-38" width="76" height="76" rx="17" fill="url(#{u}rust)" stroke="{WHITE}" stroke-width="9"/></g>
      {text(0, 15, "R", 42, WHITE, weight=800, family=FONT_MONO)}
    </g>'''

def c07(u):  # Eagle head profile
    return f'''
    <path d="{SHIELD_A}" fill="{INK}" stroke="{WHITE}" stroke-width="8" stroke-linejoin="round"/>
    <path d="{SHIELD_A}" fill="none" stroke="{INK}" stroke-width="3" transform="translate(128,128) scale(1.07) translate(-128,-128)"/>
    {head(128, 130, 1.1, WHITE, eye=INK, beak=RUST)}'''

def c08(u):  # Pin shield
    return f'''
    <path d="{SHIELD_D}" fill="{RUST}"/>
    <circle cx="128" cy="112" r="58" fill="{WHITE}"/>
    {eagle(128, 116, 0.5, RUST, eye=INK)}'''

def c09(u):  # Reticle
    return f'''
    <path d="{SHIELD_B}" fill="{INK}"/>
    <g fill="none" stroke="{WHITE}" stroke-width="3" opacity="0.9">
      <circle cx="128" cy="126" r="58"/><circle cx="128" cy="126" r="30" stroke-width="2" opacity="0.6"/>
      <path d="M128,52 V78 M128,174 V200 M54,126 H80 M176,126 H202"/>
    </g>
    {eagle(128, 128, 0.58, RUST, eye=INK)}'''

def c10(u):  # Chevron wings
    chev = lambda y, w, c: f'<path d="M{128-w},{y+18} L128,{y} L{128+w},{y+18}" fill="none" stroke="{c}" stroke-width="16" stroke-linecap="round" stroke-linejoin="round"/>'
    return f'''
    <path d="{SHIELD_B}" fill="{INK}"/>
    {chev(74, 56, RUST2)}{chev(110, 56, RUST)}{chev(146, 56, WHITE)}'''

def c11(u):  # R monogram with wing
    return f'''
    <path d="{SHIELD_A}" fill="{RUST}"/>
    {text(112, 192, "R", 150, WHITE, weight=900)}
    <g transform="translate(134,98) scale(0.62)"><path d="{WING_R}" fill="{WHITE}"/></g>'''

def c12(u):  # Angular hex, faceted
    return f'''
    <path d="{SHIELD_C}" fill="{INK}"/>
    <path d="M128,28 L208,62 L128,96 L48,62 Z" fill="{RUST}"/>
    <path d="M48,62 L128,96 V228 L48,142 Z" fill="{INK2}"/>
    {eagle(128, 158, 0.6, WHITE, eye=RUST)}'''

def c13(u):  # Ribbon banner overhanging shield
    return f'''
    <path d="{SHIELD_B}" fill="url(#{u}paper)" stroke="{INK}" stroke-width="12" stroke-linejoin="round"/>
    {eagle(128, 106, 0.6, INK, eye=RUST)}
    <path d="M30,160 L44,182 L30,204 H56 V160 Z" fill="#B7301A"/>
    <path d="M226,160 L212,182 L226,204 H200 V160 Z" fill="#B7301A"/>
    <rect x="46" y="154" width="164" height="46" fill="{RUST}"/>
    <path d="M46,200 L56,210 V200 Z M210,200 L200,210 V200 Z" fill="#8A2312"/>
    {text(128, 188, "RUSTAK", 30, WHITE, spacing=3)}'''

def c14(u):  # Feather
    return f'''
    <path d="{SHIELD_B}" fill="{INK}"/>
    <g transform="translate(128,132) rotate(28) scale(0.95)">
      <path d="{FEATHER}" fill="{RUST}"/>
      <path d="M0,-70 L0,74" stroke="{INK}" stroke-width="5" stroke-linecap="round"/>
      <g stroke="{INK}" stroke-width="5" stroke-linecap="round"><path d="M-30,-4 L-6,-24 M-26,20 L-6,2 M28,-6 L4,-28 M22,26 L4,6"/></g>
    </g>'''

def c15(u):  # Great-seal: eagle carries a rust shield
    return f'''
    {eagle(128, 112, 1.0, INK, eye=RUST)}
    <g transform="translate(128,152) scale(0.36) translate(-128,-128)">
      <path d="{SHIELD_B}" fill="{RUST}" stroke="{WHITE}" stroke-width="20" stroke-linejoin="round"/>
      <g clip-path="url(#{u}clipB)"><path d="M56,36 H200 V96 H56 Z" fill="{INK}"/></g>
    </g>'''

CONCEPTS = [
    ("01", "Heraldic homage",   "TAK's double-rim heater shield, white eagle, rust name band", c01),
    ("02", "Rust gear",         "Shield set inside Rust's gear ring",                          c02),
    ("03", "Flat minimal",      "Single rust shield, single white eagle, nothing else",        c03),
    ("04", "Party per bend",    "Heraldic diagonal split, ink and rust",                       c04),
    ("05", "Monoline",          "Outline-only, works as a favicon or on dark",                 c05),
    ("06", "mail-backup sibling","Paper shield + dark frame + rust diamond badge, same family", c06),
    ("07", "Eagle head",        "Front-facing head fills the shield, rust beak",                 c07),
    ("08", "Map pin",           "Shield tapered into a location pin (situational awareness)",  c08),
    ("09", "Reticle",           "Rust eagle over a white reticle on ink",                      c09),
    ("10", "Chevron wings",     "Abstract three-bar wings, rust to white",                     c10),
    ("11", "R monogram",        "Bold R with a wing sweeping off the top",                     c11),
    ("12", "Faceted hex",       "Angular shield with a rust cap and shaded facets",            c12),
    ("13", "Ribbon banner",     "Paper shield with an overhanging rust ribbon",                c13),
    ("14", "Feather",           "Single rust feather on ink, eagle by metonymy",               c14),
    ("15", "Great seal",        "Ink eagle carries a small rust shield as its chest",          c15),
]

def tile_svg(num, fn, standalone=True):
    u = f"t{num}"
    body = f'{defs(u)}\n  {fn(u)}'
    if standalone:
        return f'<svg viewBox="0 0 256 256" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="rustak logo concept {num}">\n  {body}\n</svg>\n'
    return body

def board():
    cols, tw, th, pad = 5, 300, 380, 24
    rows = (len(CONCEPTS) + cols - 1) // cols
    W = cols * tw + pad * 2
    H = rows * th + pad * 2 + 60
    out = [f'<svg viewBox="0 0 {W} {H}" width="{W}" height="{H}" xmlns="http://www.w3.org/2000/svg" font-family="{FONT_SANS}">',
           f'<rect width="{W}" height="{H}" fill="#EDEDEF"/>',
           text(pad, 44, "rustak — logo concept board (round 1)", 26, INK, weight=700, anchor="start")]
    for i, (num, title, desc, fn) in enumerate(CONCEPTS):
        x = pad + (i % cols) * tw
        y = pad + 60 + (i // cols) * th
        u = f"t{num}"
        out.append(f'<g transform="translate({x},{y})">')
        out.append(f'<rect x="8" y="8" width="{tw-16}" height="{th-16}" rx="14" fill="{WHITE}"/>')
        out.append(f'<svg x="{(tw-256)/2}" y="24" width="256" height="256" viewBox="0 0 256 256">{defs(u)}{fn(u)}</svg>')
        out.append(text(24, 314, f"{num}  {title}", 17, INK, weight=700, anchor="start"))
        # wrap desc
        lines = textwrap.wrap(desc, 38)[:2]
        for j, l in enumerate(lines):
            out.append(text(24, 336 + j*18, l, 13, "#6B6E75", weight=400, anchor="start"))
        out.append('</g>')
    out.append('</svg>')
    return "\n".join(out)

if __name__ == "__main__":
    os.makedirs("tiles", exist_ok=True)
    for num, title, desc, fn in CONCEPTS:
        with open(f"tiles/{num}.svg", "w") as f:
            f.write(tile_svg(num, fn))
    with open("board.svg", "w") as f:
        f.write(board())
    print("ok")
