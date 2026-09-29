/**
 * What the Transport security card says about each kind of TLS.
 *
 * The card renders a different thing for every source — `files` has three
 * rows, a "waiting" note and a differently worded error; `acme` has the
 * directory, the challenge and an order count — and a server only ever has
 * one of them. This suite's server runs with `[web.public.tls] mode = "none"`
 * (`scripts/start-server.mjs`) and could not be given another without
 * restarting it, so the sources are driven through the demo fixtures instead:
 * `?demo&tls=files` selects the state a real `mode = "files"` deployment is in
 * while a sidecar has yet to write the pair (M7-04, M2-13).
 *
 * Worth a browser rather than a unit test because the failure it guards
 * against is the card not *rendering* those fields — the labels are in
 * `settings_tls.rs`'s `html!`, where nothing but a DOM can read them.
 */

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("a listener waiting for its certificate files says so, and says which files", async ({
  page,
}) => {
  await gotoApp(page, "/admin/settings/security?demo&tls=files");

  await expect(page.getByRole("heading", { name: "Transport security" })).toBeVisible();
  await expect(page.getByText("Read from the files named in the configuration.")).toBeVisible();

  // The banner M2-14 added: `state` alone says "missing", and this is the
  // sentence that says why that is not necessarily broken yet.
  await expect(page.getByText("What the listener is waiting for.")).toBeVisible();
  await expect(
    page.getByText("Waiting for the certificate files to appear", { exact: false }),
  ).toBeVisible();

  // Nobody issues a pair of files, so the pill must not say "Not issued yet".
  await expect(page.getByText("Waiting for the files", { exact: true })).toBeVisible();

  await expect(page.getByText("Certificate file", { exact: true })).toBeVisible();
  await expect(page.getByText("Key file", { exact: true })).toBeVisible();
  await expect(page.getByText("Never — nothing has been read off disk.")).toBeVisible();

  // A files listener places no orders, so the button is a re-read and the ACME
  // rows are not on the card at all.
  await expect(page.getByRole("button", { name: "Reload certs" })).toBeVisible();
  await expect(page.getByText("Directory", { exact: true })).toHaveCount(0);
});

test("an ACME listener whose orders keep failing shows the authority's own reason", async ({
  page,
}) => {
  await gotoApp(page, "/admin/settings/security?demo");

  await expect(page.getByText("3 orders in a row have failed.")).toBeVisible();
  await expect(page.getByText("no valid A record found", { exact: false })).toBeVisible();
  await expect(page.getByRole("button", { name: "Renew now" })).toBeVisible();

  // The other source's rows belong to the other source.
  await expect(page.getByText("Certificate file", { exact: true })).toHaveCount(0);
});

/**
 * The sign-in lockouts card (M10-11). The real server behind this suite has
 * locked nobody out, which is itself worth asserting — it is the endpoint
 * answering an administrator through the embedded UI — and the demo fixtures
 * carry one lockout of each class, which is how the rows and the confirmation
 * are driven.
 */
test("a server that has locked nobody out says so", async ({ page }) => {
  await gotoApp(page, "/admin/settings/security");

  await expect(page.getByRole("heading", { name: "Sign-in lockouts" })).toBeVisible();
  await expect(page.getByText("Nothing is locked out.")).toBeVisible();
});

test("a lockout says what it names and where from, and is cleared only after asking", async ({
  page,
}) => {
  await gotoApp(page, "/admin/settings/security?demo");

  await expect(page.getByRole("heading", { name: "Sign-in lockouts" })).toBeVisible();

  const rows = page.locator(".lockout-row");
  await expect(rows).toHaveCount(3);
  await expect(page.getByText("Every 'passkey' attempt")).toBeVisible();
  await expect(page.getByText("OAuth client 'cloudtak'")).toBeVisible();
  await expect(
    page.getByText("5 lockouts started and 41 attempts refused", { exact: false }),
  ).toBeVisible();

  const linus = rows.filter({ hasText: "Account 'linus'" });
  await expect(linus).toContainText("from 203.0.113.7");
  await expect(linus).toContainText("10 failures");

  // The first click only asks, and the question names what is about to go.
  await linus.getByRole("button", { name: "Clear", exact: true }).click();
  await expect(
    linus.getByText("Clear the lockout on account 'linus' from 203.0.113.7?", { exact: false }),
  ).toBeVisible();
  await expect(rows).toHaveCount(3);

  await linus.getByRole("button", { name: "Clear it" }).click();

  await expect(rows).toHaveCount(2);
  await expect(page.getByText("Account 'linus'")).toHaveCount(0);
  await expect(page.getByText("Every 'passkey' attempt")).toBeVisible();
});
