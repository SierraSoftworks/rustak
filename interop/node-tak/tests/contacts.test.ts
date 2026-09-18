/**
 * `GET /Marti/api/contacts/all` and `GET /Marti/api/clientEndPoints` — who is
 * on the network, as CloudTAK's contact list and its "recent clients" view read
 * them.
 *
 * The shapes differ deliberately and node-tak has no fallback for either:
 * contacts is a **bare array**, `clientEndPoints` is the usual envelope
 * (`compat/contacts.md`). Handing back an envelope where a bare array is
 * expected makes `list.map(...)` throw inside the library, which is why the
 * container shape is asserted before anything in it.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { tokenClient } from "../src/client.js";
import { loadSession, unless } from "../src/session.js";

const session = loadSession();

test(
  "lists contacts as a bare array",
  { skip: unless(session, "contacts") },
  async () => {
    const contacts = await tokenClient(session).Contacts.list();

    assert.ok(
      Array.isArray(contacts),
      "node-tak types this as an array and calls `.map` on it with no envelope check",
    );

    for (const contact of contacts) {
      // node-tak's own CLI formats `contact.notes.trim()`, so an absent field
      // is a crash rather than a blank.
      assert.equal(typeof contact.uid, "string");
      assert.equal(typeof contact.callsign, "string");
      assert.equal(typeof contact.notes, "string");
      assert.equal(typeof contact.team, "string");
      assert.equal(typeof contact.role, "string");
      assert.equal(typeof contact.takv, "string");
    }
  },
);

test(
  "lists client endpoints in the envelope, with a status node-tak understands",
  { skip: unless(session, "clientEndPoints") },
  async () => {
    const endpoints = await tokenClient(session).Client.list();

    assert.ok(Array.isArray(endpoints.data), "`clientEndPoints` answers with `{version, type, data}`");

    for (const endpoint of endpoints.data) {
      assert.equal(typeof endpoint.uid, "string");
      assert.equal(typeof endpoint.callsign, "string");
      assert.equal(typeof endpoint.username, "string");
      assert.ok(
        ["Connected", "Disconnected"].includes(endpoint.lastStatus),
        `unexpected lastStatus ${endpoint.lastStatus}`,
      );
    }
  },
);
