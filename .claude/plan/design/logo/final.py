#!/usr/bin/env python3
"""Final rustak mark: B4 (gradient + oversized eagle) with inset rim. Emits logo.svg and icon.svg."""
from gen import *
from gen3 import mark

def mark_svg():
    return f'''<svg viewBox="0 0 256 256" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="rustak logo">
  {defs("")}
  {mark("", rim="inset", gradient=True, oversized=True)}
</svg>
'''

def lockup_svg():
    return f'''<svg viewBox="0 0 620 200" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="rustak logo with wordmark">
  {defs("")}
  <g transform="translate(20,6) scale(0.72)">
    {mark("", rim="inset", gradient=True, oversized=True)}
  </g>
  <text x="222" y="118" font-family="{FONT_MONO}" font-size="54" font-weight="700">
    <tspan fill="{RUST}">rust</tspan><tspan fill="{INK}">ak</tspan>
  </text>
</svg>
'''

if __name__ == "__main__":
    open("logo.svg", "w").write(mark_svg())
    open("icon.svg", "w").write(lockup_svg())
    # preview sheet: mark on light/dark at several sizes + lockup
    m = mark("p", rim="inset", gradient=True, oversized=True)
    out = [f'<svg viewBox="0 0 1000 560" xmlns="http://www.w3.org/2000/svg" font-family="{FONT_SANS}">',
           '<rect width="500" height="560" fill="#F7F7F7"/><rect x="500" width="500" height="560" fill="#1A1C20"/>']
    for bx in (0, 500):
        u = f"p{bx}"
        out.append(f'<svg x="{bx+40}" y="40" width="256" height="256" viewBox="0 0 256 256">{defs(u)}{mark(u, rim="inset", gradient=True, oversized=True)}</svg>')
        sx = bx + 320
        for sz in (96, 48, 24):
            out.append(f'<svg x="{sx}" y="{296-sz}" width="{sz}" height="{sz}" viewBox="0 0 256 256">{defs(u+str(sz))}{mark(u+str(sz), rim="inset", gradient=True, oversized=True)}</svg>')
            sx += sz + 16
        fg, fg2 = (RUST, INK) if bx == 0 else (RUST2, WHITE)
        out.append(f'<svg x="{bx+40}" y="340" width="420" height="136" viewBox="0 0 620 200">{defs(u+"l")}<g transform="translate(20,6) scale(0.72)">{mark(u+"l", rim="inset", gradient=True, oversized=True)}</g>'
                   f'<text x="222" y="118" font-family="{FONT_MONO}" font-size="54" font-weight="700"><tspan fill="{fg}">rust</tspan><tspan fill="{fg2}">ak</tspan></text></svg>')
    out.append('</svg>')
    open("preview.svg", "w").write("\n".join(out))
    print("ok")
