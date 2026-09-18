/**
 * The smallest possible proof that the server and the embedded UI are both
 * genuinely working, end to end.
 *
 * Everything else the e2e suite covers — the setup wizard, passkey sign-in,
 * navigation — depends on both of these holding, so when this file fails
 * nothing else in the suite is worth reading. It exists to catch the two
 * failure modes design 01's risk table calls out by name: the UI bundle not
 * being embedded (`rustak-server` built before `trunk build`, §9 "`include_dir!`
 * compile-time coupling") and the server not routing at all.
 */

import { expect, gotoApp, test } from "./helpers";

test("robots.txt is served before the SPA catch-all", async ({ request, baseURL }) => {
  // `/robots.txt` is registered ahead of the default SPA route (design 01
  // §6.1), so a 200 here proves the server is genuinely routing rather than
  // just answering every path with `index.html`. This is also exactly what
  // `playwright.config.ts`'s `webServer.url` polls for readiness.
  const response = await request.get(`${baseURL}/robots.txt`);
  expect(response.status()).toBe(200);

  const body = await response.text();
  expect(body.toLowerCase()).toContain("user-agent");
});

test("the API reports its own health, and says nothing about the storage behind it", async ({
  request,
}) => {
  // Public, and answered from a real reader connection — so a 200 is a
  // statement about the database as well as about the listener. It deliberately
  // does not describe what the storage is or where it lives.
  const response = await request.get("/api/v1/health");
  expect(response.status()).toBe(200);
  expect(response.headers()["content-type"]).toBe("application/json");

  const body = (await response.json()) as Record<string, unknown>;
  expect(body).toHaveProperty("status");
  expect(JSON.stringify(body)).not.toContain("sqlite");
});

test("the application boots and renders", async ({ page }) => {
  // A build where `rustak-server` was compiled against an empty
  // `rustak-ui/dist` still starts and still answers `/robots.txt`, but `GET /`
  // comes back 500 — `gotoApp`'s `waitForApp` would simply time out in that
  // case, which is the signal this test exists to catch.
  await gotoApp(page, "/");

  await expect(page.locator("body")).not.toContainText("500");
  await expect(page).toHaveTitle(/rustak/i);
});
