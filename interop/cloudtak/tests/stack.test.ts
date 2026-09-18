/**
 * The stack's own coherence: the compose file, the configuration rustak is
 * given, and the certificate the two of them have to agree about.
 *
 * Docker is not available on every machine this is developed on, so the things
 * a broken stack would fail on at three in the morning — a port published in
 * one place and expected in another, a CA mounted somewhere
 * `NODE_EXTRA_CA_CERTS` does not name, a TLS mode that is not the one this
 * suite exists to exercise — are asserted here instead, from the text of the
 * files themselves.
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

import { renderConfig } from "../../shared/src/config.js";
import { serverAltNames } from "../src/pki.js";
import { configuration } from "../src/rustak.js";
import { CLOUDTAK_TAG, CONTAINER_TLS, PORTS, SUITE_ROOT } from "../src/settings.js";

const compose = fs.readFileSync(path.join(SUITE_ROOT, "docker-compose.yml"), "utf8");

/** The `${NAME:-default}` the compose file falls back to for a variable. */
function fallback(variable: string): string | undefined {
  return new RegExp(`\\$\\{${variable}:-([^}]*)\\}`).exec(compose)?.[1];
}

test("the compose file pins the CloudTAK image this suite was written against", () => {
  assert.match(compose, /image: ghcr\.io\/dfpc-coe\/cloudtak-api:/);

  if (process.env.RUSTAK_INTEROP_CLOUDTAK_TAG === undefined) {
    assert.equal(fallback("RUSTAK_INTEROP_CLOUDTAK_TAG"), CLOUDTAK_TAG);
  }
});

test("every published port has the same default on both sides", () => {
  const defaults: [string, number][] = [
    ["RUSTAK_INTEROP_WEBTAK_PORT", PORTS.webtak],
    ["RUSTAK_INTEROP_MARTI_PORT", PORTS.marti],
    ["RUSTAK_INTEROP_STREAM_PORT", PORTS.stream],
    ["RUSTAK_INTEROP_CLOUDTAK_PORT", PORTS.cloudtak],
  ];

  for (const [variable, expected] of defaults) {
    if (process.env[variable] !== undefined) continue;

    assert.equal(fallback(variable), String(expected), `${variable} disagrees with src/settings.ts`);
  }

  // The loopback, always: nothing in this stack should be reachable from
  // another machine on the network a developer happens to be on.
  for (const line of compose.split("\n").filter((entry) => /^\s+- "/.test(entry))) {
    assert.match(line, /127\.0\.0\.1:/, `a port is published beyond the loopback: ${line.trim()}`);
  }
});

test("the CA is mounted where NODE_EXTRA_CA_CERTS says it is", () => {
  const named = /NODE_EXTRA_CA_CERTS:\s*(\S+)/.exec(compose)?.[1];

  assert.ok(named, "the CloudTAK service must name a CA file — its webtak calls verify with no override");
  assert.match(compose, new RegExp(`\\./\\.run/pki/ca\\.crt:${named}:ro`));
});

test("rustak's data directory is the bind mount the bootstrap reads the setup token from", () => {
  assert.match(compose, /- \.\/\.run\/rustak:\/data/);
  assert.match(compose, /user: "\$\{RUSTAK_INTEROP_UID:-0\}:\$\{RUSTAK_INTEROP_GID:-0\}"/);
});

test("rustak serves the public listener from the generated files, not its internal CA", () => {
  const tables = configuration();

  assert.equal(tables["web.public.tls"].mode, "files");
  assert.equal(tables["web.public.tls"].cert_file, CONTAINER_TLS.cert);
  assert.equal(tables["web.public.tls"].key_file, CONTAINER_TLS.key);

  // Not under /data/pki: that is where rustak keeps the internal CA it issues
  // client certificates from, and this suite needs both to exist at once.
  assert.equal(String(CONTAINER_TLS.cert).startsWith("/data/pki/"), false);
});

test("all three of CloudTAK's URLs are served, and the credential it uses is enabled", () => {
  const tables = configuration();

  assert.deepEqual(tables["web.public"].listen, [":8446"]);
  assert.equal(tables["web.marti"].listen, ":8443");
  assert.equal(tables["web.marti"].client_cert, "required");
  assert.equal(tables["stream.tls"].listen, ":8089");
  assert.equal(tables.auth.client_passwords_enabled, true);

  // CloudTAK's setup wizard will not save a connection until /files/api/config
  // answers with an integer here.
  assert.equal(typeof tables.marti.upload_size_limit_mb, "number");
});

test("the relying party is a name the bootstrap can register a passkey against", () => {
  const tables = configuration();

  // WebAuthn binds a passkey to a domain and rustak accepts `localhost` on any
  // port, which is what lets the bootstrap run from the host against a
  // published port while CloudTAK reaches the same listener as `rustak`.
  assert.deepEqual(tables.server.domains, ["localhost", "rustak"]);
  assert.equal(tables.server.base_url, "https://localhost:8446");
  assert.equal(tables.server.data_dir, "/data");
});

test("the configuration renders as a file with no table defined twice", () => {
  const rendered = renderConfig(configuration({ auth: { anon_group_default: false } }));
  const tables = rendered.split("\n").filter((line) => line.startsWith("["));

  assert.equal(new Set(tables).size, tables.length, `a table is emitted twice:\n${rendered}`);
  assert.match(rendered, /anon_group_default = false/);
  assert.match(rendered, /client_passwords_enabled = true/);
});

test("the server certificate names both the service and the loopback", () => {
  const names = serverAltNames();

  assert.match(names, /DNS:rustak/);
  assert.match(names, /DNS:localhost/);
  assert.match(names, /IP:127\.0\.0\.1/);
});
