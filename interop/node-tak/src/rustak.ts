/**
 * Launches a throwaway rustak for the contract suite.
 *
 * The same shape as `e2e/scripts/start-server.mjs`, with three differences that
 * the CloudTAK contract forces:
 *
 * 1. **TLS is real.** `[web.public.tls] mode = "internal"` — node-tak's
 *    `webtak` calls go through `undici`'s `fetch` with full verification and no
 *    override (`compat/cloudtak.md` §3), so the suite has to trust rustak's own
 *    authority rather than turn verification off. The runner points
 *    `NODE_EXTRA_CA_CERTS` at `<data_dir>/pki/ca.crt`, which is the very thing
 *    a CloudTAK operator has to do.
 * 2. **All three listeners are configured**, because CloudTAK stores three
 *    independent base URLs and this suite exercises all three roles.
 * 3. **The server is reached at `localhost`, never `127.0.0.1`.** WebAuthn
 *    identifies a relying party by domain, so an address cannot register the
 *    passkey the bootstrap needs (`.claude/plan/status/M0-11-web-api-auth.md`
 *    deviation 6). The listeners still *bind* `127.0.0.1`, exactly as
 *    `e2e/scripts/start-server.mjs` explains: binding the name would make
 *    start-up depend on whether this machine has an IPv6 loopback, and a bind
 *    of a family it does not have is a failure rather than a fallback. Node
 *    connects with Happy Eyeballs (`autoSelectFamily`, on by default since
 *    Node 20), so `localhost` reaches the bound address either way.
 *
 * Nothing is built here. The UI is embedded into the binary by `include_dir!`
 * at compile time, so a build started from this file would race the thing it is
 * meant to serve.
 */

import { spawn, type ChildProcess } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

/** The repository root, three levels up from `interop/node-tak/src`. */
export const REPO_ROOT = path.resolve(here, "..", "..", "..");

/** The prefix every scratch directory this suite creates carries. */
const SCRATCH_PREFIX = "rustak-interop-node-tak-";

/**
 * Where a started server is, in the form that survives being written to a file
 * and read back by another process.
 */
export interface ServerInfo {
  /** The scratch directory holding the config, the database and the store. */
  readonly directory: string;

  /** `[server] data_dir`, which is the same directory. */
  readonly dataDir: string;

  /** The authority the server issued its own certificate from. */
  readonly caFile: string;

  /** CloudTAK's `webtak`: OAuth, enrollment and Marti over a verified chain. */
  readonly webtak: string;

  /** CloudTAK's `api`: the mutually authenticated Marti listener. */
  readonly api: string;

  /** CloudTAK's `url`: the CoT stream. */
  readonly stream: string;
}

/** A running server, and how to stop it. */
export interface RunningServer extends ServerInfo {
  readonly child: ChildProcess;
  stop(): void;
}

/**
 * The server binary to run.
 *
 * `RUSTAK_INTEROP_BINARY` wins outright when it is set, and is an error when it
 * names something that is not there: silently falling back to a different
 * binary than the one explicitly asked for would test the wrong thing and say
 * nothing about it. Otherwise both profiles are accepted and the newer wins,
 * on the assumption that it is the one just built.
 */
export function resolveBinary(): string {
  const suffix = process.platform === "win32" ? ".exe" : "";
  const override = process.env.RUSTAK_INTEROP_BINARY;

  if (override) {
    if (!fs.existsSync(override)) {
      throw new Error(`RUSTAK_INTEROP_BINARY points at ${override}, which does not exist.`);
    }

    return override;
  }

  const candidates = [
    path.join(REPO_ROOT, "target", "debug", `rustak${suffix}`),
    path.join(REPO_ROOT, "target", "release", `rustak${suffix}`),
  ];

  const found = candidates
    .filter((candidate) => fs.existsSync(candidate))
    .map((candidate) => ({ candidate, mtime: fs.statSync(candidate).mtimeMs }))
    .sort((a, b) => b.mtime - a.mtime);

  if (found.length === 0) {
    throw new Error(
      [
        "",
        "No `rustak` binary was found, so there is nothing to test against.",
        "",
        "The UI is embedded into the binary at compile time, so it has to be",
        "built first. Build both, in this order:",
        "",
        "    cd rustak-ui && trunk build",
        "    cd .. && cargo build -p rustak-server",
        "",
        `Looked in:\n${candidates.map((candidate) => `    ${candidate}`).join("\n")}`,
        "",
      ].join("\n"),
    );
  }

  return found[0].candidate;
}

/**
 * Removes scratch directories a previous run did not get to.
 *
 * Ordinary exits clean up after themselves; a run that was SIGKILLed — a
 * cancelled CI job, a crash — leaves its directory and the database encryption
 * key inside it behind. Only directories old enough that no live run could own
 * them are touched.
 */
function sweepStaleWorkspaces(): void {
  const cutoff = Date.now() - 6 * 60 * 60 * 1000;

  let entries: string[];

  try {
    entries = fs.readdirSync(os.tmpdir());
  } catch {
    return;
  }

  for (const entry of entries) {
    if (!entry.startsWith(SCRATCH_PREFIX)) continue;

    const stale = path.join(os.tmpdir(), entry);

    try {
      if (fs.statSync(stale).mtimeMs < cutoff) {
        fs.rmSync(stale, { recursive: true, force: true });
      }
    } catch {
      // Another run may own it, or it may have just gone. Either way it is not
      // this run's problem.
    }
  }
}

