/**
 * Getting around the console.
 *
 * The admin UI is one wasm bundle behind a server-side catch-all: the router
 * lives in the browser, and the server answers any path it does not own with
 * `index.html`. That arrangement has two failure modes worth a test each — a
 * client-side link that routes to the wrong page, and a deep link or a reload
 * that never reaches the router at all because the server 404'd it first.
 *
 * Every destination in the navigation sidebar is here from M0, even the ones
 * whose pages arrive in a later milestone, because a link that goes nowhere is
 * worse than one that says what it is waiting for.
 *
 * Since M2 three of them are real pages rather than stubs — Devices, Channels
 * and Credentials — and since M9 the last stub is gone too: "Add-ons" is now
 * the Services page (`services.spec.ts`), at the same path. One destination is
 * reachable only from inside another:
 * an account's own page, at `/admin/users/{username}`. It has no link in the
 * sidebar on purpose, because it is about one row rather than one area, so it is
 * tested as a deep link instead.
 */

import { bootstrapAdmin, expect, gotoApp, signIn, test, waitForApp } from "./helpers";

/**
 * The navigation sidebar's label, and the heading the page it opens announces
 * itself with.
 *
 * They differ on purpose in three places: the sidebar has room for "Packages",
 * "Profiles" and "Account" where the page can afford "Data packages", "Device
 * profiles" and "Your account".
 * Asserting both is what proves the link opened the page it claimed to.
 */
const DESTINATIONS: ReadonlyArray<readonly [string, string]> = [
  ["Map", "Map"],
  ["Situation", "Situation"],
  ["Activity", "Activity"],
  ["Users", "Users"],
  ["EUDs", "EUDs"],
  ["Profiles", "Device profiles"],
  ["Channels", "Channels"],
  ["Missions", "Missions"],
  ["Packages", "Data packages"],
  ["Account", "Your account"],
  ["Security", "Security"],
  ["Storage", "Storage"],
  ["Services", "Services"],
  ["Dashboard", "Dashboard"],
];

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("every destination in the navigation sidebar opens the page it names", async ({ page }) => {
  await gotoApp(page, "/admin");
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();

  for (const [label, heading] of DESTINATIONS) {
    await page.getByRole("link", { name: label, exact: true }).click();
    await expect(page.getByRole("heading", { name: heading, exact: true })).toBeVisible();
  }
});

test("an account's own page is a deep link, and the sidebar still says where it is", async ({
  page,
}) => {
  // `Route::UserDetail` carries the username as a path segment, so this is both
  // a router test and a server-fallback test: the path has two segments below
  // `/admin`, and only the catch-all can answer it.
  const response = await page.goto("/admin/users/avery");
  expect(response?.status()).toBe(200);

  await waitForApp(page);
  await expect(page.getByRole("heading", { name: "Account", exact: true })).toBeVisible();
  await expect(page.getByRole("tab", { name: "Profile" })).toBeVisible();
  await expect(page.getByRole("tab", { name: "Channels" })).toBeVisible();

  // One account's page is somewhere inside Users, so the sidebar must not read
  // as though nothing is selected while it is open.
  await expect(page.getByRole("link", { name: "Users", exact: true })).toHaveClass(
    /admin-nav__link--active/,
  );

  await page.getByRole("link", { name: "Users", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Users", exact: true })).toBeVisible();
});

test("a name no account could have says so rather than failing to load", async ({ page }) => {
  // `__anon__` is reserved, so `Username::parse` refuses it — which is not the
  // same as an account that is missing, and the page says which of the two it is.
  await page.goto("/admin/users/__anon__");
  await waitForApp(page);

  await expect(page.getByText("That is not a username.")).toBeVisible();
});

test("a deep link into the console is served by the single-page fallback", async ({ page }) => {
  // The server owns `/api/v1`, `/robots.txt` and the bundle's own assets, and
  // answers everything else with `index.html` — which is what makes a bookmark,
  // a reload and a pasted link all work. A 404 here would mean the fallback was
  // not wired up, and only a deep link would ever show it.
  const response = await page.goto("/admin/settings/security");
  expect(response?.status()).toBe(200);

  await waitForApp(page);
  // Exact, because "Transport security" is a card heading on the same page.
  await expect(page.getByRole("heading", { name: "Security", exact: true })).toBeVisible();
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
