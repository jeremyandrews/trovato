#!/usr/bin/env python3
"""Trovato — final logo asset generator.
Reproducible master: every asset below derives from MARK geometry + WORDMARK letterpaths.
Palette: INK #221B16 | CLAY #B14B2E | CLAY_LIGHT #E08963 (dot on dark) | CREAM #FAF5EF | PEACH #FFD9A8 (dot on clay)
"""

INK   = "#221B16"
CLAY  = "#B14B2E"
CLAYL = "#E08963"
CREAM = "#FAF5EF"
PEACH = "#FFD9A8"

SW  = 30            # mark stroke width (in 240 viewBox)
DR  = 19            # found-dot radius
DOT = (155, 143)    # found-dot center — even clearance in the nook

STEM  = "M 104 54 L 104 140 Q 104 190 156 190"
CROSS = "M 64 96 L 142 96"

def mark_group(ink, dot, sw=SW, dr=DR):
    return f'''  <g fill="none" stroke="{ink}" stroke-width="{sw}" stroke-linecap="round" stroke-linejoin="round">
    <path d="{STEM}"/>
    <path d="{CROSS}"/>
  </g>
  <circle cx="{DOT[0]}" cy="{DOT[1]}" r="{dr}" fill="{dot}"/>'''

def svg240(body, label):
    return f'''<svg viewBox="0 0 240 240" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="{label}">
{body}
</svg>'''

A = "Trovato mark: a lowercase t cradling a found dot"
open('trovato-mark.svg','w').write(svg240(mark_group(INK, CLAY), A))
open('trovato-mark-mono-black.svg','w').write(svg240(mark_group('#000000','#000000'), A))
open('trovato-mark-mono-white.svg','w').write(svg240(mark_group('#FFFFFF','#FFFFFF'), A))
open('trovato-mark-on-dark.svg','w').write(svg240(mark_group('#FFFFFF', CLAYL), A))

# ---------- wordmark ----------
def circle_path(cx, cy, r):
    return f'M {cx-r} {cy} a {r} {r} 0 1 0 {2*r} 0 a {r} {r} 0 1 0 {-2*r} 0'

def build_wordmark():
    paths = []; x = 22; gap = 22
    def t_(x):
        return [f'M {x+14} 60 L {x+14} 156 Q {x+14} 200 {x+56} 200',
                f'M {x-12} 98 L {x+50} 98'], 58
    p,w = t_(x); paths += p; x += w + gap
    paths += [f'M {x} 104 L {x} 200', f'M {x} 152 Q {x} 104 {x+42} 104']; x += 50 + gap
    paths.append(circle_path(x+48, 152, 48)); x += 96 + gap
    paths.append(f'M {x} 104 L {x+38} 198 L {x+76} 104'); x += 76 + gap
    paths.append(circle_path(x+46, 154, 46)); paths.append(f'M {x+94} 104 L {x+94} 200'); x += 94 + gap
    p,w = t_(x); paths += p; x += w + gap
    paths.append(circle_path(x+48, 152, 48)); x += 96 + 24
    return '\n'.join(f'    <path d="{p}"/>' for p in paths), x

wm_d, wm_w = build_wordmark()

def wordmark_svg(ink):
    return f'''<svg viewBox="0 0 {wm_w} 260" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="trovato wordmark">
  <g fill="none" stroke="{ink}" stroke-width="24" stroke-linecap="round" stroke-linejoin="round">
{wm_d}
  </g>
</svg>'''
open('trovato-wordmark.svg','w').write(wordmark_svg(INK))
open('trovato-wordmark-white.svg','w').write(wordmark_svg('#FFFFFF'))

# ---------- lockups ----------
GAP = 78
def lockup(ink, dot, wm_ink):
    return f'''<svg viewBox="0 0 {186 + GAP + wm_w} 260" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="Trovato logo">
  <g transform="translate(-34 8)">
{mark_group(ink, dot)}
  </g>
  <g transform="translate({186 + GAP - 34}, 0)" fill="none" stroke="{wm_ink}" stroke-width="24" stroke-linecap="round" stroke-linejoin="round">
{wm_d}
  </g>
</svg>'''
open('trovato-lockup-horizontal.svg','w').write(lockup(INK, CLAY, INK))
open('trovato-lockup-horizontal-white.svg','w').write(lockup('#FFFFFF', CLAYL, '#FFFFFF'))

stacked = f'''<svg viewBox="0 0 {wm_w} 480" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="Trovato logo, stacked">
  <g transform="translate({round(wm_w/2 - 120)} -10)">
{mark_group(INK, CLAY)}
  </g>
  <g transform="translate(0 230)" fill="none" stroke="{INK}" stroke-width="24" stroke-linecap="round" stroke-linejoin="round">
{wm_d}
  </g>
</svg>'''
open('trovato-lockup-stacked.svg','w').write(stacked)

# ---------- app icon & favicon tile ----------
def tile(rx_ratio, white, dotc, grad=True):
    fill = 'url(#bg)' if grad else CLAY
    defs = f'''  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#C25A38"/>
      <stop offset="1" stop-color="#96421F"/>
    </linearGradient>
  </defs>
''' if grad else ''
    return f'''<svg viewBox="0 0 512 512" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="Trovato icon">
{defs}  <rect width="512" height="512" rx="{round(512*rx_ratio)}" fill="{fill}"/>
  <g transform="translate(256 262) scale(1.6) translate(-121 -122)">
    <g fill="none" stroke="{white}" stroke-width="{SW}" stroke-linecap="round" stroke-linejoin="round">
      <path d="{STEM}"/>
      <path d="{CROSS}"/>
    </g>
    <circle cx="{DOT[0]}" cy="{DOT[1]}" r="{DR+1}" fill="{dotc}"/>
  </g>
</svg>'''
open('trovato-appicon.svg','w').write(tile(0.225, '#FFF7F0', PEACH, grad=True))
open('trovato-favicon.svg','w').write(tile(0.22, '#FFFFFF', PEACH, grad=False))

print("wordmark width:", wm_w)
