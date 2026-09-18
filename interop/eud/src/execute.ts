/**
 * Running one scenario: start a server, start its EUDs, assert on what they
 * left behind.
 *
 * The order matters and is not an implementation detail:
 *
 * 1. The server comes up and the accounts and tokens are minted *before* any
 *    container starts, because a one-time enrolment token that does not exist
 *    yet looks exactly like one the server refused.
 * 2. The EUDs run concurrently, each with its own mounted directory, each
 *    started after its own `start_delay_seconds` — which is how a scenario
 *    arranges for B to be listening before A says anything, or for B to be
 *    *gone* when A speaks to it.
 * 3. The server side is sampled **while they run**, because
 *    `/Marti/api/clientEndPoints?showCurrentlyConnectedClients=true` is about
 *    who is connected now, and by the time a container has exited nobody is.
 * 4. The two text files are read **after** every container has exited, because
 *    `commotest`'s interface and mission-package callbacks do not flush
 *    (M1-00 §3.2).
 */

import fs from "node:fs";
import path from "node:path";

import { readLog, readXml } from "./artefacts.js";
import { runContainer } from "./docker.js";
import {
  checkAudit,
  checkClientEndpoints,
  checkEud,
  checkRuntime,
  evidence,
  type EudArtefacts,
} from "./expect.js";
import { readCertSubject } from "./openssl.js";
import type { Scenario } from "./scenario.js";
import { auditTrail, connectedCallsigns, openSession, P12_PASSWORD, revokeEud } from "./session.js";
import { renderArgv } from "./template.js";

/** How a scenario ended. */
export interface ScenarioResult {
  readonly name: string;
  readonly status: "pass" | "fail" | "skip";

  /** Why it failed, or why it was skipped. */
  readonly reasons: readonly string[];

  /** What was seen, for a reader working out why. */
  readonly notes: readonly string[];

  /** Where the artefacts were kept, when they were. */
  readonly artefacts?: string;
}

/** Sleeps, for a scenario's staggered starts. */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Runs one scenario end to end. */
export async function executeScenario(
  scenario: Scenario,
  artefactRoot: string,
): Promise<ScenarioResult> {
  const session = await openSession(scenario);
  const failures: string[] = [];
  const notes: string[] = [];

  try {
    const seen = new Set<string>();
    const sampling = sample(session, seen);

    // These start running the moment they are created and are not awaited until
    // every container has exited, so a revocation that throws would spend
    // minutes as a rejected promise nobody is watching — and node kills the
    // process for that, taking the `finally` below with it and orphaning the
    // server, which then holds the job's stdout open until the 90-minute
    // timeout. That is what happened to the whole run on 35379867680. A failed
    // revocation is a scenario failure, so it is recorded as one here and this
    // promise never rejects.
    const revocations = scenario.euds
      .filter((eud) => eud.revokeAfterSeconds !== undefined)
      .map(async (eud) => {
        await sleep((eud.revokeAfterSeconds ?? 0) * 1_000);

        try {
          await revokeEud(session, eud.id);
          notes.push(`[${eud.id}] revoked at T+${eud.revokeAfterSeconds}s`);
        } catch (error) {
          failures.push(
            `[${eud.id}] revoking at T+${eud.revokeAfterSeconds}s failed: ${
              error instanceof Error ? error.message : String(error)
            }`,
          );
        }
      });

    const runs = await Promise.all(
      scenario.euds.map(async (eud) => {
        await sleep(eud.startDelaySeconds * 1_000);

        const account = session.accounts.get(eud.id);

        if (account === undefined) throw new Error(`no account for EUD '${eud.id}'`);

        const argv = renderArgv(eud, session.substitutions(eud));

        notes.push(`[${eud.id}] commotest ${argv.join(" ")}`);

        const container = await runContainer({
          name: `rustak-eud-${scenario.name}-${eud.id}-${process.pid}`,
          mount: account.out,
          argv,
          timeoutSeconds: scenario.timeoutSeconds,
        });

        return { eud, account, container };
      }),
    );

    sampling.stop();
    await sampling.done;
    await Promise.all(revocations);

    const artefacts: EudArtefacts[] = [];

    for (const run of runs) {
      const parsed: EudArtefacts = {
        log: readLog(path.join(run.account.out, "commo-log.txt")),
        xml: readXml(path.join(run.account.out, "commo-xml.txt")),
        certSubject: readCertSubject(
          path.join(run.account.out, "commo-enroll-cert.p12"),
          P12_PASSWORD,
        ),
        ranForSeconds: run.container.seconds,
        timedOut: run.container.timedOut,
      };

      artefacts.push(parsed);
      failures.push(...checkEud(run.eud, parsed, session.substitutions(run.eud)));
      notes.push(
        `[${run.eud.id}] ran ${parsed.ranForSeconds.toFixed(0)}s, ${parsed.log.lines.length} log lines, ${parsed.xml.events.length} events received`,
        ...evidence(parsed).map((line) => `[${run.eud.id}] ${line}`),
      );
    }

    failures.push(...checkRuntime(scenario, artefacts));
    failures.push(...checkClientEndpoints(scenario.expect, [...seen]));
    notes.push(`clientEndPoints saw: ${[...seen].join(", ") || "nobody"}`);

    if (scenario.expect.auditMatches.length > 0) {
      failures.push(...checkAudit(scenario.expect, await auditTrail(session)));
    }

    const kept =
      failures.length > 0 || process.env.RUSTAK_EUD_KEEP === "1"
        ? keep(scenario, session, artefactRoot)
        : undefined;

    return {
      name: scenario.name,
      status: failures.length === 0 ? "pass" : "fail",
      reasons: failures,
      notes,
      artefacts: kept,
    };
  } finally {
    session.stop();
  }
}

/** Polls the server for who is connected until it is told to stop. */
function sample(
  session: Awaited<ReturnType<typeof openSession>>,
  into: Set<string>,
): { stop(): void; done: Promise<void> } {
  let running = true;

  const done = (async () => {
    while (running) {
      try {
        for (const callsign of await connectedCallsigns(session)) into.add(callsign);
      } catch {
        // The surface may not be served, or the server may be busy. Either way
        // the scenario's own assertions report it; a sampling failure is not a
        // failure on its own.
      }

      await sleep(2_000);
    }
  })();

  return {
    stop() {
      running = false;
    },
    done,
  };
}

/**
 * Copies a failed scenario's output files somewhere the CI job can upload them.
 *
 * The scratch directory is removed when the session stops, so this is the only
 * chance: without it, the nightly job's only evidence would be the summary this
 * runner prints.
 */
function keep(
  scenario: Scenario,
  session: Awaited<ReturnType<typeof openSession>>,
  artefactRoot: string,
): string {
  const destination = path.join(artefactRoot, scenario.name);

  for (const account of session.accounts.values()) {
    const into = path.join(destination, account.id);

    fs.mkdirSync(into, { recursive: true });

    for (const file of ["commo-log.txt", "commo-xml.txt", "commo-enroll-cert.p12"]) {
      try {
        fs.copyFileSync(path.join(account.out, file), path.join(into, file));
      } catch {
        // A scenario that never enrolled has no keystore, and one whose
        // container never started has neither text file. Nothing to keep.
      }
    }
  }

  return destination;
}
