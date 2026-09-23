/**
 * Small screens: a phone, a tablet held upright, a laptop with the window
 * halved.
 *
 * Two promises are tested. The navigation can be put away at any width — a
 * drawer on a phone, a column that folds beside the page anywhere wider — and
 * what is laid over the map is never drawn over anything else that is, whatever
 * the width, because the overlays are cells of one grid rather than things
 * pinned to corners that eventually meet.
 *
 * Driven against the demo fixtures, with no tile fetched, for the reasons
 * `map.spec.ts` gives.
 */

import type { Locator, Page } from "@playwright/test";

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

const MAP = "/admin/map?demo";
const PHONE = { width: 390, height: 844 };

interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);

  await page.route("https://tile.openstreetmap.org/**", (route) => route.abort());
});

async function box(locator: Locator): Promise<Box> {
  const found = await locator.boundingBox();
  expect(found, "the element is laid out").not.toBeNull();
  return found!;
}

/** Whether two boxes share no pixel. */
function apart(a: Box, b: Box): boolean {
  return a.x + a.width <= b.x || b.x + b.width <= a.x || a.y + a.height <= b.y || b.y + b.height <= a.y;
}

function overlays(page: Page) {
  return {
    toolbar: page.getByRole("toolbar", { name: "Map tools" }),
    list: page.getByRole("complementary", { name: "On the map" }),
    playback: page.getByRole("region", { name: "Track playback" }),
  };
}

test("the navigation folds away beside the page, and stays folded", async ({ page }) => {
  await gotoApp(page, MAP);

  const nav = page.getByRole("navigation", { name: "Admin sections" });
  const map = page.getByRole("application");
  await expect(nav).toBeVisible();
  const before = (await box(map)).width;

  await page.getByRole("button", { name: "Close the navigation" }).click();
  await expect(nav).toBeHidden();
  // The width goes to the page, and the map takes it.
  await expect.poll(async () => (await box(map)).width).toBeGreaterThan(before);

  // Remembered: somebody who wanted the width wants it next time too.
  await gotoApp(page, MAP);
  await expect(page.getByRole("application")).toBeVisible();
  await expect(nav).toBeHidden();

  await page.getByRole("button", { name: "Open the navigation" }).click();
  await expect(nav).toBeVisible();
});

test("on a phone the navigation is a drawer the same button opens and closes", async ({ page }) => {
  await page.setViewportSize(PHONE);
  await gotoApp(page, MAP);

  const nav = page.getByRole("navigation", { name: "Admin sections" });
  await expect(nav).toBeHidden();

  await page.getByRole("button", { name: "Open the navigation" }).click();
  await expect(nav).toBeVisible();

  await page.getByRole("button", { name: "Close the navigation" }).click();
  await expect(nav).toBeHidden();

  // Turned on its side, or the window let out: the navigation is a column
  // again, there without being asked for, and the button says it would fold it.
  await page.getByRole("button", { name: "Open the navigation" }).click();
  await page.setViewportSize({ width: 1280, height: 800 });
  await expect(nav).toBeVisible();
  await expect(page.getByRole("button", { name: "Close the navigation" })).toBeVisible();
});

for (const width of [1280, 1024, 768, 390]) {
  test(`at ${width}px the list is not drawn over the tools`, async ({ page }) => {
    await page.setViewportSize({ width, height: 844 });
    await gotoApp(page, MAP);

    const { toolbar, list } = overlays(page);
    await expect(toolbar).toBeVisible();
    // Drawn, and so past the point where the list decides how to start.
    await expect(page.getByRole("application")).toHaveAttribute("data-features", /^[1-9]\d*$/);

    // Open, which is the state with the most to overlap. Folded is how it
    // starts where the screen is narrow.
    const show = page.getByRole("button", { name: "Show the list" });
    if (await show.isVisible()) {
      await show.click();
    }
    await expect(page.getByRole("button", { name: /^RAO/ })).toBeVisible();

    const [tools, entities] = [await box(toolbar), await box(list)];
    expect(apart(tools, entities), JSON.stringify({ tools, entities })).toBe(true);

    // And neither runs off the side of the screen.
    for (const found of [tools, entities]) {
      expect(found.x).toBeGreaterThanOrEqual(0);
      expect(found.x + found.width).toBeLessThanOrEqual(width);
    }
  });
}

test("a wide window with a narrow map folds the list as a phone does", async ({ page }) => {
  // 1024px across with the navigation open leaves the map under its stacking
  // width: it is the map's width that decides, not the window's.
  await page.setViewportSize({ width: 1024, height: 768 });
  await gotoApp(page, MAP);

  const show = page.getByRole("button", { name: "Show the list" });
  await expect(show).toBeVisible();
  await show.click();
  await page.getByRole("button", { name: /^RAO/ }).click();

  await expect(page.getByRole("article", { name: "Details for RAO" })).toBeVisible();
  await expect(show).toBeVisible();
});

test("on a phone a track is watched with the details folded away, and nothing deselected", async ({
  page,
}) => {
  await page.setViewportSize(PHONE);
  await gotoApp(page, MAP);

  const { toolbar, list, playback } = overlays(page);

  // The list starts folded, and folds again once it has been used.
  await page.getByRole("button", { name: "Show the list" }).click();
  await page.getByRole("button", { name: /^QUINN/ }).click();
  await expect(page.getByRole("button", { name: "Show the list" })).toBeVisible();

  const details = page.getByRole("article", { name: "Details for QUINN" });
  await expect(details).toBeVisible();
  await expect(playback).toBeVisible();

  const open = await box(details);
  for (const other of [toolbar, list, playback]) {
    expect(apart(open, await box(other))).toBe(true);
  }

  // The list brought back while the details are open: there is not room for
  // both at full height, and they share it rather than overlap.
  await page.getByRole("button", { name: "Show the list" }).click();
  await expect(page.getByRole("button", { name: /^RAO/ })).toBeVisible();
  expect(apart(await box(list), await box(details))).toBe(true);
  expect(apart(await box(details), await box(playback))).toBe(true);
  await page.getByRole("button", { name: "Hide the list" }).click();

  // Folded to its heading: still says who, and the track is still there.
  await details.getByRole("button", { name: "Hide the details" }).click();
  await expect(details.getByRole("heading", { name: "QUINN" })).toBeVisible();
  await expect(details.getByText("MGRS", { exact: true })).toBeHidden();
  await expect(playback).toBeVisible();
  expect((await box(details)).height).toBeLessThan(open.height / 2);

  await details.getByRole("button", { name: "Show the details" }).click();
  await expect(details.getByText("MGRS", { exact: true })).toBeVisible();
});
