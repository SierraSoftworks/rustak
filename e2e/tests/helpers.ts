/**
 * Shared plumbing for the end-to-end suite.
 *
 * Knowing when the wasm application is actually ready to be driven; getting a
 * session on a server that has no passwords to sign in with; and naming
 * fixtures so that two tests can never see each other's.
 */

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { expect, test as base, type Page } from "@playwright/test";

import { attachAuthenticator, registerPasskey } from "./webauthn";

export { attachAuthenticator, registerPasskey };
export type { VirtualAuthenticator, VirtualCredential } from "./webauthn";

/**
 * The base test, with the readiness listener installed on every page.
 *
 * `TrunkApplicationStarted` is dispatched once, the moment wasm boots, which
 * is usually before a test has had any chance to subscribe. An init script
 * runs before any of the page's own scripts on every navigation, so the flag
 * it sets is already true by the time anybody asks — turning a race into a
 * poll.
 */
export const test = base.extend({
  page: async ({ page }, use) => {
    await page.addInitScript(() => {
      (window as unknown as Record<string, unknown>).__rustakStarted = false;
      window.addEventListener("TrunkApplicationStarted", () => {
        (window as unknown as Record<string, unknown>).__rustakStarted = true;
      });
    });

    await use(page);
  },
});

export { expect };

/** The administrator this suite creates, and the passkey it signs in with. */
export const ADMIN = {
  username: "avery",
  displayName: "Avery Quinn",
  passkeyLabel: "Virtual authenticator",
} as const;

/** A session established by a passkey, as the UI itself would hold it. */
export interface Session {
  username: string;
  token: string;
  refreshToken?: string;
}

/** sessionStorage keys the admin UI keeps its session under (`rustak-ui/src/auth/mod.rs`). */
const TOKEN_KEY = "rustak.admin.token";
const REFRESH_KEY = "rustak.admin.refresh";

/**
 * Waits until the wasm application has booted and rendered.
 *
 * Deliberately not `networkidle`. The bundle streams several megabytes and
 * the application keeps talking to `/api/v1` after it has painted, so "the
 * network went quiet" is both later than readiness and, when a poll is in
 * flight, a moment that may never arrive. The application says when it has
 * started; this listens for it.
 */
export async function waitForApp(page: Page): Promise<void> {
  await page.waitForFunction(
    () => (window as unknown as Record<string, unknown>).__rustakStarted === true,
    undefined,
    { timeout: 45_000 },
  );
}

/** Navigates to a path within the application and waits for it to boot. */
export async function gotoApp(page: Page, path: string): Promise<void> {
  await page.goto(path);
  await waitForApp(page);
}

/**
 * A name no other test could produce.
 *
 * Every test shares one database, so a record left behind by a failed run
 * must not be able to satisfy — or break — a later one. A test that asserts
 * "the device I just enrolled is in the list" is only telling the truth if
 * the name it looks for could not have come from anywhere else.
 */
export function uniqueName(prefix: string): string {
  return `${prefix} ${Math.random().toString(36).slice(2, 8)}${Date.now().toString(36).slice(-4)}`;
}

/**
 * The scratch directory the server under test is writing into.
 *
 * `playwright.config.ts` computes it and puts it in the environment, and it is
 * loaded in every worker, so this is reading what the launcher was told rather
 * than guessing at it.
 */
export function serverWorkspace(): string {
  return (
    process.env.RUSTAK_E2E_WORKSPACE ??
    path.join(
      os.tmpdir(),
      `rustak-e2e-${process.env.RUSTAK_E2E_PORT ?? 18446}`,
    )
  );
}

/**
 * The one-time setup token, read from the file the server wrote it to.
 *
 * This is the whole of the first-run trust model: a fresh installation has no
 * administrator and no passwords, so the only thing that can authorise
 * creating the first account is proof that you can read the server's own
 * filesystem. The suite reads it exactly the way an operator would — out of
 * `[auth] setup_token_file`, which `scripts/start-server.mjs` points into the
 * scratch directory.
 */
export function readSetupToken(): string {
  const file = path.join(serverWorkspace(), "setup-token");

  if (!fs.existsSync(file)) {
    throw new Error(
      [
        `No setup token at ${file}.`,
        "",
        "The server writes one at first start when there is no administrator",
        "yet, and deletes it when the wizard completes. If it is missing, this",
        "server has already been set up — which means the scratch directory",
        "was reused rather than emptied.",
      ].join("\n"),
    );
  }

  return fs.readFileSync(file, "utf8").trim();
}

/**
 * Where a session is left for later specs.
 *
 * Inside the server's own scratch directory on purpose: the session belongs to
 * that installation's database, and both are deleted together. A cache that
 * outlived the server it was minted by would be a token for an account that no
 * longer exists, and the failure it produced would point at the wrong thing.
 */
function sessionCachePath(): string {
  return path.join(serverWorkspace(), "e2e-session.json");
}

/** Reads a session an earlier spec established, if there is one. */
export function readCachedSession(): Session | undefined {
  const file = sessionCachePath();
  if (!fs.existsSync(file)) {
    return undefined;
  }

  return JSON.parse(fs.readFileSync(file, "utf8")) as Session;
}

