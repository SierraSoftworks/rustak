/**
 * rustak's own OpenID provider documents, read by a **real relying-party
 * library**.
 *
 * `rustak-server/tests/oidc_provider.rs` already drives the whole browser flow
 * against the in-process server, with a fake identity provider standing in for
 * the upstream one. What it cannot prove is that the documents are the shape a
 * library that has never heard of rustak will accept — and that is the thing
 * that decides whether CloudTAK's forthcoming single sign-on
 * (dfpc-coe/CloudTAK#661) can point at rustak at all, because it will use
 * `openid-client` or something built the same way.
 *
 * So this scenario runs the parts a library can do without a browser:
 *
 * 1. **Discovery.** `client.discovery()` fetches
 *    `/.well-known/openid-configuration`, checks that the `issuer` inside it is
 *    the one it asked for, and refuses the document outright if any required
 *    field is missing or is not an absolute URL. A document rustak serves with
 *    a relative path, a mismatched issuer or no `token_endpoint` fails here
 *    rather than three steps into a sign-in.
 * 2. **The key set.** Fetched from the `jwks_uri` the document named and parsed
 *    as a JWK set, which is what every ID-token verification starts with.
 * 3. **Userinfo.** With a token from the password grant — the credential this
 *    suite already has — asserting the claims CloudTAK reads.
 *
 * There is no code exchange here and there cannot be: this harness has no
 * upstream identity provider to sign anybody in at, and a browser flow with
 * nobody at the keyboard is not a flow. That half stays in the Rust suite,
 * which has both halves in one process.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import * as client from "openid-client";

import { loadSession, unless, unlessAll } from "../src/session.js";

const session = loadSession();
const skip = unless(session, "oidcDiscovery");

/** The issuer this server publishes, which is also where it is reached. */
const ISSUER = new URL(session.urls.webtak);

/**
 * A client identifier the library needs but the server never sees here.
 *
 * `discovery()` takes one because it builds a configuration for a registered
 * client; nothing in this scenario reaches an endpoint that checks it, and
 * registering one would mean editing the server's configuration from a test.
 */
const CLIENT_ID = "interop-node-tak";

/** The discovery document, as `openid-client` accepted it. */
async function discovered(): Promise<client.Configuration> {
  return await client.discovery(ISSUER, CLIENT_ID);
}

/** An access token from the password grant this suite already uses. */
async function accessToken(): Promise<string> {
  const response = await fetch(new URL("/oauth/token", ISSUER), {
    method: "POST",
    redirect: "manual",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "password",
      username: session.client.username,
      password: session.client.password,
    }),
  });

  assert.equal(response.status, 200);

  const { access_token: token } = (await response.json()) as { access_token: string };

  assert.equal(typeof token, "string");

  return token;
}

test("publishes a discovery document a relying-party library accepts", { skip }, async () => {
  const config = await discovered();
  const metadata = config.serverMetadata();

  assert.equal(
    metadata.issuer,
    ISSUER.href.replace(/\/$/, ""),
    "the issuer has to be the one the tokens carry, or the library refuses every token",
  );

  for (const name of [
    "authorization_endpoint",
    "token_endpoint",
    "userinfo_endpoint",
    "jwks_uri",
    "end_session_endpoint",
  ] as const) {
    const value = metadata[name];

    assert.equal(typeof value, "string", `${name} is missing`);
    assert.doesNotThrow(() => new URL(value as string), `${name} is not an absolute URL`);
    assert.ok(
      (value as string).startsWith(metadata.issuer),
      `${name} is not under the issuer: ${value}`,
    );
  }

  assert.deepEqual(metadata.response_types_supported, ["code"]);
  assert.deepEqual(metadata.id_token_signing_alg_values_supported, ["RS256"]);
  assert.deepEqual(metadata.code_challenge_methods_supported, ["S256"]);
  assert.ok(
    (metadata.grant_types_supported ?? []).includes("authorization_code"),
    "the grant CloudTAK's relying party uses",
  );
  assert.ok(
    (metadata.token_endpoint_auth_methods_supported ?? []).includes("client_secret_post"),
    "the client authentication CloudTAK's relying party uses",
  );
  for (const scope of ["openid", "profile", "email", "groups"]) {
    assert.ok((metadata.scopes_supported ?? []).includes(scope), `scope ${scope} is not offered`);
  }
});

test("publishes a key set an ID-token verification can start from", { skip }, async () => {
  const config = await discovered();
  const jwksUri = config.serverMetadata().jwks_uri as string;
  const response = await fetch(jwksUri);

  assert.equal(response.status, 200);
  assert.equal(
    response.headers.get("content-type"),
    "application/json",
    "exactly application/json, with no charset parameter",
  );
  assert.ok(
    (response.headers.get("cache-control") ?? "").includes("max-age="),
    "a key set a library refetches per sign-in is one it will hammer",
  );

  const { keys } = (await response.json()) as { keys: Record<string, string>[] };

  assert.ok(Array.isArray(keys) && keys.length >= 1, "at least the active key");

  for (const key of keys) {
    assert.equal(key.kty, "RSA");
    assert.equal(key.use, "sig");
    assert.equal(key.alg, "RS256");
    assert.equal(typeof key.kid, "string");

    for (const part of ["n", "e"] as const) {
      const value = key[part];

      assert.equal(typeof value, "string");
      assert.ok(!/[+/=]/.test(value), `${part} is not unpadded base64url: ${value}`);
    }

    // What a library does with it next: `crypto.subtle` is the same importer
    // `openid-client` reaches for, so a key it refuses here is one no ID-token
    // verification could have used.
    await assert.doesNotReject(
      crypto.subtle.importKey(
        "jwk",
        { kty: key.kty, n: key.n, e: key.e, alg: "RS256", ext: true },
        { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
        true,
        ["verify"],
      ),
      `the published key ${key.kid} is not one a verifier could import`,
    );
  }
});

test("answers userinfo with the claims a relying party maps", { skip: unlessAll(session, "oidcDiscovery", "oauthToken") }, async () => {
  const config = await discovered();
  const token = await accessToken();

  const claims = (await client.fetchUserInfo(config, token, client.skipSubjectCheck)) as Record<
    string,
    unknown
  >;

  assert.equal(
    claims.sub,
    session.client.username,
    "`sub` is the rustak username, which is also the access token's and the certificate's",
  );
  assert.equal(
    claims.preferred_username,
    session.client.username,
    "the claim CloudTAK reads first",
  );
  assert.ok(Array.isArray(claims.groups), "`groups` is a flat array of strings");
  for (const name of claims.groups as unknown[]) {
    assert.equal(typeof name, "string");
  }
});

test("refuses userinfo to a caller with no token", { skip }, async () => {
  const config = await discovered();
  const response = await fetch(config.serverMetadata().userinfo_endpoint as string);

  assert.equal(response.status, 401);
  assert.ok(
    (response.headers.get("www-authenticate") ?? "").startsWith("Bearer"),
    "RFC 6750 §3: a library has to be told what to present",
  );
});
