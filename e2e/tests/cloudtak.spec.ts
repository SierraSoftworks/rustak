/**
 * Onboarding CloudTAK: one action, and the one-shot keystore it produces.
 *
 * This is the single place in rustak where the server generates a client's
 * private key, so the console is the last line of the argument that makes it
 * defensible — it has to say so before anything is generated, and the download
 * has to behave the way the server promises.
 *
 * # Why the flow runs against `?demo`
 *
 * Issuing a certificate needs a certificate authority, and this suite's server
 * runs with `[web.marti] enabled = false` and `[stream.tls] enabled = false`
 * (`scripts/start-server.mjs`) — so `runtime::listen` installs no authority and
 * the endpoint correctly answers `503`. That refusal is worth asserting on its
 * own, and it is the first test below; the rest of the flow is exercised
 * against the demo fixtures, which is what CI's debug UI bundle exists for.
 * The endpoint itself is covered end to end by
 * `rustak-server/tests/cloudtak_onboarding.rs` and, against a real authority
 * and CloudTAK's own PKCS#12 parser, by
 * `interop/node-tak/tests/cloudtak-onboarding.test.ts`.
 *
 * What only a browser can show, and what these therefore assert:
 *
 * - **The bytes really arrive.** The keystore is behind the bearer token the
 *   app holds in `sessionStorage`, so it is fetched and saved rather than
 *   navigated to; a `download` event is the proof the whole path works.
 * - **The second click is refused, visibly.** The server deletes its copy as it
 *   answers the first one, and a page that swallowed the `410` would leave an
 *   operator clicking a button that silently does nothing.
 */

import type { Page } from "@playwright/test";

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

/**
 * A username no other test could produce.
 *
 * `uniqueName` puts a space in, which `Username::parse` refuses, so the suffix
 * is built here — the same way `identity.spec.ts` does it.
 */
function uniqueUsername(prefix: string): string {
  const suffix = `${Math.random().toString(36).slice(2, 8)}${Date.now().toString(36).slice(-4)}`;
  return `${prefix}-${suffix}`.toLowerCase();
}

/**
 * The demo account the fixture flow runs against.
 *
 * A *created* one would not do: demo state lives in a thread-local inside the
 * wasm module, and the shortcut below navigates the whole page, which reloads
 * the module and forgets it. That is a property of the fixtures rather than of
 * the feature, so the fixture tests open a seeded account directly and the
 * shortcut is proved against the real API instead.
 */
const DEMO_ACCOUNT = "service-weather";

/** Creates a CloudTAK account from the Users page and lands on its tab. */
async function createCloudTakAccount(page: Page, username: string): Promise<void> {
  await gotoApp(page, "/admin/users");

  await page.getByLabel("Username").fill(username);
  await page.getByRole("button", { name: "Create CloudTAK account" }).click();

  // It creates the service account and lands on its CloudTAK tab, so the
  // operator does not have to know those are two separate things.
  await expect(page).toHaveURL(new RegExp(`/admin/users/${username}`));
  await expect(page.getByRole("tab", { name: "CloudTAK" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
}

/** Opens a seeded demo account straight on its CloudTAK tab. */
async function openDemoPanel(page: Page): Promise<void> {
  await gotoApp(page, `/admin/users/${DEMO_ACCOUNT}?demo#cloudtak`);

  // The fragment is read once, on mount — which is the whole reason the Users
  // page can send somebody here.
  await expect(page.getByRole("tab", { name: "CloudTAK" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
}

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("the shortcut makes a service account and states the exception before generating anything", async ({
  page,
}) => {
  const username = uniqueUsername("cloudtak");

  await createCloudTakAccount(page, username);

  await expect(page.getByText(`${username} · Service`)).toBeVisible();
  await expect(
    page.getByText("This is the one place rustak generates a client's private key"),
  ).toBeVisible();

  // This server has no certificate authority, so the hand-over cannot happen —
  // and an operator must be told that rather than watching a button do nothing.
  await page.getByRole("button", { name: "Onboard CloudTAK" }).click();

  await expect(page.getByText("That hand-over could not be prepared.")).toBeVisible();
  await expect(page.getByText("no certificate authority", { exact: false })).toBeVisible();
});

test("onboarding produces the three URLs and a keystore that downloads exactly once", async ({
  page,
}) => {
  await openDemoPanel(page);
  await page.getByRole("button", { name: "Onboard CloudTAK" }).click();

  await expect(page.getByText("This is the only time this secret is shown")).toBeVisible();

  const values = page.locator(".copyable__value");

  await expect(values.filter({ hasText: "ssl://" })).toHaveCount(1);
  await expect(values.filter({ hasText: /^https:\/\/.+:\d+$/ })).toHaveCount(2);
  await expect(values.filter({ hasText: DEMO_ACCOUNT })).toHaveCount(1);

  // The sentence the whole feature turns on.
  await expect(
    page.getByText("The key was generated on the server for this download and discarded."),
  ).toBeVisible();

  const downloading = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download keystore (.p12)" }).click();
  const download = await downloading;

  expect(download.suggestedFilename()).toBe(`${DEMO_ACCOUNT}-cloudtak.p12`);

  // The server deleted its copy as it answered, so the same button must now say
  // so rather than appearing to work.
  await page.getByRole("button", { name: "Download keystore (.p12)" }).click();

  await expect(page.getByText("That keystore did not arrive.")).toBeVisible();
  await expect(page.getByText("already been downloaded", { exact: false })).toBeVisible();
});

test("the secrets are shown once and are not on the page after a reload", async ({ page }) => {
  await openDemoPanel(page);
  await page.getByRole("button", { name: "Onboard CloudTAK" }).click();

  await expect(page.getByText("This is the only time this secret is shown")).toBeVisible();

  const secret = (await page.locator(".copyable__value").first().innerText()).trim();

  expect(secret.length).toBeGreaterThan(0);

  // Nothing on the server can produce either secret again: the client password
  // is an argon2 hash and the passphrase was never stored at all.
  await page.reload();
  await page.getByRole("tab", { name: "CloudTAK" }).click();

  await expect(page.getByRole("button", { name: "Onboard CloudTAK" })).toBeVisible();
  await expect(page.locator("body")).not.toContainText(secret);
});

test("the host and ports a published deployment uses can be overridden before generating", async ({
  page,
}) => {
  await openDemoPanel(page);

  await page.getByRole("button", { name: "Override host and ports" }).click();

  await page.getByLabel("Host").fill("tak.published.example");
  await page.getByLabel("Stream port").fill("28089");
  await page.getByLabel("Marti port").fill("28443");
  await page.getByLabel("WebTAK port").fill("28446");

  await page.getByRole("button", { name: "Onboard CloudTAK" }).click();

  // rustak has no way to know which ports a container was published on, so the
  // three URLs have to come back exactly as they were asked for.
  const values = page.locator(".copyable__value");

  await expect(values.filter({ hasText: "ssl://tak.published.example:28089" })).toHaveCount(1);
  await expect(values.filter({ hasText: "https://tak.published.example:28443" })).toHaveCount(1);
  await expect(values.filter({ hasText: "https://tak.published.example:28446" })).toHaveCount(1);
});
