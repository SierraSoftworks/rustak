/**
 * `npm run test:suite`: the CloudTAK stack, start to finish.
 *
 * | Docker | `RUSTAK_INTEROP_REQUIRE_DOCKER` | What happens |
 * |---|---|---|
 * | yes | anything | the stack is built, started and driven for real |
 * | no | unset or `1` | **the run fails, loudly**, naming what it needed |
 * | no | `0` | every scenario reports as a skip and the run passes |
 *
 * The default is the opposite of `interop/eud`'s on purpose: there is no
 * probe-only mode worth having here, because every assertion this suite makes
 * is made *through CloudTAK*, and without a container runtime there is no
 * CloudTAK. What a developer without Docker still gets is `npm run test:unit` —
 * the request builders and parsers, against fixtures — which is what `npm test`
 * runs first either way.
 *
 * Arguments filter by step name: `npm run test:suite -- package` runs the
 * stack and only the package step (and whatever it depends on having run).
 */

import fs from "node:fs";

import {
  CLOUDTAK_SERVICE,
  down,
  imageExists,
  keepLogs,
  parseFailures,
  requireDocker,
  serviceLog,
  up,
  dockerAvailable,
} from "./compose.js";
import { opensslAvailable, generatePki } from "./pki.js";
import { SERVER_NAME, writeConfiguration } from "./rustak.js";
import { prepareSession, waitForStack } from "./session.js";
import { ARTIFACT_DIR, HOST_URLS, RUN_DIR, RUSTAK_IMAGE } from "./settings.js";
import { uiSmoke } from "./smoke.js";
import { STEPS, type RunState } from "./steps.js";
import { todoFor } from "./surfaces.js";

/** One step's outcome. */
interface Outcome {
  readonly name: string;
  readonly status: "pass" | "fail" | "skip";
  readonly reasons: readonly string[];
}

const results: Outcome[] = [];
const filters = process.argv.slice(2).filter((argument) => !argument.startsWith("-"));
const wanted = STEPS.filter((step) => filters.length === 0 || filters.some((filter) => step.name.includes(filter)));

/** Prints one outcome as it happens, so a slow run says where it is. */
function report(outcome: Outcome): void {
  const mark = outcome.status === "pass" ? "✔" : outcome.status === "fail" ? "✖" : "﹣";

  results.push(outcome);
  console.log(`${mark} ${outcome.name}`);

  for (const reason of outcome.reasons) console.log(`    ${reason}`);
}

/** Everything skips, with one reason, and the run still says what it would have done. */
function skipEverything(reason: string): void {
  for (const step of wanted) report({ name: step.name, status: "skip", reasons: [reason] });

  report({ name: "ui-smoke", status: "skip", reasons: [reason] });
}

/** The count a reader of the job log looks at first. */
function summarise(): number {
  const passed = results.filter((result) => result.status === "pass").length;
  const skipped = results.filter((result) => result.status === "skip").length;
  const failures = results.filter((result) => result.status === "fail").length;

  console.log(`[cloudtak] ${String(passed)} passed, ${String(skipped)} skipped, ${String(failures)} failed.`);

  return failures;
}

/** Removes the previous run's working directory, and says so if it cannot. */
function clearRunDirectory(): void {
  try {
    fs.rmSync(RUN_DIR, { recursive: true, force: true });
  } catch (error) {
    throw new Error(
      `${RUN_DIR} could not be removed (${String(error)}). A previous run's container may still own the files in it: \`docker compose down --volumes\` in interop/cloudtak, then remove it by hand.`,
    );
  }

  fs.mkdirSync(RUN_DIR, { recursive: true });
}

const required = process.env.RUSTAK_INTEROP_REQUIRE_DOCKER !== "0";

if (required) requireDocker();

if (!dockerAvailable()) {
  console.log("[cloudtak] no container runtime, and RUSTAK_INTEROP_REQUIRE_DOCKER=0 — nothing to drive.");
  skipEverything("no container runtime: this suite drives CloudTAK's own container.");
  summarise();
  process.exit(0);
}

if (!opensslAvailable()) {
  throw new Error(
    "`openssl` is not on PATH, and the runner needs it to build the test CA whose root CloudTAK is handed through NODE_EXTRA_CA_CERTS.",
  );
}

if (!imageExists(RUSTAK_IMAGE)) {
  throw new Error(
    [
      "",
      `The rustak image '${RUSTAK_IMAGE}' is not in the local image store.`,
      "",
      "Build it the way the nightly job does — the Dockerfile packages a binary",
      "rather than compiling one, and the UI is embedded into that binary at",
      "compile time, so both come first:",
      "",
      "    cd rustak-ui && trunk build --release",
      "    cd .. && cargo build --release -p rustak-server",
      "    mkdir -p dist && cp target/release/rustak dist/rustak",
      `    docker build -f rustak-server/Dockerfile -t ${RUSTAK_IMAGE} .`,
      "",
      "Or point RUSTAK_INTEROP_RUSTAK_IMAGE at one you already have.",
      "",
    ].join("\n"),
  );
}

