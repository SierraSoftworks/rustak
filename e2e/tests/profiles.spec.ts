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
 * - **A known preference is edited with its own control, and sticks.** The
 *   catalogue the server serves is a JSON Schema, drawn with the form the
 *   Services page uses; a value set through it survives a reload, which only
 *   a real save can make true.
 * - **A value is refused against its class before it is sent.** ATAK drops a
 *   preference whose value does not match its `class` attribute, so the editor
 *   checks a free key's value with the same predicate the server validates
 *   with.
 */

import type { Page } from "@playwright/test";

import {
  bootstrapAdmin,
  expect,
  gotoApp,
  signIn,
  test,
  uniqueName,
  waitForApp,
} from "./helpers";

/** Adds the catalogued preference called `title`, found by searching for it. */
async function choosePreference(page: Page, title: string): Promise<void> {
  await page.locator("#pref-add").click();
  await page.getByRole("combobox", { name: "Search" }).fill(title);
  // A search result also says where it lives and what its key is, so the
  // name is matched as part of the option's text.
  await page.getByRole("option", { name: title }).click();
}

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

  // --- a preference the catalogue knows ----------------------------------

  const save = page.getByRole("button", { name: "Save preferences" });

  // Chosen from the catalogue by searching for it, and edited with the
  // control its schema calls for: a picker, because it takes one of two
  // values. Its class comes with it and is shown rather than asked for.
  await choosePreference(page, "Reporting strategy");
  const strategy = page.locator("#pref-0-value");
  await expect(strategy.locator("option:checked")).toHaveText("Dynamic");
  await expect(page.getByText("ATAK's own default is Dynamic.", { exact: false })).toBeVisible();
  await expect(page.getByText("sent as String", { exact: false })).toBeVisible();
  await strategy.selectOption({ label: "Constant" });

  // A boolean is a switch, and it starts at ATAK's own default.
  await choosePreference(page, "Fetch profiles on every connection");
  const onConnect = page.locator("#pref-1-value");
  await expect(onConnect).not.toBeChecked();
  // The switch input is a one-pixel transparent element behind the drawn
  // track, so the click goes to the field's own label, which the browser
  // forwards — the same gesture a person makes.
  await page.locator('label[for="pref-1-value"]').click();
  await expect(onConnect).toBeChecked();

  await save.click();
  await expect(page.getByText("Saved.", { exact: true })).toBeVisible();

  // Read back from the server, not from the page's own state.
  await page.reload();
  await waitForApp(page);
  await expect(page.locator("#pref-0-value option:checked")).toHaveText("Constant");
  await expect(page.locator("#pref-1-value")).toBeChecked();

  // --- a preference nobody catalogued ------------------------------------

  // A plugin's own key: key, class and value are all free, and a value its
  // class could not hold is refused before it is sent — ATAK drops an entry
  // whose value does not match its `class` attribute.
  await page.getByRole("button", { name: "Add another preference" }).click();
  await page.locator("#pref-2-key").fill("com.example.plugin.interval");
  await page.locator("#pref-2-class").selectOption("Integer");
  await page.locator("#pref-2-value").fill("soon");
  await expect(
    page.getByText("A Integer entry cannot hold 'soon'.", { exact: false }),
  ).toBeVisible();
  await expect(save).toBeDisabled();

  // Giving it a value that class *could* hold makes the list saveable again,
  // which is the proof that the refusal was about the value rather than about
  // having edited at all.
  await page.locator("#pref-2-value").fill("20");
  await expect(save).toBeEnabled();
  await save.click();
  await expect(page.getByText("Saved.", { exact: true })).toBeVisible();

  await page.reload();
  await waitForApp(page);
  await expect(page.locator("#pref-2-key")).toHaveValue("com.example.plugin.interval");
  await expect(page.locator("#pref-2-class")).toHaveValue("Integer");
  await expect(page.locator("#pref-2-value")).toHaveValue("20");
  await expect(page.locator("#pref-0-value option:checked")).toHaveText("Constant");

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

test("the enrolment defaults are drawn as toggles, not as disagreements with the catalogue", async ({
  page,
}) => {
  // The demo's enrolment profile holds what this server's own enrolment
  // profile sends: three boolean-valued keys as Strings holding "true", which
  // is how TAK Server and OpenTAKServer send `prefs_enable_channels` too. A
  // working wire form, so each is its key's toggle, still sent as a String.
  await gotoApp(page, "/admin/profiles/1?demo");

  for (const index of [0, 1, 2]) {
    const toggle = page.locator(`#pref-${index}-value`);
    await expect(toggle).toHaveAttribute("type", "checkbox");
    await expect(toggle).toBeChecked();
  }
  await expect(page.getByText("sent as String", { exact: false })).toHaveCount(4);
  await expect(page.getByText("The catalogue knows this key", { exact: false })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^Send it as/ })).toHaveCount(0);
});
