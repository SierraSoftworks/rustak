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
  await expect(page.getByText("We could not check your session")).toBeVisible();
  await expect(page.getByText("may have been dismissed")).toBeVisible();
});

test("a passkey registered for one host is refused at another", async ({ page, baseURL }) => {
  const session = await bootstrapAdmin(page);
  const authenticator = await attachAuthenticator(page);

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
  await authenticator.makeCredentialsDiscoverable();

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

  await expect(page.getByText("We could not check your session")).toBeVisible();
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
  // `/admin/settings` exists to let somebody do before they lose their first.
  const registered = await registerPasskey(page, {
    label: uniqueName("This device"),
    token: session.token,
  });
  expect(
    registered.status,
    `the passkey registration should have succeeded: ${JSON.stringify(registered.body)}`,
  ).toBe(200);

  // KNOWN DEFECT, worked around here: the server registers passkeys with
  // `residentKey: "discouraged"`, so the credential is not discoverable — while
  // the sign-in prompt only ever runs a *discoverable* ceremony, because it
  // never asks for a username. A real authenticator that happened to store the
  // credential anyway (most platform ones do) would work; this one honours the
  // flag, so the credential is re-stored as discoverable to match. Recorded in
  // `.claude/plan/status/M0-14-e2e-specs.md`.
  expect(
    await authenticator.makeCredentialsDiscoverable(),
    "the freshly registered passkey should have been non-discoverable",
  ).toBeGreaterThan(0);

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
