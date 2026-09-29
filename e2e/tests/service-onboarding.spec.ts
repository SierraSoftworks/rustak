/**
 * "Add a service" on the Services page: one action, and everything a sidecar
 * deployment pastes in, shown once.
 *
 * Unlike the rest of the Services specs this runs against the real API rather
 * than `?demo`: the action needs no certificate authority — it creates an
 * account and mints two tokens — so the suite's server can do all of it, and
 * what is asserted is what the server actually answered. The sidecar that then
 * enrols with those values is proved in
 * `rustak-server/tests/service_onboarding.rs`; this is the layer where the
 * operator sees them.
 */

import { ADMIN, bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

/** Where the page lives. The path predates the name. */
const SERVICES = "/admin/settings/add-ons";

/**
 * A service name no other test could produce: lower-case letters, digits and
 * hyphens, which is all a service name may hold.
 */
function uniqueService(prefix: string): string {
  const suffix = `${Math.random().toString(36).slice(2, 8)}${Date.now().toString(36).slice(-4)}`;
  return `${prefix}-${suffix}`.toLowerCase();
}

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("an administrator adds a service and is shown what its deployment needs, once", async ({
  page,
}) => {
  const name = uniqueService("feed");
  await gotoApp(page, SERVICES);

  await expect(page.getByRole("heading", { name: "Add a service" })).toBeVisible();
  const add = page.getByRole("button", { name: "Add service", exact: true });
  // Nothing to add until there is a name.
  await expect(add).toBeDisabled();

  await page.locator("#service-name").fill(name);
  await page.locator("#service-account").fill(`svc.${name}`);
  await add.click();

  const result = page.getByRole("region", { name: `Service ${name}` });
  await expect(result).toBeVisible();
  await expect(
    result.getByText(`Created the service account 'svc.${name}'`, { exact: false }),
  ).toBeVisible();
  await expect(result.getByText("A service token was minted", { exact: false })).toBeVisible();
  // The fifteen minutes are the reason this is one action: the expiry is said.
  await expect(result.getByText("The enrolment token expires at", { exact: false })).toBeVisible();

  // Two secrets and two blocks to paste, each with its own copy button.
  const values = result.locator(".copyable__value");
  await expect(values).toHaveCount(4);
  await expect(result.getByRole("button", { name: "Copy", exact: true })).toHaveCount(4);

  const enrolment = (await values.nth(0).textContent()) ?? "";
  expect(enrolment.length).toBeGreaterThan(20);
  await expect(values.nth(1)).toHaveText(/^rsk_/);

  const fragment = values.nth(2);
  await expect(fragment).toContainText(`name = "${name}"`);
  await expect(fragment).toContainText(`account = "svc.${name}"`);
  // The file names the service token by its variable, never by value.
  await expect(fragment).toContainText("${{ env.RUSTAK_SERVICE_TOKEN }}");

  const environment = values.nth(3);
  await expect(environment).toContainText(`RUSTAK_ENROLLMENT_TOKEN=${enrolment}`);
  await expect(environment).toContainText("RUSTAK_SERVICE_TOKEN=rsk_");

  // Dismissed, the secrets are gone from the page and the form is empty again.
  await result.getByRole("button", { name: "Done" }).click();
  await expect(result).toHaveCount(0);
  await expect(page.getByText(enrolment)).toHaveCount(0);
  await expect(page.locator("#service-name")).toHaveValue("");
});

test("adding the same service again mints a fresh enrolment token and keeps its service token", async ({
  page,
}) => {
  const name = uniqueService("again");
  await gotoApp(page, SERVICES);

  const addIt = async () => {
    await page.locator("#service-name").fill(name);
    await page.getByRole("button", { name: "Add service", exact: true }).click();
    return page.getByRole("region", { name: `Service ${name}` });
  };

  const first = await addIt();
  const token = (await first.locator(".copyable__value").nth(0).textContent()) ?? "";
  await first.getByRole("button", { name: "Done" }).click();

  const second = await addIt();
  await expect(second.getByText("already existed", { exact: false })).toBeVisible();
  // Nothing in use was revoked, and the response says the token was kept.
  await expect(second.getByText("existing service token was kept", { exact: false })).toBeVisible();

  const values = second.locator(".copyable__value");
  await expect(values).toHaveCount(3);
  await expect(values.nth(0)).not.toHaveText(token);
  await expect(values.nth(2)).not.toContainText("RUSTAK_SERVICE_TOKEN=rsk_");
  await expect(values.nth(2)).toContainText("keep the one this deployment already has");
});

test("a person's account is refused, and the page says why", async ({ page }) => {
  await gotoApp(page, SERVICES);

  // The administrator is a person, and a sidecar cannot enrol as one.
  await page.locator("#service-name").fill(ADMIN.username);
  await page.getByRole("button", { name: "Add service", exact: true }).click();

  await expect(page.getByText("That service could not be added.")).toBeVisible();
  await expect(page.getByText("is a person's account", { exact: false })).toBeVisible();
  await expect(page.locator(".copyable__value")).toHaveCount(0);
});
