/**
 * The two pages that answer "what is happening right now": the EUDs list,
 * narrowed to what is connected, and the situational-awareness browser.
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
 * belongs to the interop suite rather than here. The one exception below runs
 * against `?demo` for the same reason `services.spec.ts` does: it is about the
 * page, not the data, and the page needs more than one row to show it.
 */

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("a server with nothing connected says so rather than failing", async ({ page }) => {
  await gotoApp(page, "/admin/euds");

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
  const live = page.locator("#euds-refresh");
  await expect(live).toBeChecked();
  await page.getByText("Auto-refresh", { exact: true }).click();
  await expect(live).not.toBeChecked();

  // Widening past what is connected shows every enrolled device (none, on
  // this server) and the day's disconnections beneath.
  await page.getByText("Connected only", { exact: true }).click();
  await expect(page.getByText("Nothing has enrolled yet", { exact: false })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Recently disconnected" })).toBeVisible();
  await expect(page.getByText("Nothing has connected in the last day.")).toBeVisible();
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

test("clicking straight from one row to another shows the second row's message", async ({
  page,
}) => {
  await gotoApp(page, "/admin/situation?demo");

  // The drawer fetches its document and history when it mounts. Selecting a
  // second row while the first is open used to hand the same instance new
  // props, so the heading changed and the bytes under it did not — a marker
  // being diagnosed under another marker's XML. The two rows differ in
  // everything this asserts on: the uid in the document, and how much history
  // the last hour holds.
  const xml = page.locator("pre.xml");
  const history = page.locator(".cot-history__row");

  await page.locator(".cot-row__select", { hasText: "QUINN" }).click();
  await expect(page.getByRole("heading", { name: "ANDROID-2f1c9a7b4e0d" })).toBeVisible();
  await expect(xml).toContainText("ANDROID-2f1c9a7b4e0d");
  await expect(history).toHaveCount(3);

  await page.locator(".cot-row__select", { hasText: "RAO" }).click();
  await expect(page.getByRole("heading", { name: "IOS-91ac4d55f207" })).toBeVisible();
  await expect(xml).toContainText("IOS-91ac4d55f207");
  await expect(xml).not.toContainText("ANDROID-2f1c9a7b4e0d");
  await expect(history).toHaveCount(1);
});

test("both live pages are reachable from the navigation sidebar", async ({ page }) => {
  await gotoApp(page, "/admin/");

  await page.getByRole("link", { name: "EUDs", exact: true }).click();
  await expect(page.getByRole("heading", { name: "EUDs", level: 1 })).toBeVisible();

  await page.getByRole("link", { name: "Situation", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Situation", level: 1 })).toBeVisible();
});
