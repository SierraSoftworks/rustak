/**
 * Reading the two text files `commotest` leaves behind.
 *
 * They are the whole assertion surface: the runner never parses network
 * traffic, never links `commoncommo` and never reads anything from `atak-civ`.
 * It reads `commo-log.txt` and `commo-xml.txt` the way CI reads the output of
 * `openssl` or `curl` — across a process boundary, as text.
 *
 * **Both are read after the process has exited.** `interfaceUp`,
 * `interfaceDown` and the mission-package callbacks do not flush, so a mid-run
 * read can miss lines that have already happened
 * (`.claude/plan/status/M1-00-eud-interop-harness-exploration.md` §3.2).
 *
 * # Why the parsers are deliberately loose
 *
 * `commotest` is an internal upstream test tool with no output-format promise,
 * and its line prefixes (timestamp, level, endpoint id) are not part of any
 * contract. So the log is kept as raw lines and every assertion is a regular
 * expression over a line, and the XML file is scanned for `<event>` elements
 * wherever they appear rather than parsed as a record format. A prefix change
 * upstream then costs nothing; a format assumption would cost a debugging
 * afternoon in a nightly job.
 */

import fs from "node:fs";

/** `commo-log.txt`, as lines. */
export interface LogFile {
  readonly lines: readonly string[];
}

/** One CoT event `commoncommo` received, accepted and re-serialised. */
export interface XmlEvent {
  /** Its position in the file, which is the order it was received in. */
  readonly index: number;

  readonly uid: string;
  readonly type: string;

  /** The uids named by the event's `<link>` elements, for `t-x-d-d` and chat. */
  readonly links: readonly string[];

  /** Whatever `commotest` wrote before the element: its timestamp and endpoint id. */
  readonly prefix: string;

  /** The element itself. */
  readonly raw: string;
}

/** `commo-xml.txt`, as the events it carries. */
export interface XmlFile {
  readonly events: readonly XmlEvent[];
}

/** Reads a file, treating "not there" as empty — a container that never started wrote nothing. */
function readOrEmpty(file: string): string {
  try {
    return fs.readFileSync(file, "utf8");
  } catch {
    return "";
  }
}

/** Parses `commo-log.txt`. */
export function parseLog(text: string): LogFile {
  return {
    lines: text
      .split("\n")
      .map((line) => line.replace(/\r$/, ""))
      .filter((line) => line.length > 0),
  };
}

/** Reads and parses `commo-log.txt`. */
export function readLog(file: string): LogFile {
  return parseLog(readOrEmpty(file));
}

/**
 * Parses `commo-xml.txt`.
 *
 * Every `<event` opens an element that ends at the next `</event>`; anything
 * else on the line before it is kept as the prefix. An unterminated element at
 * the end of the file — a container killed mid-write — is taken as far as it
 * goes rather than dropped, because a truncated event is evidence too.
 */
export function parseXml(text: string): XmlFile {
  const events: XmlEvent[] = [];

  for (let at = text.indexOf("<event"); at !== -1; at = text.indexOf("<event", at + 1)) {
    const close = text.indexOf("</event>", at);
    const next = text.indexOf("<event", at + 1);
    const end = close === -1 || (next !== -1 && next < close) ? (next === -1 ? text.length : next) : close + "</event>".length;
    const raw = text.slice(at, end);
    const open = raw.slice(0, raw.indexOf(">") === -1 ? raw.length : raw.indexOf(">") + 1);

    events.push({
      index: events.length,
      uid: attribute(open, "uid"),
      type: attribute(open, "type"),
      links: [...raw.matchAll(/<link\b[^>]*\buid="([^"]*)"/g)].map((match) => match[1]),
      prefix: text.slice(text.lastIndexOf("\n", at) + 1, at).trim(),
      raw,
    });
  }

  return { events };
}

/** Reads and parses `commo-xml.txt`. */
export function readXml(file: string): XmlFile {
  return parseXml(readOrEmpty(file));
}

/** One attribute of an opening tag, or the empty string. */
function attribute(tag: string, name: string): string {
  const match = new RegExp(`\\b${name}="([^"]*)"`).exec(tag);

  return match === null ? "" : match[1];
}

/** The first line index matching `pattern` at or after `from`, or `-1`. */
export function findLine(log: LogFile, pattern: string, from = 0): number {
  const expression = new RegExp(pattern);

  for (let index = from; index < log.lines.length; index += 1) {
    if (expression.test(log.lines[index])) return index;
  }

  return -1;
}

/** Every line matching `pattern`, for reporting what a failure actually saw. */
export function matchingLines(log: LogFile, pattern: string): string[] {
  const expression = new RegExp(pattern);

  return log.lines.filter((line) => expression.test(line));
}

/**
 * The few log lines worth naming in a report, whatever a scenario asserts.
 *
 * Keeping this list short is deliberate: it exists so that a failing scenario
 * prints what the EUD thought happened, not so that assertions can be written
 * against a structure. Assertions are regular expressions over lines.
 */
export function highlights(log: LogFile): string[] {
  const interesting =
    /(Proto Negotiate|EnrollUpdate|Completed status for enrollment|Interface (Up|Down|Error)|Contact (Added|Removed)|Mission package|Receive of MP)/;

  return log.lines.filter((line) => interesting.test(line));
}
