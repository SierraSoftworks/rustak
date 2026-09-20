/**
 * Signing in, failing to sign in, and signing out.
 *
 * rustak has no local passwords — `[auth] local_login` was removed from the
 * design on purpose — so a passkey is the whole of the sign-in story for an
 * installation with no identity provider. These tests drive the real ceremony
 * in a real browser against a software CTAP2 authenticator, so what they
 * exercise is WebAuthn's own origin binding, user verification and signature
 * counter rather than a stand-in for them.
 *
 * The two failure paths matter as much as the success: a passkey's value is
 * that it *cannot* be used anywhere but the host it was registered against, and
 * that a failed attempt says nothing about whether the account exists.
 */

import {
  ADMIN,
  attachAuthenticator,
  bootstrapAdmin,
  cacheSession,
  expect,
  gotoApp,
  registerPasskey,
  signIn,
  storedSession,
  test,
  uniqueName,
  waitForApp,
} from "./helpers";

test("a browser holding no passkey for this server cannot sign in, and is not told why", async ({
  page,
}) => {
  // An authenticator with nothing in it: the browser will find no credential
  // for this relying party and refuse the ceremony.
  await attachAuthenticator(page);

  await gotoApp(page, "/admin");
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();

  await page.getByRole("button", { name: "Sign in with a passkey" }).click();

  // One message for every failure. The browser deliberately reports "cancelled"
  // and "no matching credential" identically, because telling them apart would
  // say whether an account has a passkey; the UI does not undo that by guessing.
  //
  // The refusal is shown on the sign-in page itself, beside the buttons, rather
  // than replacing the view with a session-check error: `ec7320f` moved it there
  // on purpose, and the new title is the more accurate of the two — nothing here
  // has a session to check yet, because the ceremony never completed.
  await expect(page.getByText("The sign-in could not be completed")).toBeVisible();
  await expect(page.getByText("may have been dismissed")).toBeVisible();
});

test("a passkey registered for one host is refused at another", async ({ page, baseURL }) => {
  const session = await bootstrapAdmin(page);
  await attachAuthenticator(page);

  // Registered against `localhost`, which is what `[server] base_url` makes the
  // relying party. Deliberately *without* signing the page in: the credential
  // has to exist, and a stored session at the other origin would be answering a
  // different question.
  await page.goto("/");
  const registered = await registerPasskey(page, {
    label: uniqueName("Wrong origin"),
    token: session.token,
  });
  expect(
    registered.status,
    `the passkey registration should have succeeded: ${JSON.stringify(registered.body)}`,
  ).toBe(200);

  // The same server, the same credential, the same authenticator — only the
  // name in the address bar differs. `127.0.0.1` is not a registrable suffix of
  // `localhost`, so the browser refuses before the server is ever asked to
  // verify anything. That refusal is what makes a passkey unphishable.
  const elsewhere = new URL(baseURL!);
  elsewhere.hostname = "127.0.0.1";
  elsewhere.pathname = "/admin";

  await page.goto(elsewhere.toString());
  await waitForApp(page);

  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
  await page.getByRole("button", { name: "Sign in with a passkey" }).click();

  await expect(page.getByText("The sign-in could not be completed")).toBeVisible();

  // The *same* wording as the empty-authenticator case above, asserted here too:
  // "one message for every failure" is only true if both failures produce it, and
  // a wrong-origin refusal that said so specifically would confirm the credential
  // exists — which is the property this test is named for.
  await expect(page.getByText("may have been dismissed")).toBeVisible();

  expect(await storedSession(page)).toBeUndefined();
});

test("an administrator signs in with a passkey, and signing out ends the session", async ({
  page,
}) => {
  const session = await bootstrapAdmin(page);
  const authenticator = await attachAuthenticator(page);

  await signIn(page, session);
  await gotoApp(page, "/admin");
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();

  // The passkey the wizard registered lives in a browser profile that has
  // already gone, so this profile registers its own — which is also the thing
  // `/admin/settings/account` exists to let somebody do before they lose their
  // first.
  const registered = await registerPasskey(page, {
    label: uniqueName("This device"),
    token: session.token,
  });
  expect(
    registered.status,
    `the passkey registration should have succeeded: ${JSON.stringify(registered.body)}`,
  ).toBe(200);

  // Nothing is done to the credential between registering it and using it. The
  // server asks for `residentKey: "required"`, so the authenticator stored it
  // and the browser can offer it to a ceremony that names nobody — which is the
  // only kind the sign-in prompt runs.
  const held = await authenticator.credentials();
  expect(held.length, "the registration should have produced a credential").toBeGreaterThan(0);
  expect(
    held.every((credential) => credential.isResidentCredential),
    "every passkey this server registers has to be discoverable",
  ).toBe(true);

  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
  expect(
    await storedSession(page),
    "signing out should leave nothing in sessionStorage to restore",
  ).toBeUndefined();

  // Nothing was typed: the account is named by the authenticator, not by the
  // person, which is what stops this prompt being a way to ask the server which
  // accounts exist.
  await page.getByRole("button", { name: "Sign in with a passkey" }).click();
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();

  const renewed = await storedSession(page);
  expect(renewed, "the ceremony should have established a session").toBeTruthy();

  // Signing out revoked the session every other spec was going to use, and the
  // passkey that could mint another is in this profile. Hand the fresh one on.
  cacheSession(renewed!);
});

test("a passkey the browser cannot offer on its own is reached by naming the account", async ({
  page,
}) => {
  // The fallback, and the one case it exists for. Everything rustak registers
  // now is discoverable, so the only way to produce a credential the prompt
  // cannot find is to make one: a key registered against another server, an
  // authenticator that ignored `residentKey`, or anything from a version of
  // rustak that asked for `discouraged` would all look like this.
  const session = await bootstrapAdmin(page);
  const authenticator = await attachAuthenticator(page);

  await signIn(page, session);
  await gotoApp(page, "/admin");
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();

  const registered = await registerPasskey(page, {
    label: uniqueName("Not discoverable"),
    token: session.token,
  });
  expect(
    registered.status,
    `the passkey registration should have succeeded: ${JSON.stringify(registered.body)}`,
  ).toBe(200);

  expect(
    await authenticator.makeCredentialsUndiscoverable(),
    "there should have been a discoverable credential to convert",
  ).toBeGreaterThan(0);

  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();

  // Folded away by default, so that a username field is never the first thing
  // this page offers — naming an account before proving anything is how
  // somebody would find out which accounts exist.
  await expect(page.getByLabel("Username")).toHaveCount(0);
  await page.getByRole("button", { name: "Sign in with a username instead" }).click();

  await page.getByLabel("Username").fill(ADMIN.username);
  await page.getByRole("button", { name: "Sign in as this account" }).click();

  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();

  const renewed = await storedSession(page);
  expect(renewed, "the named ceremony should have established a session").toBeTruthy();
  cacheSession(renewed!);
});
