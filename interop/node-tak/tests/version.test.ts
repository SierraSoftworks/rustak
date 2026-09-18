/**
 * `GET /Marti/api/version` — the cheapest authoritative "are my credentials
 * accepted" call, and the one node-tak's own `Certificate.probe()` is built
 * around.
 *
 * It is exercised twice, because rustak serves Marti on both listeners
 * (`compat/cloudtak.md` §1): once with a bearer token on `webtak`, and once
 * with an enrolled client certificate on the mutually authenticated `api`
 * listener, which is how CloudTAK checks a connection is still good.
 *
 * The third scenario is the redirect rule from `compat/cloudtak.md` §5: node-tak
 * treats any status below 400 as success and parses the body, so a `3xx` from a
 * Marti route is worse than an error — it is a silent, confusing failure
 * downstream.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { certificateClient, enroll, tokenClient } from "../src/client.js";
import { loadSession, unless, unlessAll } from "../src/session.js";

const session = loadSession();

test(
  "answers a bearer token with a plain-text version string",
  { skip: unless(session, "martiVersion") },
  async () => {
    const probe = await tokenClient(session).Certificate.probe();

    assert.equal(probe.accepted, true, `the server refused the administrator's token: ${JSON.stringify(probe)}`);
    assert.equal(typeof probe.version, "string");
    assert.match(
      String(probe.version),
      /^TAK Server /,
      "ATAK's ServerVersion parser expects the `TAK Server <version>` form",
    );
  },
);

test(
  "answers an enrolled client certificate on the mutually authenticated listener",
  { skip: unlessAll(session, "martiVersion", "tlsConfig", "oauthToken") },
  async () => {
    const enrolled = await enroll(session);
    const probe = await certificateClient(session, enrolled).Certificate.probe();

    assert.equal(probe.accepted, true, `the server refused the enrolled certificate: ${JSON.stringify(probe)}`);
    assert.match(String(probe.version), /^TAK Server /);
  },
);

test(
  "never answers a Marti route with a redirect",
  { skip: unless(session, "martiVersion") },
  async () => {
    // node-tak's status check is `status < 200 || status >= 400`, so a 3xx is
    // treated as success and its (empty) body is parsed as the payload. The
    // trailing slash is the classic way a framework produces one.
    for (const route of ["/Marti/api/version", "/Marti/api/version/"]) {
      const response = await fetch(new URL(route, session.urls.webtak), {
        method: "GET",
        redirect: "manual",
        headers: { Authorization: `Bearer ${session.admin.token}` },
      });

      assert.ok(
        response.status < 300 || response.status >= 400,
        `${route} answered ${response.status}, and node-tak would treat that as a parseable success`,
      );
    }
  },
);
