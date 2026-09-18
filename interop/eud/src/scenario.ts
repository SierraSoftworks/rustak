/**
 * Scenario files: what a run of this suite is made of.
 *
 * A scenario is data — `scenarios/<name>.toml` — because everything it decides
 * is data: which configuration rustak is started with, what each EUD is called,
 * what `commotest` script it is given, and what its two output files must and
 * must not contain afterwards. Nothing about a scenario needs code, and a
 * scenario written as code is a scenario nobody reads before changing.
 *
 * Validation is strict and happens before anything is started. Upstream's own
 * documentation warns that `commotest`'s argument parser is not error tolerant
 * and does not always catch a bad argument, and it returns 0 from every path —
 * so a typo in a script line would otherwise look exactly like a rustak
 * failure. Everything checkable is checked here instead: the keys, the types,
 * the placeholders, the regular expressions, that the script alternates
 * wait-seconds and commands, and that it ends in `quit`.
 */

import fs from "node:fs";
import path from "node:path";

import { parse as parseToml } from "smol-toml";

import type { ConfigTables } from "../../shared/src/config.js";
import type { Grant } from "../../shared/src/bootstrap.js";
import { PLACEHOLDERS } from "./template.js";
import { isSurfaceName, type SurfaceName } from "./surfaces.js";

/** A CoT event a `commo-xml.txt` must, or must not, contain. */
export interface XmlMatcher {
  readonly uid?: string;
  readonly type?: string;

  /** A uid one of the event's `<link>` elements must name — `t-x-d-d`'s subject. */
  readonly linkUid?: string;
}

/** What one EUD's output files must look like when it has exited. */
export interface EudExpectations {
  /** Regular expressions that must match `commo-log.txt` lines, in this order. */
  readonly log: readonly string[];

  /** Regular expressions that must match some line, in any order. */
  readonly logAny: readonly string[];

  /** Regular expressions that must match no line at all. */
  readonly logAbsent: readonly string[];

  /** Events the EUD must have received. */
  readonly xmlPresent: readonly XmlMatcher[];

  /** Events the EUD must never have received. */
  readonly xmlAbsent: readonly XmlMatcher[];

  /** The common name the issued `commo-enroll-cert.p12` must carry first. */
  readonly enrollCertCn?: string;
}

/** One `commotest` container. */
export interface EudSpec {
  readonly id: string;
  readonly uid: string;
  readonly callsign: string;
  readonly username: string;
  readonly channels: readonly Grant[];

  /** How long after the scenario starts this EUD is launched. */
  readonly startDelaySeconds: number;

  /**
   * When set, the runner revokes what this EUD enrolled with, this many seconds
   * after the scenario started — while it is still connected.
   */
  readonly revokeAfterSeconds?: number;

  /** The argv after `<uid> <callsign> /work`, with placeholders unresolved. */
  readonly script: readonly string[];

  readonly expect: EudExpectations;
}

/** What the server must say about the run, read back through `/api/v1` and Marti. */
export interface ServerExpectations {
  /** Callsigns that must appear in `/Marti/api/clientEndPoints`. */
  readonly clientEndPointsPresent: readonly string[];

  /** Callsigns that must not. */
  readonly clientEndPointsAbsent: readonly string[];

  /** Regular expressions that must match `/api/v1/audit`, the server's own record. */
  readonly auditMatches: readonly string[];
}

/** One scenario, as loaded and validated. */
export interface Scenario {
  readonly name: string;
  readonly summary: string;

  /** Where it was loaded from, so a failure names a file somebody can open. */
  readonly file: string;

  readonly requires: readonly SurfaceName[];

  /** The outer limit on a container; `commotest` has no exit-code contract. */
  readonly timeoutSeconds: number;

  /** How long the EUDs must have been running before the assertions mean anything. */
  readonly minRuntimeSeconds: number;

  /** Configuration tables merged over the launcher's defaults. */
  readonly config: ConfigTables;

  /** Ports to bind instead of reserving free ones. See `[ports]` below. */
  readonly ports: Partial<{ web: number; marti: number; stream: number }>;

  readonly euds: readonly EudSpec[];
  readonly expect: ServerExpectations;
}

/** A scenario file that cannot be run, with the file and key named. */
export class ScenarioError extends Error {}

/** Reads and validates every scenario in `directory`, in file-name order. */
export function loadScenarios(directory: string): Scenario[] {
  return fs
    .readdirSync(directory)
    .filter((entry) => entry.endsWith(".toml"))
    .sort()
    .map((entry) => loadScenario(path.join(directory, entry)));
}

/** Reads and validates one scenario file. */
export function loadScenario(file: string): Scenario {
  return parseScenario(fs.readFileSync(file, "utf8"), file);
}

