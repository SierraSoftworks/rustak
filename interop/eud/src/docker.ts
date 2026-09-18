/**
 * Running one EUD, which means running one container.
 *
 * The process boundary is the licence boundary. `commotest` and the GPL library
 * it links live in the image; this repository holds a `Dockerfile` that clones
 * a pinned upstream commit, scenario data, and this — a `docker run` and a
 * parser for the text it leaves behind. Nothing is linked, included or
 * vendored. See `interop/eud/README.md` → Licence posture before changing
 * anything here.
 *
 * `--network host` because the EUD has to reach three of rustak's listeners on
 * the loopback address the runner reserved ports on, and because `commotest`
 * also *serves* a local HTTPS mission-package endpoint that the server dials
 * back. `--rm` because the only thing worth keeping is the mounted directory.
 */

import { spawn, spawnSync } from "node:child_process";

/** The container runtime, so a runner on podman can say so. */
export const DOCKER = process.env.RUSTAK_EUD_DOCKER ?? "docker";

/** The image the scenarios run, published by the `interop-eud-image` nightly job. */
export const IMAGE =
  process.env.RUSTAK_EUD_IMAGE ?? "ghcr.io/sierrasoftworks/rustak-interop-commoncommo:latest";

/** Whether the container runtime is here at all. */
export function dockerAvailable(): boolean {
  try {
    return spawnSync(DOCKER, ["version", "--format", "{{.Server.Version}}"], {
      stdio: "ignore",
      timeout: 30_000,
    }).status === 0;
  } catch {
    return false;
  }
}

/**
 * Fails, loudly and with the reason, when a run that must have Docker has none.
 *
 * CI sets `RUSTAK_EUD_REQUIRE_DOCKER=1`, so a runner that quietly reported
 * "everything skipped" there would be a green job that tested nothing. A
 * developer's machine sets nothing and gets the probe-only run instead.
 */
export function requireDocker(): void {
  if (dockerAvailable()) return;

  throw new Error(
    [
      "",
      `RUSTAK_EUD_REQUIRE_DOCKER=1 is set, but '${DOCKER}' is not usable here.`,
      "",
      "Every scenario in this suite runs ATAK's own commoncommo inside",
      `${IMAGE},`,
      "so without a container runtime there is nothing to drive rustak with and",
      "a skipped run would prove nothing at all.",
      "",
      "  * In CI: the job must install/start Docker and pull the image first.",
      "  * Locally: start Docker, or unset RUSTAK_EUD_REQUIRE_DOCKER to get the",
      "    probe-only run (scenario files validated, surfaces probed, no EUDs).",
      "",
    ].join("\n"),
  );
}

/** What a container did. */
export interface ContainerResult {
  readonly code: number | null;
  readonly signal: NodeJS.Signals | null;

  /** Whether the runner had to kill it rather than waiting for `quit`. */
  readonly timedOut: boolean;

  /** How long it ran, which is what a scenario's `min_runtime_seconds` is about. */
  readonly seconds: number;

  readonly stdout: string;
  readonly stderr: string;
}

/** How one EUD is started. */
export interface ContainerOptions {
  /** A name, so a timed-out container can be killed by it. */
  readonly name: string;

  /** The host directory mounted at `/work`, where the two text files land. */
  readonly mount: string;

  /** Everything after the image name. */
  readonly argv: readonly string[];

  readonly timeoutSeconds: number;
}

/** Runs one EUD to completion, or kills it when the scenario's timeout passes. */
export function runContainer(options: ContainerOptions): Promise<ContainerResult> {
  const argv = [
    "run",
    "--rm",
    "--name",
    options.name,
    "--network",
    "host",
    "-v",
    `${options.mount}:/work`,
    IMAGE,
    ...options.argv,
  ];

  const started = Date.now();

  return new Promise((resolve, reject) => {
    const child = spawn(DOCKER, argv, { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    let timedOut = false;

    child.stdout.on("data", (chunk: Buffer) => (stdout += chunk.toString("utf8")));
    child.stderr.on("data", (chunk: Buffer) => (stderr += chunk.toString("utf8")));

    const timer = setTimeout(() => {
      timedOut = true;
      // `docker kill` rather than killing the client: the container is the
      // process that matters, and killing the client leaves it running.
      spawnSync(DOCKER, ["kill", options.name], { stdio: "ignore", timeout: 30_000 });
    }, options.timeoutSeconds * 1_000);

    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });

    child.once("close", (code, signal) => {
      clearTimeout(timer);
      resolve({
        code,
        signal,
        timedOut,
        seconds: (Date.now() - started) / 1_000,
        stdout,
        stderr,
      });
    });
  });
}

/** Pulls the image, so the first scenario's timeout is not spent downloading it. */
export function pullImage(): { ok: boolean; output: string } {
  const result = spawnSync(DOCKER, ["pull", IMAGE], { encoding: "utf8", timeout: 900_000 });

  return {
    ok: result.status === 0,
    output: `${result.stdout ?? ""}${result.stderr ?? ""}`.trim(),
  };
}
