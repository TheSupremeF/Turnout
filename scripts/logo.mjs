// The Turnout mark, in one place.
//
// A railway turnout ("makas"): one track arrives and the switch sends it down
// one of two branches — what the product does to a model request.
//
// `icon.svg` is the source artwork. It styles itself through CSS classes and a
// `prefers-color-scheme` block, which does not survive being embedded (the class
// names would leak into the app's stylesheet, and resvg — used for the PNG icons
// — ignores the media query). `scripts/build-logo.mjs` resolves that to
// presentation attributes plus the colour tokens below.
//
// Regenerate after replacing the artwork:
//
//   node scripts/build-logo.mjs && node scripts/generate-assets.mjs

import { ARTWORK } from "./logo-artwork.mjs";

/// The artwork's own canvas size.
export const CANVAS = 512;

/// The source palette, plus the dark variant it declares.
export const PALETTES = {
  light: { bg: "#f6f6f8", primary: "#1a3bd1", secondary: "#b4b1a8" },
  dark: { bg: "#0d1017", primary: "#2f55ff", secondary: "#d6d3cc" },
};

/// Fill in the tokens. `bg` can be overridden to sit on a surface that already
/// supplies its own backdrop.
export function renderArtwork({ scheme = "light", bg } = {}) {
  const palette = { ...(PALETTES[scheme] || PALETTES.light) };
  if (bg) palette.bg = bg;
  return ARTWORK.replace(/__BG__/g, palette.bg)
    .replace(/__PRIMARY__/g, palette.primary)
    .replace(/__SECONDARY__/g, palette.secondary);
}

/// A complete `<svg>` element, for inline use (the GUI, the app icon).
export function TurnoutSymbol({ size = 78, scheme = "light", bg, title = "Turnout" } = {}) {
  return `<svg width="${size}" height="${size}" viewBox="0 0 ${CANVAS} ${CANVAS}" role="img" aria-label="${title}">
${renderArtwork({ scheme, bg })}
</svg>`;
}

/// A standalone SVG document, for `turnout-logo.svg` and icon rendering.
///
/// `tile: false` drops the rounded background, for surfaces that supply their
/// own (the dark sidebar, a printed page).
export function turnoutLogoSvg({ size = CANVAS, scheme = "light", tile = true, title = "Turnout" } = {}) {
  // The tile is removed rather than merely made transparent, so a host page
  // cannot show its own colour through the rounded corners.
  const artwork = tile
    ? renderArtwork({ scheme })
    : renderArtwork({ scheme }).replace(
        /\s*<rect fill="#[0-9a-fA-F]{3,8}" width="512" height="512" rx="112"\s*\/>/,
        "",
      );
  return `<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${CANVAS} ${CANVAS}" width="${size}" height="${size}" role="img" aria-label="${title}">
${artwork}
</svg>
`;
}
