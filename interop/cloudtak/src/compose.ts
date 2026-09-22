/**
 * Driving the compose stack: is there a runtime, bring it up, take it down,
 * keep its logs.
 *
 * `docker compose` (the plugin), not `docker-compose` (the old Python script),
 * because the health-gated `depends_on` the stack uses is v2-only. Every
 * variable the compose file interpolates is passed explicitly rather than left
 * to the ambient environment, so `src/settings.ts` and `docker-compose.yml`
 * cannot drift apart about which port is published — the unit tests assert the
 * same thing from the other side.
 */

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

import {
  ARTIFACT_DIR,
  CLOUDTAK_TAG,
  PORTS,
  RUSTAK_IMAGE,
  SUITE_ROOT,
} from "./settings.js";

/** The compose service CloudTAK itself runs as, for a log read by name. */
export const CLOUDTAK_SERVICE = "cloudtak";

/** The container runtime, so a runner on podman can say so. */
export const DOCKER = process.env.RUSTAK_INTEROP_DOCKER ?? "docker";

/** What one compose invocation did. */
export interface Ran {
  readonly ok: boolean;
  readonly output: string;
}

/** The environment the compose file interpolates. */
function environment(): NodeJS.ProcessEnv {
  return {
    ...process.env,
    RUSTAK_INTEROP_RUSTAK_IMAGE: RUSTAK_IMAGE,
    RUSTAK_INTEROP_CLOUDTAK_TAG: CLOUDTAK_TAG,
    RUSTAK_INTEROP_WEBTAK_PORT: String(PORTS.webtak),
    RUSTAK_INTEROP_MARTI_PORT: String(PORTS.marti),
    RUSTAK_INTEROP_STREAM_PORT: String(PORTS.stream),
    RUSTAK_INTEROP_CLOUDTAK_PORT: String(PORTS.cloudtak),
    // rustak writes its database, its keys and the one-time setup token into
    // the bind mount, and the runner reads that token back. Running the
    // container as the invoking user is what keeps those files readable here
    // and removable afterwards; on a platform without uids this falls back to
    // the image's own root, which is what compose defaults to anyway.
    RUSTAK_INTEROP_UID: String(process.getuid?.() ?? 0),
    RUSTAK_INTEROP_GID: String(process.getgid?.() ?? 0),
  };
}

/** Runs one `docker compose` subcommand in the suite directory. */
export function compose(args: readonly string[], timeoutMs = 300_000): Ran {
  const result = spawnSync(DOCKER, ["compose", ...args], {
    cwd: SUITE_ROOT,
    encoding: "utf8",
    env: environment(),
    timeout: timeoutMs,
  });

  return {
    ok: result.status === 0,
    output: `${result.stdout ?? ""}${result.stderr ?? ""}`.trim(),
  };
}

/** Whether the runtime is here and answering at all. */
export function dockerAvailable(): boolean {
  try {
    const version = spawnSync(DOCKER, ["version", "--format", "{{.Server.Version}}"], {
      stdio: "ignore",
      timeout: 30_000,
    });

    if (version.status !== 0) return false;

    return spawnSync(DOCKER, ["compose", "version"], { stdio: "ignore", timeout: 30_000 }).status === 0;
  } catch {
    return false;
  }
}

/** Whether an image is already in the local store. */
export function imageExists(image: string): boolean {
  try {
    return spawnSync(DOCKER, ["image", "inspect", image], { stdio: "ignore", timeout: 60_000 }).status === 0;
  } catch {
    return false;
  }
}

/**
 * Fails, loudly and with the reason, when a run that must have Docker has none.
 *
 * The default is to require it — unlike `interop/eud`, where a probe-only run
 * still says something useful, there is nothing at all this suite can assert
 * without the stack. `RUSTAK_INTEROP_REQUIRE_DOCKER=0` is the developer's
 * opt-out: the unit tests still run, and every scenario reports as a skip.
 */
export function requireDocker(): void {
  if (dockerAvailable()) return;

  throw new Error(
    [
      "",
      `'${DOCKER} compose' is not usable here, and this suite is nothing without it.`,
      "",
      "Every assertion in it is made against CloudTAK's own container talking to",
      "a rustak built from this checkout, so a skipped run would prove nothing.",
      "",
      "  * In CI: the job must have Docker and must have built the rustak image",
      `    (${RUSTAK_IMAGE}) from rustak-server/Dockerfile.`,
      "  * Locally: start Docker, or set RUSTAK_INTEROP_REQUIRE_DOCKER=0 to get",
      "    the unit tests and a run in which every scenario skips.",
      "",
    ].join("\n"),
  );
}

/** Starts the stack, waiting for the images to be pulled and the containers created. */
export function up(): Ran {
  return compose(["up", "--detach", "--no-build", "--quiet-pull"], 900_000);
}

