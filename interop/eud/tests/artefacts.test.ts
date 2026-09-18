/**
 * The two parsers, against the fixtures in `fixtures/`.
 *
 * Both are deliberately loose — `commotest` promises nothing about its line
 * prefixes — so what is tested here is that the looseness holds: that a fuller
 * prefix than any assertion needs does not stop a line being found, and that an
 * `<event>` is recognised wherever in a line it appears.
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import { findLine, highlights, matchingLines, parseLog, parseXml } from "../src/artefacts.js";

const fixtures = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "fixtures");

/** One fixture file, as text. */
function fixture(...parts: string[]): string {
  return fs.readFileSync(path.join(fixtures, ...parts), "utf8");
}

test("the log parser keeps every line and drops the blank ones", () => {
  const log = parseLog(`${fixture("enroll-basic", "commo-log.txt")}\n\n`);

  assert.equal(log.lines.length, 12);
  assert.ok(log.lines.every((line) => line.length > 0));
});

test("a line is found whatever prefix the tool put in front of it", () => {
  const log = parseLog(fixture("enroll-basic", "commo-log.txt"));

  assert.ok(findLine(log, "Proto Negotiate: Protocol negotiation request accepted") !== -1);
  assert.ok(findLine(log, "Completed status for enrollment \\d+, adding stream connection") !== -1);
  assert.equal(findLine(log, "Interface Error"), -1);
});

test("searching from a line only finds what comes after it", () => {
  const log = parseLog(fixture("enroll-basic", "commo-log.txt"));
  const accepted = findLine(log, "negotiation request accepted");

  assert.equal(findLine(log, "Requesting transition to protocol version 1", accepted), -1);
});

test("the three enrollment steps are three lines, in order", () => {
  const log = parseLog(fixture("enroll-basic", "commo-log.txt"));

  assert.equal(matchingLines(log, "EnrollUpdate: step \\d+ id \\d+").length, 3);
});

test("the highlights are the lines a failure should print", () => {
  const lines = highlights(parseLog(fixture("enroll-basic", "commo-log.txt")));

  assert.ok(lines.some((line) => line.includes("Interface Up")));
  assert.ok(lines.some((line) => line.includes("Contact Added")));
  assert.ok(!lines.some((line) => line.includes("commotest quitting")));
});

test("the negative negotiation outcomes are told apart by their own lines", () => {
  const refused = parseLog(fixture("negotiate-refused", "commo-log.txt"));
  const silent = parseLog(fixture("negotiate-silent", "commo-log.txt"));

  assert.ok(findLine(refused, "negotiation request denied, using xml only") !== -1);
  assert.equal(findLine(refused, "Timed out waiting for protocol"), -1);

  assert.ok(findLine(silent, "Timed out waiting for protocol version support message") !== -1);
  assert.equal(findLine(silent, "Server supports protocol versions"), -1);
  assert.equal(findLine(silent, "reconnecting"), -1);
});

test("the mission-package results are read as written", () => {
  const log = parseLog(fixture("mission", "commo-log.txt"));

  assert.ok(findLine(log, "Mission package \\d+ sent to TAK Server, result = SUCCESS") !== -1);
  assert.ok(findLine(log, "Receive of MP .* result OK") !== -1);
});

test("the xml parser reads uid and type off every event", () => {
  const xml = parseXml(fixture("enroll-basic", "commo-xml.txt"));

  assert.equal(xml.events.length, 2);
  assert.equal(xml.events[0].uid, "EUD-ENROLL-BRAVO");
  assert.equal(xml.events[0].type, "a-f-G-U-C");
  assert.ok(xml.events[0].prefix.includes("ssl:127.0.0.1:8089"));
  assert.equal(xml.events[1].index, 1);
});

test("a t-x-d-d's link is what says whose disconnect it is", () => {
  const xml = parseXml(fixture("disconnect", "commo-xml.txt"));
  const notice = xml.events.find((event) => event.type === "t-x-d-d");

  assert.ok(notice !== undefined);
  assert.deepEqual(notice.links, ["EUD-GONE-ALPHA"]);
  assert.deepEqual(
    xml.events.find((event) => event.type === "a-f-G-U-C")?.links,
    [],
  );
});

test("an event truncated by a killed container is still read", () => {
  const xml = parseXml('12:00:00 ep <event uid="EUD-A" type="a-f-G-U-C"><point lat="1" lon="2"');

  assert.equal(xml.events.length, 1);
  assert.equal(xml.events[0].uid, "EUD-A");
});

test("an empty or missing file is no events and no lines", () => {
  assert.deepEqual(parseXml("").events, []);
  assert.deepEqual(parseLog("").lines, []);
});
