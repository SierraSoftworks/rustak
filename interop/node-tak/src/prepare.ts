/**
 * The bootstrap child: everything that has to happen *after* the certificate
 * authority exists.
 *
 * `NODE_EXTRA_CA_CERTS` is read once, when a Node process starts. The runner
 * starts before rustak has generated its authority, so the runner can never
 * trust it — and turning verification off in the runner would mean the suite
 * proved nothing about the trust path CloudTAK actually walks. So the runner
 * starts the server, waits for `pki/ca.crt`, and then runs this file in a fresh
 * process with that file already in the trust store.
 *
 * It bootstraps the installation, probes which compatibility surfaces exist,
 * and writes the session the scenario processes read.
 *
 * Invoked as `prepare.ts <server.json> <session.json>`; not meant to be run by
 * hand.
 */

import fs from "node:fs";

import { bootstrap } from "./bootstrap.js";
import { probeSurfaces } from "./probe.js";
import type { ServerInfo } from "./rustak.js";
import { writeSession } from "./session.js";
import { SURFACE_NAMES } from "./surfaces.js";

const [serverFile, sessionFile] = process.argv.slice(2);

if (!serverFile || !sessionFile) {
  throw new Error("usage: prepare.ts <server.json> <session.json>");
}

const server = JSON.parse(fs.readFileSync(serverFile, "utf8")) as ServerInfo;
const { admin, client } = await bootstrap(server);
const surfaces = await probeSurfaces(server, admin.token);

writeSession(sessionFile, {
  urls: { webtak: server.webtak, api: server.api, stream: server.stream },
  dataDir: server.dataDir,
  caFile: server.caFile,
  admin,
  client,
  surfaces,
});

const served = SURFACE_NAMES.filter((name) => surfaces[name]);
const missing = SURFACE_NAMES.filter((name) => !surfaces[name]);

console.log(`[interop] bootstrapped ${admin.username} and minted a client password.`);
console.log(`[interop] surfaces served:  ${served.join(", ") || "(none yet)"}`);
console.log(`[interop] surfaces missing: ${missing.join(", ") || "(none)"}`);
