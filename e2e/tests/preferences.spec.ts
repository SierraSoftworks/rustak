/**
 * What an account chooses for itself.
 *
 * Against the real server rather than the demo fixtures, because the point of
 * a preference is that it is *kept*: chosen on a page, stored by the API, and
 * still there after a navigation that demo mode would have forgotten it
 * across. Where the choice goes next — the account's device profile — is the
 * server's business and is covered by its own tests.
 *
 * Every test shares one database, so the choice is put back at the end.
 */

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

const ACCOUNT = "/admin/settings/account";

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("the edition of MIL-STD-2525 chosen under Account is kept by the server", async ({ page }) => {
  await gotoApp(page, ACCOUNT);
  const symbols = page.getByLabel("Symbol edition");
  await expect(symbols).toHaveValue("2525c");
  await symbols.selectOption("2525d");

  // Saved the moment it is chosen, and the session re-read: no button to press.
  await expect(symbols).toHaveValue("2525d");
  await expect(symbols).toBeEnabled();

  // A full navigation, so this is the server's answer and not the page's memory.
  await gotoApp(page, ACCOUNT);
  await expect(page.getByLabel("Symbol edition")).toHaveValue("2525d");

  await page.getByLabel("Symbol edition").selectOption("2525c");
  await expect(page.getByLabel("Symbol edition")).toHaveValue("2525c");
  await expect(page.getByLabel("Symbol edition")).toBeEnabled();
});
