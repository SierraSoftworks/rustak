/**
 * A Data Sync mission, as an operator deals with one.
 *
 * The mission itself is created the way a client creates it — `PUT
 * /Marti/api/missions/{name}` — because nothing in the admin API creates one
 * and inventing a second route to do it would be testing a path no TAK client
 * takes. Everything after that is driven through the real console.
 *
 * Two things this asserts that no unit test can:
 *
 * - **A mission a client made is visible to an administrator.** The admin
 *   listing is deliberately not the Marti one: it hides nothing, so a mission
 *   that is invite-only or password-protected still appears.
 * - **A subscription can be changed and taken away.** Setting somebody else's
 *   role and unsubscribing their device are the two operations no TAK client
 *   performs, which makes them the two this page exists for.
 * - **Deleting is not immediate forgetting.** The row stays in the database, so
 *   a client syncing late is told the mission went rather than simply failing
 *   to find it — `GET /api/v1/missions/{guid}` answers `410 Gone` afterwards,
 *   which is why the page reports the outcome of the delete rather than
 *   re-reading.
 */

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

/**
 * A mission name no other test could produce.
 *
 * `uniqueName` puts a space in, and a mission name travels in a path segment
 * on every Marti route — so the suffix is built here without one rather than
 * having to be encoded and decoded at each step.
 */
function uniqueMission(): string {
  const suffix = `${Math.random().toString(36).slice(2, 8)}${Date.now().toString(36).slice(-4)}`;
  return `e2e-mission-${suffix}`;
}

test("a mission a client created is listed, opened, and deleted", async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);

  const name = uniqueMission();
  const creator = `E2E-${name}`;

  const created = await page.request.put(
    `/Marti/api/missions/${name}?creatorUid=${creator}&description=Created+by+the+end-to-end+suite&tool=public`,
    { headers: { Authorization: `Bearer ${session.token}` } },
  );
  expect(
    created.status(),
    `PUT /Marti/api/missions/${name} should have created it: ${await created.text()}`,
  ).toBe(201);

  // --- the listing -------------------------------------------------------

  await gotoApp(page, "/admin/missions");

  await page.getByLabel("Filter").fill(name);

  const link = page.getByRole("link", { name, exact: true });
  await expect(link).toBeVisible();
  await expect(page.locator(".mission-row")).toHaveCount(1);

  await link.click();

  // --- the detail page ---------------------------------------------------

  await expect(page.getByRole("heading", { name })).toBeVisible();
  await expect(page.getByText(creator, { exact: true }).first()).toBeVisible();
  await expect(
    page.getByText("Created by the end-to-end suite", { exact: false }),
  ).toBeVisible();

  await expect(page.getByRole("tab", { name: "Overview" })).toHaveAttribute(
    "aria-selected",
    "true",
  );

  // --- subscribers -------------------------------------------------------

  // Creating a mission subscribes its creator as the owner, so there is a real
  // subscription here to change and then take away — which is the pair of
  // actions this page exists for and the pair a TAK client cannot perform on
  // somebody else's device.
  await page.getByRole("tab", { name: "Subscribers" }).click();

  const subscriber = page.locator(".subscriber-row");
  await expect(subscriber).toHaveCount(1);
  await expect(subscriber).toContainText(creator);

  const role = subscriber.locator("select");
  await expect(role).toHaveValue("MISSION_OWNER");

  await role.selectOption("MISSION_READONLY_SUBSCRIBER");

  // The page re-reads the mission rather than keeping what it sent, so seeing
  // the new role here means the server wrote it.
  await expect(role).toHaveValue("MISSION_READONLY_SUBSCRIBER");

  await subscriber.getByRole("button", { name: "Remove", exact: true }).click();
  await subscriber.getByRole("button", { name: "Remove it" }).click();
  await expect(page.getByText("Nobody is subscribed.", { exact: false })).toBeVisible();

  await page.getByRole("tab", { name: "Changes" }).click();
  // Creating a mission is itself a change, so the log is never empty — which
  // is what makes this a real assertion about the endpoint rather than about
  // an empty state.
  await expect(page.locator(".change-row").first()).toBeVisible();
  await expect(page.locator(".change-list")).toContainText("Created");

  // Squashing asks the server a different question — "what would a device be
  // told if it asked now" — so it is its own request, and the toggle has to
  // cause one rather than only changing what the next one would send.
  await page.getByText("Squashed", { exact: false }).click();
  await expect(page.locator("#mission-changes-squashed")).toBeChecked();
  await expect(
    page.getByText("We could not load the changes.", { exact: false }),
  ).toHaveCount(0);

  await page.getByRole("tab", { name: "Layers" }).click();
  await expect(page.getByText("No layers.", { exact: false })).toBeVisible();

  // --- deleting ----------------------------------------------------------

  await page.getByRole("tab", { name: "Overview" }).click();
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await page.getByRole("button", { name: "Delete it" }).click();

  await expect(
    page.getByText("This mission has been deleted.", { exact: false }),
  ).toBeVisible();

  // The row is kept in the database so that a client syncing late is told the
  // mission went — but `GET /api/v1/missions` takes no `include_deleted`, so
  // the console cannot show it and the listing is simply empty. That gap is
  // recorded in the M3-04 status file; this asserts what the server actually
  // does rather than what the page is ready for.
  await gotoApp(page, "/admin/missions");
  await page.getByLabel("Filter").fill(name);
  await expect(page.getByRole("link", { name, exact: true })).toHaveCount(0);
  await expect(page.locator(".mission-row")).toHaveCount(0);
});

test("an address that is not a mission identifier says so rather than failing to load", async ({
  page,
}) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);

  // Missions are addressed by guid here, never by name: a name may be renamed
  // and may itself be a bare UUID, so the immutable identifier is the only
  // thing a deep link can safely carry.
  await gotoApp(page, "/admin/missions/not-a-guid");

  await expect(
    page.getByText("That is not a mission identifier.", { exact: false }),
  ).toBeVisible();
});
