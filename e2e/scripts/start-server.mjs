#!/usr/bin/env node
//! Launches a throwaway rustak server for the end-to-end suite.
//
// The server is *not* built here. The UI is embedded into the binary by
// `include_dir!` at compile time, so a build started from this script would
// race the very thing it is meant to serve. Building is the developer's (or
// CI's) job, in this order, and this script only checks the result exists:
//
//     cd rustak-ui && trunk build
//     cd .. && cargo build -p rustak-server
//
// Everything the server writes — the SQLite database, the encryption key
// beside it, the content store, the CA it generates, the setup token, the
// configuration itself — goes into a scratch directory that is emptied before
// each run and removed when the process exits, so a run never sees another
// run's records and never touches a developer's own `config.toml` or `data/`
// directory.
//
// The scratch directory's path is *derived from the port* rather than randomly
// generated, because the tests have to find the setup token inside it: a
// first-run installation's only credential is a file on the server's own
// filesystem, and `tests/helpers.ts` reads it to bootstrap the first
// administrator. `playwright.config.ts` computes the same path and exports it
// as `RUSTAK_E2E_WORKSPACE`, which wins here when it is set.

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..", "..");

const PORT = Number(process.env.RUSTAK_E2E_PORT ?? 18446);

/**
 * The address the listener binds.
 *
 * Loopback by IP, while the *browser* reaches it by name — see `HOST` below.
 * Binding `localhost` instead would make start-up depend on whether this
 * machine resolves it to `::1`, `127.0.0.1` or both, and a bind of a family
 * the host does not have is a start-up failure rather than a fallback.
 */
const BIND = process.env.RUSTAK_E2E_BIND ?? "127.0.0.1";

/**
 * The host name the suite addresses the server by, and therefore the WebAuthn
 * relying party.
 *
 * It has to be a *name*: WebAuthn identifies a relying party by domain, so a
 * console reached at `http://127.0.0.1:18446` cannot register a passkey at all
 * (`rustak-server`'s `Passkeys::for_base_url` refuses it in as many words).
 * `localhost` is the one name a browser treats as a secure context over plain
 * HTTP, which is what lets this suite skip TLS and still run real ceremonies.
 */
const HOST = process.env.RUSTAK_E2E_HOST ?? "localhost";

/**
 * What this installation calls itself.
 *
 * Free text an operator types, and therefore hostile on purpose: a space, an
 * ampersand, parentheses and a non-ASCII letter. Every one of those is legal in
 * a display name and illegal or special somewhere the name is interpolated —
 * an XML attribute name (the flow tag on every relayed message), an HTTP header
 * value, a JWT `iss`, a URL. A one-word name proves none of it, which is how a
 * production outage went three days undetected on 2026-09-22.
 *
 * The same string is `rustak-server`'s `config::TEST_SERVER_NAME` and
 * `interop/shared/src/names.ts`'s `HOSTILE_SERVER_NAME`; changing one means
 * changing all three.
 */
const SERVER_NAME = "Rustak Test & Co. (n\u00e4me)";

/**
 * The server binary to run.
 *
 * `RUSTAK_E2E_BINARY` wins outright when it is set, and is an error when it
 * names something that is not there — falling back to a different binary
 * than the one that was explicitly asked for would test the wrong thing and
 * say nothing about it.
 *
 * Otherwise both profiles are accepted, because either is a perfectly good
 * thing to test against and a developer who has just run `cargo build
 * --release` should not be told to build again. When both exist the newer
 * one wins, on the assumption that it is the one just built.
 */
function resolveBinary() {
  const suffix = process.platform === "win32" ? ".exe" : "";

  const override = process.env.RUSTAK_E2E_BINARY;
  if (override) {
    if (!fs.existsSync(override)) {
      throw new Error(
        `RUSTAK_E2E_BINARY points at ${override}, which does not exist.`,
      );
    }
    return override;
  }

  const candidates = [
    path.join(repoRoot, "target", "debug", `rustak${suffix}`),
    path.join(repoRoot, "target", "release", `rustak${suffix}`),
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
        "built first — building the server against an empty `rustak-ui/dist`",
        "produces a server that answers `GET /` with a 500. Build both, in",
        "this order:",
        "",
        "    cd rustak-ui && trunk build",
        "    cd .. && cargo build -p rustak-server",
        "",
        `Looked in:\n${candidates.map((c) => `    ${c}`).join("\n")}`,
        "",
      ].join("\n"),
    );
  }

  return found[0].candidate;
}

/**
 * Removes scratch directories a previous run did not get to.
 *
 * The cleanup below handles every ordinary exit, but a run that was
 * SIGKILLed — a cancelled CI job, a crash — leaves its directory (and the
 * database encryption key inside it) behind. Only directories old enough
 * that no live run could own them are touched; this run's own directory is
 * emptied outright by `prepareWorkspace`.
 */
