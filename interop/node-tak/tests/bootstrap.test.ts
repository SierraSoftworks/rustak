/**
 * The harness's own contract: the parts of a CloudTAK bring-up that do not
 * depend on any Marti route existing.
 *
 * Every other scenario here is skipped until the brief that owns its endpoints
 * lands, and a suite where everything skips looks exactly like a suite that is
 * quietly broken. This one always runs, and proves the things the rest depend
 * on:
 *
 * - the internal certificate authority issues a certificate for the configured
 *   host, and Node verifies it through `NODE_EXTRA_CA_CERTS` — which is the
 *   single most common CloudTAK bring-up failure (`compat/cloudtak.md` §3) and
 *   the reason this suite runs the way it does;
 * - the `/api/v1` bootstrap produced a working administrator session;
 * - the credential CloudTAK will authenticate with is a **client password**,
 *   the compatibility-only kind rustak accepts on `/oauth/token` and the Marti
 *   enrollment endpoints and nowhere else;
 * - the probe reached a conclusion about every surface, and every missing one
 *   names the brief that will flip it.
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import { once } from "node:events";
import { test } from "node:test";
import tls from "node:tls";

import { loadSession } from "../src/session.js";
import { SURFACES, SURFACE_NAMES } from "../src/surfaces.js";

const session = loadSession();

test("Node verifies the server's chain against the internal authority", async () => {
  assert.equal(
    process.env.NODE_EXTRA_CA_CERTS,
    session.caFile,
    "the scenarios run with rustak's own authority in the trust store, as a CloudTAK container must",
  );

  const url = new URL(session.urls.webtak);
  const socket = tls.connect({
    host: url.hostname,
    port: Number(url.port),
    servername: url.hostname,
    // Explicit rather than inherited, so this asserts the chain itself rather
    // than the environment the other scenarios happen to run in.
    ca: [fs.readFileSync(session.caFile, "utf8")],
    rejectUnauthorized: true,
  });

  try {
    await once(socket, "secureConnect");

    assert.equal(socket.authorized, true, socket.authorizationError?.message);

    const certificate = socket.getPeerCertificate();

    assert.match(
      String(certificate.subjectaltname),
      /DNS:localhost/,
      "the listener has to present the name the suite reaches it by, or every verified call fails",
    );
    assert.match(String(certificate.issuer.CN), /rustak/i);
  } finally {
    socket.destroy();
  }
});

test("the bootstrap left a working administrator session", async () => {
  const response = await fetch(new URL("/api/v1/me", session.urls.webtak), {
    headers: { Authorization: `Bearer ${session.admin.token}` },
  });

  assert.equal(response.status, 200);

  const me = (await response.json()) as { username: string; is_admin: boolean };

  assert.equal(me.username, session.admin.username);
  assert.equal(me.is_admin, true);
});

test("the credential CloudTAK will use is a client password", async () => {
  assert.equal(session.client.username, session.admin.username);
  assert.ok(session.client.password.length > 0, "the secret is returned exactly once, at minting");

  const response = await fetch(new URL("/api/v1/credentials", session.urls.webtak), {
    headers: { Authorization: `Bearer ${session.admin.token}` },
  });

  assert.equal(response.status, 200);

  const held = (await response.json()) as Array<{ kind: string; expires_at?: string }>;
  const password = held.find((credential) => credential.kind === "client_password");

  assert.ok(password, `no client password was minted; the account holds ${JSON.stringify(held)}`);
  assert.ok(
    password.expires_at,
    "a client password expires; it is the only reusable secret rustak issues",
  );
});

test("every surface was probed, and every missing one names what will flip it", () => {
  for (const name of SURFACE_NAMES) {
    assert.equal(
      typeof session.surfaces[name],
      "boolean",
      `the runner reached no conclusion about \`${name}\``,
    );

    if (!session.surfaces[name]) {
      assert.match(
        SURFACES[name].todo,
        /^TODO\((M\d(-\d\d)?)\): /,
        `\`${name}\` is skipped without naming the milestone that will serve it`,
      );
    }
  }
});
