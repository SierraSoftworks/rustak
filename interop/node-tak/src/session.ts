/**
 * What the runner hands the scenarios.
 *
 * `src/run.ts` starts one rustak, bootstraps it through `/api/v1` and probes
 * which compatibility surfaces it serves; the result is written to a file in
 * the scratch directory and its path is passed down in
 * `RUSTAK_INTEROP_SESSION`. Every scenario file reads it at import time, so the
 * whole suite shares one server and one bootstrap.
 *
 * # Why a file rather than environment variables
 *
 * It carries a bearer token and a client password. A file in the scratch
 * directory is created 0600 and removed with the directory; an environment
 * variable is inherited by every descendant of the runner and readable from
 * `/proc/<pid>/environ` for the lifetime of each of them. Neither is a secret
 * that outlives the run — the server is thrown away — but the file is the
 * cheaper habit.
 */

import fs from "node:fs";

import type { SurfaceName } from "./surfaces.js";
import { SURFACES } from "./surfaces.js";

/** The three base URLs CloudTAK stores for one server. */
export interface ServerUrls {
  /** CloudTAK's `webtak`: OAuth, enrollment, and Marti for the browser half. */
  readonly webtak: string;

  /** CloudTAK's `api`: the mutually authenticated Marti listener. */
  readonly api: string;

  /** CloudTAK's `url`: the CoT stream. */
  readonly stream: string;
}

/** An account and the secret it authenticates with. */
export interface Credentials {
  readonly username: string;
  readonly password: string;
}

/** Everything a scenario needs to talk to the server the runner started. */
export interface Session {
  readonly urls: ServerUrls;

  /** The scratch directory the server writes into. Removed on exit. */
  readonly dataDir: string;

  /** `<data_dir>/pki/ca.crt`, which `NODE_EXTRA_CA_CERTS` already points at. */
  readonly caFile: string;

  /** The administrator the setup wizard created, and its bearer token. */
  readonly admin: { readonly username: string; readonly token: string };

  /**
   * The client password CloudTAK would authenticate with.
   *
   * Minted through `POST /api/v1/credentials` as
   * [`CredentialKind::ClientPassword`], which rustak accepts only on
   * `/oauth/token` and the Marti enrollment endpoints — see
   * `compat/oauth.md` §1.
   */
  readonly client: Credentials;

  /** Which surfaces answered the runner's probe. */
  readonly surfaces: Readonly<Record<SurfaceName, boolean>>;
}

/** The environment variable naming the session file. */
export const SESSION_ENV = "RUSTAK_INTEROP_SESSION";

/** Writes the session for the scenario processes to read. */
export function writeSession(path: string, session: Session): void {
  fs.writeFileSync(path, JSON.stringify(session, null, 2), { encoding: "utf8", mode: 0o600 });
}

/**
 * Reads the session the runner wrote.
 *
 * @throws when the suite was started without the runner, which is the only way
 * this can be missing and is always a mistake rather than a skip.
 */
export function loadSession(): Session {
  const path = process.env[SESSION_ENV];

  if (!path) {
    throw new Error(
      [
        `${SESSION_ENV} is not set, so there is no server to test against.`,
        "",
        "Scenarios are not run directly: `npm test` starts a throwaway rustak,",
        "bootstraps it and then runs them with the certificate authority it",
        "generated already trusted. Run `npm test` from interop/node-tak.",
      ].join("\n"),
    );
  }

  return JSON.parse(fs.readFileSync(path, "utf8")) as Session;
}

/**
 * The `skip` value for a scenario that needs `surface`.
 *
 * `false` when the server serves it — `node:test` runs the scenario — and the
 * surface's TODO otherwise, which is what the reporter prints beside the
 * skipped name.
 */
export function unless(session: Session, surface: SurfaceName): false | string {
  return session.surfaces[surface] ? false : SURFACES[surface].todo;
}

/**
 * The `skip` value for a scenario that needs all of `surfaces`.
 *
 * The first missing one is reported, because that is the one to go and read
 * about; a scenario needing two things that are both missing is not twice as
 * skipped.
 */
export function unlessAll(session: Session, ...surfaces: SurfaceName[]): false | string {
  for (const surface of surfaces) {
    const skip = unless(session, surface);

    if (skip !== false) {
      return skip;
    }
  }

  return false;
}
