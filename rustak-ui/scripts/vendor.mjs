// Copies the map's run-time JavaScript into the bundle Trunk is assembling.
//
// The map page is the only part of the console that needs anything but wasm:
// MapLibre GL draws it and milsymbol draws the MIL-STD-2525 symbols on it.
// Both are served by rustak itself rather than from a CDN, because a TAK
// server is routinely run on a network with no route to one. They are pinned
// in package.json, locked in package-lock.json, and copied — not bundled — so
// what the browser runs is byte for byte what the two projects published.
//
// Trunk runs this as a `post_build` hook with TRUNK_STAGING_DIR set. Run by
// hand it writes to ./dist, which is where Trunk would have moved it.

import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = join(process.env.TRUNK_STAGING_DIR ?? join(root, "dist"), "vendor");

// [package, [file in the package, ...]]. Only what the browser fetches, plus
// the licences: no source maps, no development builds, no type definitions.
const FILES = [
  [
    "maplibre-gl",
    [
      "dist/maplibre-gl.mjs",
      "dist/maplibre-gl-shared.mjs",
      "dist/maplibre-gl-worker.mjs",
      "dist/maplibre-gl.css",
      "LICENSE.txt",
    ],
  ],
  ["milsymbol", ["dist/milsymbol.js", "LICENSE"]],
];

const json = (path) => JSON.parse(readFileSync(path, "utf8"));
const pinned = json(join(root, "package.json")).dependencies;

// `npm ci` only when what is installed is not what is pinned, so that a
// rebuild under `trunk serve` costs a few file copies rather than a network
// round trip.
const current = Object.entries(pinned).every(([name, version]) => {
  const manifest = join(root, "node_modules", name, "package.json");
  return existsSync(manifest) && json(manifest).version === version;
});

if (!current) {
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  execFileSync(npm, ["ci", "--no-audit", "--no-fund"], { cwd: root, stdio: "inherit" });
}

for (const [name, files] of FILES) {
  for (const file of files) {
    const to = join(out, name, file.replace(/^dist\//, ""));
    mkdirSync(dirname(to), { recursive: true });
    cpSync(join(root, "node_modules", name, file), to);
  }
}