function sweepStaleWorkspaces(mine) {
  const cutoff = Date.now() - 6 * 60 * 60 * 1000;

  let entries;
  try {
    entries = fs.readdirSync(os.tmpdir());
  } catch {
    return;
  }

  for (const entry of entries) {
    if (!entry.startsWith("rustak-e2e-")) {
      continue;
    }

    const stale = path.join(os.tmpdir(), entry);
    if (stale === mine) {
      continue;
    }

    try {
      if (fs.statSync(stale).mtimeMs < cutoff) {
        fs.rmSync(stale, { recursive: true, force: true });
      }
    } catch {
      // Another run may own it, or it may have just gone. Either way it is
      // not this run's problem.
    }
  }
}

/** Where this run keeps everything it writes. */
function workspacePath() {
  return (
    process.env.RUSTAK_E2E_WORKSPACE ??
    path.join(os.tmpdir(), `rustak-e2e-${PORT}`)
  );
}

/**
 * An empty directory holding this run's configuration, database, certificate
 * authority, setup token and content store.
 *
 * It is emptied rather than reused, because almost everything this suite
 * asserts about the first run is **one-shot**: the setup wizard closes itself
 * for good, `POST /setup/admin` answers `409` once an administrator exists,
 * and the setup token is deleted when the wizard completes. A run that
 * inherited a previous run's database would be testing a different server
 * from the one the specs describe.
 *
 * `[server].data_dir` is where the database, its encryption key file, the
 * generated CA and the content-addressed store all live, so the whole lot has
 * to sit somewhere disposable or a run would leave a key behind in the
 * repository.
 *
 * The configuration deliberately does not open a TLS or TAK stream listener,
 * and disables the Marti listener entirely: this suite exercises the admin UI
 * and `/api/v1` only, over plain HTTP, the same way automate's e2e config
 * admits every request with `user_acl = 'true'` / `admin_acl = 'true'`.
 * `allow_insecure_http = true` (on `[web.public]`, beside the `[web.public.tls]`
 * table rather than inside it) is required because rustak refuses to serve
 * `[web.public]` over plaintext otherwise — see design 01 §7.1 and
 * `rustak-server/src/config/validate.rs`.
 *
 * `[server] base_url` is the load-bearing one. It is what
 * `identity::settings::base_url` returns — in preference to anything the
 * wizard later stores — and therefore what `auth::passkeys::Passkeys` derives
 * the WebAuthn relying party from. Pointing it at `http://localhost:<port>`
 * is what makes the relying party `localhost`, which is the only thing a
 * browser will run a ceremony for here.
 */
function prepareWorkspace() {
  const directory = workspacePath();

  fs.rmSync(directory, { recursive: true, force: true });
  fs.mkdirSync(directory, { recursive: true });

  const configPath = path.join(directory, "config.toml");
  const setupTokenFile = path.join(directory, "setup-token");

  fs.writeFileSync(
    configPath,
    [
      "# Generated by e2e/scripts/start-server.mjs. Do not edit; it is thrown away.",
      "[server]",
      `name = ${JSON.stringify(SERVER_NAME)}`,
      `data_dir = ${JSON.stringify(directory)}`,
      // Not `domains`: leaving it empty is what lets the setup wizard's
      // "Server name" step be a step, since `[server] domains` in the file
      // wins over whatever the wizard stores.
      `base_url = ${JSON.stringify(`http://${HOST}:${PORT}`)}`,
      "",
      "[web.public]",
      `listen = ["${BIND}:${PORT}"]`,
      "allow_insecure_http = true",
      "",
      "[web.public.tls]",
      'mode = "none"',
      "",
      "[web.marti]",
      "enabled = false",
      "",
      "[stream.tls]",
      "enabled = false",
      "",
      "[auth]",
      "user_acl  = 'true'",
      "admin_acl = 'true'",
      // Named explicitly although it is also the default, because the suite
      // reads this file to bootstrap the first administrator and a default
      // that moved would be a test failure nobody could read.
      `setup_token_file = ${JSON.stringify(setupTokenFile)}`,
      "",
    ].join("\n"),
    "utf8",
  );

  return { directory, configPath, setupTokenFile };
}

const binary = resolveBinary();
const { directory, configPath, setupTokenFile } = prepareWorkspace();
sweepStaleWorkspaces(directory);

// On stderr rather than stdout, because `playwright.config.ts` only pipes the
// server's stdout when it is asked to (the request log is enormous) and these
// four lines are the ones somebody reads when a run will not start.
console.error(`[e2e] server binary: ${binary}`);
console.error(`[e2e] workspace:     ${directory}`);
console.error(`[e2e] setup token:   ${setupTokenFile}`);
console.error(`[e2e] listening on:  http://${HOST}:${PORT} (bound ${BIND}:${PORT})`);

