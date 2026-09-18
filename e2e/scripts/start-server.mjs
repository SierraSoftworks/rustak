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
// beside it, the content store, the configuration itself — goes into a fresh
// temporary directory that is removed when the process exits, so a run never
// sees another run's records and never touches a developer's own
// `config.toml` or `data/` directory.

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..", "..");

const PORT = Number(process.env.RUSTAK_E2E_PORT ?? 18446);

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
 * that no live run could own them are touched.
 */
function sweepStaleWorkspaces() {
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

/**
 * A fresh directory holding this run's configuration, database and content
 * store.
 *
 * `[server].data_dir` is where the database, its encryption key file and the
 * content-addressed store all live, so the whole lot has to sit somewhere
 * disposable or a run would leave a key behind in the repository.
 *
 * The config deliberately does not open a TLS or plaintext TAK stream
 * listener, and disables the Marti listener entirely: this suite exercises
 * the admin UI and `/api/v1` only, over plain HTTP, the same way automate's
 * e2e config admits every request with `user_acl = 'true'` /
 * `admin_acl = 'true'`. `allow_insecure_http = true` is required because
 * rustak refuses to serve `[web.public]` over plaintext otherwise — see
 * design 01 §7.1 / plan.md's config-naming delta.
 */
function prepareWorkspace() {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "rustak-e2e-"));
  const configPath = path.join(directory, "config.toml");

  fs.writeFileSync(
    configPath,
    [
      "# Generated by e2e/scripts/start-server.mjs. Do not edit; it is thrown away.",
      "[server]",
      'name = "rustak-e2e"',
      `data_dir = ${JSON.stringify(directory)}`,
      "",
      "[web.public]",
      `listen = ["127.0.0.1:${PORT}"]`,
      "",
      "[web.public.tls]",
      'mode = "none"',
      "allow_insecure_http = true",
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
      "",
    ].join("\n"),
    "utf8",
  );

  return { directory, configPath };
}

const binary = resolveBinary();
sweepStaleWorkspaces();
const { directory, configPath } = prepareWorkspace();

console.log(`[e2e] server binary: ${binary}`);
console.log(`[e2e] workspace:     ${directory}`);
console.log(`[e2e] listening on:  http://127.0.0.1:${PORT}`);

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

let cleanedUp = false;

function cleanUp() {
  if (cleanedUp) {
    return;
  }
  cleanedUp = true;

  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
  }

  try {
    fs.rmSync(directory, { recursive: true, force: true });
  } catch (error) {
    console.error(`[e2e] could not remove ${directory}: ${error}`);
  }
}

process.on("exit", cleanUp);
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => {
    cleanUp();
    process.exit(130);
  });
}

child.on("error", (error) => {
  console.error(`[e2e] failed to start the server: ${error.message}`);
  cleanUp();
  process.exit(1);
});

child.on("exit", (code, signal) => {
  cleanUp();
  process.exit(code ?? (signal ? 143 : 1));
});
