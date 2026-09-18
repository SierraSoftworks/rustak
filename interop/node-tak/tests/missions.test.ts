/**
 * `/Marti/api/missions/*` — Data Sync, as CloudTAK's mission pages drive it
 * through node-tak's `Mission` commands.
 *
 * The envelope `type` string is deliberately *not* consistent across the
 * mission family (`compat/missions.md` §2): the Mission payloads say `Mission`,
 * the singular subscription route says a fully-qualified class name and the
 * plural one says a bare literal. A client that switches on `type` rejects an
 * unexpected value, so each one is asserted where it is produced rather than
 * pattern-matched loosely.
 *
 * `Mission.create()` also auto-sets `allowGroupChange` whenever a `group` is
 * supplied (`compat/cloudtak.md` §10) — a normal flag, not a hostile client.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { tokenClient } from "../src/client.js";
import { loadSession, unless } from "../src/session.js";

const session = loadSession();
const skip = unless(session, "missions");

/** A name no other run will collide with, and one CloudTAK will address by name. */
function missionName(): string {
  return `interop-${Date.now()}-${Math.floor(Math.random() * 1000)}`;
}

test("lists missions in the Mission envelope", { skip }, async () => {
  const list = await tokenClient(session).Mission.list({});

  assert.equal(list.type, "Mission", "the mission-family envelope for Mission payloads");
  assert.ok(Array.isArray(list.data));
});

test("creates, reads back and deletes a mission", { skip }, async () => {
  const api = tokenClient(session);
  const name = missionName();

  const created = await api.Mission.create({
    name,
    creatorUid: `connection-interop-data-${name}`,
    description: "node-tak interop suite",
  });

  try {
    assert.equal(created.name, name);
    assert.match(
      created.guid,
      /^[{]?[0-9a-fA-F]{8}-([0-9a-fA-F]{4}-){3}[0-9a-fA-F]{12}[}]?$/,
      "CloudTAK sniffs the identifier and routes UUID-shaped ones to the /guid/ family",
    );

    // The GUID family has to work as well as the name family, because CloudTAK
    // will always prefer it once it is holding a GUID.
    const byGuid = await api.Mission.getGuid(created.guid, {});

    assert.equal(byGuid.name, name);
  } finally {
    await api.Mission.delete(name, {});
  }

  const after = await api.Mission.list({});

  assert.ok(
    !after.data.some((mission) => mission.name === name),
    "a deleted mission is gone from the listing",
  );
});

test("subscribes to a mission and reports the subscription", { skip }, async () => {
  const api = tokenClient(session);
  const name = missionName();
  const uid = `ANDROID-CloudTAK-${session.client.username}`;

  await api.Mission.create({ name, creatorUid: uid, description: "node-tak interop suite" });

  try {
    await api.Mission.subscribe(name, { uid });

    const subscriptions = await api.Mission.subscriptions(name);

    assert.equal(
      subscriptions.type,
      "MissionSubscription",
      "the plural route's type is the bare literal, not the fully-qualified name the singular route uses",
    );

    await api.Mission.unsubscribe(name, { uid });
  } finally {
    await api.Mission.delete(name, {});
  }
});
