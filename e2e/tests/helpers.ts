/**
 * Shared plumbing for the end-to-end suite.
 *
 * Knowing when the wasm application is actually ready to be driven; creating
 * the fixtures a test needs without going through the UI to do it; and naming
 * those fixtures so that two tests can never see each other's.
 */

import {
  expect,
  test as base,
  type Page,
  type APIRequestContext,
} from "@playwright/test";

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
 * Bootstraps the first admin account through `/api/v1/setup/admin`.
 *
 * The setup wizard's admin step is single-shot (design 01 §6.3): the server
 * accepts exactly one `POST /api/v1/setup/admin` and answers `409` ever
 * after. Every test run against a fresh scratch database is "the first admin
 * this server has ever had", so this call always attempts creation and
 * tolerates the 409 a later spec in the same run will hit, falling back to
 * `/api/v1/auth/local` to obtain a session for that already-created admin.
 *
 * NOTE: this helper is ahead of what M0 has built so far — `/api/v1/setup/*`
 * and `/api/v1/auth/local` are specified in design 01 §6.3 but not
 * necessarily implemented yet. It is included here, unused by `smoke.spec.ts`,
 * so the setup/auth specs a later brief adds do not have to re-derive this
 * sequence.
 */
export async function bootstrapAdmin(
  request: APIRequestContext,
  credentials: { username: string; password: string; displayName?: string },
): Promise<{ token: string; refreshToken: string }> {
  const created = await request.post("/api/v1/setup/admin", {
    data: {
      username: credentials.username,
      password: credentials.password,
      display_name: credentials.displayName ?? credentials.username,
    },
  });

  if (created.status() === 409) {
    const signedIn = await request.post("/api/v1/auth/local", {
      data: {
        username: credentials.username,
        password: credentials.password,
      },
    });
    expect(signedIn.status(), `local sign-in should have succeeded: ${await signedIn.text()}`).toBe(200);
    const body = (await signedIn.json()) as { token: string; refresh_token: string };
    return { token: body.token, refreshToken: body.refresh_token };
  }

  expect(created.status(), `admin bootstrap should have succeeded: ${await created.text()}`).toBe(200);
  const body = (await created.json()) as { token: string; refresh_token: string };
  return { token: body.token, refreshToken: body.refresh_token };
}

/**
 * Signs a page in without driving the login form.
 *
 * Writes the bearer token straight into `sessionStorage` under the key the UI
 * itself uses (`rustak.admin.token`, mirroring automate's
 * `automate.admin.token`), via an init script so it is present before the
 * application's own scripts run on the next navigation.
 */
export async function signIn(page: Page, token: string): Promise<void> {
  await page.addInitScript((value) => {
    window.sessionStorage.setItem("rustak.admin.token", value);
  }, token);
}
