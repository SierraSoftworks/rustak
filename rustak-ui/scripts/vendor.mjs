// Copies the map's run-time JavaScript into the bundle Trunk is assembling.
//
// The map page is the only part of the console that needs anything but wasm:
// MapLibre GL draws it and milsymbol draws the MIL-STD-2525 symbols on it.
// Both are served by rustak itself rather than from a CDN, because a TAK
// server is routinely run on a network with no route to one. They are pinned
// in package.json, locked in package-lock.json, and copied — not bundled — so
// what the browser runs is byte for byte what the two projects published.
//
// The pickers that choose a marker's type and symbol search a catalogue of
// every symbol by name. Those two lists are the one thing here that is derived
// rather than copied: see the end of this file for what from, and why.
//
// Trunk runs this as a `post_build` hook with TRUNK_STAGING_DIR set. Run by
// hand it writes to ./dist, which is where Trunk would have moved it.

import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
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

// The symbol catalogues the map's pickers search: every MIL-STD-2525C
// warfighting symbol and every 2525D entity, each with the path of names that
// leads to it. They are derived rather than copied, because mil-std-2525 ships
// all three editions with their modifiers and tactical graphics in one 340 KB
// script, and a picker wants two lists of `[code, [name, ...]]`:
//
//   2525c.json  ["GUCI---", ...]  battle dimension + function id  (7 columns)
//   2525d.json  ["10121100", ...] symbol set + entity code        (8 digits)
//
// What is left out is what a point on a map cannot be: 2525C's tactical
// graphics, and 2525D's control measures (symbol set 25), which are lines and
// areas.
const tables = createRequire(import.meta.url)(join(root, "node_modules", "mil-std-2525", "milstd2525.js"));

// The tables carry the odd trailing space, which would make two branches of
// one name; and 2525D keeps a few rows "{Reserved for future use}", which are
// not symbols anybody can mean.
const tidy = (name) => name.trim().replace(/\s+/g, " ");
const real = ([, names]) => names.length > 0 && !names.some((name) => /^\{.*\}$/.test(name));

// 2525C is published in capitals. Sentence case reads better in a list, and
// an abbreviation in brackets — "(SOF)" — stays one.
const sentence = (name) =>
  (name.charAt(0) + name.slice(1).toLowerCase()).replace(/\(([^)]*)\)/g, (all) => all.toUpperCase());

const catalogues = {
  "2525c": Object.values(tables.ms2525c.WAR)
    .flatMap((group) => group.mainIcon ?? [])
    .map((icon) => [icon.battledimension + icon.functionid, icon.names.map(tidy).filter(Boolean).map(sentence)])
    .filter(real),
  "2525d": Object.values(tables.ms2525d)
    .filter((set) => set.symbolset !== "25")
    .flatMap((set) =>
      set.mainIcon.map((icon) => [
        set.symbolset + icon.Code,
        [set.name, icon.Entity, icon["Entity Type"], icon["Entity Subtype"]].map(tidy).filter(Boolean),
      ]),
    )
    .filter(real),
};

mkdirSync(join(out, "symbology"), { recursive: true });
for (const [edition, rows] of Object.entries(catalogues)) {
  writeFileSync(join(out, "symbology", `${edition}.json`), JSON.stringify(rows));
}
cpSync(join(root, "node_modules", "mil-std-2525", "LICENSE"), join(out, "symbology", "LICENSE"));
