/**
 * The Data Sync and package half of the API surface, against fixtures.
 *
 * The shapes here are the ones M4 exists to serve, so they are asserted the
 * same way `interop/node-tak` asserts the wire: from the outside, by the thing
 * that would break.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  addsContent,
  addsResource,
  createMission,
  getMission,
  listPackages,
  markerFeature,
  missionChanges,
  parseChanges,
  parseContent,
  parseMission,
  parsePackages,
  parseSubmitted,
  sharePackage,
  submitFeatures,
  uploadFile,
} from "../src/missions.js";
import { contentHash } from "../src/steps.js";

import { json } from "./fixtures.js";

const GUID = "6c2f1f0a-3d2b-4f5e-9a71-0b8c2d4e6f80";

test("creating a Data Sync sends the name, keywords and channel, and no creatorUid", () => {
  const call = createMission({
    name: "rustak-interop-cloudtak-4f2a91c7",
    description: "Created by interop/cloudtak",
    keywords: ["interop", "rustak"],
    groups: ["Interop"],
  });

  assert.equal(call.method, "POST");
  assert.equal(call.path, "/api/marti/mission");

  const body = call.body as Record<string, unknown>;

  assert.deepEqual(body.keywords, ["interop", "rustak"]);
  assert.deepEqual(body.group, ["Interop"]);

  // CloudTAK adds `creatorUid: ANDROID-CloudTAK-{email}` itself and rejects a
  // body that tries to set it.
  assert.equal("creatorUid" in body, false);
});

test("a created mission parses to its guid, and the guid is what later calls route on", () => {
  const mission = parseMission("POST /api/marti/mission", json("mission-created.json"));

  assert.equal(mission.guid, GUID);
  assert.equal(mission.token, "mission-token-for-the-creator");
  assert.equal(mission.contents.length, 1);
  assert.equal(getMission(mission.guid).path, `/api/marti/missions/${GUID}`);
});

test("a mission without a guid is refused, however complete it otherwise looks", () => {
  assert.throws(
    () => parseMission("POST /api/marti/mission", { name: "no-guid", contents: [] }),
    /without a name and a guid/,
  );
});

test("a marker is a GeoJSON point with a CoT type and a stale in the future", () => {
  const now = new Date("2026-09-18T09:00:00.000Z");
  const feature = markerFeature({
    uid: "interop-marker-1",
    callsign: "Interop Marker",
    longitude: -104.9903,
    latitude: 39.7392,
    now,
  });

  const properties = feature.properties as Record<string, string>;
  const geometry = feature.geometry as { type: string; coordinates: number[] };

  assert.equal(feature.id, "interop-marker-1");
  assert.equal(feature.type, "Feature");
  assert.equal(properties.type, "a-f-G-U-C");
  assert.equal(properties.callsign, "Interop Marker");

  // Longitude first: GeoJSON order, which is the opposite of the lat/lon a CoT
  // event carries, and the single easiest thing to put the wrong way round.
  assert.deepEqual(geometry.coordinates, [-104.9903, 39.7392]);
  assert.ok(Date.parse(properties.stale) > now.getTime());
});

test("submitting features and reading back the uids CloudTAK confirmed", () => {
  const call = submitFeatures(GUID, [markerFeature({ uid: "u", callsign: "c", longitude: 0, latitude: 0 })]);

  assert.equal(call.method, "PUT");
  assert.equal(call.path, `/api/marti/missions/${GUID}/cot`);
  assert.equal((call.body as { features: unknown[] }).features.length, 1);

  assert.deepEqual(parseSubmitted({ status: 200, message: "CoTs Submitted", uids: ["u"] }), ["u"]);
});

test("a submission that confirmed nothing is a failure, not an empty success", () => {
  assert.throws(() => parseSubmitted({ status: 200, message: "CoTs Submitted" }), /without listing uids/);
});

test("a file upload names the file in the query string and sends the bytes raw", () => {
  const bytes = Buffer.from("rustak interop file\n", "utf8");
  const call = uploadFile(GUID, "interop notes.txt", bytes);

  assert.equal(call.method, "POST");
  assert.equal(call.path, `/api/marti/missions/${GUID}/upload?name=interop%20notes.txt`);
  // No `Content-Type` on purpose. CloudTAK's router consumes
  // `application/octet-stream` (and `text/*`, and JSON) with a body parser
  // before the handler runs, and the handler streams the request onward — so a
  // typed body arrives at rustak empty. See `uploadFile`.
  assert.equal(call.contentType, undefined);
  assert.equal(call.raw?.equals(bytes), true);

  // The hash the runner looks for in the mission's contents afterwards is the
  // SHA-256 of exactly these bytes.
  assert.match(contentHash(bytes), /^[0-9a-f]{64}$/);
});

test("the change log records the marker by uid and the file by hash", () => {
  const changes = parseChanges(json("mission-changes.json"));

  assert.equal(missionChanges(GUID).path, `/api/marti/missions/${GUID}/changes?secago=3600`);
  assert.equal(changes.length, 2);
  assert.equal(addsContent(changes, "interop-marker-9c7b1d2e-2f40-4a1b-9c8d-7e6f5a4b3c21"), true);
  assert.equal(
    addsResource(changes, "3f6b2c1d9e8a7b5c4d3e2f10112233445566778899aabbccddeeff0011223344"),
    true,
  );
  assert.equal(addsContent(changes, "a uid nothing added"), false);
});

test("sharing a package is public, carries the features, and asks for no assets", () => {
  const call = sharePackage({
    name: "rustak-interop-package-1b2c3d4e",
    keywords: ["interop"],
    features: [markerFeature({ uid: "u", callsign: "c", longitude: 0, latitude: 0 })],
  });

  assert.equal(call.method, "PUT");
  assert.equal(call.path, "/api/marti/package");

  const body = call.body as Record<string, unknown>;

  // `public` is what sends it through /Marti/sync/missionupload and publishes
  // it to the package list; the three empty arrays are what keep the call off
  // CloudTAK's own object store, which this stack does not run.
  assert.equal(body.public, true);
  assert.deepEqual(body.assets, []);
  assert.deepEqual(body.basemaps, []);
  assert.deepEqual(body.destinations, []);
});

test("a shared package parses to its hash, and the list is searched by name", () => {
  const content = parseContent("PUT /api/marti/package", json("package-content.json"));

  assert.match(content.Hash, /^[0-9a-f]{64}$/);
  assert.equal(content.Name, "rustak-interop-package-1b2c3d4e");

  const listed = parsePackages(json("package-list.json"));

  assert.equal(listPackages("rustak interop").path, "/api/marti/package?filter=rustak%20interop");
  assert.equal(listed.length, 1);
  assert.equal(listed[0].hash, content.Hash);
});

test("a stored file without a hash is refused", () => {
  assert.throws(() => parseContent("PUT /api/marti/package", { Name: "x" }), /without a 'Hash'/);
});