/** Stops it and removes the volumes, so the next run starts from an empty database. */
export function down(): Ran {
  return compose(["down", "--volumes", "--remove-orphans", "--timeout", "20"], 300_000);
}

/**
 * Writes every container's log to the artefact directory.
 *
 * Called on failure, and only then: on a green run the logs say nothing that
 * the assertions did not already say, and rustak's are noisy by design.
 */
export function keepLogs(name = "compose.log"): string | undefined {
  const logs = compose(["logs", "--no-color", "--timestamps"], 120_000);

  if (logs.output.length === 0) return undefined;

  fs.mkdirSync(ARTIFACT_DIR, { recursive: true });

  const file = path.join(ARTIFACT_DIR, name);

  fs.writeFileSync(file, `${logs.output}\n`, "utf8");

  return file;
}

/**
 * The lines CloudTAK writes when rustak hands it XML its parser will not read.
 *
 * This is the whole symptom of the 2026-09-22 outage, and it appeared *only*
 * here: rustak had answered `200`, so nothing in rustak's own log said
 * anything. CloudTAK parses every CoT it receives with sax, which is strict,
 * and the flow tag rustak stamps on each relay is an attribute named after the
 * installation's display name — `SierraSoftworks TAK`, with a space in it. sax
 * read `TAK-Server-SierraSoftworks TAK="…"` as an attribute with no value,
 * threw, and CloudTAK dropped the message off its socket. Three days of feed
 * tracks, EUD positions and chat went nowhere.
 *
 * Substrings rather than expressions, because what is being matched is the
 * text of somebody else's error messages: a regular expression tuned to the
 * exact wording of one CloudTAK release stops matching at the next one, and a
 * guard that silently stops matching is worse than none.
 */
export const PARSE_FAILURES: readonly string[] = [
  // CloudTAK's own wrapper around a failed `sax` parse of an inbound message.
  "Failed to parse CoT XML",
  // sax itself, which is the line that actually appeared in production.
  "Attribute without value",
  // The neighbouring sax refusals a badly derived name could produce instead:
  // a name that starts with a digit or punctuation, or an unquoted value.
  "Invalid attribute name",
  "Unquoted attribute value",
  "Invalid character in tag name",
];

/**
 * One service's log, without compose's `service | ` prefix.
 *
 * Returns the whole `Ran`, not just its output, because a caller looking for
 * the *absence* of a line cannot tell "the log said nothing bad" from "the log
 * could not be read" unless it is told which happened.
 */
export function serviceLog(service: string, timeoutMs = 120_000): Ran {
  return compose(["logs", "--no-color", "--no-log-prefix", service], timeoutMs);
}

/**
 * The lines of `log` that say a parser refused something we sent.
 *
 * Case-insensitive: the wording is somebody else's and its capitalisation is
 * not something this suite should depend on.
 */
/** What reading CloudTAK's log for parse failures proved, and how it reads. */
export interface LogVerdict {
  readonly status: "pass" | "fail";
  readonly reasons: readonly string[];
}

/**
 * Whether `log` shows CloudTAK's parser accepting everything rustak sent it.
 *
 * A step that looks for the absence of a line passes when it finds nothing —
 * including when it had nothing to look at. An unreadable or empty log is the
 * one way this check can lie, and it would lie *green*, so both are failures
 * here rather than a pass nobody earned. The passing reason counts the lines it
 * read and names the server the stack ran, so the green is checkable against
 * the log that produced it.
 */
export function parseLogVerdict(log: Ran, serverName: string): LogVerdict {
  if (!log.ok) {
    return {
      status: "fail",
      reasons: [
        `could not read CloudTAK's log, so nothing here proves its parser accepted anything: ${log.output || "no output"}`,
      ],
    };
  }

  const lines = log.output.split("\n").filter((line) => line.trim() !== "");

  if (lines.length === 0) {
    return {
      status: "fail",
      reasons: ["CloudTAK's log came back empty, so nothing here proves its parser accepted anything."],
    };
  }

  const refused = parseFailures(log.output);

  if (refused.length > 0) {
    return {
      status: "fail",
      reasons: [
        `CloudTAK refused ${String(refused.length)} message(s) rustak sent it; the first few:`,
        ...refused.slice(0, 5),
      ],
    };
  }

  return {
    status: "pass",
    reasons: [
      `CloudTAK's parser refused nothing rustak sent it across ${String(lines.length)} log line(s), as '${serverName}'.`,
    ],
  };
}

export function parseFailures(log: string): string[] {
  const needles = PARSE_FAILURES.map((needle) => needle.toLowerCase());

  return log
    .split("\n")
    .filter((line) => needles.some((needle) => line.toLowerCase().includes(needle)));
}
