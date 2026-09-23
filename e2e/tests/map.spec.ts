/**
 * The map: what is reporting, drawn where it says it is.
 *
 * Driven against the demo fixtures (`?demo`), for the reason `live.spec.ts`
 * gives: putting a real track on a real stream needs an EUD, and that belongs
 * to the interop suite. What is tested here is the page — that the map
 * libraries load from this server, that what the API returns reaches the map,
 * and that the object list, the properties panel and the tools work — and the
 * fixtures are a snapshot and a feed like any other as far as the page can
 * tell.
 *
 * A WebGL canvas has nothing in it for a test to find, so the assertions go
 * through what the page offers anybody who cannot use one either: the object
 * list over the map, and the count the map publishes on its own element.
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

  await expect(page.getByRole("heading", { name: "On the map" })).toBeVisible();
  await expect(page.getByText("Live", { exact: true })).toBeVisible();
  // The map is the page: it is still headed, but nothing is drawn above it.
  await expect(page.getByRole("heading", { name: "Map", exact: true })).toBeAttached();
  await expect(page.locator(".page-title")).toHaveCount(0);

  // Set by the map itself once the features have reached it, so this is the
  // libraries having loaded from /vendor and the data having crossed into them.
  await expect(page.getByRole("application")).toHaveAttribute("data-features", /^[1-9]\d*$/);
  await expect(page.locator(".map-page__canvas canvas")).toBeVisible();
});

test("picking something from the list opens its properties beside the map", async ({ page }) => {
  await gotoApp(page, MAP);

  // Grouped by what things are, and the group says so.
  await expect(page.getByRole("button", { name: /a-f-G.*Friendly · Ground/ })).toBeVisible();
  await page.getByRole("button", { name: /^RAO/ }).click();

  const details = page.getByRole("article", { name: "Details for RAO" });
  await expect(details).toBeVisible();
  await expect(details.getByText("Friendly · Ground")).toBeVisible();
  await expect(details.getByText("Cyan · Team Member")).toBeVisible();
  // What only the server knows, and the first thing to check when somebody
  // says they cannot see a marker.
  await expect(details.getByText("Blue Team", { exact: false })).toBeVisible();
  // A device's own report is not something the console edits.
  await expect(details.getByLabel("Name")).toHaveCount(0);

  // Straight to somebody else, without closing the first.
  await page.getByRole("button", { name: /^OKAFOR/ }).click();
  const other = page.getByRole("article", { name: "Details for OKAFOR" });
  await expect(other).toBeVisible();
  await expect(details).toHaveCount(0);

  await other.getByRole("button", { name: "Close", exact: true }).click();
  await expect(other).toHaveCount(0);

  // A group folds away, and back.
  const ground = page.getByRole("button", { name: /a-f-G.*Friendly · Ground/ });
  await ground.click();
  await expect(page.getByRole("button", { name: /^RAO/ })).toHaveCount(0);
  await ground.click();
  await expect(page.getByRole("button", { name: /^RAO/ })).toBeVisible();
});

test("the pin tool places a marker that can be edited and deleted", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  await gotoApp(page, MAP);
  const application = page.getByRole("application");
  await expect(application).toHaveAttribute("data-features", /^[1-9]\d*$/);
  const before = Number(await application.getAttribute("data-features"));

  await page.getByRole("button", { name: "Place a marker" }).click();
  await expect(page.getByText("Click the map to place a marker.")).toBeVisible();

  const canvas = page.locator(".map-page__canvas canvas");
  const box = (await canvas.boundingBox())!;
  await canvas.click({ position: { x: box.width * 0.55, y: box.height * 0.62 } });

  // Placed, on the map and in the list, and open for editing.
  const details = page.getByRole("article", { name: /^Details for Marker \d+$/ });
  await expect(details).toBeVisible();
  await expect(application).toHaveAttribute("data-features", String(before + 1));
  await expect(page.getByRole("button", { name: /b-m-p.*Markers/ })).toBeVisible();
  // The tool hands back to select.
  await expect(page.getByRole("button", { name: "Place a marker" })).toHaveAttribute("aria-pressed", "false");

  await details.getByLabel("Name").fill("CCP SOUTH");

  // What it is, found by a word rather than remembered as a code; then whose,
  // which a spot marker has none of until it is something that can have one.
  await expect(details.getByLabel("Affiliation")).toBeDisabled();
  await details.getByLabel("Type").click();
  await details.getByRole("combobox", { name: "Search" }).fill("ground vehicle");
  await details.getByRole("option", { name: /^Ground vehicle/ }).first().click();
  await details.getByLabel("Affiliation").selectOption({ label: "Hostile" });
  await expect(details.getByLabel("Type")).toContainText("a-h-G-E-V");

  await details.getByLabel("Remarks").fill("Two vehicles, stationary.");
  await details.getByRole("button", { name: "Save" }).click();

  const renamed = page.getByRole("article", { name: "Details for CCP SOUTH" });
  await expect(renamed).toBeVisible();
  await expect(renamed.getByText("Hostile · Ground")).toBeVisible();
  await expect(page.getByRole("button", { name: /^CCP SOUTH/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /a-h-G.*Hostile · Ground/ })).toBeVisible();

  await renamed.getByRole("button", { name: "Delete" }).click();
  await renamed.getByRole("button", { name: "Delete it" }).click();
  await expect(renamed).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^CCP SOUTH/ })).toHaveCount(0);
  await expect(application).toHaveAttribute("data-features", String(before));
});

test("selecting something shows where it has been, and its past can be scrubbed", async ({ page }) => {
  await gotoApp(page, MAP);

  const application = page.getByRole("application");
  await expect(application).toHaveAttribute("data-features", /^[1-9]\d*$/);
  const live = (await application.getAttribute("data-features"))!;

  // Quinn has been walking for half an hour, in the fixtures.
  await page.getByRole("button", { name: /^QUINN/ }).click();
  const playback = page.getByRole("region", { name: "Track playback" });
  await expect(playback).toBeVisible();
  await expect(playback).toContainText(/\d+ fixes over/);
  await expect(playback.getByRole("button", { name: "Live" })).toBeDisabled();

  // Scrubbing back to the start shows Quinn where they were then, and
  // nothing else: nothing else's past has been read.
  await playback.getByRole("slider").fill("0");
  await expect(application).toHaveAttribute("data-features", "1");
  await expect(page.getByText(/^Showing where QUINN was at/)).toBeVisible();
  await expect(page.getByRole("article", { name: "Details for QUINN" })).toBeVisible();

  // Nothing is edited while a moment from the past is shown.
  await expect(page.getByLabel("Name")).toHaveCount(0);

  // Back to live: everything returns.
  await playback.getByRole("button", { name: "Live" }).click();
  await expect(application).toHaveAttribute("data-features", live);
  await expect(page.getByText(/^Showing where QUINN was at/)).toHaveCount(0);

  // Closing the panel takes the track away.
  await page.getByRole("article", { name: "Details for QUINN" }).getByRole("button", { name: "Close", exact: true }).click();
  await expect(playback).toHaveCount(0);
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
  // the two are under the same pixel at every zoom. The list puts that pixel
  // in the middle of the map, which is the one place a test can find it.
  await page.getByRole("button", { name: /^CCP NORTH/ }).click();
  const details = page.getByRole("article", { name: "Details for CCP NORTH" });
  await expect(details).toBeVisible();
  await details.getByRole("button", { name: "Close", exact: true }).click();
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

test("a type and a symbol are chosen by name, with a picture of each, and never typed as a code", async ({
  page,
}) => {
  await gotoApp(page, MAP);

  await page.getByRole("button", { name: "Place a marker" }).click();
  const canvas = page.locator(".map-page__canvas canvas");
  const box = (await canvas.boundingBox())!;
  await canvas.click({ position: { x: box.width * 0.45, y: box.height * 0.4 } });

  const details = page.getByRole("article", { name: /^Details for Marker \d+$/ });
  const type = details.getByLabel("Type");
  const search = details.getByRole("combobox", { name: "Search" });
  await expect(type).toContainText("Spot marker");

  // It opens where what is chosen lives, with the trail saying where that is
  // and every step of it a way back up.
  await type.click();
  const trail = details.getByRole("navigation", { name: "Where this list is" });
  await expect(trail).toContainText("Markers");
  await expect(details.getByRole("option", { name: /^Spot marker/ })).toHaveAttribute("aria-selected", "true");

  // Walked: one level at a time, by the pointer or by the keys.
  await trail.getByRole("button", { name: "All" }).click();
  await expect(details.getByRole("option", { name: /^Ground track a-u-G\b/ })).toBeVisible();
  await details.getByRole("button", { name: "Show what is inside Air track" }).click();
  await expect(trail).toContainText("Air track");
  await search.press("ArrowLeft");
  await expect(details.getByRole("option", { name: /^Markers/ })).toBeVisible();

  // Searched: by a word, from anywhere in the hierarchy, with where it lives
  // beside it and the symbol it will be drawn as in front of it.
  await search.fill("mortar heavy");
  const mortar = details.getByRole("option", { name: /^Mortar heavy/ });
  await expect(mortar).toContainText("Ground track equipment");
  await expect(mortar.locator("img")).toHaveAttribute("src", /^data:image\/svg\+xml/);
  await search.press("Enter");

  // Chosen, the panel is gone and the field says what it holds.
  await expect(search).toHaveCount(0);
  await expect(type).toContainText("a-u-G-E-W-O-H");

  // Whose it is redraws every preview, and rewrites a symbol already chosen.
  await details.getByLabel("Affiliation").selectOption({ label: "Friendly" });
  await expect(type).toContainText("a-f-G-E-W-O-H");

  const symbol = details.getByLabel("Symbol");
  await expect(symbol).toContainText("The type's own symbol");
  await symbol.click();
  await search.fill("mortar medium");
  await details.getByRole("option", { name: /^Mortar medium/ }).click();
  await expect(symbol.locator("code")).toHaveText(/^SF/);
  await details.getByLabel("Affiliation").selectOption({ label: "Hostile" });
  await expect(symbol.locator("code")).toHaveText(/^SH/);

  // Something no catalogue lists can still be said, once it is well formed.
  await type.click();
  await search.fill("a-h-G-X-Y-Z");
  await details.getByRole("option", { name: /Use a-h-G-X-Y-Z as typed/ }).click();
  await expect(type).toContainText("a-h-G-X-Y-Z");

  await details.getByRole("button", { name: "Delete" }).click();
  await details.getByRole("button", { name: "Delete it" }).click();
});
