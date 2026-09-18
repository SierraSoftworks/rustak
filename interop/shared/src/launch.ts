/**
 * Launches a throwaway rustak for an interop suite.
 *
 * This is `interop/node-tak/src/rustak.ts` as it stood after M2-05, lifted here
 * so that `interop/eud` drives the same launcher rather than a copy of it. Two
 * suites that start the server differently would eventually disagree about what
 * "a rustak with an internal CA in a scratch directory" means, and the first
 * symptom of that is a scenario failing in one suite and passing in the other.
 *
 * What the callers keep in common, and why:
 *
 * 1. **TLS is real.** `[web.public.tls] mode = "internal"` — no suite may turn
 *    verification off, because the trust path is part of what is under test:
 *    node-tak's `webtak` calls go through `undici` with full verification, and
 *    `commotest` verifies the chain against the truststore it is handed.
 * 2. **All three listeners are configured**, because both suites exercise all
 *    three roles (browser/enrollment, mutually authenticated Marti, CoT stream).
 * 3. **The server is reached at a name, never an address.** WebAuthn identifies
 *    a relying party by domain, so the bootstrap's passkey cannot be registered
 *    against `127.0.0.1` (`.claude/plan/status/M0-11-web-api-auth.md` deviation
 *    6). The listeners still *bind* `127.0.0.1`: binding the name would make
 *    start-up depend on whether this machine has an IPv6 loopback, and a bind
 *    of a family it does not have is a failure rather than a fallback. Node
 *    connects with Happy Eyeballs, so `localhost` reaches the bound address
 *    either way — and an EUD in a `--network host` container reaches the same
 *    socket as `127.0.0.1`.
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

import { renderConfig, mergeConfig, type ConfigTables } from "./config.js";

const here = path.dirname(fileURLToPath(import.meta.url));

/** The repository root, three levels up from `interop/shared/src`. */
export const REPO_ROOT = path.resolve(here, "..", "..", "..");

/** The ports one server binds. */
export interface ServerPorts {
  /** `[web.public]`: the admin UI, `/api/v1`, `/oauth/*` and enrollment. */
  readonly web: number;

  /** `[web.marti]`: the mutually authenticated Marti listener. */
  readonly marti: number;

  /** `[stream.tls]`: the CoT stream. */
  readonly stream: number;
}

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

  /** The host name the suite reaches it at. */
  readonly host: string;

  /** The ports it bound. */
  readonly ports: ServerPorts;
}

/** A running server, and how to stop it. */
export interface RunningServer extends ServerInfo {
  readonly child: ChildProcess;
  stop(): void;
}

/** What a suite chooses about the server it starts. */
export interface LaunchOptions {
  /** The scratch-directory prefix, which is also how stale ones are swept. */
  readonly prefix: string;

  /** `[server] name`. */
  readonly name?: string;

  /** The host name the suite reaches the server at. Never an address. */
  readonly host?: string;

  /** Configuration tables merged over the defaults below. */
  readonly config?: ConfigTables;

