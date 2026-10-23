//! The Turnout symbol, in one place.
//!
//! A railway turnout ("makas"): one track arrives, splits, and the switch
//! decides which branch every train takes — exactly what the product does to a
//! model request.
//!
//! Two inks are defined because the mark is used on both surfaces: pale on the
//! dark GUI rail, dark on the light icon backdrop. Drawing the pale version on
//! a light background makes the through-track disappear, which is why the ink
//! is a parameter rather than hardcoded stroke colors.
//!
//! Consumed by the GUI (`src/App.jsx`), the Electron icon and the README. Do not
//! redraw it elsewhere.

const GEOMETRY = {
  straight: "M104 328H408",
  branch: "M104 328H196C290 328 306 184 408 184",
  nodes: [
    { cx: 104, cy: 328 },
    { cx: 408, cy: 328 },
    { cx: 408, cy: 184, accent: true },
  ],
  strokeWidth: 32,
  nodeRadius: 26,
  nodeStroke: 16,
};

export const INKS = {
  /// For the dark sidebar: pale track, blue branch, dark node centers.
  dark: { base: "#E5E9F0", accent: "#6EA0FF", fill: "#14233F" },
  /// For light backgrounds (icon, README on white, printed docs).
  light: { base: "#1B2B4A", accent: "#2F6BFF", fill: "#F4F5F8" },
};

/// The drawing without an outer `<svg>`; assumes a 0 0 512 512 viewBox.
export function TurnoutSymbolBody({ ink = "dark" } = {}) {
  const colors = INKS[ink] || INKS.dark;
  const nodes = GEOMETRY.nodes
    .map((node) => {
      const stroke = node.accent ? colors.accent : colors.base;
      return `<circle cx="${node.cx}" cy="${node.cy}" r="${GEOMETRY.nodeRadius}" fill="${colors.fill}" stroke="${stroke}" stroke-width="${GEOMETRY.nodeStroke}" />`;
    })
    .join("\n    ");
  return `  <g transform="rotate(-90 256 256)" fill="none" stroke-linecap="round" stroke-linejoin="round" stroke-width="${GEOMETRY.strokeWidth}">
    <path d="${GEOMETRY.straight}" stroke="${colors.base}" />
    <path d="${GEOMETRY.branch}" stroke="${colors.accent}" />
  </g>
  <g>
    ${nodes}
  </g>`;
}

/// A complete `<svg>` element, for inline use (the GUI, the app icon).
export function TurnoutSymbol({ size = 78, title = "Turnout", ink = "dark" } = {}) {
  return `<svg width="${size}" height="${size}" viewBox="0 0 512 512" role="img" aria-label="${title}">
${TurnoutSymbolBody({ ink })}
</svg>`;
}

/// A standalone SVG document, for `turnout-logo.svg` and icon rendering.
export function turnoutLogoSvg({ size = 512, ink = "light", background = null } = {}) {
  const backdrop = background
    ? `<rect width="512" height="512" rx="112" fill="${background}" />`
    : "";
  return `<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 512 512" role="img" aria-label="Turnout">
${backdrop}
${TurnoutSymbolBody({ ink })}
</svg>
`;
}

/// Raw geometry for renderers that cannot take SVG markup.
export const TURNOUT_SYMBOL_PATHS = GEOMETRY;
