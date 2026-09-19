/**
 * Bringing somebody on: an account, a channel, an enrolment token and a QR
 * code — then taking the token back again.
 *
 * This is the flow the whole identity milestone exists for, and every step of
 * it is driven through the real console against the real API. Two things it
 * asserts that no unit test can:
 *
 * - **The QR code is rendered in the browser.** The enrolment URL carries a
 *   one-time secret, so it is encoded by wasm in the page rather than fetched
 *   from an image service. A `<svg role="img">` with the right label is the
 *   proof that the encoder ran and that nothing was requested from anywhere.
 * - **The secret is shown exactly once.** The server kept an argon2 hash and
 *   nothing else, so a reload must not bring it back — and the list it reloads
 *   into must not carry it either.
 */

import type { Locator } from "@playwright/test";

import {
  bootstrapAdmin,
  expect,
  gotoApp,
  signIn,
  test,
  uniqueName,
} from "./helpers";

/**
 * A username no other test could produce.
 *
 * `uniqueName` puts a space in, which `Username::parse` refuses — a name has to
 * be lower case, start with a letter or a digit, and carry no whitespace — so
 * the suffix is built here instead of borrowing that helper.
 */
function uniqueUsername(prefix: string): string {
  const suffix = `${Math.random().toString(36).slice(2, 8)}${Date.now().toString(36).slice(-4)}`;
  return `${prefix}-${suffix}`.toLowerCase();
}

/**
 * Flips one of the console's switches, the way somebody using it would.
 *
 * The checkbox itself is a one-pixel, transparent element behind the drawn
 * track — kept in the accessibility tree and focusable, but not the thing a
 * pointer ever lands on. So the click goes to the switch's own text, which is
 * inside the `<label>` that wraps the input, and the browser forwards it.
 */
