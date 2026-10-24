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

// The artwork carries its own tile, so no backdrop is added here. The icon is
// rendered in the light palette: a launcher icon sits on an unknown desktop
// background, so it must not depend on the viewer's colour scheme.
/// A Windows `.ico` holding several raster sizes.
///
/// Windows picks the size it needs (16 px in the taskbar, 256 px in Explorer),
/// so all of them must be present or it scales one badly. Entries store PNG
/// data, which Vista and later accept directly.
function encodeIco(pngs) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); // reserved
  header.writeUInt16LE(1, 2); // type: icon
  header.writeUInt16LE(pngs.length, 4);

  const directory = Buffer.alloc(pngs.length * 16);
  let offset = header.length + directory.length;
  pngs.forEach(({ size, data }, index) => {
    const at = index * 16;
    directory.writeUInt8(size >= 256 ? 0 : size, at); // width, 0 means 256
    directory.writeUInt8(size >= 256 ? 0 : size, at + 1); // height
    directory.writeUInt8(0, at + 2); // palette size
    directory.writeUInt8(0, at + 3); // reserved
    directory.writeUInt16LE(1, at + 4); // colour planes
    directory.writeUInt16LE(32, at + 6); // bits per pixel
    directory.writeUInt32LE(data.length, at + 8);
    directory.writeUInt32LE(offset, at + 12);
    offset += data.length;
  });

  return Buffer.concat([header, directory, ...pngs.map((entry) => entry.data)]);
}

const square = turnoutLogoSvg({ scheme: "light" });

mkdirSync(path.join(root, "build"), { recursive: true });
writeFileSync(path.join(root, "build", "icon.png"), render(square, 1024));
// Electron uses this as the window icon on Linux and in development.
writeFileSync(path.join(root, "electron", "icon.png"), render(square, 512));
// Docs: the light ink on the app's own backdrop, plus a transparent mark.
writeFileSync(path.join(root, "turnout-logo.svg"), square);
// Windows shell icon for the CLI executable.
mkdirSync(path.join(root, "crates", "relay-cli", "assets"), { recursive: true });
writeFileSync(
  path.join(root, "crates", "relay-cli", "assets", "turnout.ico"),
  encodeIco([16, 24, 32, 48, 64, 128, 256].map((size) => ({ size, data: render(square, size) }))),
);
// Tray/rail variant with no tile, for surfaces that supply their own backdrop.
writeFileSync(path.join(root, "turnout-logo-mark.svg"), turnoutLogoSvg({ tile: false }));
writeFileSync(path.join(root, "turnout-logo-dark.svg"), turnoutLogoSvg({ scheme: "dark" }));

console.log(
  "Wrote build/icon.png, electron/icon.png, turnout.ico, turnout-logo.svg, " +
    "turnout-logo-dark.svg, turnout-logo-mark.svg",
);
