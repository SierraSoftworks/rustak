/**
 * The enrollment call that mints CloudTAK's admin certificate.
 *
 * Every rule here is one `compat/enrollment.md` records and one that breaks
 * CloudTAK in a place far away from the cause if it is wrong, so each is a
 * separate assertion against a fixture rather than something the runner finds
 * out at three in the morning.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { armour, basicHeader, csrBody, parseNameEntries, parseSignedCertificate, signClientPath } from "../src/enroll.js";

import { json, text } from "./fixtures.js";

test("the certificate config supplies the O and OU a CSR must carry", () => {
  const entries = parseNameEntries(text("tls-config.xml"));

  assert.deepEqual(entries, { organisation: "rustak", unit: "interop" });
});

test("a config document with no name entries still yields a usable subject", () => {
  const entries = parseNameEntries("<ns2:certificateConfig><nameEntries/></ns2:certificateConfig>");

  assert.equal(entries.organisation.length > 0, true);
  assert.equal(entries.unit.length > 0, true);
});

test("an empty nameEntry value is ignored rather than used", () => {
  // commoncommo refuses to build a CSR from an empty value, and a server that
  // emits one is a bring-up failure; the client-side half of that rule is not
  // to take it.
  const entries = parseNameEntries('<nameEntry name="O" value=""/><nameEntry name="OU" value="unit"/>');

  assert.notEqual(entries.organisation, "");
  assert.equal(entries.unit, "unit");
});

test("the signing request names the device and the client version in the query", () => {
  const path = signClientPath("cloudtak interop/admin");

  assert.ok(path.startsWith("/Marti/api/tls/signClient/v2?"));
  assert.match(path, /clientUid=cloudtak\+interop%2Fadmin/);
  assert.match(path, /version=rustak-interop/);
});

test("Basic is the only credential this endpoint takes, and it is built by hand", () => {
  assert.equal(
    basicHeader("cloudtak-operator", "secret"),
    `Basic ${Buffer.from("cloudtak-operator:secret").toString("base64")}`,
  );
});

test("the request body is the DER as bare base64, banners and newlines stripped", () => {
  const pem = "-----BEGIN CERTIFICATE REQUEST-----\nAAAA\nBBBB\n-----END CERTIFICATE REQUEST-----\n";

  assert.equal(csrBody(pem), "AAAABBBB");
});

test("bare base64 is re-armoured at sixty-four columns, as node-tak does", () => {
  const long = "A".repeat(200);
  const pem = armour(long);

  const lines = pem.trim().split("\n");

  assert.equal(lines[0], "-----BEGIN CERTIFICATE-----");
  assert.equal(lines[lines.length - 1], "-----END CERTIFICATE-----");
  assert.deepEqual(
    lines.slice(1, -1).map((line) => line.length),
    [64, 64, 64, 8],
  );
});

test("a signed certificate parses into a PEM leaf and the chain behind it", () => {
  const { cert, ca } = parseSignedCertificate(text("sign-client.json"));

  assert.match(cert, /^-----BEGIN CERTIFICATE-----\n/);
  assert.equal(ca.length, 2, "ca0 and ca1 both come back");
  assert.match(ca[0], /^-----BEGIN CERTIFICATE-----\n/);

  // The fixture is what the endpoint answers; the parser's job is to armour it.
  const fields = json("sign-client.json") as Record<string, string>;

  assert.equal(cert.includes(fields.signedCert.slice(0, 32)), true);
});

test("an already-armoured signedCert is refused, because node-tak would double it", () => {
  assert.throws(
    () =>
      parseSignedCertificate(
        JSON.stringify({ signedCert: "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----", ca0: "AAAA" }),
      ),
    /bare base64/,
  );
});

test("an answer with no chain is refused, because a client would have nothing to trust", () => {
  assert.throws(() => parseSignedCertificate(JSON.stringify({ signedCert: "AAAA" })), /no 'ca0'/);
});

test("a non-JSON answer says what it was instead of throwing a parse error", () => {
  assert.throws(() => parseSignedCertificate("<html>Bad Request</html>"), /did not answer JSON/);
});