/** Leaves a session for the specs that run after this one. */
export function cacheSession(session: Session): void {
  fs.writeFileSync(sessionCachePath(), JSON.stringify(session), "utf8");
}

/**
 * Gets an administrator session, creating the administrator if there is none.
 *
 * The setup wizard is a one-way door — `POST /api/v1/setup/admin` answers
 * `409` once an administrator exists and every `/setup/*` route answers `410`
 * once it has been completed — so this runs at most once per server. When
 * `setup.spec.ts` has already walked the wizard through the UI, its session is
 * reused; when a spec is run on its own against a fresh server, the same
 * sequence is driven through the API instead.
 *
 * The passkey step is the only one that cannot be: `navigator.credentials`
 * exists in a page and nowhere else, so it runs in `page`, against `page`'s
 * origin, with a virtual authenticator attached (see `./webauthn`).
 */
export async function bootstrapAdmin(page: Page): Promise<Session> {
  const cached = readCachedSession();
  if (cached) {
    return cached;
  }

  const status = await page.request.get("/api/v1/setup/status");
  expect(
    status.status(),
    `GET /api/v1/setup/status should have answered: ${await status.text()}`,
  ).toBe(200);
  const setup = (await status.json()) as {
    has_admin: boolean;
    setup_completed: boolean;
  };

  if (setup.has_admin || setup.setup_completed) {
    throw new Error(
      [
        "This server already has an administrator, but this run has no session",
        "for them — and there is no way to make one, because rustak has no",
        "passwords and the passkey that account holds lives in a browser",
        "profile that has gone.",
        "",
        "That means the scratch database survived a previous run. Stop any",
        "leftover `rustak` on this port and run again.",
      ].join("\n"),
    );
  }

  // The ceremony needs an origin, and the origin has to be the one the
  // relying party was derived from.
  await page.goto("/");

  const created = await page.request.post("/api/v1/setup/admin", {
    data: {
      setup_token: readSetupToken(),
      username: ADMIN.username,
      display_name: ADMIN.displayName,
    },
  });
  expect(
    created.status(),
    `POST /api/v1/setup/admin should have succeeded: ${await created.text()}`,
  ).toBe(200);
  const admin = (await created.json()) as { registration_token: string };

  await attachAuthenticator(page);
  const registered = await registerPasskey(page, {
    label: ADMIN.passkeyLabel,
    registrationToken: admin.registration_token,
  });
  expect(
    registered.status,
    `the passkey registration should have succeeded: ${JSON.stringify(registered.body)}`,
  ).toBe(200);

  // Registering the wizard's first passkey answers with a session, rather than
  // asking the same person to prove the same thing a second time.
  const tokens = registered.body as { token: string; refresh_token?: string };
  const session: Session = {
    username: ADMIN.username,
    token: tokens.token,
    refreshToken: tokens.refresh_token,
  };
  expect(session.token, "the registration should have issued a session").toBeTruthy();

  const authorised = { Authorization: `Bearer ${session.token}` };
  const host = new URL(page.url()).hostname;

  const named = await page.request.post("/api/v1/setup/server", {
    headers: authorised,
    data: { name: "rustak e2e", domains: [host] },
  });
  expect(
    named.status(),
    `POST /api/v1/setup/server should have succeeded: ${await named.text()}`,
  ).toBe(200);

  // Tolerated, not asserted: rustak creates its root authority during start-up
  // (`runtime::listen`), so by the time the wizard asks for one there already
  // is one and this answers `409`. Recorded in this brief's status file.
  await page.request.post("/api/v1/setup/ca", {
    headers: authorised,
    data: { common_name: "rustak e2e CA", key_type: "ecdsa_p256" },
  });

  const completed = await page.request.post("/api/v1/setup/complete", {
    headers: authorised,
    data: {},
  });
  expect(
    completed.status(),
    `POST /api/v1/setup/complete should have succeeded: ${await completed.text()}`,
  ).toBe(204);

  cacheSession(session);
  return session;
}

/**
 * Signs a page in without driving the sign-in prompt.
 *
 * Writes the bearer token straight into `sessionStorage` under the keys the UI
 * itself uses, via an init script so they are present before the application's
 * own scripts run on the next navigation. It is what a page that had just
 * completed a ceremony would be holding — the ceremony itself is
 * `auth.spec.ts`'s subject, and every other spec would only be repeating it.
 */
export async function signIn(page: Page, session: Session): Promise<void> {
  await page.addInitScript(
    ([tokenKey, refreshKey, token, refresh]) => {
      window.sessionStorage.setItem(tokenKey, token);
      if (refresh) {
        window.sessionStorage.setItem(refreshKey, refresh);
      }
    },
    [TOKEN_KEY, REFRESH_KEY, session.token, session.refreshToken ?? ""] as const,
  );
}

/** The session the page is holding, as the application stores it. */
export async function storedSession(page: Page): Promise<Session | undefined> {
  const token = await page.evaluate(
    (key) => window.sessionStorage.getItem(key),
    TOKEN_KEY,
  );
  if (!token) {
    return undefined;
  }

  const refresh = await page.evaluate(
    (key) => window.sessionStorage.getItem(key),
    REFRESH_KEY,
  );

  return {
    username: ADMIN.username,
    token,
    refreshToken: refresh ?? undefined,
  };
}
