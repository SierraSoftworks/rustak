/**
 * Getting around the console.
 *
 * The admin UI is one wasm bundle behind a server-side catch-all: the router
 * lives in the browser, and the server answers any path it does not own with
 * `index.html`. That arrangement has two failure modes worth a test each — a
 * client-side link that routes to the wrong page, and a deep link or a reload
 * that never reaches the router at all because the server 404'd it first.
 *
 * Every destination in the navigation strip is here from M0, even the ones
 * whose pages arrive in a later milestone, because a link that goes nowhere is
 * worse than one that says what it is waiting for.
 */

import { bootstrapAdmin, expect, gotoApp, signIn, test, waitForApp } from "./helpers";

/**
 * The navigation strip's label, and the heading the page it opens announces
 * itself with.
 *
 * They differ on purpose in two places: the strip has room for "Packages" and
 * "Profiles" where the page can afford "Data packages" and "Device profiles".
 * Asserting both is what proves the link opened the page it claimed to.
 */
const DESTINATIONS: ReadonlyArray<readonly [string, string]> = [
  ["Devices", "Devices"],
  ["Users", "Users"],
  ["Channels", "Channels"],
  ["Credentials", "Credentials"],
  ["Missions", "Missions"],
  ["Packages", "Data packages"],
  ["Profiles", "Device profiles"],
  ["Services", "Services"],
  ["Activity", "Activity"],
  ["Settings", "Settings"],
  ["Dashboard", "Dashboard"],
];

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("every destination in the navigation strip opens the page it names", async ({ page }) => {
  await gotoApp(page, "/admin");
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();

  for (const [label, heading] of DESTINATIONS) {
    await page.getByRole("link", { name: label, exact: true }).click();
    await expect(page.getByRole("heading", { name: heading, exact: true })).toBeVisible();
  }
});

test("a deep link into the console is served by the single-page fallback", async ({ page }) => {
  // The server owns `/api/v1`, `/robots.txt` and the bundle's own assets, and
  // answers everything else with `index.html` — which is what makes a bookmark,
  // a reload and a pasted link all work. A 404 here would mean the fallback was
  // not wired up, and only a deep link would ever show it.
  const response = await page.goto("/admin/settings");
  expect(response?.status()).toBe(200);

  await waitForApp(page);
  await expect(page.getByRole("heading", { name: "Settings" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "This server", exact: true })).toBeVisible();
});

test("an address nothing matches reaches the application's own not-found page", async ({
  page,
}) => {
  // Because of that fallback the *server* cannot answer 404 for a mistyped
  // path, so the page saying so has to be the application's. Asserting the
  // status here would assert the opposite of the design.
  const response = await page.goto("/there-is-nothing-here");
  expect(response?.status()).toBe(200);

  await waitForApp(page);
  await expect(page.getByRole("heading", { name: "Page not found" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Back to the start" })).toBeVisible();
});

test("the landing page gets out of the way of somebody already signed in", async ({ page }) => {
  // It exists for the moment before anybody knows what they are looking at. A
  // session resolving to "signed in" is that moment ending, so it should not
  // cost a click.
  await gotoApp(page, "/");

  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();
  expect(new URL(page.url()).pathname).toBe("/admin");
});
