/**
 * `npm run test:scenarios`: the EUD suite, start to finish.
 *
 * Three modes, decided by what is on the machine rather than by a flag:
 *
 * | Docker | `RUSTAK_EUD_REQUIRE_DOCKER` | What happens |
 * |---|---|---|
 * | yes | anything | every scenario whose surfaces exist is run for real |
 * | no | `1` | **the run fails, loudly**, naming the image it needed |
 * | no | unset | probe-only: the scenario files are validated and the server's surfaces are probed, and every scenario reports as a skip |
 *
 * The probe-only mode is what a developer gets — Docker is not a prerequisite
 * for working on the loader, the parsers or the assertions, which have their own
 * unit tests (`npm run test:unit`). CI sets `RUSTAK_EUD_REQUIRE_DOCKER=1`, so a
 * nightly job that lost its container runtime fails instead of reporting a
 * green run in which nothing ran.
 *
 * Arguments filter by name: `npm run test:scenarios -- enroll` runs the
 * enrollment scenarios only.
 */

import path from "node:path";
import { fileURLToPath } from "node:url";

import { bootstrap } from "../../shared/src/bootstrap.js";
import { httpsClient } from "../../shared/src/http.js";
import { hasBinary, startServer, waitForPort, waitForServer } from "../../shared/src/launch.js";
import { probeSurfaces } from "../../shared/src/probe.js";

import { dockerAvailable, IMAGE, pullImage, requireDocker } from "./docker.js";
import { executeScenario, type ScenarioResult } from "./execute.js";
import { opensslAvailable } from "./openssl.js";
import { loadScenarios, type Scenario } from "./scenario.js";
import { SURFACES, SURFACE_NAMES, todoFor, type SurfaceName } from "./surfaces.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const suiteRoot = path.resolve(here, "..");

/** Starts a plain server, bootstraps it and reports which surfaces it serves. */
async function probe(): Promise<Record<SurfaceName, boolean>> {
  const server = await startServer({
    prefix: "rustak-interop-eud-probe-",
    name: "rustak-interop-eud-probe",
    host: "localhost",
  });

  try {
    await waitForServer(server);
    // The stream listener binds after the public one, so probing it the moment
    // the API answers would report it absent on a fast machine.
    await waitForPort(server.ports.stream, 20_000);

    const client = httpsClient(server.webtak, server.caFile);
    const admin = await bootstrap(server, client, {
      adminUsername: "interop-admin",
      displayName: "EUD interop probe",
      serverName: "rustak-interop-eud-probe",
      domains: [server.host],
      baseUrl: server.webtak,
    });

    return await probeSurfaces(SURFACES, {
      client,
      token: admin.token,
      stream: server.stream,
    });
  } finally {
    server.stop();
  }
}

/** The first surface a scenario needs and the server does not serve. */
function missingSurface(
  scenario: Scenario,
  surfaces: Record<SurfaceName, boolean> | undefined,
): string | undefined {
  if (surfaces === undefined) return undefined;

  const missing = scenario.requires.find((name) => !surfaces[name]);

  return missing === undefined ? undefined : todoFor(missing);
}

/** Prints one scenario's outcome. */
function report(result: ScenarioResult): void {
  const mark = result.status === "pass" ? "✔" : result.status === "fail" ? "✖" : "﹣";

  console.log(`${mark} ${result.name}`);

  for (const reason of result.reasons) console.log(`    ${reason}`);

  if (result.status === "fail") {
    for (const note of result.notes) console.log(`    · ${note}`);
    if (result.artefacts) console.log(`    · artefacts kept in ${result.artefacts}`);
  }
}

const filters = process.argv.slice(2).filter((argument) => !argument.startsWith("-"));
const scenarios = loadScenarios(path.join(suiteRoot, "scenarios")).filter(
  (scenario) => filters.length === 0 || filters.some((filter) => scenario.name.includes(filter)),
);

console.log(`[eud] ${scenarios.length} scenario(s) loaded and validated.`);

if (process.env.RUSTAK_EUD_REQUIRE_DOCKER === "1") requireDocker();

const docker = dockerAvailable();
const server = hasBinary();

if (!docker) {
  console.log(`[eud] no container runtime — probe-only run. The image would be ${IMAGE}.`);
}

if (docker && !opensslAvailable()) {
  throw new Error(
    "`openssl` is not on PATH, and the runner needs it to build the PKCS#12 truststore `estream:` verifies rustak against.",
  );
}

if (!server) {
  console.log(
    "[eud] no rustak binary — nothing to probe. Build it first:\n" +
      "        cd rustak-ui && trunk build && cd .. && cargo build -p rustak-server",
  );
}

if (docker) {
  const pulled = pullImage();

  console.log(`[eud] docker pull ${IMAGE}: ${pulled.ok ? "ok" : `failed — ${pulled.output}`}`);
}

const surfaces = server ? await probe() : undefined;

if (surfaces !== undefined) {
  const served = SURFACE_NAMES.filter((name) => surfaces[name]);
  const missing = SURFACE_NAMES.filter((name) => !surfaces[name]);

  console.log(`[eud] surfaces served:  ${served.join(", ") || "(none yet)"}`);
  console.log(`[eud] surfaces missing: ${missing.join(", ") || "(none)"}`);
}

const results: ScenarioResult[] = [];
const artefactRoot = process.env.RUSTAK_EUD_ARTIFACTS ?? path.join(suiteRoot, "artifacts");

for (const scenario of scenarios) {
  const missing = missingSurface(scenario, surfaces);

  if (missing !== undefined) {
    results.push({ name: scenario.name, status: "skip", reasons: [missing], notes: [] });
  } else if (!docker || !server) {
    results.push({
      name: scenario.name,
      status: "skip",
      reasons: [
        docker
          ? "no rustak binary: build it, then run this again."
          : `no container runtime: this scenario needs ${IMAGE}.`,
      ],
      notes: [],
    });
  } else {
    try {
      results.push(await executeScenario(scenario, artefactRoot));
    } catch (error) {
      results.push({
        name: scenario.name,
        status: "fail",
        reasons: [`the scenario could not be run: ${String(error)}`],
        notes: [],
      });
    }
  }

  report(results[results.length - 1]);
}

const failed = results.filter((result) => result.status === "fail").length;
const passed = results.filter((result) => result.status === "pass").length;
const skipped = results.filter((result) => result.status === "skip").length;

console.log(`[eud] ${passed} passed, ${skipped} skipped, ${failed} failed.`);

process.exitCode = failed === 0 ? 0 : 1;
