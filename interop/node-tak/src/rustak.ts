/**
 * The throwaway rustak this suite runs against.
 *
 * The launcher itself lives in `interop/shared/src/launch.ts`, which
 * `interop/eud` drives too — a second copy would eventually disagree with this
 * one about what "a rustak with an internal CA in a scratch directory" means,
 * and the first symptom of that is a scenario failing in one suite and passing
 * in the other. What stays here is only what is true of *this* suite:
 *
 * - **`client_passwords_enabled`.** CloudTAK has no other way to authenticate,
 *   so the password grant is the credential the whole suite hangs off. The EUD
 *   harness deliberately does not set it: ATAK enrols with a one-time token.
 * - **The names.** The scratch-directory prefix the sweeper recognises, and the
 *   `[server] name` that turns up on `/Marti/api/version`.
 *
 * The rest — the three listeners, `mode = "internal"`, `localhost` rather than
 * `127.0.0.1` for WebAuthn's sake, the stale-workspace sweep — is shared, and
 * the reasoning behind each of those choices is documented there.
 */

import {
  startServer as launch,
  type LaunchOptions,
  type RunningServer,
  type ServerInfo,
} from "../../shared/src/launch.js";

export { REPO_ROOT, resolveBinary, reservePort, waitForServer } from "../../shared/src/launch.js";
export type { RunningServer, ServerInfo } from "../../shared/src/launch.js";

/** The prefix every scratch directory this suite creates carries. */
const SCRATCH_PREFIX = "rustak-interop-node-tak-";

/** What this suite asks the shared launcher for. */
const OPTIONS: LaunchOptions = {
  prefix: SCRATCH_PREFIX,
  name: "rustak-interop-node-tak",
  host: "localhost",
  config: {
    auth: {
      // CloudTAK has no other way to authenticate, so this is the credential
      // the whole suite hangs off.
      client_passwords_enabled: true,
    },
  },
};

/** Starts a server in a fresh scratch directory. */
export function startServer(): Promise<RunningServer> {
  return launch(OPTIONS);
}

/** Re-exported so nothing downstream has to know where the types moved to. */
export type NodeTakServer = ServerInfo;
