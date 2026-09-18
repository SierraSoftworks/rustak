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
