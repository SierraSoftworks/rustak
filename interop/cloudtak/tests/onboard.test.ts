/**
 * The CloudTAK hand-over's parsers, offline.
 *
 * `src/onboard.ts` drives `POST /api/v1/users/{username}/cloudtak-onboarding`
 * and turns the PKCS#12 it produces into the PEM pair CloudTAK's
 * `PATCH /api/server` stores. The network and `openssl` halves need the stack;
 * these are the parts that do not, and they are the parts where a wrong answer
 * is a confusing failure three layers away rather than an obvious one.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { onboardingPath, p12ReadArgs, parseOnboarding, splitPem } from "../src/onboard.js";
import { json, text } from "./fixtures.js";

test("the route carries the account in its path, escaped", () => {
  assert.equal(
    onboardingPath("cloudtak-operator"),
    "/api/v1/users/cloudtak-operator/cloudtak-onboarding",
  );
  assert.equal(
    onboardingPath("a b"),
    "/api/v1/users/a%20b/cloudtak-onboarding",
    "a name is escaped rather than pasted into a path",
  );
});

test("a hand-over is read for the download, the passphrase and the three URLs", () => {
  const onboarding = parseOnboarding(text("cloudtak-onboarding.json"));

  assert.equal(onboarding.username, "cloudtak-operator");
  assert.equal(onboarding.password, "H4NK-3D0V-2R9M-QTWX");
  assert.match(onboarding.p12DownloadUrl, /^\/api\/v1\/cloudtak-onboarding\/[A-Za-z0-9_-]+\.p12$/);
  assert.equal(onboarding.p12Password, "kR3nQ8vLmZ2dY1xP");
  assert.equal(onboarding.urls.stream, "ssl://rustak:8089");
  assert.equal(onboarding.urls.api, "https://rustak:8443");
  assert.equal(onboarding.urls.webtak, "https://rustak:8446");
});

test("a reused credential leaves the password out rather than inventing one", () => {
  // The server keeps only an argon2id hash of a client password, so a
  // hand-over that reused one has nothing to show. Reading `""` here and
  // configuring CloudTAK with it would fail at login with no clue why.
  const body = json("cloudtak-onboarding.json") as Record<string, unknown>;
  delete body.password;

  assert.equal(parseOnboarding(JSON.stringify(body)).password, undefined);
});

test("a hand-over missing anything CloudTAK needs is named, not returned half-built", () => {
  const complete = json("cloudtak-onboarding.json") as Record<string, unknown>;

  for (const [field, matcher] of [
    ["p12_download_url", /p12_download_url/],
    ["p12_password", /p12_password/],
  ] as const) {
    const body = { ...complete };
    delete body[field];

    assert.throws(() => parseOnboarding(JSON.stringify(body)), matcher, field);
  }

  for (const url of ["stream", "api", "webtak"]) {
    const body = { ...complete, urls: { ...(complete.urls as object) } as Record<string, unknown> };
    delete body.urls[url];

    assert.throws(() => parseOnboarding(JSON.stringify(body)), new RegExp(`'${url}' URL`), url);
  }

  assert.throws(() => parseOnboarding("<html>not json</html>"), /did not answer JSON/);
  assert.throws(() => parseOnboarding("[]"), /not an object/);
});

test("the keystore is read with the legacy provider, unencrypted", () => {
  // Both flags are load-bearing: the bundle is deliberately PBES1/3DES, which
  // OpenSSL 3 refuses without `-legacy`, and CloudTAK's `auth.key` has to be a
  // key it can use without a passphrase.
  const args = p12ReadArgs("/tmp/admin.p12", "s3cret");

  assert.ok(args.includes("-legacy"), args.join(" "));
  assert.ok(args.includes("-nodes"), args.join(" "));
  assert.ok(args.includes("pass:s3cret"), "the passphrase is passed rather than prompted for");
});

test("the leaf comes out first and the rest becomes the chain", () => {
  // `openssl pkcs12` prints bag attributes between the blocks and prints the
  // leaf before the authorities. Taking the wrong one would hand CloudTAK the
  // CA as its client identity, which fails at the handshake with nothing to
  // read.
  const split = splitPem(
    [
      "Bag Attributes",
      "    friendlyName: cloudtak",
      "-----BEGIN CERTIFICATE-----",
      "TEFG",
      "-----END CERTIFICATE-----",
      "Bag Attributes: <No Attributes>",
      "-----BEGIN CERTIFICATE-----",
      "Q0E=",
      "-----END CERTIFICATE-----",
      "Key Attributes: <No Attributes>",
      "-----BEGIN PRIVATE KEY-----",
      "S0VZ",
      "-----END PRIVATE KEY-----",
      "",
    ].join("\n"),
  );

  assert.match(split.cert, /TEFG/);
  assert.equal(split.ca.length, 1);
  assert.match(split.ca[0]!, /Q0E=/);
  assert.match(split.key, /BEGIN PRIVATE KEY/);
  assert.ok(!split.cert.includes("Bag Attributes"), "the attributes are not part of the PEM");
});

test("a keystore missing either half is refused with which half", () => {
  assert.throws(
    () => splitPem("-----BEGIN CERTIFICATE-----\nTEFG\n-----END CERTIFICATE-----"),
    /no private key/,
  );
  assert.throws(
    () => splitPem("-----BEGIN PRIVATE KEY-----\nS0VZ\n-----END PRIVATE KEY-----"),
    /no certificate/,
  );
});
