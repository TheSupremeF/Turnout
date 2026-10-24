#!/usr/bin/env node
//! Regenerate `scripts/logo-artwork.mjs` from `icon.svg`.
//!
//! `icon.svg` styles itself with CSS classes and a `prefers-color-scheme`
//! block. Neither survives being embedded: the class names (`.p`, `.s`, `.bg`)
//! would collide with the app's own stylesheet, and resvg — which renders the
//! PNG icons — ignores media queries, so every raster would be light-mode.
//!
//! This resolves the classes to presentation attributes and replaces the
//! colours with `__TOKEN__` placeholders that `logo.mjs` fills in. Run it after
//! replacing `icon.svg`, then run `generate-assets.mjs`.

import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = path.join(root, "icon.svg");

/// Class -> presentation attributes. Mirrors the `<style>` block in icon.svg;
/// keep them in step when the artwork changes.
const CLASS_ATTRIBUTES = {
  bg: 'fill="__BG__"',
  pf: 'fill="__BG__" stroke="__PRIMARY__" stroke-width="8"',
  sf: 'fill="__BG__" stroke="__SECONDARY__" stroke-width="8"',
  ss: 'fill="__SECONDARY__" opacity=".6"',
  ps: 'fill="__PRIMARY__" opacity=".6"',
};
const RAIL = 'fill="none" stroke-width="11" stroke-linecap="butt" stroke-linejoin="round"';
const RAIL_COLOURS = { p: "__PRIMARY__", s: "__SECONDARY__" };

const svg = readFileSync(source, "utf8");
const inner = svg.slice(svg.indexOf(">", svg.indexOf("<svg")) + 1, svg.lastIndexOf("</svg>"));
const body = inner.replace(/<style>[\s\S]*?<\/style>/, "").trim();

const converted = body.replace(
  /<(\w+)([^>]*?)\s+class="([^"]+)"([^>]*?)(\/?)>/g,
  (match, tag, before, classes, after, selfClose) => {
    const list = classes.trim().split(/\s+/);
    let attributes;
    if (list.includes("rail")) {
      const colour = list.find((name) => name in RAIL_COLOURS);
      if (!colour) throw new Error(`rail without a colour class: ${classes}`);
      attributes = `${RAIL} stroke="${RAIL_COLOURS[colour]}"`;
    } else {
      const known = list.filter((name) => name in CLASS_ATTRIBUTES);
      if (known.length === 0) throw new Error(`unknown class: ${classes}`);
      attributes = known.map((name) => CLASS_ATTRIBUTES[name]).join(" ");
    }
    return `<${tag}${before} ${attributes}${after}${selfClose ? "/>" : ">"}`;
  },
);

if (converted.includes('class="')) {
  throw new Error("icon.svg still has a class attribute that was not converted");
}
const tokens = new Set(converted.match(/__[A-Z]+__/g) || []);
if (tokens.size === 0) throw new Error("no colour tokens were emitted");

writeFileSync(
  path.join(root, "scripts", "logo-artwork.mjs"),
  `// GENERATED from icon.svg by scripts/build-logo.mjs — do not edit by hand.\n` +
    `//\n` +
    `// CSS classes resolved to presentation attributes, colours replaced by\n` +
    `// tokens that \`logo.mjs\` fills in. Rendering this verbatim works in a\n` +
    `// browser, a data URL and resvg, which ignores prefers-color-scheme.\n\n` +
    `export const ARTWORK = \`\n${converted}\n\`;\n`,
);

console.log(
  `Wrote scripts/logo-artwork.mjs (${[...tokens].sort().join(", ")}), ` +
    `${(converted.match(/<(path|rect|circle|g)\b/g) || []).length} elements`,
);