/** Validates scenario text, so the unit tests do not need a file for every case. */
export function parseScenario(text: string, file: string): Scenario {
  let raw: unknown;

  try {
    raw = parseToml(text);
  } catch (error) {
    throw new ScenarioError(`${file} is not valid TOML: ${String(error)}`);
  }

  const table = object(raw, file, "the file");
  const name = string(table, "name", file);

  if (!/^[a-z0-9-]+$/.test(name)) {
    throw new ScenarioError(`${file}: 'name' must be lower-case, digits and dashes.`);
  }

  const euds = array(table.euds, `${file}: euds`).map((entry, index) =>
    eud(object(entry, file, `euds[${index}]`), file, index),
  );

  if (euds.length === 0) {
    throw new ScenarioError(`${file}: a scenario needs at least one [[euds]] entry.`);
  }

  const ids = new Set(euds.map((entry) => entry.id));

  if (ids.size !== euds.length) {
    throw new ScenarioError(`${file}: two EUDs share an 'id', so their output would collide.`);
  }

  return {
    name,
    summary: string(table, "summary", file),
    file,
    requires: requires(table.requires, file),
    timeoutSeconds: integer(table, "timeout_seconds", file, 300),
    minRuntimeSeconds: integer(table, "min_runtime_seconds", file, 0),
    config: (table.config ?? {}) as ConfigTables,
    ports: ports(table.ports, file),
    euds,
    expect: serverExpectations(table.expect, file),
  };
}

/**
 * `[ports]` — the ports this scenario needs bound, rather than free ones.
 *
 * Only for a scenario that has to reproduce ATAK's defaults: `commotest`'s
 * mission-package transfers build their own HTTPS URL rather than being handed
 * one, so they can only be exercised on the port that convention names.
 * Everything else takes whatever is free, because a fixed port is a port
 * something else may already hold.
 */
function ports(raw: unknown, file: string): Partial<{ web: number; marti: number; stream: number }> {
  if (raw === undefined) return {};

  const table = object(raw, file, "ports");
  const named: Partial<{ web: number; marti: number; stream: number }> = {};

  for (const key of ["web", "marti", "stream"] as const) {
    if (table[key] === undefined) continue;

    const port = integer(table, key, `${file}: ports`, 0);

    if (port < 1024 || port > 65_535) {
      throw new ScenarioError(`${file}: ports.${key} is ${port}, which is not a port to bind.`);
    }

    named[key] = port;
  }

  return named;
}

/** One `[[euds]]` entry. */
function eud(table: Record<string, unknown>, file: string, index: number): EudSpec {
  const where = `${file}: euds[${index}]`;
  const script = array(table.script, `${where}.script`).map((entry, position) => {
    if (typeof entry !== "string") {
      throw new ScenarioError(`${where}.script[${position}] is not a string.`);
    }

    return entry;
  });

  checkScript(script, where);

  return {
    id: string(table, "id", where),
    uid: string(table, "uid", where),
    callsign: string(table, "callsign", where),
    username: string(table, "username", where),
    channels: channels(table.channels, where),
    startDelaySeconds: integer(table, "start_delay_seconds", where, 0),
    revokeAfterSeconds:
      table.revoke_after_seconds === undefined
        ? undefined
        : integer(table, "revoke_after_seconds", where, 0),
    script,
    expect: eudExpectations(table.expect, where),
  };
}

/**
 * Checks the argv shape `commotest <uid> <callsign> <dir> { <wait> <command> }`.
 *
 * A script that does not end in `quit` runs forever, and a wait that is not a
 * number is silently taken as a command by a parser that does not complain.
 */
function checkScript(script: readonly string[], where: string): void {
  if (script.length === 0 || script.length % 2 !== 0) {
    throw new ScenarioError(
      `${where}.script must be pairs of <wait-seconds> <command>; it has ${script.length} entries.`,
    );
  }

  for (let index = 0; index < script.length; index += 2) {
    if (!/^\d+$/.test(script[index])) {
      throw new ScenarioError(
        `${where}.script[${index}] is '${script[index]}', which is not a whole number of seconds.`,
      );
    }

    checkPlaceholders(script[index + 1], `${where}.script[${index + 1}]`);
  }

  if (script[script.length - 1] !== "quit") {
    throw new ScenarioError(
      `${where}.script must end with 'quit'; commotest runs forever otherwise.`,
    );
  }
}

/** Refuses a placeholder the runner would not substitute. */
function checkPlaceholders(command: string, where: string): void {
  for (const match of command.matchAll(/\{([a-z_]+)\}/g)) {
    if (!(PLACEHOLDERS as readonly string[]).includes(match[1])) {
      throw new ScenarioError(
        `${where} uses {${match[1]}}, which is not a placeholder the runner fills. Known: ${PLACEHOLDERS.map((entry) => `{${entry}}`).join(", ")}.`,
      );
    }
  }
}

/** `[euds.expect]`. */
function eudExpectations(raw: unknown, where: string): EudExpectations {
  const table = raw === undefined ? {} : object(raw, where, "expect");
  const log = regexes(table.log, `${where}.expect.log`);
  const logAny = regexes(table.log_any, `${where}.expect.log_any`);
  const logAbsent = regexes(table.log_absent, `${where}.expect.log_absent`);
  const enrollCertCn = optional(table, "enroll_cert_cn", `${where}.expect`);

  return {
    log,
    logAny,
    logAbsent,
    xmlPresent: matchers(table.xml_present, `${where}.expect.xml_present`),
    xmlAbsent: matchers(table.xml_absent, `${where}.expect.xml_absent`),
    enrollCertCn,
  };
}

