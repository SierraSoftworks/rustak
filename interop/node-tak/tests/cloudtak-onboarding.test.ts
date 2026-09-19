/**
 * `POST /api/v1/users/{username}/cloudtak-onboarding` — the one action that
 * produces everything CloudTAK's *Configure Server* page asks for.
 *
 * Every other scenario in this suite enrols the way CloudTAK does: a CSR built
 * from `GET /Marti/api/tls/config` and posted to `signClient/v2`, which leaves
 * the private key in the client. This one is the exception rustak makes for
 * CloudTAK itself, where the server generates the key and hands it over inside
 * a PKCS#12 — because CloudTAK takes an uploaded `.p12` and cannot enrol.
 *
 * # Why this belongs in *this* suite
 *
 * The file is only useful if CloudTAK can open it, and CloudTAK opens it with
 * `@tak-ps/node-p12` — which is `node-forge` underneath, reads PBES1 with 3DES
 * and a SHA-1 MAC, and reads nothing newer. That library is already here as a
 * dependency of `@tak-ps/node-tak`, so parsing the bundle with it is a direct
 * test of the thing that actually has to work, rather than of our own reader.
 * `rustak-server/tests/cloudtak_onboarding.rs` checks the same file from the
 * other side, without a network.
 *
 * The PEM that comes out then has to work as a client identity, so it goes
 * straight into the mutually authenticated `api` listener — the same
 * `Certificate.probe()` `version.test.ts` uses, with the certificate this
 * hand-over produced instead of an enrolled one.
 *
 * # One quirk of the library, not of the bundle
 *
 * `convertToPem` runs node-forge's output through `.replace(/\r\n/g, "")`,
 * which *removes* the line breaks rather than converting them — so the PEM it
 * returns is a single line and Node's own TLS refuses it with
 * `ERR_OSSL_PEM_NO_START_LINE`. That is the library's behaviour whatever
 * produced the file, so [`rewrap`] puts the lines back before the probe. It is
 * deliberately not worked around in the bundle: the bytes are correct, and a
 * server bending to a formatting bug in one consumer would be a server nobody
 * else could read.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { TAKAPI, APIAuthCertificate } from "@tak-ps/node-tak";
import { convertToPem } from "@tak-ps/node-p12";

import { loadSession, unlessAll } from "../src/session.js";
import type { Session } from "../src/session.js";

const session = loadSession();

/** The account the hand-over is prepared for. Created by `src/bootstrap.ts`. */
const ACCOUNT = session.client.username;

/** What the server answers with, as `rustak_api::CloudTakOnboarding`. */
interface Onboarding {
  readonly username: string;
  readonly password?: string;
  readonly p12_download_url: string;
  readonly p12_password: string;
  readonly urls: { readonly stream: string; readonly api: string; readonly webtak: string };
  readonly certificate_id: number;
  readonly credential_id: number;
  readonly expires_at: string;
}

/** Prepares a hand-over as an administrator. */
async function onboard(session: Session, request: unknown = {}): Promise<Onboarding> {
  const response = await fetch(
    new URL(`/api/v1/users/${encodeURIComponent(ACCOUNT)}/cloudtak-onboarding`, session.urls.webtak),
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${session.admin.token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(request),
    },
  );

  // Read once: `assert.equal` builds its message eagerly, so consuming the
  // body inside the message argument leaves nothing for `json()` to parse.
  const body = await response.text();

  assert.equal(response.status, 200, `onboarding answered ${response.status}: ${body}`);

  return JSON.parse(body) as Onboarding;
}

/**
 * Puts the line breaks back into a PEM block that `convertToPem` flattened.
 *
 * Works whether or not the input already has them, so it is safe if the
 * library is ever fixed.
 */
function rewrap(pem: string): string {
  const match = /-----BEGIN ([A-Z0-9 ]+)-----([\s\S]*?)-----END \1-----/.exec(pem);

  if (match === null) return pem;

  const label = match[1]!;
  const body = match[2]!.replace(/\s+/g, "");

  return [`-----BEGIN ${label}-----`, ...(body.match(/.{1,64}/g) ?? []), `-----END ${label}-----`, ""].join("\n");
}

