/**
 * `npm test`: the whole suite, start to finish.
 *
 * 1. Start a throwaway rustak with a scratch configuration (`rustak.ts`).
 * 2. Wait for the listener and for the authority it issued its own certificate
 *    from.
 * 3. Run `prepare.ts` in a child process with `NODE_EXTRA_CA_CERTS` pointing at
 *    that authority: it bootstraps the installation through `/api/v1`, probes
 *    which compatibility surfaces the server has, and writes the session.
 * 4. Run the scenarios under `node:test`, in child processes with the same
 *    trust store and the session's path in the environment.
 * 5. Stop the server and remove the scratch directory, whatever happened.
 *
 * Steps 3 and 4 are separate processes from this one because
 * `NODE_EXTRA_CA_CERTS` is read at process start and the authority does not
 * exist until step 1 has finished — see `prepare.ts`.
 *
 * `--` passes arguments through to `node --test`, so
 * `npm test -- tests/login.test.ts` runs one scenario file and
 * `npm test -- --test-name-pattern=version` one scenario.
 */

import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { startServer, waitForServer, type RunningServer } from "./rustak.js";
import { SESSION_ENV } from "./session.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const suiteRoot = path.resolve(here, "..");

/** Every scenario file, in the order the contract is walked. */
const SCENARIOS = [
  // The harness's own contract first: if this fails, nothing below means
  // anything, and a suite where every scenario skips looks the same as one that
  // is quietly broken.
  "bootstrap.test.ts",
  "version.test.ts",
  "login.test.ts",
  "enrollment.test.ts",
  "groups.test.ts",
  "contacts.test.ts",
  "stream.test.ts",
  "missions.test.ts",
  "files.test.ts",
  "cloudtak-onboarding.test.ts",
].map((name) => path.join("tests", name));

/** Runs a Node child with TypeScript support and the server's authority trusted. */
function run(args: string[], env: NodeJS.ProcessEnv): Promise<number> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ["--import", "tsx", ...args], {
      cwd: suiteRoot,
      stdio: "inherit",
      env: { ...process.env, ...env },
    });

    child.on("error", reject);
    child.on("exit", (code, signal) => resolve(code ?? (signal ? 1 : 0)));
  });
}

let server: RunningServer | undefined;

function stop(): void {
  server?.stop();
  server = undefined;
}

process.on("exit", stop);

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
  process.on(signal, () => {
    stop();
    process.exit(130);
  });
}

try {
  server = await startServer();

  console.log(`[interop] server binary: ${process.env.RUSTAK_INTEROP_BINARY ?? "target/{debug,release}/rustak"}`);
  console.log(`[interop] workspace:     ${server.directory}`);
  console.log(`[interop] webtak:        ${server.webtak}`);
  console.log(`[interop] marti (mTLS):  ${server.api}`);
  console.log(`[interop] stream:        ${server.stream}`);

  await waitForServer(server);

  const serverFile = path.join(server.directory, "server.json");
  const sessionFile = path.join(server.directory, "session.json");

  fs.writeFileSync(
    serverFile,
    JSON.stringify(
      {
        directory: server.directory,
        dataDir: server.dataDir,
        caFile: server.caFile,
        webtak: server.webtak,
        api: server.api,
        stream: server.stream,
      },
      null,
      2,
    ),
    "utf8",
  );

  const trusted: NodeJS.ProcessEnv = { NODE_EXTRA_CA_CERTS: server.caFile };

  const prepared = await run([path.join("src", "prepare.ts"), serverFile, sessionFile], trusted);

  if (prepared !== 0) {
    console.error("[interop] the bootstrap failed, so no scenario was run.");
    process.exitCode = prepared;
  } else {
    const passthrough = process.argv.slice(2);
    const files = passthrough.some((argument) => !argument.startsWith("-")) ? [] : SCENARIOS;

    process.exitCode = await run(["--test", ...passthrough, ...files], {
      ...trusted,
      [SESSION_ENV]: sessionFile,
    });
  }
} finally {
  stop();
}
