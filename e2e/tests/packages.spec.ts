/**
 * A data package, from a file on somebody's desktop to a row a client can
 * fetch.
 *
 * Three things this asserts that no unit test can:
 *
 * - **A multipart upload reaches the store.** The bytes travel as the
 *   browser's own `File` object inside a `FormData`, never through the wasm
 *   heap — and the request must leave `Content-Type` alone so the browser can
 *   write its own boundary. If any of that breaks, nothing but a real browser
 *   would notice.
 * - **The metadata beside the file arrives with it.** The name and the
 *   keywords are separate form fields, and `missionpackage` is the keyword
 *   that decides whether a client's own data-package browser lists the file at
 *   all.
 * - **A download works.** `GET /packages/{hash}/content` is behind the bearer
 *   token, so it cannot be a plain link: the body is fetched, turned into a
 *   blob and clicked at.
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

test("a package is uploaded, given a channel, downloaded and deleted", async ({ page }) => {
  const name = uniqueName("Package");

  await gotoApp(page, "/admin/packages");

  await page.getByLabel("Name").fill(name);
  await page.getByLabel("Keywords").fill("missionpackage, e2e");

  await page
    .locator("#package-upload")
    .setInputFiles({
      name: "brief.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("A briefing this test wrote.\n"),
    });

  await expect(page.getByText(`'${name}' was stored.`, { exact: false })).toBeVisible();

  // The list reloads from the server rather than pushing the row in locally,
  // so seeing it here means the package was genuinely written.
  const row = page.locator(".package-row").filter({ hasText: name });
  await expect(row).toHaveCount(1);

  // `missionpackage` is a keyword on the wire and an indexed column in the
  // database; the pill is rendered from the column, so seeing it means the two
  // agree.
  await expect(row).toContainText("Mission package");

  // An upload that names no channel lands in `__ANON__`, which everybody
  // holds — so a package is reachable the moment it is stored rather than
  // invisible until somebody remembers to give it one.
  await expect(row).toContainText("__ANON__");

  // --- the filter --------------------------------------------------------

  await page.getByLabel("Filter").fill("a name no package could have");
  await expect(page.locator(".package-row")).toHaveCount(0);
  await page.getByLabel("Filter").fill(name);
  await expect(page.locator(".package-row")).toHaveCount(1);

  // --- editing -----------------------------------------------------------

  await row.getByRole("button", { name: "Edit" }).click();

  // Scoped to the editor: the channel's name appears twice on an open row —
  // once in the metadata saying where the package is, and once as the switch
  // that puts it there.
  const editor = row.locator(".package-row__editor");

  // Each channel toggle is its own PATCH carrying the whole set, so the row
  // has to come back from the server with the change on it — and taking the
  // last channel away has to say so rather than looking like nothing happened.
  await editor.getByText("__ANON__", { exact: true }).click();
  await expect(row).toContainText("No channel");

  await editor.getByText("__ANON__", { exact: true }).click();
  await expect(row).not.toContainText("No channel");

  await editor.getByText("Install on enrolment", { exact: true }).click();
  await expect(row).toContainText("On enrolment");

  // --- the bytes ---------------------------------------------------------

  const downloading = page.waitForEvent("download");
  await row.getByRole("button", { name: "Download" }).click();
  const download = await downloading;
  expect(download.suggestedFilename()).toBe("brief.txt");

  // --- and taking it away ------------------------------------------------

  // The editor is still open: the toggle above is what opened it, and it says
  // "Close" now.
  await editor.getByRole("button", { name: "Delete", exact: true }).click();
  await editor.getByRole("button", { name: "Delete it" }).click();

  await expect(page.locator(".package-row").filter({ hasText: name })).toHaveCount(0);
});

test("a package upload with no file is not a request at all", async ({ page }) => {
  await gotoApp(page, "/admin/packages");

  // Nothing is sent until a file is chosen: the drop zone is the trigger, not
  // a separate Upload button that could be pressed with an empty form.
  await page.getByLabel("Name").fill(uniqueName("Never uploaded"));
  await expect(page.getByText("was stored.", { exact: false })).toHaveCount(0);
  await expect(page.getByText("could not be uploaded", { exact: false })).toHaveCount(0);
});
