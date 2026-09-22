/**
 * The map: what is reporting, drawn where it says it is.
 *
 * Driven against the demo fixtures (`?demo`), for the reason `live.spec.ts`
 * gives: putting a real track on a real stream needs an EUD, and that belongs
 * to the interop suite. What is tested here is the page — that the map
 * libraries load from this server, that what the API returns reaches the map,
 * and that the roster and the pop-over work — and the fixtures are a snapshot
 * and a feed like any other as far as the page can tell.
 *
 * A WebGL canvas has nothing in it for a test to find, so the assertions go
 * through what the page offers anybody who cannot use one either: the roster
 * beside the map, and the count the map publishes on its own element.
 *
 * No test run fetches a tile. OpenStreetMap's tile server is a donated
 * resource with a usage policy, and the map draws everything rustak knows
 * about whether or not the base layer arrives — which is itself worth proving,
 * because that is the state an air-gapped installation is in.
 */

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

const MAP = "/admin/map?demo";

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);

  await page.route("https://tile.openstreetmap.org/**", (route) => route.abort());
});

test("the map draws what the server reports, with no base layer to draw it on", async ({ page }) => {
  await gotoApp(page, MAP);

  await expect(page.getByRole("heading", { name: "Map", exact: true })).toBeVisible();
  await expect(page.getByText("Live", { exact: true })).toBeVisible();

  // Set by the map itself once the features have reached it, so this is the
  // libraries having loaded from /vendor and the data having crossed into them.
  await expect(page.getByRole("application")).toHaveAttribute("data-features", /^[1-9]\d*$/);
  await expect(page.locator(".map-page__canvas canvas")).toBeVisible();
});

test("picking something from the roster opens its pop-over on the map", async ({ page }) => {
  await gotoApp(page, MAP);

  await page.getByRole("button", { name: /^RAO/ }).click();

  const details = page.getByRole("article", { name: "Details for RAO" });
  await expect(details).toBeVisible();
  await expect(details.getByText("Friendly · Ground")).toBeVisible();
  await expect(details.getByText("Cyan · Team Member")).toBeVisible();
  // What only the server knows, and the first thing to check when somebody
  // says they cannot see a marker.
  await expect(details.getByText("Blue Team", { exact: false })).toBeVisible();

  // Straight to somebody else, without closing the first: the pop-over moves
  // rather than shutting, which re-anchoring an open one used to do.
  await page.getByRole("button", { name: /^OKAFOR/ }).click();
  const other = page.getByRole("article", { name: "Details for OKAFOR" });
  await expect(other).toBeVisible();
  await expect(details).toHaveCount(0);

  await page.getByRole("button", { name: "Close popup" }).click();
  await expect(other).toHaveCount(0);
});

test("the roster is searched by callsign and by type", async ({ page }) => {
  await gotoApp(page, MAP);

  const search = page.getByPlaceholder("Search callsigns, uids or types");
  const rows = page.locator(".map-roster__row");

  await search.fill("clipper");
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText("THAMES CLIPPER");

  // A CoT type prefix: everything hostile, whatever it is called.
  await search.fill("a-h");
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText("CONTACT 1");

  await search.fill("nothing is called this");
  await expect(page.getByText("Nothing on the map matches.")).toBeVisible();
});

test("a click that lands on several things asks which one was meant", async ({ page }) => {
  // Tall enough that the pop-over fits above a point in the middle of the map,
  // so that opening it does not nudge the view and the middle stays the middle.
  await page.setViewportSize({ width: 1440, height: 1200 });
  await gotoApp(page, MAP);

  // The casualty collection point is also where the CASEVAC route starts, so
  // the two are under the same pixel at every zoom. The roster puts that pixel
  // in the middle of the map, which is the one place a test can find it.
  await page.getByRole("button", { name: /^CCP NORTH/ }).click();
  await expect(page.getByRole("article", { name: "Details for CCP NORTH" })).toBeVisible();
  await page.getByRole("button", { name: "Close popup" }).click();
  await page.waitForTimeout(1000);

  const canvas = page.locator(".map-page__canvas canvas");
  const box = (await canvas.boundingBox())!;
  await canvas.click({ position: { x: box.width / 2, y: box.height / 2 } });

  const chooser = page.getByRole("region", { name: "Choose what to look at" });
  await expect(chooser).toBeVisible();
  await expect(chooser.getByRole("button")).toHaveCount(2);
  await expect(chooser.getByRole("button", { name: /^CCP NORTH/ })).toBeVisible();

  await chooser.getByRole("button", { name: /^CASEVAC ROUTE/ }).click();

  await expect(page.getByRole("article", { name: "Details for CASEVAC ROUTE" })).toBeVisible();
  await expect(chooser).toHaveCount(0);
});

test("a symbol code the sender wrote is drawn, whichever edition it is in", async ({ page }) => {
  // The demo helicopter says which symbol it means in a 2525D number code, the
  // way a device set to 2525D does; everything else leaves it to its CoT type.
  // MapLibre says so when an image it asked for never arrived.
  const warnings: string[] = [];
  page.on("console", (message) => {
    if (message.text().includes("could not be loaded")) warnings.push(message.text());
  });

  await gotoApp(page, MAP);

  await expect(page.getByRole("application")).toHaveAttribute("data-features", /^[1-9]\d*$/);
  await page.waitForTimeout(1500);
  expect(warnings).toEqual([]);
});
