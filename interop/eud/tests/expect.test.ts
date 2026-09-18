/**
 * The assertion engine, driven by the real `enroll-basic` scenario against a
 * fixture that stands in for a good run.
 *
 * This is the test that would have caught an engine which passes everything:
 * the same scenario is run against artefacts that are wrong in one way at a
 * time, and each failure has to be reported — in particular the *ordered* log
 * expectations, which are what separate "the server negotiated" from "the
 * server said three plausible things in any order".
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import { parseLog, parseXml } from "../src/artefacts.js";
import {
  checkAudit,
  checkClientEndpoints,
  checkEud,
  checkRuntime,
  type EudArtefacts,
} from "../src/expect.js";
import { loadScenario } from "../src/scenario.js";
import type { Substitutions } from "../src/template.js";

const suiteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** The shipped scenario, so the engine is tested against what actually runs. */
const scenario = loadScenario(path.join(suiteRoot, "scenarios", "enroll-basic.toml"));

/** What the runner would have substituted for that scenario's one EUD. */
const values: Substitutions = {
  host: "127.0.0.1",
  stream_port: "8089",
  enroll_port: "8446",
  marti_port: "8443",
  username: "eud-alpha",
  token: "not-a-real-token",
  truststore: "/work/truststore.p12",
  work: "/work",
  uid: "EUD-ENROLL-ALPHA",
  callsign: "ALPHA",
};

/** A good run, which every case then spoils in one way. */
function artefacts(overrides: Partial<EudArtefacts> = {}): EudArtefacts {
  return {
    log: parseLog(
      fs.readFileSync(path.join(suiteRoot, "fixtures", "enroll-basic", "commo-log.txt"), "utf8"),
    ),
    xml: parseXml(
      fs.readFileSync(path.join(suiteRoot, "fixtures", "enroll-basic", "commo-xml.txt"), "utf8"),
    ),
    certSubject: "CN=eud-alpha, O=rustak",
    ranForSeconds: 96,
    timedOut: false,
    ...overrides,
  };
}

test("a good run satisfies the scenario it came from", () => {
  assert.deepEqual(checkEud(scenario.euds[0], artefacts(), values), []);
  assert.deepEqual(checkRuntime(scenario, [artefacts()]), []);
  assert.deepEqual(checkClientEndpoints(scenario.expect, ["ALPHA"]), []);
});

test("a missing step in the sequence is reported, naming the pattern", () => {
  const log = parseLog(
    fs
      .readFileSync(path.join(suiteRoot, "fixtures", "enroll-basic", "commo-log.txt"), "utf8")
      .split("\n")
      .filter((line) => !line.includes("Requesting transition"))
      .join("\n"),
  );

  const failures = checkEud(scenario.euds[0], artefacts({ log }), values);

  assert.equal(failures.length, 1);
  assert.match(failures[0], /Requesting transition to protocol version 1/);
});

test("the sequence is a sequence: the same lines out of order fail", () => {
  const lines = fs
    .readFileSync(path.join(suiteRoot, "fixtures", "enroll-basic", "commo-log.txt"), "utf8")
    .split("\n");
  const swapped = [...lines];
  const request = swapped.findIndex((line) => line.includes("Requesting transition"));
  const accepted = swapped.findIndex((line) => line.includes("request accepted"));

  [swapped[request], swapped[accepted]] = [swapped[accepted], swapped[request]];

  const failures = checkEud(scenario.euds[0], artefacts({ log: parseLog(swapped.join("\n")) }), values);

  // The acceptance is now *before* the request, so the scan finds the request
  // where the acceptance was and then has nothing left to match.
  assert.equal(failures.length, 1);
  assert.match(failures[0], /negotiation request accepted/);
});

test("a forbidden line is reported with the line that matched it", () => {
  const log = parseLog(
    `${fs.readFileSync(path.join(suiteRoot, "fixtures", "enroll-basic", "commo-log.txt"), "utf8")}\n2026-09-18 04:01:00.000 WARN  Interface Error: 3\n`,
  );

  const failures = checkEud(scenario.euds[0], artefacts({ log }), values);

  assert.equal(failures.length, 1);
  assert.match(failures[0], /forbidden \/Interface Error\/: .*Interface Error: 3/);
});

test("an empty log is the one failure worth reporting, and stops there", () => {
  const failures = checkEud(scenario.euds[0], artefacts({ log: parseLog("") }), values);

  assert.equal(failures.length, 1);
  assert.match(failures[0], /wrote no commo-log.txt at all/);
});

test("a container the runner had to kill is a failure of its own", () => {
  const failures = checkEud(scenario.euds[0], artefacts({ timedOut: true }), values);

  assert.equal(failures.length, 1);
  assert.match(failures[0], /did not exit within/);
});

