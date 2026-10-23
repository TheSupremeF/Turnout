#!/usr/bin/env node
//! Stage the `turnout` CLI where electron-builder will pick it up.
//!
//! The app is useless without the CLI, so packaging must fail loudly when no
//! built binary exists rather than shipping an installer that opens an error
//! dialog. This never builds Rust itself: a cross-compiled binary is fine, and
//! the release workflow builds natively on each platform.

import { copyFileSync, existsSync, mkdirSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const out = path.join(root, "dist", "release");

const name = process.platform === "win32" ? "turnout.exe" : "turnout";

/// Candidate locations, most preferred first: a native build for this platform
/// wins over a cross-compiled one.
function candidates() {
  const targetDirs = existsSync(path.join(root, "target"))
    ? readdirSync(path.join(root, "target")).filter((entry) => {
        try {
          return statSync(path.join(root, "target", entry)).isDirectory();
        } catch {
          return false;
        }
      })
    : [];
  const list = [
    path.join(root, "target", "release", name),
    path.join(root, "target", "debug", name),
  ];
  for (const dir of targetDirs) {
    list.push(path.join(root, "target", dir, "release", name));
    list.push(path.join(root, "target", dir, "debug", name));
  }
  return list;
}

const found = candidates().find((file) => existsSync(file));
if (!found) {
  console.error(
    [
      `No ${name} found. Build the CLI first:`,
      "",
      "  cargo build --release",
      "",
      "Then run this again.",
    ].join("\n"),
  );
  process.exit(1);
}

mkdirSync(out, { recursive: true });
const destination = path.join(out, name);
copyFileSync(found, destination);
if (process.platform !== "win32") {
  const { chmodSync } = await import("node:fs");
  chmodSync(destination, 0o755);
}
console.log(`Staged ${path.relative(root, found)} -> ${path.relative(root, destination)}`);
