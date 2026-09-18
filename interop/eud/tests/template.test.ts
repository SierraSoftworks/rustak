/**
 * Argv templating, including the shape of the command line itself.
 *
 * `commotest <uid> <callsign> <output-dir> { <wait> <command> }` is positional
 * with no flags, so getting the first three arguments right is not something a
 * later assertion would catch: a run with the directory in the wrong place
 * writes its files somewhere the runner never looks and reports nothing.
 */

import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import { loadScenario } from "../src/scenario.js";
import { PLACEHOLDERS, render, renderArgv, type Substitutions } from "../src/template.js";

const suiteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

const values: Substitutions = {
  host: "127.0.0.1",
  stream_port: "41001",
  enroll_port: "41000",
  marti_port: "41002",
  username: "eud-alpha",
  token: "one-time-token",
  truststore: "/work/truststore.p12",
  work: "/work",
  uid: "EUD-ENROLL-ALPHA",
  callsign: "ALPHA",
};

test("the argv starts with uid, callsign and the mounted directory", () => {
  const scenario = loadScenario(path.join(suiteRoot, "scenarios", "enroll-basic.toml"));
  const argv = renderArgv(scenario.euds[0], values);

  assert.deepEqual(argv.slice(0, 3), ["EUD-ENROLL-ALPHA", "ALPHA", "/work"]);
  assert.equal(argv[argv.length - 1], "quit");
  assert.equal(argv.length % 2, 1, "three fixed arguments plus wait/command pairs");
});

test("enrollment is encoded the way ATAK does it", () => {
  const scenario = loadScenario(path.join(suiteRoot, "scenarios", "enroll-basic.toml"));
  const estream = renderArgv(scenario.euds[0], values).find((entry) =>
    entry.startsWith("estream:"),
  );

  assert.ok(estream !== undefined);

  const fields = estream.split(":");

  // estream:<truststore>:<trustpass>:<keypass>:<clientpass>:<capass>:<user>:
  //         <pass>:<host>:<eport>:<port>:<versioninfo>
  assert.equal(fields[1], "/work/truststore.p12");
  assert.equal(fields[5], "", "an empty CA password is normal enrollment, not quick connect");
  assert.equal(fields[6], "eud-alpha", "no '-' prefix: Basic, not Bearer");
  assert.equal(fields[7], "one-time-token", "ATAK sends the token as the Basic password");
  assert.equal(fields[8], "-127.0.0.1", "the '-' turns host-name verification off, as ATAK does");
  assert.equal(fields[9], "41000", "the enrollment port, ATAK's <eport>");
  assert.equal(fields[10], "41001", "the stream port");
});

test("every placeholder the scenarios use resolves", () => {
  for (const placeholder of PLACEHOLDERS) {
    assert.equal(render(`{${placeholder}}`, values), values[placeholder]);
  }
});

test("a placeholder the runner does not know is refused at substitution too", () => {
  assert.throws(() => render("{nonsense}", values), /not a placeholder/);
});

test("text with no placeholders is passed through untouched", () => {
  assert.equal(render("remiface:0", values), "remiface:0");
});