/** `[expect]`, the half of the assertion that is read back from the server. */
function serverExpectations(raw: unknown, file: string): ServerExpectations {
  const table = raw === undefined ? {} : object(raw, file, "expect");

  return {
    clientEndPointsPresent: strings(
      table.client_endpoints_present,
      `${file}: expect.client_endpoints_present`,
    ),
    clientEndPointsAbsent: strings(
      table.client_endpoints_absent,
      `${file}: expect.client_endpoints_absent`,
    ),
    auditMatches: regexes(table.audit_matches, `${file}: expect.audit_matches`),
  };
}

/** `channels = [{ group = "…", direction = "IN" | "OUT" | "BOTH" }]`. */
function channels(raw: unknown, where: string): Grant[] {
  return array(raw, `${where}.channels`).map((entry, index) => {
    const table = object(entry, where, `channels[${index}]`);
    const direction = string(table, "direction", `${where}.channels[${index}]`);

    if (direction !== "IN" && direction !== "OUT" && direction !== "BOTH") {
      throw new ScenarioError(
        `${where}.channels[${index}].direction is '${direction}'; TAK knows IN, OUT and BOTH.`,
      );
    }

    return { group: string(table, "group", `${where}.channels[${index}]`), direction };
  });
}

/** `requires = ["enrollment", …]`, checked against the surfaces the runner probes. */
function requires(raw: unknown, file: string): SurfaceName[] {
  return strings(raw, `${file}: requires`).map((entry) => {
    if (!isSurfaceName(entry)) {
      throw new ScenarioError(
        `${file}: requires names '${entry}', which is not a surface src/surfaces.ts knows how to probe.`,
      );
    }

    return entry;
  });
}

/**
 * A list of regular expressions, compiled here so a bad one fails loading.
 *
 * They may carry placeholders too — a scenario asserting on its own account
 * name should not have to repeat it — which is why the placeholder check runs
 * here as well. `{2}` and friends are unaffected: a placeholder is letters and
 * underscores only, so a quantifier is never mistaken for one.
 */
function regexes(raw: unknown, where: string): string[] {
  return strings(raw, where).map((pattern, index) => {
    try {
      new RegExp(pattern);
    } catch (error) {
      throw new ScenarioError(`${where}[${index}] is not a regular expression: ${String(error)}`);
    }

    checkPlaceholders(pattern, `${where}[${index}]`);

    return pattern;
  });
}

/** A list of `{ uid = "…", type = "…", link_uid = "…" }` matchers, at least one key each. */
function matchers(raw: unknown, where: string): XmlMatcher[] {
  return array(raw, where).map((entry, index) => {
    const table = object(entry, where, `[${index}]`);
    const at = `${where}[${index}]`;
    const matcher = {
      uid: optional(table, "uid", at),
      type: optional(table, "type", at),
      linkUid: optional(table, "link_uid", at),
    };

    if (matcher.uid === undefined && matcher.type === undefined && matcher.linkUid === undefined) {
      throw new ScenarioError(`${at} matches everything; give it a uid, a type or a link_uid.`);
    }

    return matcher;
  });
}

/** An optional string that may carry placeholders. */
function optional(
  table: Record<string, unknown>,
  key: string,
  where: string,
): string | undefined {
  const value = table[key];

  if (value === undefined) return undefined;

  if (typeof value !== "string") {
    throw new ScenarioError(`${where}.${key} is not a string.`);
  }

  checkPlaceholders(value, `${where}.${key}`);

  return value;
}

/** A table, or a failure naming where it should have been. */
function object(raw: unknown, file: string, where: string): Record<string, unknown> {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    throw new ScenarioError(`${file}: ${where} is not a table.`);
  }

  return raw as Record<string, unknown>;
}

/** An array, defaulting to empty. */
function array(raw: unknown, where: string): unknown[] {
  if (raw === undefined) return [];

  if (!Array.isArray(raw)) {
    throw new ScenarioError(`${where} is not an array.`);
  }

  return raw;
}

/** An array of strings, defaulting to empty. */
function strings(raw: unknown, where: string): string[] {
  return array(raw, where).map((entry, index) => {
    if (typeof entry !== "string") {
      throw new ScenarioError(`${where}[${index}] is not a string.`);
    }

    return entry;
  });
}

/** A required string. */
function string(table: Record<string, unknown>, key: string, where: string): string {
  const value = table[key];

  if (typeof value !== "string" || value.length === 0) {
    throw new ScenarioError(`${where}: '${key}' is required and must be a non-empty string.`);
  }

  return value;
}

/** An optional whole number, with a default. */
function integer(
  table: Record<string, unknown>,
  key: string,
  where: string,
  fallback: number,
): number {
  const value = table[key];

  if (value === undefined) return fallback;

  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
    throw new ScenarioError(`${where}: '${key}' must be a whole number of seconds.`);
  }

  return value;
}
