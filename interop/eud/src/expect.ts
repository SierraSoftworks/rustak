/**
 * The assertion engine: what a scenario's expectations mean.
 *
 * Everything here is a pure function of a scenario's expectations and the text
 * an EUD left behind, which is what lets the whole engine be tested on fixtures
 * with no Docker and no server (`tests/expect.test.ts`).
 *
 * Two rules the scenarios lean on, both from
 * `.claude/plan/status/M1-00-eud-interop-harness-exploration.md`:
 *
 * - **Assert on a positive marker, not only on the absence of an error.**
 *   `commotest` returns 0 from every path, including an argument error, so "no
 *   `Interface Error`" is satisfied by a run that never connected at all. Every
 *   scenario therefore names lines that must be *there*, and the absence list
 *   is a second check rather than the check.
 * - **The ordered list is ordered.** A negotiation that is offered, requested
 *   and accepted is a sequence; asserting the three lines in any order would
 *   pass against a server that answered before it offered.
 */

import { findLine, highlights, matchingLines, type LogFile, type XmlFile } from "./artefacts.js";
import type { EudSpec, Scenario, ServerExpectations, XmlMatcher } from "./scenario.js";
import { render, type Substitutions } from "./template.js";

/** What one EUD left behind. */
export interface EudArtefacts {
  readonly log: LogFile;
  readonly xml: XmlFile;

  /** The subject of the certificate in `commo-enroll-cert.p12`, when one was issued. */
  readonly certSubject?: string;

  /** How long the container ran, which is what `min_runtime_seconds` is about. */
  readonly ranForSeconds: number;

  /** Whether the runner had to kill it rather than waiting for `quit`. */
  readonly timedOut: boolean;
}

/** Everything the assertions found wrong, in the order a reader should see it. */
export function checkEud(
  spec: EudSpec,
  artefacts: EudArtefacts,
  values: Substitutions,
): string[] {
  const failures: string[] = [];
  const where = `[${spec.id}]`;

  if (artefacts.timedOut) {
    failures.push(
      `${where} did not exit within the scenario's timeout; its script should end in 'quit'.`,
    );
  }

  if (artefacts.log.lines.length === 0) {
    failures.push(
      `${where} wrote no commo-log.txt at all — the container never ran, or its argv was refused.`,
    );

    return failures;
  }

  failures.push(...checkOrdered(spec, artefacts, values, where));
  failures.push(...checkUnordered(spec, artefacts, values, where));
  failures.push(...checkAbsent(spec, artefacts, values, where));
  failures.push(...checkXml(spec, artefacts, values, where));
  failures.push(...checkCertificate(spec, artefacts, values, where));

  return failures;
}

/** The ordered log expectations, each matched after the one before it. */
function checkOrdered(
  spec: EudSpec,
  artefacts: EudArtefacts,
  values: Substitutions,
  where: string,
): string[] {
  const failures: string[] = [];
  let from = 0;

  for (const pattern of spec.expect.log) {
    const rendered = render(pattern, values);
    const at = findLine(artefacts.log, rendered, from);

    if (at === -1) {
      failures.push(
        `${where} commo-log.txt has no line matching /${rendered}/ after line ${from}.`,
      );

      // Carry on from where we were: reporting every missing step at once is
      // more useful than stopping at the first.
      continue;
    }

    from = at + 1;
  }

  return failures;
}

/** The unordered log expectations. */
function checkUnordered(
  spec: EudSpec,
  artefacts: EudArtefacts,
  values: Substitutions,
  where: string,
): string[] {
  return spec.expect.logAny
    .map((pattern) => render(pattern, values))
    .filter((pattern) => findLine(artefacts.log, pattern) === -1)
    .map((pattern) => `${where} commo-log.txt has no line matching /${pattern}/.`);
}

/** The lines that must not be there, reported with the line that was. */
function checkAbsent(
  spec: EudSpec,
  artefacts: EudArtefacts,
  values: Substitutions,
  where: string,
): string[] {
  const failures: string[] = [];

  for (const pattern of spec.expect.logAbsent) {
    const rendered = render(pattern, values);
    const found = matchingLines(artefacts.log, rendered);

    if (found.length > 0) {
      failures.push(
        `${where} commo-log.txt matches the forbidden /${rendered}/: ${found[0]}${found.length > 1 ? ` (and ${found.length - 1} more)` : ""}`,
      );
    }
  }

  return failures;
}

/** Whether a received event is the one a matcher describes. */
export function matches(event: XmlFile["events"][number], matcher: XmlMatcher): boolean {
  if (matcher.uid !== undefined && event.uid !== matcher.uid) return false;
  if (matcher.type !== undefined && event.type !== matcher.type) return false;
  if (matcher.linkUid !== undefined && !event.links.includes(matcher.linkUid)) return false;

  return true;
}

/** A matcher, written the way a failure should read. */
function describe(matcher: XmlMatcher): string {
  return (
    [
      matcher.uid === undefined ? undefined : `uid=${matcher.uid}`,
      matcher.type === undefined ? undefined : `type=${matcher.type}`,
      matcher.linkUid === undefined ? undefined : `link uid=${matcher.linkUid}`,
    ]
      .filter((part) => part !== undefined)
      .join(" ") || "anything"
  );
}

