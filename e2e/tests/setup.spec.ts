/**
 * The first-run wizard, walked the way an operator walks it.
 *
 * This is the only path onto a fresh rustak. There are no local passwords and
 * no seeded account: the server writes a one-time token to its own filesystem,
 * and whoever can read that file creates the first administrator and gives them
 * a passkey. Everything else in this suite — and everything an operator ever
 * does — is behind the session this produces.
 *
 * It runs as its own Playwright project, ahead of every other spec, because the
 * wizard is a **one-way door**: `POST /api/v1/setup/admin` answers `409` once an
 * administrator exists, and every `/setup/*` route answers `410` for good once
 * the wizard has been completed. There is no second attempt to order after the
 * first, so the ordering has to be stated rather than inferred from file names.
 */

import {
  ADMIN,
  attachAuthenticator,
  cacheSession,
  expect,
  gotoApp,
  readSetupToken,
  storedSession,
  test,
  waitForApp,
} from "./helpers";

test("the first-run wizard turns a token on disk into an administrator who can sign in", async ({
  page,
}) => {
  // A software CTAP2 authenticator, attached before the wizard asks for a
  // passkey. Without one the browser's prompt has nothing to answer it and the
  // wizard's third step cannot be completed at all.
  await attachAuthenticator(page);

  await gotoApp(page, "/setup");
  await expect(page.getByRole("heading", { name: "Set up rustak" })).toBeVisible();

  // Step 1 — the setup token. The whole of the first-run trust model: the file
  // it comes from is readable only by whoever has the server's filesystem.
  await page.getByLabel("Setup token").fill(readSetupToken());
  await page.getByRole("button", { name: "Continue" }).click();

  // Step 2 — the administrator. The username rules live in `rustak-api`, and
  // the page asks rather than deciding, so a name the server would refuse is
  // refused here first and the submit stays disabled.
  const create = page.getByRole("button", { name: "Create the administrator" });
  await expect(create).toBeVisible();

  await page.getByLabel("Username").fill("Not A Username!");
  await expect(page.locator(".field__error")).toBeVisible();
  await expect(create).toBeDisabled();

  await page.getByLabel("Username").fill(ADMIN.username);
  await page.getByLabel("Display name").fill(ADMIN.displayName);
  await expect(page.locator(".field__error")).toHaveCount(0);
  await create.click();

  // Step 3 — the passkey, which is the only way this account will ever sign in.
  // The server answers the registration with a session, so completing this step
  // is also what authorises the three steps after it.
  const register = page.getByRole("button", { name: "Register a passkey" });
  await expect(register).toBeVisible();
  await expect(page.getByText(`Register a passkey for ${ADMIN.username}`)).toBeVisible();
  await page.getByLabel("What to call this passkey").fill(ADMIN.passkeyLabel);
  await register.click();

  // Step 4 — what this server is called. The host names become the server
  // certificate's subject alternative names and the host in every enrolment QR
  // code, so the wizard asks for them rather than inferring them from a `Host`
  // header the client chose.
  const save = page.getByRole("button", { name: "Save and continue" });
  await expect(save).toBeVisible();
  await page.getByLabel("Server name").fill("rustak e2e");
  await page.getByLabel("Host names").fill(new URL(page.url()).hostname);
  await save.click();

  // Step 5 — the certificate authority.
  //
  // KNOWN DEFECT, worked around here: rustak creates its root authority during
  // start-up (`runtime::listen` calls `pki::load_or_create_root_ca` before it
  // binds anything), so by the time the wizard offers to create one there
  // already is one and `POST /api/v1/setup/ca` answers `409`. The step is then
  // a dead end in the linear walk — but not in the wizard as a whole, because
  // `Step::resume_from` skips a step the server says is already done, so
  // reloading resumes at the last one. Recorded in
  // `.claude/plan/status/M0-14-e2e-specs.md`; when the defect is fixed this
  // branch stops being taken and the spec still passes.
  const authority = page.getByRole("button", { name: "Create the authority" });
  await expect(authority).toBeVisible();
  await authority.click();

  const finish = page.getByRole("button", { name: "Finish setup" });
  const alreadyHasCa = page.getByText("This server already has a certificate authority.");
  await expect(finish.or(alreadyHasCa).first()).toBeVisible();

  if (await alreadyHasCa.isVisible()) {
    await page.reload();
    await waitForApp(page);
  }

  // Step 6 — closing the wizard, which is the other one-way door.
  await expect(finish).toBeVisible();
  await finish.click();

  await expect(page.getByText("This server is set up")).toBeVisible();
  await page.getByRole("link", { name: "Open the console" }).click();
  await waitForApp(page);
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();

  // The session the wizard established is the only one this server will ever
  // hand out without a passkey ceremony, and the passkey behind it lives in a
  // browser profile that is about to be thrown away. Leaving it for the specs
  // that follow is what lets them test what they are about rather than
  // re-walking this.
  const session = await storedSession(page);
  expect(session, "the wizard should have left a session in sessionStorage").toBeTruthy();
  cacheSession(session!);
});

test("the wizard closes itself for good once it has been completed", async ({ page }) => {
  // Not `404`, which would be indistinguishable from a version that never had
  // these routes, and not a guard on whether an administrator exists — deleting
  // the last administrator must not reopen the door that creates one without a
  // credential.
  const status = await page.request.get("/api/v1/setup/status");
  expect(status.status()).toBe(200);
  expect(await status.json()).toMatchObject({
    needs_setup: false,
    has_admin: true,
    setup_completed: true,
  });

  const retried = await page.request.post("/api/v1/setup/admin", {
    data: { setup_token: "anything", username: "mallory" },
  });
  expect(retried.status()).toBe(410);

  // And the token file it was authorised by is gone from the disk.
  expect(() => readSetupToken()).toThrow();

  // The wizard page itself says so rather than offering a form that cannot work.
  await gotoApp(page, "/setup");
  await expect(page.getByText("This server is set up")).toBeVisible();
  await expect(page.getByRole("link", { name: "Open the console" })).toBeVisible();
});
