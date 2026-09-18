/**
 * Walks a fresh rustak all the way to a credential a client could use.
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
 *
 * From there a suite mints whatever it needs with [`createUser`],
 * [`setGroups`] and [`mintCredential`]: a client password for CloudTAK's
 * password grant, a one-time enrolment token for an EUD's `estream:`.
 *
 * The passkey step is why `webauthn.ts` exists. There is no non-browser path to
 * an administrator bearer token today, and no suite may add one — see
 * `interop/node-tak/README.md` → "The bootstrap gap".
 */

import fs from "node:fs";
import path from "node:path";

import { api, type HttpClient } from "./http.js";
import type { ServerInfo } from "./launch.js";
import { SoftAuthenticator, type CredentialOptions } from "./webauthn.js";

/** The administrator a bootstrap produced, and its bearer token. */
export interface Admin {
  readonly username: string;
  readonly token: string;
}

/** What a suite calls the installation it just created. */
export interface BootstrapOptions {
  /** The account the wizard creates. */
  readonly adminUsername: string;

  /** What to show beside it, and to label its passkey with. */
  readonly displayName: string;

  /** `[server] name`, recorded through the wizard. */
  readonly serverName: string;

  /** The domains the wizard records. The first is canonical. */
  readonly domains: readonly string[];

  /** The base URL the wizard records, which must be one of `domains`. */
  readonly baseUrl: string;
}

/** Waits until `/api/v1/health` answers, which is the first thing that can. */
export async function waitForHealth(client: HttpClient, timeoutMs = 60_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let last = "";

  while (Date.now() < deadline) {
    try {
      await api(client, "/health");
      return;
    } catch (error) {
      last = error instanceof Error ? error.message : String(error);
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
  }

  throw new Error(`${client.base}/api/v1/health never answered. Last failure: ${last}`);
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
async function signIn(
  server: ServerInfo,
  client: HttpClient,
  options: BootstrapOptions,
): Promise<Admin> {
  const status = await api<{ needs_setup: boolean; has_admin: boolean }>(client, "/setup/status");

  if (!status.needs_setup || status.has_admin) {
    throw new Error("the scratch server already has an administrator, which cannot happen.");
  }

  const created = await api<{ username: string; registration_token: string }>(
    client,
    "/setup/admin",
    {
      body: {
        setup_token: await readSetupToken(server.dataDir),
        username: options.adminUsername,
        display_name: options.displayName,
      },
    },
  );

  const authenticator = new SoftAuthenticator(server.webtak);

  const challenge = await api<{ challenge_id: string; options: CredentialOptions }>(
    client,
    "/auth/passkey/register/start",
    { body: { label: options.displayName, registration_token: created.registration_token } },
  );

  const session = await api<{ token: string }>(client, "/auth/passkey/register/finish", {
    body: {
      challenge_id: challenge.challenge_id,
      credential: authenticator.create(challenge.options),
    },
  });

  return { username: created.username, token: session.token };
}

/** Records the host name and closes the wizard, as an operator would. */
async function finishWizard(
  client: HttpClient,
  token: string,
  options: BootstrapOptions,
): Promise<void> {
  await api(client, "/setup/server", {
    body: { name: options.serverName, domains: options.domains, base_url: options.baseUrl },
    token,
  });

  await api(client, "/setup/complete", { method: "POST", token });
}

/** Performs the whole walk against a server that has just started. */
export async function bootstrap(
  server: ServerInfo,
  client: HttpClient,
  options: BootstrapOptions,
): Promise<Admin> {
  await waitForHealth(client);

  const admin = await signIn(server, client, options);

  await finishWizard(client, admin.token, options);

  const me = await api<{ username: string; is_admin: boolean }>(client, "/me", {
    token: admin.token,
  });

  if (me.username !== admin.username || !me.is_admin) {
    throw new Error(`the bootstrap signed in as ${me.username}, which is not an administrator.`);
  }

  return admin;
}

/** Creates an ordinary account, and returns the username the server settled on. */
export async function createUser(
  client: HttpClient,
  token: string,
  username: string,
  displayName: string,
): Promise<string> {
  const created = await api<{ username: string }>(client, "/users", {
    body: { username, display_name: displayName },
    token,
  });

  return created.username;
}

/** What minting handed back: the only moment the secret exists outside the server. */
export interface MintedCredential {
  readonly id: number;
  readonly secret: string;

  /** The `tak://` URL an ATAK user scans, for an enrolment token. */
  readonly enrollUrl?: string;
}

/** Mints a credential for `username`, as an administrator may on anyone's behalf. */
export async function mintCredential(
  client: HttpClient,
  token: string,
  request: {
    kind: "enrollment_token" | "client_password";
    label: string;
    username: string;
    expiresInDays?: number;
    maxUses?: number;
  },
): Promise<MintedCredential> {
  const minted = await api<{
    credential: { id: number };
    secret: string;
    enroll_url?: string;
  }>(client, "/credentials", {
    body: {
      kind: request.kind,
      label: request.label,
      username: request.username,
      ...(request.expiresInDays === undefined ? {} : { expires_in_days: request.expiresInDays }),
      ...(request.maxUses === undefined ? {} : { max_uses: request.maxUses }),
    },
    token,
  });

  return { id: minted.credential.id, secret: minted.secret, enrollUrl: minted.enroll_url };
}

/** One channel grant, in the shape `PUT /api/v1/users/{username}/groups` takes. */
export interface Grant {
  readonly group: string;

  /** TAK's own vocabulary: `IN` is permission to write, `OUT` to read. */
  readonly direction: "IN" | "OUT" | "BOTH";
}

/** Creates a channel, tolerating one that is already there. */
export async function createGroup(
  client: HttpClient,
  token: string,
  name: string,
): Promise<void> {
  const response = await client.request("/api/v1/groups", {
    body: { name },
    token,
  });

  // 409 is "somebody already made it", which is the state this asked for.
  if (response.status === 409 || (response.status >= 200 && response.status < 300)) return;

  throw new Error(`POST /api/v1/groups answered ${response.status}: ${response.body}`);
}

/** Replaces the channels an account is granted by hand. */
export async function setGroups(
  client: HttpClient,
  token: string,
  username: string,
  grants: readonly Grant[],
): Promise<void> {
  await api(client, `/users/${encodeURIComponent(username)}/groups`, {
    method: "PUT",
    body: grants.map((grant) => ({ group: grant.group, direction: grant.direction })),
    token,
  });
}
