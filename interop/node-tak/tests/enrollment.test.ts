/**
 * `GET /Marti/api/tls/config` and `POST /Marti/api/tls/signClient/v2` — turning
 * a username and a client password into a client certificate, exactly as
 * CloudTAK's connection setup does.
 *
 * `Credentials.generate()` is the whole flow in one call: fetch the name
 * entries, build a CSR from them with `node-forge`, post it as bare base64 and
 * reassemble the PEM from the bare base64 that comes back. Everything this file
 * asserts is something that call would otherwise fail on in a way that points
 * at the wrong place, so each rule from `compat/enrollment.md` is checked on its
 * own first.
 */

import assert from "node:assert/strict";
import { X509Certificate } from "node:crypto";
import { test } from "node:test";

import { xml2js } from "@tak-ps/xml-js";

import { enroll, tokenClient } from "../src/client.js";
import { loadSession, unless, unlessAll } from "../src/session.js";

const session = loadSession();

/** One `<nameEntry name= value=>` as `xml-js`'s compact mode renders it. */
interface NameEntry {
  _attributes?: { name?: string; value?: string };
}

test(
  "the certificate config is the ns2 document CloudTAK's parser requires",
  { skip: unless(session, "tlsConfig") },
  async () => {
    const xml = await tokenClient(session).Credentials.config();
    const parsed = xml2js(xml, { compact: true }) as Record<string, any>;
    const root = parsed["ns2:certificateConfig"];

    assert.ok(
      root,
      "CloudTAK's client indexes `config['ns2:certificateConfig']` literally, so the prefix is part of the contract",
    );

    const entries = root.nameEntries?.nameEntry as NameEntry[] | NameEntry | undefined;

    assert.ok(
      Array.isArray(entries),
      "xml-js collapses a single-element array to a bare object, and CloudTAK then iterates a non-iterable — emit at least two nameEntry elements",
    );
    assert.ok(entries.length >= 2, `expected at least two nameEntry elements, got ${entries.length}`);

    const named = new Map(
      entries.map((entry) => [entry._attributes?.name, entry._attributes?.value]),
    );

    assert.ok(named.has("O"), "the CSR's O is read from this document");
    assert.ok(named.has("OU"), "the CSR's OU is read from this document");
  },
);

test(
  "signs a CSR into a client certificate for the authenticated account",
  { skip: unlessAll(session, "oauthToken", "tlsConfig") },
  async () => {
    const enrolled = await enroll(session);

    assert.match(enrolled.cert, /^-----BEGIN CERTIFICATE-----\n/);
    assert.match(enrolled.key, /-----BEGIN (RSA )?PRIVATE KEY-----/);

    // node-tak wraps the banner around the response itself, so a certificate
    // that parses proves `signedCert` came back as bare base64 rather than
    // already armoured — the rule in `compat/enrollment.md` §3.
    const certificate = new X509Certificate(enrolled.cert);

    assert.match(
      certificate.subject,
      new RegExp(`CN=${session.client.username}`),
      `the issued subject is ${certificate.subject}, which does not name the enrolling account`,
    );

    assert.ok(enrolled.ca.length >= 1, "the response carries the chain the client will trust");

    for (const ca of enrolled.ca) {
      assert.doesNotMatch(ca, /BEGIN CERTIFICATE/, "each caN is bare base64, without PEM armour");
      assert.doesNotThrow(
        () => new X509Certificate(Buffer.from(ca, "base64")),
        "each caN decodes to a DER certificate",
      );
    }
  },
);

test(
  "accepts a second enrollment for the same identity",
  { skip: unlessAll(session, "oauthToken", "tlsConfig") },
  async () => {
    // CloudTAK re-enrolls automatically whenever a stored certificate is within
    // seven days of expiry, reusing the password it already has cached, so a
    // repeated call has to be a fresh issuance rather than a conflict.
    const first = new X509Certificate((await enroll(session)).cert);
    const second = new X509Certificate((await enroll(session)).cert);

    assert.notEqual(
      first.serialNumber,
      second.serialNumber,
      "each enrollment is a fresh certificate, not the previous one handed back",
    );
  },
);