async function flip(row: Locator, name: string): Promise<void> {
  await row.getByText(name, { exact: true }).click();
}

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("an administrator creates an account, and it opens on its own page", async ({ page }) => {
  const username = uniqueUsername("created");
  const displayName = uniqueName("Created Person");

  await gotoApp(page, "/admin/users");

  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Display name").fill(displayName);
  await page.getByLabel("Email").fill(`${username}@example.com`);
  await page.getByRole("button", { name: "Create account" }).click();

  // The list reloads from the server rather than pushing the row in locally, so
  // seeing it here means the account was genuinely written.
  const link = page.getByRole("link", { name: displayName, exact: true });
  await expect(link).toBeVisible();

  await link.click();
  await expect(page.getByRole("heading", { name: displayName })).toBeVisible();
  await expect(page.getByText(username, { exact: true }).first()).toBeVisible();

  // Profile is the tab it opens on, and it reads back what was just sent.
  await expect(page.getByRole("tab", { name: "Profile" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(page.getByText(`${username}@example.com`)).toBeVisible();
});

test("minting an enrolment token shows the secret once, with a scannable QR code", async ({
  page,
}) => {
  const username = uniqueUsername("enrolling");
  const label = uniqueName("Handset");

  await gotoApp(page, "/admin/users");
  await page.getByLabel("Username").fill(username);
  await page.getByRole("button", { name: "Create account" }).click();
  await page.getByRole("link", { name: username, exact: true }).click();

  await page.getByRole("tab", { name: "Credentials" }).click();
  await page.getByLabel("Label").fill(label);
  await page.getByRole("button", { name: "Mint", exact: true }).click();

  // The panel says in as many words that this will not be shown again, because
  // a secret shown once without saying so is a secret somebody loses.
  await expect(
    page.getByText("This is the only time this secret is shown"),
  ).toBeVisible();

  // Encoded in the page by wasm. The label is what a screen reader reads, and
  // its presence is what says the encoder produced a code rather than an error.
  await expect(
    page.getByRole("img", { name: "Enrolment QR code for ATAK" }),
  ).toBeVisible();

  // The URL ATAK's quick connect parses, with the account it enrols and the
  // host the certificate will be issued for.
  const link = page.locator(".copyable__value").filter({ hasText: "tak://" });
  await expect(link).toBeVisible();
  const url = (await link.innerText()).trim();
  expect(url).toContain("tak://com.atakmap.app/enroll?host=");
  expect(url).toContain(`username=${username}`);
  expect(url).toContain("&token=");

  // A secret and a link that embeds it are the same secret, so the two boxes
  // have to agree — a link built from the wrong token would enrol nothing.
  const secret = (
    await page.locator(".copyable__value").first().innerText()
  ).trim();
  expect(secret.length).toBeGreaterThan(8);
  expect(url).toContain(encodeURIComponent(secret));

  // Reloading is the test: the server kept a hash, so nothing can bring it
  // back, and the list it reloads into must not be carrying it either.
  await page.reload();
  await page.getByRole("tab", { name: "Credentials" }).click();
  await expect(page.getByText(label, { exact: true })).toBeVisible();
  await expect(page.locator("body")).not.toContainText(secret);
  await expect(
    page.getByText("This is the only time this secret is shown"),
  ).toBeHidden();
});

test("a credential can be revoked, and says so afterwards", async ({ page }) => {
  const username = uniqueUsername("revoking");
  const label = uniqueName("Spare");

  await gotoApp(page, "/admin/users");
  await page.getByLabel("Username").fill(username);
  await page.getByRole("button", { name: "Create account" }).click();
  await page.getByRole("link", { name: username, exact: true }).click();
  await page.getByRole("tab", { name: "Credentials" }).click();

  await page.getByLabel("Label").fill(label);
  await page.getByRole("button", { name: "Mint", exact: true }).click();
  await expect(
    page.getByRole("img", { name: "Enrolment QR code for ATAK" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Done" }).click();

  const row = page.locator(".credential-row").filter({ hasText: label });
  await expect(row.getByText("Active")).toBeVisible();

  // Destructive, so it asks — and the question names what is about to go rather
  // than being a bare "are you sure".
  await row.getByRole("button", { name: "Revoke", exact: true }).click();
  await expect(row.getByText(`Revoke '${label}'?`)).toBeVisible();
  await row.getByRole("button", { name: "Revoke it" }).click();

  await expect(row.getByText("Revoked")).toBeVisible();
  await expect(row.getByRole("button", { name: "Revoke", exact: true })).toBeDisabled();

  // And it survives a reload, which is what says the server was told.
  await page.reload();
  await page.getByRole("tab", { name: "Credentials" }).click();
  await expect(
    page.locator(".credential-row").filter({ hasText: label }).getByText("Revoked"),
  ).toBeVisible();
});

test("a channel is created and granted to an account", async ({ page }) => {
  const username = uniqueUsername("channelled");
  const channel = uniqueName("Channel");

  await gotoApp(page, "/admin/users");
  await page.getByLabel("Username").fill(username);
  await page.getByRole("button", { name: "Create account" }).click();

  // The bit position is the server's to allocate, so the form does not offer
  // one — it is shown on the row afterwards.
  await gotoApp(page, "/admin/groups");
  await page.getByLabel("Name").fill(channel);
  await page.getByRole("button", { name: "Create channel" }).click();

  const channelRow = page.locator(".channel-row").filter({ hasText: channel });
  await expect(channelRow).toBeVisible();
  await expect(channelRow.getByText(/^bit \d+$/)).toBeVisible();

  await gotoApp(page, "/admin/users");
  await page.getByRole("link", { name: username, exact: true }).click();
  await page.getByRole("tab", { name: "Channels" }).click();

  const pickerRow = page.locator(".channel-picker__row").filter({ hasText: channel });
  await expect(pickerRow).toBeVisible();
  await flip(pickerRow, "Write");
  await expect(pickerRow.getByRole("checkbox", { name: "Write" })).toBeChecked();
  await page.getByRole("button", { name: "Save channels" }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();

  // `PUT` replaces the whole set, so the only proof that the right set was sent
  // is reading it back out of the server.
  await page.reload();
  await page.getByRole("tab", { name: "Channels" }).click();
  const reloaded = page.locator(".channel-picker__row").filter({ hasText: channel });
  await expect(reloaded.getByRole("checkbox", { name: "Write" })).toBeChecked();
  await expect(reloaded.getByRole("checkbox", { name: "Read" })).not.toBeChecked();

  // And the channel's own page shows the same membership from the other side.
  // The row's selector is named after the channel and its bit position; the
  // row's action menu is named after the channel too, so the bit is what tells
  // the two apart.
  await gotoApp(page, "/admin/groups");
  await page.locator(".channel-row").filter({ hasText: channel }).getByRole("button", {
    name: `${channel} bit`,
  }).click();
  const member = page.locator(".member-row").filter({ hasText: username });
  await expect(member.getByRole("checkbox", { name: "Write" })).toBeChecked();
});

test("a row's menu closes on Escape and outside clicks, and a destructive item asks first", async ({
  page,
}) => {
  // Every row with several actions shares one split button, so one row's
  // menu stands for all of them. A channel's is the one with nothing else on
  // the page to confuse it with.
  const channel = uniqueName("Channel");

  await gotoApp(page, "/admin/groups");
  await page.getByLabel("Name").fill(channel);
  await page.getByRole("button", { name: "Create channel" }).click();

  const row = page.locator(".channel-row").filter({ hasText: channel });
  await expect(row).toBeVisible();
  const toggle = row.getByRole("button", { name: `More actions for ${channel}` });
  const remove = row.getByRole("menuitem", { name: "Delete" });

  // Opening puts the focus inside the menu, which is what lets Escape reach it.
  await toggle.click();
  await expect(remove).toBeVisible();
  await expect(remove).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(remove).toHaveCount(0);

  // A click anywhere else lands on the backdrop the open menu puts under
  // itself, and closes it without doing anything to what was clicked.
  await toggle.click();
  await expect(remove).toBeVisible();
  await page.locator(".split-btn__backdrop").click({ position: { x: 5, y: 5 } });
  await expect(remove).toHaveCount(0);
  await expect(row).toBeVisible();

  // A destructive item asks in place, naming what is about to go, and can be
  // backed out of — which leaves the button as it was and the row intact.
  await toggle.click();
  await remove.click();
  await expect(row.getByText(`Delete '${channel}'?`)).toBeVisible();
  await row.getByRole("button", { name: "Cancel" }).click();
  await expect(toggle).toBeVisible();
  await expect(row).toBeVisible();

  await toggle.click();
  await remove.click();
  await row.getByRole("button", { name: "Delete it" }).click();
  await expect(row).toHaveCount(0);

  // Gone on the server too, not just from the list it was removed from.
  await page.reload();
  await expect(page.getByRole("heading", { name: "Channels", exact: true })).toBeVisible();
  await expect(page.locator(".channel-row").filter({ hasText: channel })).toHaveCount(0);
});

test("anybody can mint an enrolment token for their own phone", async ({ page }) => {
  const label = uniqueName("My own phone");

  // No username anywhere in this flow: `POST /api/v1/credentials` mints for the
  // caller, which is what lets somebody enrol a device without an administrator.
  await gotoApp(page, "/admin/credentials");
  await expect(page.getByRole("heading", { name: "Your credentials" })).toBeVisible();

  await page.getByLabel("Label").fill(label);
  await page.getByRole("button", { name: "Mint", exact: true }).click();

  await expect(
    page.getByRole("img", { name: "Enrolment QR code for ATAK" }),
  ).toBeVisible();
  await expect(page.locator(".copyable__value").filter({ hasText: "tak://" })).toBeVisible();

  await page.getByRole("button", { name: "Done" }).click();
  await expect(
    page.locator(".credential-row").filter({ hasText: label }).getByText("Active"),
  ).toBeVisible();
});