// `--env` is not optional here, and it must point at a path that does not
// exist. rustak_core::config::load_env_file guards against exactly this (it
// only loads a path that `is_file()`), but this repository's root `.env` is a
// *named pipe* — never read it directly, and never run a recursive grep from
// the repository root either — so a path in the scratch directory that cannot
// be a pipe somebody left lying around is belt-and-braces on top of that
// guard, not a substitute for it.
const child = spawn(
  binary,
  ["--config", configPath, "--env", path.join(directory, ".env.absent")],
  {
    // Also run from the scratch directory, so nothing the server resolves
    // relatively can reach the repository's own `.env` or `config.toml`.
    cwd: directory,
    stdio: ["ignore", "inherit", "inherit"],
    env: process.env,
  },
);

/**
 * How long a stop waits for the server to finish stopping.
 *
 * The server drains for `[server] shutdown_timeout` (8 s by default) and is
 * then allowed a further two for the WAL checkpoint that makes its data
 * directory tidy — so anything shorter than ten removes the directory out from
 * under a checkpoint that is still running, and the run ends with the server
 * logging an I/O failure against a path that no longer exists. Two seconds of
 * slack on top of the budget it was given, and still inside the 15 s
 * `gracefulShutdown` Playwright allows this process (`playwright.config.ts`).
 */
const STOP_TIMEOUT_MS = 12_000;

/** Whether the child has already ended, whatever ended it. */
function hasExited() {
  return child.exitCode !== null || child.signalCode !== null;
}

/** Resolves when the child has exited, or after `STOP_TIMEOUT_MS`. */
function awaitExit() {
  if (hasExited()) {
    return Promise.resolve();
  }

  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      console.error(
        `[e2e] the server did not stop within ${STOP_TIMEOUT_MS} ms; killing it`,
      );
      child.kill("SIGKILL");
      resolve();
    }, STOP_TIMEOUT_MS);

    // Nothing else should be held open by this wait: the process is on its
    // way out either way.
    timer.unref?.();

    child.once("exit", () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

let cleanedUp = false;

/**
 * Removes the scratch directory.
 *
 * Only ever called once the child has gone. Removing it while the server is
 * still running deletes the database out from under the checkpoint it was
 * asked to make — see `stop` below, which is the whole reason this is a
 * separate function from it.
 */
function cleanUp() {
  if (cleanedUp) {
    return;
  }
  cleanedUp = true;

  // `RUSTAK_E2E_KEEP` leaves the database, the log and the CA behind for
  // somebody debugging a failure. It is off by default because the directory
  // holds this installation's secret key and its certificate authority's
  // private key, and neither belongs in a temporary directory indefinitely.
  if (process.env.RUSTAK_E2E_KEEP) {
    console.error(`[e2e] keeping ${directory} (RUSTAK_E2E_KEEP is set)`);
    return;
  }

  try {
    fs.rmSync(directory, { recursive: true, force: true });
  } catch (error) {
    console.error(`[e2e] could not remove ${directory}: ${error}`);
  }
}

/**
 * Stops the server and *then* removes what it was writing into.
 *
 * Playwright stops this launcher with SIGTERM and expects the port to be free
 * when it returns. Forwarding the signal and exiting immediately — which is
 * what this used to do — left the server checkpointing into a directory that
 * had already been removed, so every run ended with an error in its log and a
 * database that was never closed cleanly.
 */
async function stop(code) {
  if (!hasExited()) {
    child.kill("SIGTERM");
  }

  await awaitExit();
  cleanUp();
  process.exit(code);
}

/** Whether a signal handler is already stopping the server. */
let stopping = false;

// A last-resort sweep for an exit no handler below covers (an uncaught throw,
// `process.exit` from somewhere else). Synchronous, so it cannot wait for
// anything; `stop` is the path that can.
process.on("exit", cleanUp);

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => {
    // A second SIGTERM — Playwright's, or an impatient developer's — must not
    // start a second stop and race the first one's cleanup.
    if (stopping) {
      return;
    }
    stopping = true;

    void stop(130);
  });
}

child.on("error", (error) => {
  console.error(`[e2e] failed to start the server: ${error.message}`);
  cleanUp();
  process.exit(1);
});

child.on("exit", (code, signal) => {
  // A stop we asked for is waiting on this same event and owns the cleanup and
  // the exit code; anything else — the server falling over on its own — ends
  // the run here.
  if (stopping) {
    return;
  }

  cleanUp();
  process.exit(code ?? (signal ? 143 : 1));
});
