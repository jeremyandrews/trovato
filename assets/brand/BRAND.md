# Trovato — Logo & Brand Sheet

*Trovato* is Italian for **found**. The logo is a rounded, monoline lowercase **t** cradling a small clay dot in the crook of its foot — the thing you were looking for, found and held. It is a sibling of the Jesse mark (`j` + `>`): same thick rounded strokes, same lowercase friendliness, its own story.

## The mark

- Lowercase monoline **t**, stroke weight 30/240, round caps and joins.
- Foot curves into an open scoop (no upturn — an upturned tip crowds the dot).
- The **found-dot** (r 19) sits in the nook bounded by stem, crossbar and scoop, at (155, 143) in the 240 viewBox — even clearance on all three sides. It must **never touch** the strokes and never wander outside the nook.
- The dot is the only element that takes the accent color. In monochrome contexts the dot is the same color as the t — the mark is designed mono-first and survives it.

## Palette

| Role | Name | Hex |
|---|---|---|
| Primary ink | Ink | `#221B16` |
| Accent (dot, buttons) | Clay | `#B14B2E` |
| Dot on dark backgrounds | Clay Light | `#E08963` |
| Dot on clay backgrounds | Peach | `#FFD9A8` |
| Warm background | Cream | `#FAF5EF` |
| App-icon gradient | `#C25A38 → #96421F` (vertical) |

Why clay/terracotta: Italian roots (the name, the terracotta of Tuscany), a quiet nod to Rust (the language Trovato is built in), and clear separation from the CMS field's blues and purples (WordPress, Drupal, Strapi) without landing on Craft's bright red-orange.

## Wordmark

`trovato` — custom rounded monoline lettering, drawn as SVG paths (no font dependency, nothing to license or install). Stroke 24, round caps. All lowercase, always. Do not typeset the wordmark in a font; use the provided paths.

## Lockups

- **Horizontal** (`trovato-lockup-horizontal.svg`): mark + wordmark, gap locked at 78/240 units — never tighten it or the mark reads as a letter of the word ("ttrovato").
- **Stacked** (`trovato-lockup-stacked.svg`): mark centered above wordmark. For squarish spaces, social avatars with captions, tee backs.
- Clearspace around any lockup: at least the width of the t's crossbar on all sides.

## Icons

- **App icon** (`trovato-appicon.svg`): white t + peach dot on clay gradient tile.
- **Favicon** (`trovato-favicon.svg` + `png/favicon-16/32/64`): flat clay tile, white t, peach dot. At 16 px a tile survives where a bare glyph turns to mush — always ship the tile version as favicon.
- The bare mark may be used at 32 px and above; below that, use the tile.

## Do / Don't

- **Do** use ink-on-light or white-on-dark with the clay/clay-light dot.
- **Do** use the all-one-color mono versions for engraving, embroidery, stamps.
- **Don't** let the dot touch a stroke, move the dot, add more dots, outline the mark, add gradients to strokes, rotate, or squeeze the lockup gap.
- **Don't** set the wordmark in Comic Sans. Or any font. It's paths.

## Files

```
trovato-mark.svg                  ink t + clay dot (primary mark)
trovato-mark-mono-black.svg       one-color black
trovato-mark-mono-white.svg       one-color white
trovato-mark-on-dark.svg          white t + clay-light dot (for dark bg)
trovato-wordmark.svg / -white     custom lettering
trovato-lockup-horizontal.svg / -white
trovato-lockup-stacked.svg
trovato-appicon.svg               gradient tile, rx 22.5%
trovato-favicon.svg               flat tile
png/                              transparent PNG exports incl. favicon 16/32/64,
                                  app icon 1024/180, mark 1024/512, lockups 2000w
gen_final.py                      regenerates every SVG from geometry constants —
                                  the single source of truth; edit and re-run to tweak
```

All SVGs are hand-authored viewBox vectors — they scale losslessly to billboard size and minify cleanly.