  /**
   * Ports to bind instead of reserving free ones.
   *
   * Every suite should take what it is given — a fixed port is a port another
   * process can already hold. The exception is a scenario that has to reproduce
   * ATAK's *defaults*, because a client that builds a URL out of a convention
   * rather than out of what it was told can only be tested on the port the
   * convention names.
   */
  readonly ports?: Partial<ServerPorts>;
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

/** Whether a server binary is there at all, for suites that degrade instead of failing. */
export function hasBinary(): boolean {
  try {
    resolveBinary();
    return true;
  } catch {
    return false;
  }
}

/**
 * Removes scratch directories a previous run did not get to.
 *
 * Ordinary exits clean up after themselves; a run that was SIGKILLed — a
 * cancelled CI job, a crash — leaves its directory and the database encryption
 * key inside it behind. Only directories old enough that no live run could own
 * them are touched.
 */
function sweepStaleWorkspaces(prefix: string): void {
  const cutoff = Date.now() - 6 * 60 * 60 * 1000;

  let entries: string[];

  try {
    entries = fs.readdirSync(os.tmpdir());
  } catch {
    return;
  }

  for (const entry of entries) {
    if (!entry.startsWith(prefix)) continue;

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

/**
 * The configuration every suite starts from.
 *
 * A suite adds to it with `LaunchOptions.config`, which is merged key by key so
 * that a scenario can set `[auth] anon_group_default = false` without having to
 * restate the table — TOML has no way to define one twice, so appending text
 * would be a parse error rather than an override.
 */
function defaults(directory: string, host: string, name: string, ports: ServerPorts): ConfigTables {
  return {
    server: {
      name,
      // The first domain is canonical: it is the subject of the internal server
      // certificate and the host in every enrollment QR code.
      domains: [host],
      base_url: `https://${host}:${ports.web}`,
      data_dir: directory,
    },
    storage: { reader_connections: 1 },
    "web.public": { listen: [`127.0.0.1:${ports.web}`] },
    // Not "none": both suites verify the chain, so an internal CA they have to
    // trust is exactly the deployment under test.
    "web.public.tls": { mode: "internal" },
    "web.marti": { enabled: true, listen: `127.0.0.1:${ports.marti}` },
    "stream.tls": { enabled: true, listen: `127.0.0.1:${ports.stream}` },
    auth: {
      // The suite is the whole directory: no identity provider, so both filters
      // would otherwise deny everybody.
      user_acl: "true",
      admin_acl: "true",
    },
  };
}

/** Starts a server in a fresh scratch directory. */
export async function startServer(options: LaunchOptions): Promise<RunningServer> {
  const binary = resolveBinary();
  const host = options.host ?? "localhost";

  sweepStaleWorkspaces(options.prefix);

  const directory = fs.mkdtempSync(path.join(os.tmpdir(), options.prefix));
  const configPath = path.join(directory, "config.toml");
  const ports: ServerPorts = {
    web: options.ports?.web ?? (await reservePort()),
    marti: options.ports?.marti ?? (await reservePort()),
    stream: options.ports?.stream ?? (await reservePort()),
  };

  const tables = mergeConfig(
    defaults(directory, host, options.name ?? options.prefix.replace(/-$/, ""), ports),
    options.config ?? {},
  );

  fs.writeFileSync(
    configPath,
    `# Generated by interop/shared/src/launch.ts. Do not edit; it is thrown away.\n${renderConfig(tables)}`,
    "utf8",
  );

  // `--env` is not optional, and it must point at a path that does not exist:
  // this repository's root `.env` is a named pipe, so a path inside the scratch
  // directory is belt-and-braces on top of rustak's own `is_file()` guard.
  const child = spawn(
    binary,
    ["--config", configPath, "--env", path.join(directory, ".env.absent")],
    { cwd: directory, stdio: ["ignore", "inherit", "inherit"], env: process.env },
  );

  let stopped = false;

  // The child inherits this process's stdout and stderr, which is what makes a
  // scenario's server log readable in the job output — and also means that a
  // runner which dies without calling `stop()` leaves it holding that pipe
  // open. In CI the step is piped through `tee`, so `tee` never sees EOF, the
  // step never returns, and the job sits there until its timeout: 82 wasted
  // minutes on run 35379867680, after an unhandled rejection killed the runner
  // mid-scenario. `exit` fires for a normal exit, an uncaught exception and an
  // unhandled rejection alike, and `kill` is synchronous, which is all a
  // handler is allowed to be here.
  const onExit = () => {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
  };

  process.on("exit", onExit);

  return {
    child,
    directory,
    dataDir: directory,
    caFile: path.join(directory, "pki", "ca.crt"),
    webtak: `https://${host}:${ports.web}`,
    api: `https://${host}:${ports.marti}`,
    stream: `ssl://${host}:${ports.stream}`,
    host,
    ports,
    stop() {
      if (stopped) return;
      stopped = true;
      process.removeListener("exit", onExit);

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
}

/**
 * Waits until the listener is bound and the authority it issued its own
 * certificate from has been written out.
 *
 * Both, because the suites need the port *and* the file they will trust — a
 * socket that accepts before `pki/ca.crt` exists would hand the caller a
 * certificate it has no way to verify.
 */
export async function waitForServer(server: RunningServer, timeoutMs = 60_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;

  while (Date.now() < deadline) {
    if (server.child.exitCode !== null || server.child.signalCode !== null) {
      throw new Error(
        `rustak exited before it was ready (code ${String(server.child.exitCode)}, signal ${String(server.child.signalCode)}).`,
      );
    }

    if (fs.existsSync(server.caFile) && (await canConnect(server.ports.web))) {
      return;
    }

    await new Promise((resolve) => setTimeout(resolve, 100));
  }

  throw new Error(`rustak did not start listening on ${server.webtak} within ${timeoutMs}ms.`);
}

/**
 * Waits for one of the server's own ports to accept a connection.
 *
 * The listeners do not all bind at once: the public one is up while the CoT
 * stream listener is still starting, so a suite that probed the stream the
 * moment `/api/v1/health` answered would be told there is nothing there. This
 * is the wait that turns that race into a bounded one.
 */
export async function waitForPort(port: number, timeoutMs = 30_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;

  while (Date.now() < deadline) {
    if (await canConnect(port)) return true;

    await new Promise((resolve) => setTimeout(resolve, 100));
  }

  return false;
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
