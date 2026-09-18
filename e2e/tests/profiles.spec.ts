/**
 * A device profile, from nothing to the package a phone would receive.
 *
 * This is the flow the profiles milestone exists for, driven through the real
 * console against the real API. Three things it asserts that no unit test can:
 *
 * - **The preview is built by the server.** The button asks for the Mission
 *   Package a device would actually be handed, assembled by the same code path
 *   as the real delivery — so a download arriving is proof that what an
 *   operator opens is what a client would import, rather than a rendering of
 *   the editor's own state.
 * - **A download works at all.** Every one of these endpoints is behind the
 *   bearer token the application holds in `sessionStorage`, so none of them can
 *   be a plain link: the body is fetched, turned into a blob and clicked at. If
 *   that plumbing breaks, no unit test would notice and every download in the
 *   console would silently do nothing.
 * - **A value is refused against its class before it is sent.** ATAK drops a
 *   preference whose value does not match its `class` attribute, so the editor
 *   checks with the same predicate the server validates with.
 */

import {
  bootstrapAdmin,
  expect,
  gotoApp,
  signIn,
  test,
  uniqueName,
} from "./helpers";

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("a profile is created, given a preference, and previewed as the package a device receives", async ({
  page,
}) => {
  const name = uniqueName("Profile");

  await gotoApp(page, "/admin/profiles");

  await page.getByLabel("Name").fill(name);
  await page.getByLabel("Description").fill("Created by the end-to-end suite.");

  // The switch input is a one-pixel transparent element behind the drawn
  // track, so the click goes to its own text inside the `<label>` and the
  // browser forwards it — the same gesture a person makes.
  await page.getByText("On enrolment", { exact: true }).click();

  await page.getByRole("button", { name: "Create profile" }).click();

  // The list reloads from the server rather than pushing the row in locally,
  // so seeing it here means the profile was genuinely written.
  const link = page.getByRole("link", { name, exact: true });
  await expect(link).toBeVisible();
  await link.click();

  await expect(page.getByRole("heading", { name })).toBeVisible();
  await expect(page.getByText("On enrolment", { exact: true }).first()).toBeVisible();

  // --- preferences -------------------------------------------------------

  await page.getByRole("button", { name: "Add a preference" }).click();

  const key = page.locator("#pref-0-key");
  const value = page.locator("#pref-0-value");
  const cls = page.locator("#pref-0-class");
  const save = page.getByRole("button", { name: "Save preferences" });

  await key.fill("deviceProfileEnableOnConnect");
  await value.fill("true");

  // Choosing a catalogue key brings its class with it: the class is a fact
  // about the preference rather than a choice, and it is the one thing nobody
  // can be expected to know.
  await expect(cls).toHaveValue("String");
  await expect(
    page.getByText("Fetch connection and tool profiles on every stream connect.", {
      exact: false,
    }),
  ).toBeVisible();

  await save.click();
  await expect(page.getByText("Saved.", { exact: true })).toBeVisible();

  // --- a value its class could not hold ----------------------------------

  await cls.selectOption("Integer");
  await expect(
    page.getByText("A Integer entry cannot hold 'true'.", { exact: false }),
  ).toBeVisible();
  await expect(save).toBeDisabled();

  // Giving it a value that class *could* hold makes the list saveable again,
  // which is the proof that the refusal was about the value rather than about
  // having edited at all.
  await value.fill("20");
  await expect(save).toBeEnabled();

  // --- the package a device would receive --------------------------------

  const downloading = page.waitForEvent("download");
  await page.getByRole("button", { name: "Preview package" }).click();
  const download = await downloading;

  expect(download.suggestedFilename()).toBe("profile.zip");

  // --- and taking it away again ------------------------------------------

  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await page.getByRole("button", { name: "Delete it" }).click();

  await expect(page).toHaveURL(/\/admin\/profiles$/);
  await expect(page.getByRole("link", { name, exact: true })).toHaveCount(0);
});

test("a profile with nothing in it says so rather than handing a device an empty package", async ({
  page,
}) => {
  const name = uniqueName("Empty profile");

  await gotoApp(page, "/admin/profiles");
  await page.getByLabel("Name").fill(name);
  await page.getByRole("button", { name: "Create profile" }).click();

  await page.getByRole("link", { name, exact: true }).click();
  await expect(page.getByRole("heading", { name })).toBeVisible();

  await page.getByRole("button", { name: "Preview package" }).click();

  // A `400` with something worth reading, rather than a zip containing
  // nothing — which is why the bytes are fetched before the browser is told to
  // save anything.
  await expect(
    page.getByText("has no preferences and no files", { exact: false }),
  ).toBeVisible();

  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await page.getByRole("button", { name: "Delete it" }).click();
  await expect(page).toHaveURL(/\/admin\/profiles$/);
});
