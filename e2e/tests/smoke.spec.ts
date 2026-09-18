/**
 * The smallest possible proof that the server and the embedded UI are both
 * genuinely working, end to end.
 *
 * Everything else the e2e suite will eventually cover — setup wizard,
 * sign-in, device/credential/mission management — depends on both of these
 * holding, so this spec is deliberately the only one M0 ships. It exists to
 * catch the two failure modes design 01's risk table calls out by name: the
 * UI bundle not being embedded (`rustak-server` built before `trunk build`,
 * §9 "`include_dir!` compile-time coupling") and the server not routing at
 * all.
 */

import { test, expect, gotoApp } from "./helpers";

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

test("the application boots and renders", async ({ page }) => {
  // A build where `rustak-server` was compiled against an empty
  // `rustak-ui/dist` still starts and still answers `/robots.txt`, but `GET /`
  // comes back 500 — `gotoApp`'s `waitForApp` would simply time out in that
  // case, which is the signal this test exists to catch.
  await gotoApp(page, "/");

  await expect(page.locator("body")).not.toContainText("500");
  await expect(page).toHaveTitle(/rustak/i);
});
