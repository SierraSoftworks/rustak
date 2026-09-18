/**
 * Loading the fixtures the parser tests run against.
 *
 * They are ours: hand-written from the response shapes in CloudTAK's own route
 * definitions and `@tak-ps/node-tak`'s types, not captures of a running system,
 * and nothing in them came from any GPL source (`conventions.md` → Licensing).
 * The point of them is that a machine with no Docker can still hold the runner
 * to the whole contract — which is the only thing this suite can be developed
 * against on a laptop.
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

/** The directory the fixtures live in. */
export const FIXTURES = path.resolve(here, "..", "fixtures");

/** One fixture, as text. */
export function text(name: string): string {
  return fs.readFileSync(path.join(FIXTURES, name), "utf8");
}

/** One fixture, parsed — which is what the runner's parsers are handed. */
export function json(name: string): unknown {
  return JSON.parse(text(name));
}
