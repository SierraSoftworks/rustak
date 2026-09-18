/**
 * The request builders and response parsers for CloudTAK's configuration,
 * login and channel surfaces.
 *
 * These run with no Docker and no server, which is what makes the suite
 * developable on a machine that cannot run the stack — and they are where the
 * rules from `compat/cloudtak.md` are asserted as behaviour rather than as
 * comments.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  configureServer,
  expectObject,
  expectTakList,
  listGroups,
  login,
  parseGroups,
  parseLogin,
  parseServer,
  toggled,
  updateGroups,
  type Group,
} from "../src/api.js";

import { json } from "./fixtures.js";

test("configuring a server carries the three URLs, the credentials and the certificate", () => {
  const call = configureServer({
    name: "rustak (interop)",
    url: "ssl://rustak:8089",
    api: "https://rustak:8443",
    webtak: "https://rustak:8446",
    username: "cloudtak-operator",
    password: "a client password",
    cert: "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    key: "-----BEGIN PRIVATE KEY-----\nBBBB\n-----END PRIVATE KEY-----\n",
  });

  assert.equal(call.method, "PATCH");
  assert.equal(call.path, "/api/server");

  const body = call.body as Record<string, unknown>;

  // The three URLs are independent fields, not one host and three ports:
  // CloudTAK stores exactly these (compat/cloudtak.md §1).
  assert.equal(body.url, "ssl://rustak:8089");
  assert.equal(body.api, "https://rustak:8443");
  assert.equal(body.webtak, "https://rustak:8446");

  // The username and password are what make the first successful pair
  // CloudTAK's own system administrator, and the auth pair is what it validates
  // by calling GET /files/api/config through it.
  assert.equal(body.username, "cloudtak-operator");
  assert.ok((body.auth as Record<string, string>).cert.startsWith("-----BEGIN CERTIFICATE-----"));
  assert.ok((body.auth as Record<string, string>).key.startsWith("-----BEGIN PRIVATE KEY-----"));
});

test("a configured server reports its URLs and the certificate it kept", () => {
  const server = parseServer(json("server-configured.json"));

  assert.equal(server.status, "configured");
  assert.equal(server.api, "https://rustak:8443");
  assert.equal(server.auth, true);
  assert.match(server.certificate?.subject ?? "", /CN=cloudtak-operator/);
});

test("a server that is still unconfigured is a failure, not an answer", () => {
  assert.throws(
    () => parseServer(json("server-unconfigured.json")),
    /still reports its server as 'unconfigured'/,
  );
});

test("a JSON string answer is named as the Content-Type parameter bug", () => {
  // This is what CloudTAK emits when node-tak could not parse rustak's answer:
  // it compares the header with === against 'application/json', so a
  // '; charset=UTF-8' suffix makes it hand the raw text through untouched, and
  // the route looks like it worked (compat/cloudtak.md §4).
  assert.throws(
    () => expectObject("GET /api/marti/group", '{"version":"3","data":[]}'),
    /content-type === 'application\/json'/,
  );
});

test("an envelope without a data array says so rather than returning nothing", () => {
  assert.throws(() => expectTakList("GET /api/marti/group", { version: "3" }), /without a 'data' array/);
});

test("a sign-in parses to the session CloudTAK hands the browser", () => {
  const session = parseLogin(json("login.json"));

  assert.equal(session.email, "cloudtak-operator");
  assert.equal(session.access, "admin");
  assert.ok(session.token.length > 0);

  const call = login("cloudtak-operator", "a client password");

  assert.equal(call.method, "POST");
  assert.equal(call.path, "/api/login");
  assert.deepEqual(call.body, { username: "cloudtak-operator", password: "a client password" });
});

test("channels come out of the TAK envelope with their direction and bit position", () => {
  const groups = parseGroups("GET /api/marti/group", json("groups.json"));

  assert.equal(listGroups().path, "/api/marti/group");
  assert.equal(groups.length, 3);
  assert.deepEqual(
    groups.map((group) => `${group.name}/${group.direction}`),
    ["__ANON__/OUT", "Interop/OUT", "Interop/IN"],
  );
});

test("toggling a channel changes that one and leaves the rest alone", () => {
  const groups = parseGroups("GET /api/marti/group", json("groups.json"));
  const after = toggled(groups, "Interop");

  assert.deepEqual(
    after.map((group) => group.active),
    [true, false, false],
  );

  // Every field goes back: node-tak's Group.update() posts the whole row, and
  // a missing bitpos or direction is how a channel silently moves.
  const body = updateGroups(after).body as Record<string, unknown>[];

  assert.equal(updateGroups(after).method, "PUT");
  assert.deepEqual(Object.keys(body[0]).sort(), [
    "active",
    "bitpos",
    "created",
    "description",
    "direction",
    "name",
    "type",
  ]);
  assert.equal(body[1].description, undefined);
});

test("a channel without a name or an active flag is refused", () => {
  const broken = { version: "3", type: "Group", data: [{ name: "Interop" } as unknown as Group] };

  assert.throws(() => parseGroups("GET /api/marti/group", broken), /without a name and an active flag/);
});
