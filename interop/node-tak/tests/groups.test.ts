/**
 * `GET /Marti/api/groups/all` and `PUT /Marti/api/groups/active` — channels, as
 * node-tak's `Group.list()`/`Group.update()` see them.
 *
 * Both CloudTAK and ATAK gate real behaviour on these fields, and two of the
 * rules are ones OpenTAKServer gets wrong (`compat/groups.md`, §"Known-wrong in
 * OTS"): `bitpos` is a bit position, not a counter, and `active` is the
 * device's own per-channel state rather than whether the channel is enabled. A
 * fixture that only checked "a list came back" would pass against both.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { tokenClient } from "../src/client.js";
import { loadSession, unless } from "../src/session.js";

const session = loadSession();
const skip = unless(session, "groups");

test("lists channels in the envelope CloudTAK types against", { skip }, async () => {
  const list = await tokenClient(session).Group.list();

  assert.equal(
    list.type,
    "com.bbn.marti.remote.groups.Group",
    "CloudTAK reads the envelope's `type` to decide what it is holding",
  );
  assert.ok(Array.isArray(list.data), "`data` is the array of channels");

  for (const group of list.data) {
    assert.equal(typeof group.name, "string");
    assert.ok(["IN", "OUT"].includes(group.direction), `unexpected direction ${group.direction}`);
    assert.equal(typeof group.type, "string");
    assert.match(
      group.created,
      /^\d{4}-\d{2}-\d{2}$/,
      "ATAK parses `created` as yyyy-MM-dd and drops a channel whose date it cannot read",
    );
    assert.equal(typeof group.bitpos, "number");
    assert.ok(group.bitpos >= 0, "`bitpos` is a bit position; a negative one is not addressable");
    assert.equal(typeof group.active, "boolean");
  }

  assert.ok(
    list.data.some((group) => group.name === "__ANON__"),
    "TAK clients expect the default channel to exist",
  );
});

test("takes a channel out of a device's active set and puts it back", { skip }, async () => {
  const api = tokenClient(session);
  const before = await api.Group.list();
  const target = before.data.find((group) => group.direction === "IN");

  assert.ok(target, "there is no IN channel to flip, so this account can send nowhere");

  // ATAK sends the whole list back with the flags it wants, which is what this
  // reproduces — not a patch of the one entry.
  await api.Group.update(
    before.data.map((group) =>
      group.name === target.name && group.direction === target.direction
        ? { ...group, active: !target.active }
        : group,
    ),
  );

  const flipped = await api.Group.list();
  const now = flipped.data.find(
    (group) => group.name === target.name && group.direction === target.direction,
  );

  assert.equal(now?.active, !target.active, "the per-device active state did not follow the update");

  await api.Group.update(before.data);
});