console.log(`[cloudtak] ${String(wanted.length)} step(s); rustak image ${RUSTAK_IMAGE}.`);

let failed = false;

try {
  // A previous run that was killed leaves containers holding the published
  // ports, so the teardown comes first as well as last.
  down();
  clearRunDirectory();
  generatePki();
  writeConfiguration();

  const started = up();

  if (!started.ok) throw new Error(`docker compose up failed:\n${started.output}`);

  await waitForStack();

  const session = await prepareSession();
  const state: RunState = {};

  console.log(`[cloudtak] rustak at ${HOST_URLS.webtak}, CloudTAK at ${HOST_URLS.cloudtak}.`);
  console.log(
    `[cloudtak] surfaces missing: ${
      Object.entries(session.surfaces)
        .filter(([, served]) => !served)
        .map(([name]) => name)
        .join(", ") || "(none)"
    }`,
  );

  for (const step of wanted) {
    const missing = step.requires.find((surface) => !session.surfaces[surface]);

    if (missing !== undefined) {
      report({ name: step.name, status: "skip", reasons: [todoFor(missing)] });
      continue;
    }

    if (session.enrolled === undefined) {
      report({
        name: step.name,
        status: "skip",
        reasons: [
          `CloudTAK has no client certificate to be configured with: ${session.enrollmentFailure ?? "enrollment did not run"}`,
        ],
      });
      continue;
    }

    // Configuring and signing in are the two calls an unauthenticated caller
    // makes; everything after them needs the session the second one produced.
    const needsSession = step.name !== "configure-server" && step.name !== "login";

    if (needsSession && !session.cloudtak.authenticated) {
      report({ name: step.name, status: "skip", reasons: ["no CloudTAK session: the login step did not pass."] });
      continue;
    }

    try {
      const reasons = await step.run({
        cloudtak: session.cloudtak,
        operator: session.operator,
        enrolled: session.enrolled,
        state,
      });

      report({ name: step.name, status: "pass", reasons });
    } catch (error) {
      failed = true;
      report({ name: step.name, status: "fail", reasons: [error instanceof Error ? error.message : String(error)] });
    }
  }

  const smoke = session.cloudtak.authenticated
    ? await uiSmoke({
        base: HOST_URLS.cloudtak,
        username: session.operator.username,
        password: session.operator.password,
        // Undefined when the Data Sync step did not run: the smoke then opens
        // the menu and looks at it rather than waiting for a name that was
        // never created.
        missionName: state.mission?.name,
        artifacts: ARTIFACT_DIR,
      })
    : { status: "skip" as const, reasons: ["no CloudTAK session to sign in with."], screenshots: [] };

  if (smoke.status === "fail") failed = true;

  report({
    name: "ui-smoke",
    status: smoke.status,
    reasons: [...smoke.reasons, ...smoke.screenshots.map((file) => `screenshot: ${file}`)],
  });

  // Last, because it is about everything that came before it. CloudTAK parses
  // every CoT rustak sends it with sax, which is strict, and when sax refuses
  // a message CloudTAK drops it off the socket and answers 500 for a whole
  // mission document — while rustak, which answered 200, logs nothing at all.
  // That asymmetry is why a suite full of green steps ran for three days over
  // an installation relaying XML nothing could read (2026-09-22): the only
  // place the truth was written down was CloudTAK's own log, so this run reads
  // it. The server this stack runs is deliberately called something a careless
  // derivation breaks on — see `src/rustak.ts`.
  const refused = parseFailures(serviceLog(CLOUDTAK_SERVICE));

  if (refused.length > 0) failed = true;

  report({
    name: "cloudtak-parse-log",
    status: refused.length === 0 ? "pass" : "fail",
    reasons:
      refused.length === 0
        ? [`CloudTAK's parser refused nothing rustak sent it, as '${SERVER_NAME}'.`]
        : [
            `CloudTAK refused ${String(refused.length)} message(s) rustak sent it; the first few:`,
            ...refused.slice(0, 5),
          ],
  });
} catch (error) {
  failed = true;
  report({
    name: "stack",
    status: "fail",
    reasons: [error instanceof Error ? error.message : String(error)],
  });
} finally {
  // Before the teardown, and only on a failure: `docker compose logs` reads
  // stopped containers too, but not removed ones, and on a green run rustak's
  // log says nothing the assertions did not.
  if (failed) {
    const log = keepLogs();

    if (log !== undefined) console.log(`[cloudtak] container logs kept in ${log}`);
  }

  const stopped = down();

  if (!stopped.ok) console.error(`[cloudtak] docker compose down failed:\n${stopped.output}`);
}

process.exitCode = summarise() === 0 ? 0 : 1;
