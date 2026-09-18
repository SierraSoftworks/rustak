/**
 * Walks a fresh rustak all the way to a credential CloudTAK could use.
 *
 * rustak has no local passwords and no "create an admin" flag: an installation
 * with nobody in it writes a one-time setup token to a file (0600) and the
 * first administrator is created with it, then immediately registers a passkey
 * — which is what actually hands back a bearer token
 * (`.claude/plan/status/M0-11-web-api-auth.md`). So this is the whole ceremony,
 * in the order an operator performs it:
 *
 *     GET  /api/v1/health                        wait for the listener
 *     GET  /api/v1/setup/status                  confirm nobody is here yet
 *     read <data_dir>/setup-token                the file the server wrote
 *     POST /api/v1/setup/admin                   → a registration token
 *     POST /api/v1/auth/passkey/register/start   → WebAuthn options
 *     POST /api/v1/auth/passkey/register/finish  → the administrator's session
 *     POST /api/v1/setup/server                  record the host name
 *     POST /api/v1/setup/complete                close the wizard for good
 *     POST /api/v1/users                         an ordinary account for the EUD
 *     POST /api/v1/credentials?username=…        → its client password
 *
 * The passkey step is why `webauthn.ts` exists. There is no non-browser path to
 * an administrator bearer token today, and this suite may not add one — see
 * `interop/node-tak/README.md` → "The bootstrap gap".
 *
 * Every call here goes through the global `fetch`, so the chain is verified
 * against `NODE_EXTRA_CA_CERTS` exactly as node-tak's own `webtak` calls are.
 */

import fs from "node:fs";
import path from "node:path";

import type { ServerInfo } from "./rustak.js";
import type { Credentials } from "./session.js";
import { SoftAuthenticator, type CredentialOptions } from "./webauthn.js";

/** The account the wizard creates. */
const ADMIN_USERNAME = "interop-admin";

/**
 * The ordinary account the scenarios enrol and authenticate as.
 *
 * Deliberately not the administrator: a suite that only ever exercised an
 * administrator would miss an authorisation bug that bites everybody else.
 */
const CLIENT_USERNAME = "interop-eud";

/** What the bootstrap produced. */
export interface Bootstrapped {
  readonly admin: { readonly username: string; readonly token: string };
  readonly client: Credentials;
}

/** One `/api/v1` call, with the failure a reader can act on. */
async function call<T>(
  base: string,
  route: string,
  init: { method?: string; body?: unknown; token?: string } = {},
): Promise<T> {
  const headers: Record<string, string> = {};

  if (init.body !== undefined) headers["Content-Type"] = "application/json";
  if (init.token) headers.Authorization = `Bearer ${init.token}`;

  const response = await fetch(new URL(`/api/v1${route}`, base), {
    method: init.method ?? (init.body === undefined ? "GET" : "POST"),
    headers,
    body: init.body === undefined ? undefined : JSON.stringify(init.body),
  });

  if (!response.ok) {
    throw new Error(
      `${init.method ?? "GET"} /api/v1${route} answered ${response.status}: ${await response.text()}`,
    );
  }

  if (response.status === 204) return undefined as T;

  return (await response.json()) as T;
}

/** Waits until `/api/v1/health` answers, which is the first thing that can. */
async function waitForHealth(base: string, timeoutMs = 60_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let last = "";

  while (Date.now() < deadline) {
    try {
      await call(base, "/health");
      return;
    } catch (error) {
      last = error instanceof Error ? error.message : String(error);
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
  }

  throw new Error(`${base}/api/v1/health never answered. Last failure: ${last}`);
}

/**
 * Reads the one-time setup token the server wrote at first start.
 *
 * It is minted during start-up and the path is logged, so by the time the
 * health check answers it is there; the short retry is for the gap between the
 * listener binding and the file being flushed.
 */
async function readSetupToken(dataDir: string, timeoutMs = 10_000): Promise<string> {
  const file = path.join(dataDir, "setup-token");
  const deadline = Date.now() + timeoutMs;

  while (Date.now() < deadline) {
    try {
      const token = fs.readFileSync(file, "utf8").trim();

      if (token.length > 0) return token;
    } catch {
      // Not written yet.
    }

    await new Promise((resolve) => setTimeout(resolve, 100));
  }

  throw new Error(
    `${file} was never written, so there is no way to create the first administrator.`,
  );
}

/** Creates the administrator and signs it in with a freshly registered passkey. */
async function signIn(server: ServerInfo): Promise<{ username: string; token: string }> {
  const status = await call<{ needs_setup: boolean; has_admin: boolean }>(
    server.webtak,
    "/setup/status",
  );

  if (!status.needs_setup || status.has_admin) {
    throw new Error("the scratch server already has an administrator, which cannot happen.");
  }

  const created = await call<{ username: string; registration_token: string }>(
    server.webtak,
    "/setup/admin",
    {
      body: {
        setup_token: await readSetupToken(server.dataDir),
        username: ADMIN_USERNAME,
        display_name: "node-tak interop suite",
      },
    },
  );

  const authenticator = new SoftAuthenticator(server.webtak);

  const challenge = await call<{ challenge_id: string; options: CredentialOptions }>(
    server.webtak,
    "/auth/passkey/register/start",
    {
      body: { label: "interop suite", registration_token: created.registration_token },
    },
  );

  const session = await call<{ token: string }>(server.webtak, "/auth/passkey/register/finish", {
    body: {
      challenge_id: challenge.challenge_id,
      credential: authenticator.create(challenge.options),
    },
  });

  return { username: created.username, token: session.token };
}

/** Records the host name and closes the wizard, as an operator would. */
async function finishWizard(server: ServerInfo, token: string): Promise<void> {
  await call(server.webtak, "/setup/server", {
    body: {
      name: "rustak-interop-node-tak",
      domains: ["localhost"],
      base_url: server.webtak,
    },
    token,
  });

  await call(server.webtak, "/setup/complete", { method: "POST", token });
}

/** Creates the ordinary account and mints the reusable credential CloudTAK authenticates with. */
async function mintClientPassword(server: ServerInfo, token: string): Promise<Credentials> {
  const created = await call<{ username: string }>(server.webtak, "/users", {
    body: { username: CLIENT_USERNAME, display_name: "node-tak interop EUD" },
    token,
  });

  const minted = await call<{ secret: string }>(server.webtak, "/credentials", {
    body: {
      kind: "client_password",
      label: "node-tak interop suite",
      expires_in_days: 1,
      username: created.username,
    },
    token,
  });

  return { username: created.username, password: minted.secret };
}

/** Performs the whole walk against a server that has just started. */
export async function bootstrap(server: ServerInfo): Promise<Bootstrapped> {
  await waitForHealth(server.webtak);

  const admin = await signIn(server);

  await finishWizard(server, admin.token);

  const me = await call<{ username: string; is_admin: boolean }>(server.webtak, "/me", {
    token: admin.token,
  });

  if (me.username !== admin.username || !me.is_admin) {
    throw new Error(`the bootstrap signed in as ${me.username}, which is not an administrator.`);
  }

  return { admin, client: await mintClientPassword(server, admin.token) };
}