/** A port nothing is listening on, found by binding it and letting go. */
export function reservePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const probe = net.createServer();

    probe.once("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();

      if (address === null || typeof address === "string") {
        probe.close(() => reject(new Error("could not read the reserved port")));
        return;
      }

      probe.close(() => resolve(address.port));
    });
  });
}

/** The configuration this run's server is given. */
function configFor(directory: string, ports: { web: number; marti: number; stream: number }): string {
  return [
    "# Generated by interop/node-tak/src/rustak.ts. Do not edit; it is thrown away.",
    "[server]",
    'name = "rustak-interop-node-tak"',
    // The first domain is canonical: it is the subject of the internal server
    // certificate and the host in every enrollment QR code.
    'domains = ["localhost"]',
    `base_url = "https://localhost:${ports.web}"`,
    `data_dir = ${JSON.stringify(directory)}`,
    "",
    "[storage]",
    "reader_connections = 1",
    "",
    "[web.public]",
    `listen = ["127.0.0.1:${ports.web}"]`,
    "",
    "[web.public.tls]",
    // Not "none": node-tak's webtak calls verify the chain, so an internal CA
    // the suite has to trust is exactly the deployment under test.
    'mode = "internal"',
    "",
    "[web.marti]",
    "enabled = true",
    `listen = "127.0.0.1:${ports.marti}"`,
    "",
    "[stream.tls]",
    "enabled = true",
    `listen = "127.0.0.1:${ports.stream}"`,
    "",
    "[auth]",
    // The suite is the whole directory: no identity provider, so both filters
    // would otherwise deny everybody.
    "user_acl  = 'true'",
    "admin_acl = 'true'",
    // CloudTAK has no other way to authenticate, so this is the credential the
    // whole suite hangs off.
    "client_passwords_enabled = true",
    "",
  ].join("\n");
}

/** Starts a server in a fresh scratch directory. */
export async function startServer(): Promise<RunningServer> {
  const binary = resolveBinary();

  sweepStaleWorkspaces();

  const directory = fs.mkdtempSync(path.join(os.tmpdir(), SCRATCH_PREFIX));
  const configPath = path.join(directory, "config.toml");
  const ports = {
    web: await reservePort(),
    marti: await reservePort(),
    stream: await reservePort(),
  };

  fs.writeFileSync(configPath, configFor(directory, ports), "utf8");

  // `--env` is not optional, and it must point at a path that does not exist:
  // this repository's root `.env` is a named pipe, so a path inside the scratch
  // directory is belt-and-braces on top of rustak's own `is_file()` guard.
  const child = spawn(
    binary,
    ["--config", configPath, "--env", path.join(directory, ".env.absent")],
    {
      cwd: directory,
      stdio: ["ignore", "inherit", "inherit"],
      env: process.env,
    },
  );

  let stopped = false;

  const server: RunningServer = {
    child,
    directory,
    dataDir: directory,
    caFile: path.join(directory, "pki", "ca.crt"),
    webtak: `https://localhost:${ports.web}`,
    api: `https://localhost:${ports.marti}`,
    stream: `ssl://localhost:${ports.stream}`,
    stop() {
      if (stopped) return;
      stopped = true;

      if (child.exitCode === null && child.signalCode === null) {
        child.kill("SIGTERM");
      }

      try {
        fs.rmSync(directory, { recursive: true, force: true });
      } catch (error) {
        console.error(`[interop] could not remove ${directory}: ${String(error)}`);
      }
    },
  };

  return server;
}

/**
 * Waits until the listener is bound and the authority it issued its own
 * certificate from has been written out.
 *
 * Both, because the scenarios need the port *and* the file `NODE_EXTRA_CA_CERTS`
 * will name — a socket that accepts before `pki/ca.crt` exists would hand the
 * suite a certificate it has no way to verify.
 */
export async function waitForServer(server: RunningServer, timeoutMs = 60_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  const port = Number(new URL(server.webtak).port);

  while (Date.now() < deadline) {
    if (server.child.exitCode !== null || server.child.signalCode !== null) {
      throw new Error(
        `rustak exited before it was ready (code ${String(server.child.exitCode)}, signal ${String(server.child.signalCode)}).`,
      );
    }

    if (fs.existsSync(server.caFile) && (await canConnect(port))) {
      return;
    }

    await new Promise((resolve) => setTimeout(resolve, 100));
  }

  throw new Error(`rustak did not start listening on ${server.webtak} within ${timeoutMs}ms.`);
}

/** Whether something accepts a TCP connection on the address the server binds. */
function canConnect(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = net.connect({ port, host: "127.0.0.1" });

    const settle = (answer: boolean) => {
      socket.destroy();
      resolve(answer);
    };

    socket.once("connect", () => settle(true));
    socket.once("error", () => settle(false));
    socket.setTimeout(2_000, () => settle(false));
  });
}