/** Fetches a prepared keystore, returning the status alongside the bytes. */
async function collect(
  session: Session,
  url: string,
): Promise<{ status: number; bytes: Uint8Array }> {
  const response = await fetch(new URL(url, session.urls.webtak), {
    headers: { Authorization: `Bearer ${session.admin.token}` },
  });

  return {
    status: response.status,
    bytes: new Uint8Array(await response.arrayBuffer()),
  };
}

test(
  "hands CloudTAK a keystore its own parser can open, and a certificate the mTLS listener accepts",
  { skip: unlessAll(session, "cloudtakOnboarding", "martiVersion") },
  async () => {
    const onboarding = await onboard(session);

    assert.equal(onboarding.username, ACCOUNT);
    assert.ok(onboarding.password, "a minted client password is shown once");
    assert.ok(onboarding.p12_password.length > 0, "the keystore carries a passphrase");
    assert.match(onboarding.urls.stream, /^ssl:\/\/.+:\d+$/);
    assert.match(onboarding.urls.api, /^https:\/\/.+:\d+$/);
    assert.match(onboarding.urls.webtak, /^https:\/\/.+:\d+$/);

    const collected = await collect(session, onboarding.p12_download_url);

    assert.equal(collected.status, 200, "the prepared keystore downloads");
    assert.ok(collected.bytes.length > 0, "and is not empty");

    // The whole point: `@tak-ps/node-p12` is what CloudTAK itself calls, and it
    // reads only the legacy algorithms. A bundle written with PBES2 throws here.
    let pem;
    try {
      pem = convertToPem(collected.bytes, onboarding.p12_password);
    } catch (error) {
      assert.fail(
        `@tak-ps/node-p12 could not read the bundle, so CloudTAK could not either: ${String(error)}`,
      );
    }

    // node-p12 takes the *first* certificate bag and reads its common name, so
    // a chain led by the authority would give CloudTAK the CA's name for this
    // connection instead of the account's.
    assert.equal(
      pem.commonName,
      ACCOUNT,
      "the leaf must come first in the bundle, or CloudTAK names the connection after the CA",
    );
    assert.match(pem.pemCertificate, /BEGIN CERTIFICATE/);
    assert.match(pem.pemKey, /BEGIN (RSA )?PRIVATE KEY/);

    // And the pair actually authenticates, which is what CloudTAK does with it
    // next — the same probe `version.test.ts` runs against an enrolled cert.
    const probe = await new TAKAPI(
      new URL(onboarding.urls.api),
      new APIAuthCertificate(rewrap(pem.pemCertificate), rewrap(pem.pemKey)),
    ).Certificate.probe();

    assert.equal(
      probe.accepted,
      true,
      `the mutually authenticated listener refused the hand-over's certificate: ${JSON.stringify(probe)}`,
    );
    assert.match(String(probe.version), /^TAK Server /);
  },
);

test(
  "hands the keystore over exactly once",
  { skip: unlessAll(session, "cloudtakOnboarding") },
  async () => {
    const onboarding = await onboard(session);

    assert.equal((await collect(session, onboarding.p12_download_url)).status, 200);

    // A keystore that could be fetched twice is one a proxy log or a browser
    // history could be replayed from.
    const second = await collect(session, onboarding.p12_download_url);

    assert.equal(second.status, 410, "a second fetch is refused, not served");
  },
);

test(
  "echoes back the host and ports a published deployment actually uses",
  { skip: unlessAll(session, "cloudtakOnboarding") },
  async () => {
    // rustak inside a container, forwarded to other ports outside: CloudTAK has
    // to be told the outside numbers, and nothing on the server knows them.
    const onboarding = await onboard(session, {
      credential: "mint",
      host: "tak.published.example",
      ports: { stream: 28089, marti: 28443, public: 28446 },
    });

    assert.equal(onboarding.urls.stream, "ssl://tak.published.example:28089");
    assert.equal(onboarding.urls.api, "https://tak.published.example:28443");
    assert.equal(onboarding.urls.webtak, "https://tak.published.example:28446");
  },
);

test(
  "refuses a hand-over to anything but an administrator",
  { skip: unlessAll(session, "cloudtakOnboarding") },
  async () => {
    // The key is generated on the server here, so the authorisation on this
    // route is the whole of what keeps that defensible.
    const response = await fetch(
      new URL(
        `/api/v1/users/${encodeURIComponent(ACCOUNT)}/cloudtak-onboarding`,
        session.urls.webtak,
      ),
      { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" },
    );

    assert.equal(response.status, 401, "no session, no hand-over");
  },
);
