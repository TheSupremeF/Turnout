#!/usr/bin/env node
//! Generate the logo files that other tools need in raster or standalone form.
//!
//! The mark itself is defined once in `scripts/logo.mjs`; this script only
//! renders it. Run it after changing the symbol:
//!
//!   node scripts/generate-assets.mjs

import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { Resvg } from "@resvg/resvg-js";

import { turnoutLogoSvg } from "./logo.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/// macOS and Windows both want a square raster; 1024 px covers the largest
/// slot either uses and downsamples cleanly.
function render(svg, width) {
  return new Resvg(svg, {
    fitTo: { mode: "width", value: width },
    background: "rgba(0,0,0,0)",
  })
    .render()
    .asPng();
}

// The artwork carries its own navy tile, so no backdrop is added here.
const square = turnoutLogoSvg({ tile: true });

mkdirSync(path.join(root, "build"), { recursive: true });
writeFileSync(path.join(root, "build", "icon.png"), render(square, 1024));
// Electron uses this as the window icon on Linux and in development.
writeFileSync(path.join(root, "electron", "icon.png"), render(square, 512));
// Docs: the light ink on the app's own backdrop, plus a transparent mark.
writeFileSync(path.join(root, "turnout-logo.svg"), square);
// Tray/rail variant with no tile, for surfaces that supply their own backdrop.
writeFileSync(path.join(root, "turnout-logo-mark.svg"), turnoutLogoSvg({ tile: false }));

console.log("Wrote build/icon.png, electron/icon.png, turnout-logo.svg, turnout-logo-mark.svg");
