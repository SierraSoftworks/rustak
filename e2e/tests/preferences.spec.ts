/**
 * What an account chooses about how the console looks to it.
 *
 * Against the real server rather than the demo fixtures, because the point of
 * a preference is that it is *kept*: chosen on one page, stored by the API,
 * and read back by another page after a navigation that demo mode would have
 * forgotten it across. The map has nothing on it here — nothing has ever
 * connected to this suite's server — and does not need anything: what it
 * publishes on its own element is which edition it would draw in.
 *
 * Every test shares one database, so the choice is put back at the end.
 */

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);

  await page.route("https://tile.openstreetmap.org/**", (route) => route.abort());
});

test("the edition of MIL-STD-2525 chosen under Account is the one the map draws in", async ({ page }) => {
  await gotoApp(page, "/admin/map");
  await expect(page.getByRole("application")).toHaveAttribute("data-symbology", "2525c");

  await gotoApp(page, "/admin/settings/account");
  const symbols = page.getByLabel("Map symbols");
  await expect(symbols).toHaveValue("2525c");
  await symbols.selectOption("2525d");

  // Saved the moment it is chosen, and the session re-read: no button to press.
  await expect(symbols).toHaveValue("2525d");
  await expect(symbols).toBeEnabled();

  // A full navigation, so this is the server's answer and not the page's memory.
  await gotoApp(page, "/admin/map");
  await expect(page.getByRole("application")).toHaveAttribute("data-symbology", "2525d");

  await gotoApp(page, "/admin/settings/account");
  await page.getByLabel("Map symbols").selectOption("2525c");
  await expect(page.getByLabel("Map symbols")).toHaveValue("2525c");
  await expect(page.getByLabel("Map symbols")).toBeEnabled();
});
