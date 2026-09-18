/**
 * The loader, which is the only thing standing between a typo in a scenario
 * file and a nightly failure that looks like rustak's fault.
 *
 * `commotest` has no exit-code contract and an argument parser upstream
 * describes as not error tolerant, so a malformed script produces a container
 * that exits 0 having done nothing — indistinguishable, from the runner's side,
 * from a server that refused the connection. Everything checkable is therefore
 * checked before anything starts, and this is where that is proven.
 */

import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import { loadScenarios, parseScenario, ScenarioError } from "../src/scenario.js";
import { SURFACE_NAMES } from "../src/surfaces.js";

const suiteRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** A scenario that loads, which each case then breaks in exactly one way. */
function valid(body = ""): string {
  return `
name = "example"
summary = "An example scenario."
requires = ["enrollment"]
timeout_seconds = 120
min_runtime_seconds = 30

[[euds]]
id = "alpha"
uid = "EUD-A"
callsign = "ALPHA"
username = "eud-a"
channels = [{ group = "__ANON__", direction = "BOTH" }]
script = ["0", "estream:{truststore}:atakatak:atakatak:atakatak::{username}:{token}:-{host}:{enroll_port}:{stream_port}:v", "30", "quit"]
${body}
`;
}

test("every scenario this suite ships loads and validates", () => {
  const scenarios = loadScenarios(path.join(suiteRoot, "scenarios"));

  assert.ok(scenarios.length >= 9, `expected the night-one set, found ${scenarios.length}`);

  const names = scenarios.map((scenario) => scenario.name);

  for (const expected of [
    "enroll-basic",
    "two-eud-routing",
    "chat-direct",
    "disconnect",
    "negotiate-refused",
    "negotiate-silent",
    "enroll-revoked",
    "mp-upload",
    "mp-download",
  ]) {
    assert.ok(names.includes(expected), `${expected} is missing from scenarios/`);
  }

  for (const scenario of scenarios) {
    assert.ok(scenario.summary.length > 0, `${scenario.name} has no summary`);
    assert.ok(scenario.requires.length > 0, `${scenario.name} requires nothing, so it never skips`);

    for (const required of scenario.requires) {
      assert.ok(SURFACE_NAMES.includes(required));
    }
  }
});

test("a scenario carries its configuration overrides through", () => {
  const scenario = parseScenario(
    `${valid()}
[config.stream]
negotiation = "refuse"
`,
    "example.toml",
  );

  assert.deepEqual(scenario.config, { stream: { negotiation: "refuse" } });
});

test("the negotiation scenarios set the knob they exist to exercise", () => {
  const scenarios = loadScenarios(path.join(suiteRoot, "scenarios"));
  const byName = new Map(scenarios.map((scenario) => [scenario.name, scenario]));

  assert.deepEqual(byName.get("negotiate-refused")?.config, { stream: { negotiation: "refuse" } });
  assert.deepEqual(byName.get("negotiate-silent")?.config, { stream: { negotiation: "silent" } });
});

test("a script that does not end in quit is refused", () => {
  assert.throws(
    () =>
      parseScenario(
        valid().replace('"30", "quit"', '"30", "safreq:500"'),
        "example.toml",
      ),
    (error: unknown) => error instanceof ScenarioError && /end with 'quit'/.test(String(error)),
  );
});

test("a script whose pairs do not line up is refused", () => {
  assert.throws(
    () => parseScenario(valid().replace('"30", "quit"', '"quit"'), "example.toml"),
    (error: unknown) => error instanceof ScenarioError && /pairs of/.test(String(error)),
  );
});

test("a wait that is not a number of seconds is refused", () => {
  assert.throws(
    () => parseScenario(valid().replace('"30", "quit"', '"soon", "quit"'), "example.toml"),
    (error: unknown) => error instanceof ScenarioError && /whole number of seconds/.test(String(error)),
  );
});

test("a placeholder the runner does not fill is refused", () => {
  assert.throws(
    () => parseScenario(valid().replace("{truststore}", "{keystore}"), "example.toml"),
    (error: unknown) => error instanceof ScenarioError && /\{keystore\}/.test(String(error)),
  );
});

test("a surface the runner cannot probe is refused", () => {
  assert.throws(
    () => parseScenario(valid().replace('"enrollment"', '"telepathy"'), "example.toml"),
    (error: unknown) => error instanceof ScenarioError && /telepathy/.test(String(error)),
  );
});

test("two EUDs with the same id are refused, because their output would collide", () => {
  const twice = `${valid()}
[[euds]]
id = "alpha"
uid = "EUD-B"
callsign = "BRAVO"
username = "eud-b"
script = ["0", "quit"]
`;

  assert.throws(
    () => parseScenario(twice, "example.toml"),
    (error: unknown) => error instanceof ScenarioError && /share an 'id'/.test(String(error)),
  );
});

test("an expectation that is not a regular expression is refused", () => {
  assert.throws(
    () =>
      parseScenario(
        `${valid()}
[euds.expect]
log = ["Interface ((Up"]
`,
        "example.toml",
      ),
    (error: unknown) => error instanceof ScenarioError && /regular expression/.test(String(error)),
  );
});

test("an event matcher has to match something in particular", () => {
  assert.throws(
    () =>
      parseScenario(
        `${valid()}
[euds.expect]
xml_present = [{ }]
`,
        "example.toml",
      ),
    (error: unknown) => error instanceof ScenarioError && /matches everything/.test(String(error)),
  );
});

test("a channel direction outside TAK's vocabulary is refused", () => {
  assert.throws(
    () => parseScenario(valid().replace('"BOTH"', '"READ"'), "example.toml"),
    (error: unknown) => error instanceof ScenarioError && /IN, OUT and BOTH/.test(String(error)),
  );
});

test("a regular expression's quantifier is not mistaken for a placeholder", () => {
  const scenario = parseScenario(
    `${valid()}
[euds.expect]
log = ["EnrollUpdate: step \\\\d{1,2} id"]
`,
    "example.toml",
  );

  assert.equal(scenario.euds[0].expect.log[0], "EnrollUpdate: step \\d{1,2} id");
});