/** The events that must and must not have reached this EUD. */
function checkXml(
  spec: EudSpec,
  artefacts: EudArtefacts,
  values: Substitutions,
  where: string,
): string[] {
  const failures: string[] = [];
  const resolve = (matcher: XmlMatcher): XmlMatcher => ({
    uid: matcher.uid === undefined ? undefined : render(matcher.uid, values),
    type: matcher.type === undefined ? undefined : render(matcher.type, values),
    linkUid: matcher.linkUid === undefined ? undefined : render(matcher.linkUid, values),
  });

  for (const matcher of spec.expect.xmlPresent) {
    const wanted = resolve(matcher);

    if (!artefacts.xml.events.some((event) => matches(event, wanted))) {
      failures.push(
        `${where} commo-xml.txt has no event with ${describe(wanted)}; it received ${summarise(artefacts.xml)}.`,
      );
    }
  }

  for (const matcher of spec.expect.xmlAbsent) {
    const unwanted = resolve(matcher);
    const seen = artefacts.xml.events.find((event) => matches(event, unwanted));

    if (seen !== undefined) {
      failures.push(
        `${where} commo-xml.txt has the forbidden event ${describe(unwanted)} (uid=${seen.uid} type=${seen.type}).`,
      );
    }
  }

  return failures;
}

/** What an EUD saw, as a short phrase for a failure message. */
export function summarise(xml: XmlFile): string {
  if (xml.events.length === 0) return "nothing at all";

  const kinds = new Map<string, number>();

  for (const event of xml.events) {
    const key = `${event.uid} ${event.type}`;

    kinds.set(key, (kinds.get(key) ?? 0) + 1);
  }

  return [...kinds]
    .slice(0, 8)
    .map(([key, count]) => `${key} x${count}`)
    .join(", ");
}

/** The certificate rustak issued, inspected with ordinary tooling. */
function checkCertificate(
  spec: EudSpec,
  artefacts: EudArtefacts,
  values: Substitutions,
  where: string,
): string[] {
  if (spec.expect.enrollCertCn === undefined) return [];

  const wanted = render(spec.expect.enrollCertCn, values);

  if (artefacts.certSubject === undefined) {
    return [
      `${where} expected an issued commo-enroll-cert.p12 with CN=${wanted} first, and there is none.`,
    ];
  }

  // The subject is normalised to `CN=x, O=y` in DER order, so "first" is a
  // prefix test: ATAK builds the CSR with the common name leading, and a
  // server that reordered it would hand back a certificate ATAK's own
  // enrollment path would not recognise as its own.
  return artefacts.certSubject.startsWith(`CN=${wanted}`)
    ? []
    : [`${where} commo-enroll-cert.p12 subject is '${artefacts.certSubject}', not CN=${wanted} first.`];
}

/** The server half of a scenario's expectations. */
export function checkClientEndpoints(
  expect: ServerExpectations,
  everSeen: readonly string[],
  stillConnected: readonly string[] = everSeen,
): string[] {
  const failures: string[] = [];

  // "Present" is a question about the whole run — did this EUD ever get far
  // enough to be listed — so it reads the union the sampler accumulated.
  for (const wanted of expect.clientEndPointsPresent) {
    if (!everSeen.includes(wanted)) {
      failures.push(
        `/Marti/api/clientEndPoints does not list '${wanted}'; it lists ${everSeen.join(", ") || "nobody"}.`,
      );
    }
  }

  // "Absent" is a question about a moment, and it cannot be asked of the union:
  // an EUD that is revoked mid-scenario had to connect first, so it is in the
  // union by construction. The caller passes the reading taken after the
  // revocation instead, and a scenario with no revocation falls back to the
  // union, where the two are the same question.
  for (const unwanted of expect.clientEndPointsAbsent) {
    if (stillConnected.includes(unwanted)) {
      failures.push(
        `/Marti/api/clientEndPoints still lists '${unwanted}' after it should have gone; it lists ${
          stillConnected.join(", ") || "nobody"
        }.`,
      );
    }
  }

  return failures;
}

/**
 * The server's own record of what it did, for the scenarios that need one.
 *
 * Matched against the raw `/api/v1/audit` body rather than against a parsed
 * shape, deliberately: this suite asserts that rustak *recorded* an action, not
 * what the audit DTO looks like — node-tak and the Rust tests own that.
 */
export function checkAudit(expect: ServerExpectations, trail: string): string[] {
  return expect.auditMatches
    .filter((pattern) => !new RegExp(pattern).test(trail))
    .map((pattern) => `/api/v1/audit has nothing matching /${pattern}/.`);
}

/**
 * Whether the scenario ran long enough for its assertions to mean anything.
 *
 * Measured against the EUD that ran *longest*, because that is the scenario's
 * own wall clock. The shortest is the wrong number: a scenario may script one
 * EUD to leave early on purpose — `chat-direct`'s BRAVO quits at T+35 so that
 * ALPHA can speak into the gap at T+50 — and reading that deliberate departure
 * as a run cut short fails the scenario for doing exactly what it was written
 * to do.
 *
 * Nothing is lost by it: an EUD that died before its script finished is caught
 * by `timedOut` (the runner killed it) and by its own `log`/`xml` expectations,
 * both of which are per EUD.
 */
export function checkRuntime(scenario: Scenario, artefacts: readonly EudArtefacts[]): string[] {
  const longest = Math.max(...artefacts.map((entry) => entry.ranForSeconds));

  return longest + 1 < scenario.minRuntimeSeconds
    ? [
        `the scenario ran for ${longest.toFixed(0)}s, short of the ${scenario.minRuntimeSeconds}s it needs to prove anything.`,
      ]
    : [];
}

/** The lines worth printing beside a failure. */
export function evidence(artefacts: EudArtefacts): string[] {
  return highlights(artefacts.log).slice(-20);
}
