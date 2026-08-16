/**
 * WCAG contrast ratio utilities.
 *
 * Calculates relative luminance and contrast ratios per WCAG 2.1 §1.4.3.
 * Used by the page builder editor to warn authors about low-contrast
 * background color choices.
 */

/**
 * Calculate WCAG relative luminance from linear RGB values.
 * @see https://www.w3.org/TR/WCAG21/#dfn-relative-luminance
 */
function relativeLuminance(r: number, g: number, b: number): number {
  const [rs, gs, bs] = [r, g, b].map((c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * rs + 0.7152 * gs + 0.0722 * bs;
}

/**
 * Parse a CSS hex color to [r, g, b].
 * Supports #rgb and #rrggbb formats.
 */
export function parseHexColor(color: string): [number, number, number] | null {
  if (!color.startsWith("#")) return null;
  const hex = color.slice(1);
  if (hex.length === 3) {
    return [
      parseInt(hex[0] + hex[0], 16),
      parseInt(hex[1] + hex[1], 16),
      parseInt(hex[2] + hex[2], 16),
    ];
  }
  if (hex.length === 6) {
    return [
      parseInt(hex.slice(0, 2), 16),
      parseInt(hex.slice(2, 4), 16),
      parseInt(hex.slice(4, 6), 16),
    ];
  }
  return null;
}

/**
 * Calculate WCAG contrast ratio between two hex colors.
 * Returns a value between 1 (identical) and 21 (black vs white).
 * Returns 0 if either color can't be parsed.
 */
export function contrastRatio(bg: string, fg: string): number {
  const bgRgb = parseHexColor(bg);
  const fgRgb = parseHexColor(fg);
  if (!bgRgb || !fgRgb) return 0;

  const l1 = relativeLuminance(...bgRgb);
  const l2 = relativeLuminance(...fgRgb);
  const lighter = Math.max(l1, l2);
  const darker = Math.min(l1, l2);
  return (lighter + 0.05) / (darker + 0.05);
}

/** WCAG AA minimum contrast for normal text. */
export const WCAG_AA_NORMAL = 4.5;

/** WCAG AA minimum contrast for large text (18pt+ or 14pt+ bold). */
export const WCAG_AA_LARGE = 3.0;

/**
 * Check whether a background color meets WCAG AA contrast against a text color.
 * Returns null if the colors can't be parsed (skip validation).
 */
export function checkContrast(
  backgroundColor: string,
  textColor: string
): { ratio: number; passes: boolean } | null {
  const ratio = contrastRatio(backgroundColor, textColor);
  if (ratio === 0) return null;
  return { ratio, passes: ratio >= WCAG_AA_NORMAL };
}
