/**
 * `POST /oauth/token` — the password grant CloudTAK's login and its enrollment
 * both hang off, and the JWT shape that is the most fragile compatibility
 * surface in the project (`compat/oauth.md` §2).
 *
 * CloudTAK's JWT "parser" — `OAuth.parse` in node-tak, exercised here — does no
 * signature verification and no JWKS fetch. It base64-decodes the **whole**
 * token as one blob (Node's decoder drops the `.` separators), then finds the
 * payload by splitting on the first `}`. That only lands on a payload boundary
 * if the header's base64url encoding is a multiple of four characters, and only
 * yields valid JSON if the claims are flat. Both are asserted directly as well
 * as through `parse`, so a failure says *which* rule was broken rather than
 * only `Unexpected TAK JWT Format`.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { TAKAPI, APIAuthPassword } from "@tak-ps/node-tak";

import { loadSession, unless } from "../src/session.js";

const session = loadSession();
const skip = unless(session, "oauthToken");

/** The raw grant, so the response body can be asserted byte for byte. */
async function grant(username: string, password: string): Promise<Response> {
  return await fetch(new URL("/oauth/token", session.urls.webtak), {
    method: "POST",
    redirect: "manual",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "password", username, password }),
  });
}

test("exchanges a client password for a token node-tak can parse", { skip }, async () => {
  const auth = new APIAuthPassword(session.client.username, session.client.password);

  // `TAKAPI.init` is what performs the grant: this is CloudTAK's login, in one
  // call, with node-tak's own parser deciding whether the token is usable.
  const api = await TAKAPI.init(new URL(session.urls.webtak), auth);
  const contents = api.OAuth.parse(auth.jwt);

  assert.equal(
    contents.sub,
    session.client.username,
    "`sub` is the only claim CloudTAK reads, and it is used as the account identity",
  );
});

test("issues exactly the body CloudTAK's client expects", { skip }, async () => {
  const response = await grant(session.client.username, session.client.password);

  assert.equal(response.status, 200);
  assert.equal(
    response.headers.get("content-type"),
    "application/json",
    "node-tak compares the content type by string equality; a charset parameter makes it return raw text",
  );

  const body = (await response.json()) as Record<string, unknown>;

  assert.equal(typeof body.access_token, "string");
  assert.equal(body.token_type, "Bearer");
  assert.equal(typeof body.expires_in, "number");
  assert.equal(
    body.refresh_token,
    undefined,
    "the password grant issues no refresh token; rustak's own refresh model stays out of this response",
  );
});

test("issues a JWT whose header and claims survive CloudTAK's parser", { skip }, async () => {
  const response = await grant(session.client.username, session.client.password);
  const { access_token: token } = (await response.json()) as { access_token: string };
  const [header, payload] = token.split(".");

  assert.ok(header && payload, "a JWT has three dot-separated segments");

  assert.equal(
    header.length % 4,
    0,
    "the header's base64url encoding must be a multiple of four characters, or the payload does not start on a decode boundary",
  );

  const claims = JSON.parse(Buffer.from(payload, "base64url").toString("utf8")) as Record<string, unknown>;

  for (const [name, value] of Object.entries(claims)) {
    assert.ok(
      value === null || typeof value !== "object" || Array.isArray(value),
      `claim \`${name}\` is a nested object, which breaks CloudTAK's brace-splitting parser`,
    );
  }

  assert.equal(claims.sub, session.client.username);
});

test("refuses a password that is not the one minted", { skip }, async () => {
  const response = await grant(session.client.username, "not-the-password");

  assert.ok(
    [400, 401].includes(response.status),
    `a bad password answered ${response.status}; CloudTAK treats 3xx as success and requires JSON`,
  );

  const body = (await response.json()) as Record<string, unknown>;

  assert.equal(body.error, "invalid_grant");
  assert.equal(body.access_token, undefined);
});