test("the issued certificate has to carry the common name that was asked for", () => {
  const wrong = checkEud(
    scenario.euds[0],
    artefacts({ certSubject: "O=rustak, CN=eud-alpha" }),
    values,
  );

  assert.equal(wrong.length, 1);
  assert.match(wrong[0], /not CN=eud-alpha first/);

  const missing = checkEud(scenario.euds[0], artefacts({ certSubject: undefined }), values);

  assert.equal(missing.length, 1);
  assert.match(missing[0], /there is none/);
});

test("a run that was cut short cannot prove what the scenario claims", () => {
  const failures = checkRuntime(scenario, [artefacts({ ranForSeconds: 12 })]);

  assert.equal(failures.length, 1);
  assert.match(failures[0], /short of the 90s/);
});

test("an EUD scripted to leave early does not make the scenario a short run", () => {
  // `chat-direct`'s shape: one EUD quits half way through on purpose so the
  // other can speak into the gap. The scenario's clock is the longest run.
  assert.deepEqual(
    checkRuntime(scenario, [artefacts({ ranForSeconds: 35 }), artefacts({ ranForSeconds: 96 })]),
    [],
  );
});

test("the server half reports both directions", () => {
  assert.deepEqual(checkClientEndpoints(scenario.expect, ["ALPHA"]), []);

  const absent = checkClientEndpoints(scenario.expect, []);

  assert.equal(absent.length, 1);
  assert.match(absent[0], /does not list 'ALPHA'/);

  const forbidden = checkClientEndpoints(
    { clientEndPointsPresent: [], clientEndPointsAbsent: ["ALPHA"], auditMatches: [] },
    ["ALPHA"],
  );

  assert.equal(forbidden.length, 1);
  assert.match(forbidden[0], /lists 'ALPHA', which it should not/);
});

test("the audit expectation reads the server's own record", () => {
  const expect = { clientEndPointsPresent: [], clientEndPointsAbsent: [], auditMatches: ["uploaded"] };

  assert.deepEqual(checkAudit(expect, '[{"action":"uploaded","category":"content"}]'), []);
  assert.match(checkAudit(expect, "[]")[0], /nothing matching \/uploaded\//);
});

test("the routing scenario's expectations are asymmetric, as the grants are", () => {
  const routing = loadScenario(path.join(suiteRoot, "scenarios", "two-eud-routing.toml"));
  const alpha = routing.euds.find((eud) => eud.id === "alpha");
  const bravo = routing.euds.find((eud) => eud.id === "bravo");

  assert.deepEqual(alpha?.channels, [{ group: "relay", direction: "IN" }]);
  assert.deepEqual(bravo?.channels, [{ group: "relay", direction: "OUT" }]);
  assert.deepEqual(alpha?.expect.xmlAbsent, [
    { uid: "EUD-ROUTE-BRAVO", type: undefined, linkUid: undefined },
  ]);
  assert.deepEqual(bravo?.expect.xmlPresent, [
    { uid: "EUD-ROUTE-ALPHA", type: "a-f-G-U-C", linkUid: undefined },
  ]);
});

test("the disconnect scenario asserts whose disconnect it was", () => {
  const disconnect = loadScenario(path.join(suiteRoot, "scenarios", "disconnect.toml"));
  const bravo = disconnect.euds.find((eud) => eud.id === "bravo");
  const xml = parseXml(
    fs.readFileSync(path.join(suiteRoot, "fixtures", "disconnect", "commo-xml.txt"), "utf8"),
  );

  assert.ok(bravo !== undefined);

  const good = checkEud(
    bravo,
    {
      log: parseLog(
        "2026-09-18 04:30:02.410 INFO  Completed status for enrollment 1, adding stream connection\n" +
          "2026-09-18 04:30:02.905 INFO  Interface Up: ssl:127.0.0.1:8089\n" +
          "2026-09-18 04:30:08.551 INFO  Contact Added: EUD-GONE-ALPHA\n" +
          "2026-09-18 04:30:33.120 INFO  Contact Removed: EUD-GONE-ALPHA\n",
      ),
      xml,
      ranForSeconds: 56,
      timedOut: false,
    },
    values,
  );

  assert.deepEqual(good, []);

  // The same notice about somebody else must not satisfy it.
  const elsewhere = parseXml(
    '12:00:00 ep <event uid="rustak-dd-2" type="t-x-d-d"><detail><link uid="SOMEBODY-ELSE"/></detail></event>',
  );
  const failures = checkEud(
    bravo,
    {
      log: parseLog(
        "Completed status for enrollment 1, adding stream connection\nContact Added: x\nContact Removed: x\n",
      ),
      xml: elsewhere,
      ranForSeconds: 56,
      timedOut: false,
    },
    values,
  );

  assert.ok(failures.some((failure) => /link uid=EUD-GONE-ALPHA/.test(failure)));
});
