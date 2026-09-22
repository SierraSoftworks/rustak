/**
 * The Services page: the sidecars registered with this server.
 *
 * Driven against the demo fixtures (`?demo`) rather than against a live
 * sidecar. A real registration needs a service account, a minted service
 * token and a second process that heartbeats — three things this suite's
 * server has no way to produce from a browser — and none of them is what these
 * assertions are about. What they are about is the page: that a fleet with
 * something wrong in it sorts the wrong thing to the top, that a heartbeat's
 * `metrics` become a table rather than a wall of JSON, that a configuration
 * can be edited and saved, and that removing a registration takes its row
 * away. `rustak-server`'s own route tests cover the API underneath
 * (`web/api/services.rs`), and `rustak-ui`'s unit tests cover the renderer
 * (`pages/service_metrics.rs`); this is the layer where neither can see.
 *
 * The three fixtures are in `rustak-ui/src/fixtures/services.rs`: a healthy
 * AIS feed, a degraded ADS-B feed, and an example sidecar that stopped
 * reporting.
 */

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

/** Where the page lives. The path predates the name, and bookmarks outlive both. */
const SERVICES = "/admin/settings/add-ons?demo";

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);
});

test("the list puts what needs attention above what does not", async ({ page }) => {
  await gotoApp(page, SERVICES);

  await expect(page.getByRole("heading", { name: "Services", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Registered services" })).toBeVisible();

  // Worst first, healthy last — not the order they registered in, which is the
  // order the fixtures are written in.
  await expect(page.locator(".service-row__name")).toHaveText([
    "Example sidecar",
    "ADS-B feed",
    "AIS feed",
  ]);
  await expect(page.locator(".service-row .status-pill")).toHaveText([
    "Unhealthy",
    "Degraded",
    "Healthy",
  ]);

  // The pill is not the only signal: the two rows asking for something carry
  // the rule down their edge as well, so the state does not depend on colour.
  await expect(page.locator(".service-row--attention")).toHaveCount(2);

  // Each row says what it is, what version, and when it last reported.
  const adsb = page.locator(".service-row", { hasText: "ADS-B feed" });
  await expect(adsb.locator(".service-row__id")).toHaveText("rustak-plugin-adsb");
  await expect(adsb.getByText("Heartbeat", { exact: false })).toBeVisible();
  await expect(adsb.getByText("The upstream feed has not answered", { exact: false })).toBeVisible();
  // `config.validate` is what every sidecar on the harness advertises: it can
  // be asked about a candidate configuration before one is stored.
  await expect(adsb.locator(".tag")).toHaveText(["cot.publish", "feed.adsb", "config.validate"]);
});

test("a service's detail shows its endpoints and its metrics as a table", async ({ page }) => {
  await gotoApp(page, SERVICES);

  await page.locator(".service-row__select", { hasText: "AIS feed" }).click();
  await expect(page.getByRole("heading", { name: "AIS feed" })).toBeVisible();

  // What the sidecar says it reached us on. Reported rather than assigned, so
  // a plugin pointed at the wrong address shows the wrong address.
  await expect(page.getByText("Stream endpoint", { exact: true })).toBeVisible();
  await expect(page.getByText("ssl://rustak:8089", { exact: true })).toBeVisible();
  await expect(page.getByText("Control endpoint", { exact: true })).toBeVisible();

  const metrics = page.locator(".metrics");
  await expect(metrics).toBeVisible();
  await expect(metrics.getByText("offered", { exact: true })).toBeVisible();
  await expect(metrics.getByText("18422", { exact: true })).toBeVisible();
  await expect(metrics.getByText("tracked", { exact: true })).toBeVisible();

  // A nested object is a heading with its own fields under it, not a blob.
  await expect(metrics.getByText("source", { exact: true })).toBeVisible();
  await expect(metrics.getByText("aisstream", { exact: true })).toBeVisible();
  await expect(metrics.getByText("connected", { exact: true })).toBeVisible();

  // Which is the point: no braces reach the page.
  await expect(metrics).not.toContainText("{");
});

test("an administrator edits a service's configuration and is told it was saved", async ({
  page,
}) => {
  await gotoApp(page, SERVICES);
  await page.locator(".service-row__select", { hasText: "AIS feed" }).click();

  const editor = page.locator("#service-config");
  await expect(editor).toHaveValue(/interval_seconds/);

  const save = page.getByRole("button", { name: "Save configuration" });
  // Nothing has changed yet, so there is nothing to save.
  await expect(save).toBeDisabled();

  // What the server would refuse is refused here, before the request.
  await editor.fill("[1, 2, 3]");
  await expect(page.getByText("has to be a JSON object", { exact: false })).toBeVisible();
  await expect(save).toBeDisabled();

  await editor.fill('{\n  "interval_seconds": 45\n}');
  await expect(save).toBeEnabled();
  await save.click();

  await expect(page.getByText("Saved. The service picks it up on its next tick.")).toBeVisible();
  await expect(save).toBeDisabled();
  await expect(editor).toHaveValue(/45/);
});

test("a service that registered a schema is configured through a form and asked before a save", async ({
  page,
}) => {
  await gotoApp(page, SERVICES);
  await page.locator(".service-row__select", { hasText: "ADS-B feed" }).click();

  // The schema names the keys and the stored document fills them: no JSON box.
  const radius = page.locator("#service-config-area-radius_km");
  await expect(radius).toHaveValue("120");
  await expect(page.locator("#service-config")).toHaveCount(0);
  // A plugin's doc comment is the field's help text.
  await expect(page.getByText("Radius in kilometres.")).toBeVisible();

  const save = page.getByRole("button", { name: "Save configuration" });
  await expect(save).toBeDisabled();

  // What a schema cannot say is the running service's to refuse, and nothing
  // is stored when it does.
  await radius.fill("0");
  await save.click();
  await expect(page.getByText("A circle needs a radius greater than zero.").first()).toBeVisible();
  await expect(page.getByText("Saved. The service picks it up on its next tick.")).toHaveCount(0);

  await radius.fill("80");
  await save.click();
  await expect(page.getByText("Saved. The service picks it up on its next tick.")).toBeVisible();
  await expect(page.getByText("The running service checked it", { exact: false })).toBeVisible();

  // A tagged union is a picker, and the JSON is one click away.
  await page.locator("#service-config-area-variant").selectOption({ label: "Bbox" });
  await expect(page.locator("#service-config-area-south")).toBeVisible();
  await page.getByRole("button", { name: "Edit as JSON" }).click();
  await expect(page.locator("#service-config")).toHaveValue(/"kind": "bbox"/);
});

test("removing a registration takes its row off the list", async ({ page }) => {
  await gotoApp(page, SERVICES);

  await page.locator(".service-row__select", { hasText: "Example sidecar" }).click();
  await expect(page.getByRole("heading", { name: "Example sidecar" })).toBeVisible();

  await page.getByRole("button", { name: "Remove", exact: true }).click();
  // The question names what is about to happen to what, in place.
  await expect(page.getByText("Remove the registration for", { exact: false })).toBeVisible();
  await page.getByRole("button", { name: "Remove it" }).click();

  await expect(page.locator(".service-row__name")).toHaveText(["ADS-B feed", "AIS feed"]);
  // The drawer closes with it, rather than describing something that has gone.
  await expect(page.getByRole("heading", { name: "Example sidecar" })).toHaveCount(0);
});
