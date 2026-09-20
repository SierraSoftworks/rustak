/**
 * The two pages that answer "what is happening right now": the connected
 * clients and the situational-awareness browser.
 *
 * Neither has anything to show on a server nothing has ever connected to,
 * which is exactly the state this suite's server is in — and that is worth
 * asserting rather than skipping. An operator opening these pages on a quiet
 * installation must be told *nothing is connected*, not shown an error and not
 * left looking at a spinner, because "quiet" and "broken" are the two things
 * they are trying to tell apart.
 *
 * The rest of both pages — the rows, the role and channel columns, the
 * incognito switch, the XML drawer — is exercised against the demo fixtures in
 * the walkthrough, because producing a real stream connection needs an EUD and
 * belongs to the interop suite rather than here.
 */

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("a server with nothing connected says so rather than failing", async ({ page }) => {
  await gotoApp(page, "/admin/clients");

  // `GET /clients` answers `[]` rather than a 503 on an installation with no
  // stream listener, so the page needs no special case for it — and this is
  // what proves the page did not get one by accident.
  await expect(page.getByText("Nothing is connected.", { exact: false })).toBeVisible();
  await expect(page.getByText("could not read", { exact: false })).toHaveCount(0);

  // `[]` alone cannot say whether the listener is quiet or absent, which is a
  // configuration problem and a normal Tuesday respectively. `GET
  // /clients/status` is what tells them apart, and the empty state says which
  // one it is looking at rather than describing both.
  await expect(
    page.getByText("no stream listener", { exact: false }),
  ).toBeVisible();

  // The auto-refresh is on by default and switchable off, because a page left
  // open on a wall display should not be a request every five seconds for a
  // week.
  const live = page.locator("#clients-live");
  await expect(live).toBeChecked();
  await page.getByText("Refresh every 5 seconds", { exact: false }).click();
  await expect(live).not.toBeChecked();
});

test("the situational-awareness browser is empty rather than broken", async ({ page }) => {
  await gotoApp(page, "/admin/situation");

  await expect(
    page.getByText("This server holds the latest message per identifier", { exact: false }),
  ).toBeVisible();
  await expect(page.getByText("could not read", { exact: false })).toHaveCount(0);

  // The three filters are the three the endpoint takes. Narrowing an empty
  // list is still a real request, so this asserts the page survives one.
  await page.getByLabel("Type").fill("a-f");
  await page.getByLabel("Callsign").fill("ALPHA");
  await expect(page.getByText("Nothing matches.", { exact: false })).toBeVisible();
});

test("both live pages are reachable from the navigation sidebar", async ({ page }) => {
  await gotoApp(page, "/admin/");

  // Under the sidebar's "Live" heading the link is called what the page is.
  await page.getByRole("link", { name: "Clients", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Clients", level: 1 })).toBeVisible();

  await page.getByRole("link", { name: "Situation", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Situational awareness", level: 1 }),
  ).toBeVisible();
});
