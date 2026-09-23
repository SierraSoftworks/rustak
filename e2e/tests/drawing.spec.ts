/**
 * Drawing on the map: lines, polygons, rectangles, circles and routes — the
 * shapes ATAK's own drawing tools make, published as the CoT they publish.
 *
 * Against the demo fixtures, as `map.spec.ts` is and for its reasons. What a
 * drawing becomes on the wire is the server's tests' business; what is tested
 * here is that clicks on the map add up to a drawing, that it can be named,
 * coloured, reshaped and deleted, and that what was drawn somewhere else is
 * treated with the care it needs.
 *
 * A WebGL canvas has nothing in it to find, so the map says what it is
 * drawing on its own element: `data-shapes` for the outlines on it, and
 * `data-sketch` for the handles of whatever is being drawn or reshaped.
 */

import type { Locator, Page } from "@playwright/test";

import { bootstrapAdmin, expect, gotoApp, signIn, test } from "./helpers";

const MAP = "/admin/map?demo";

test.beforeEach(async ({ page }) => {
  const session = await bootstrapAdmin(page);
  await signIn(page, session);

  await page.route("https://tile.openstreetmap.org/**", (route) => route.abort());
  await page.setViewportSize({ width: 1440, height: 1000 });
});

/** The map, once it has something on it, and a way to click it by fraction. */
async function openMap(page: Page) {
  await gotoApp(page, MAP);
  const application = page.getByRole("application");
  await expect(application).toHaveAttribute("data-features", /^[1-9]\d*$/);

  const canvas = page.locator(".map-page__canvas canvas");
  const box = (await canvas.boundingBox())!;
  const at = (x: number, y: number) => ({ x: box.width * x, y: box.height * y });
  const click = (x: number, y: number) => canvas.click({ position: at(x, y) });

  return { application, box, click };
}

const fact = (details: Locator, term: string) => details.locator("dt", { hasText: term }).locator("+ dd");

test("a polygon is drawn click by click, then coloured, reshaped and deleted", async ({ page }) => {
  const { application, box, click } = await openMap(page);
  const shapes = Number(await application.getAttribute("data-shapes"));

  const tool = page.getByRole("button", { name: "Draw a polygon" });
  await tool.click();
  await expect(tool).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByText("Click the map to start drawing.")).toBeVisible();
  await expect(page.getByRole("button", { name: "Finish" })).toBeDisabled();

  // Clear of everything else on the map, which a click while drawing would
  // go through anyway.
  await click(0.45, 0.75);
  await click(0.55, 0.75);
  await click(0.55, 0.85);
  await expect(application).toHaveAttribute("data-sketch", "3");
  await expect(page.getByRole("button", { name: "Finish" })).toBeEnabled();

  // The last point again, which is what a double click is.
  await click(0.55, 0.85);

  const details = page.getByRole("article", { name: /^Details for Polygon \d+$/ });
  await expect(details).toBeVisible();
  await expect(application).toHaveAttribute("data-shapes", String(shapes + 1));
  await expect(page.getByRole("button", { name: "Select" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("button", { name: /u-d.*Drawings/ })).toBeVisible();

  // A drawing has a colour where a marker has a type and a symbol.
  await expect(details.getByLabel("Type")).toHaveCount(0);
  await expect(details.getByRole("radio", { name: "Red" })).toBeChecked();
  await details.getByLabel("Name").fill("CORDON SOUTH");
  await details.getByRole("radio", { name: "Blue" }).check();
  await details.getByRole("button", { name: "Save" }).click();

  const renamed = page.getByRole("article", { name: "Details for CORDON SOUTH" });
  await expect(renamed).toBeVisible();
  await expect(renamed.getByRole("radio", { name: "Blue" })).toBeChecked();

  // Its corners are handles, and a dragged one is published when it is saved.
  await expect(application).toHaveAttribute("data-sketch", "3");
  const before = await fact(renamed, "Perimeter").textContent();
  await page.mouse.move(box.x + box.width * 0.55, box.y + box.height * 0.85);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * 0.6, box.y + box.height * 0.9, { steps: 5 });
  await page.mouse.up();
  await expect(renamed.getByText("The outline has been moved.")).toBeVisible();

  await renamed.getByRole("button", { name: "Save" }).click();
  await expect(renamed.getByText("Drag a point on the map to move it.")).toBeVisible();
  await expect(fact(renamed, "Perimeter")).not.toHaveText(before!);

  await renamed.getByRole("button", { name: "Delete" }).click();
  await renamed.getByRole("button", { name: "Delete it" }).click();
  await expect(renamed).toHaveCount(0);
  await expect(application).toHaveAttribute("data-shapes", String(shapes));
  await expect(application).toHaveAttribute("data-sketch", "");
});

for (const { form, group, measure, finish } of [
  { form: "line", group: /u-d.*Drawings/, measure: "Length", finish: "Enter" },
  { form: "route", group: /b-m-r.*Routes/, measure: "Length", finish: "Finish" },
  // A corner and its opposite, a centre and its edge: the second click is the last.
  { form: "rectangle", group: /u-d.*Drawings/, measure: "Perimeter", finish: null },
  { form: "circle", group: /u-d.*Drawings/, measure: "Radius", finish: null },
]) {
  test(`a ${form} is drawn and is on the map as one`, async ({ page }) => {
    const { application, click } = await openMap(page);
    const shapes = Number(await application.getAttribute("data-shapes"));

    await page.getByRole("button", { name: `Draw a ${form}` }).click();
    await click(0.45, 0.75);
    await click(0.55, 0.85);
    if (finish === "Enter") {
      await page.keyboard.press("Enter");
    } else if (finish) {
      await page.getByRole("button", { name: finish }).click();
    }

    const name = new RegExp(`^Details for ${form[0].toUpperCase()}${form.slice(1)} \\d+$`);
    const details = page.getByRole("article", { name });
    await expect(details).toBeVisible();
    await expect(application).toHaveAttribute("data-shapes", String(shapes + 1));
    await expect(page.getByRole("button", { name: group })).toBeVisible();
    await expect(fact(details, measure)).toHaveText(/\d (m|km)$/);
  });
}

test("a circle is as wide as it is typed to be", async ({ page }) => {
  const { click } = await openMap(page);

  await page.getByRole("button", { name: "Draw a circle" }).click();
  await expect(page.getByText("Click the centre of the circle.")).toBeVisible();
  await click(0.5, 0.8);
  await expect(page.getByText("Click where its edge should be.")).toBeVisible();
  await click(0.55, 0.8);

  const details = page.getByRole("article", { name: /^Details for Circle \d+$/ });
  await details.getByLabel("Radius (m)").fill("none");
  await details.getByRole("button", { name: "Save" }).click();
  await expect(details.getByText("The radius is not a number.")).toBeVisible();

  await details.getByLabel("Radius (m)").fill("750");
  await details.getByRole("button", { name: "Save" }).click();
  await expect(fact(details, "Radius")).toHaveText("750 m");
});

test("a click can be taken back and a drawing given up", async ({ page }) => {
  const { application, click } = await openMap(page);
  const shapes = await application.getAttribute("data-shapes");

  await page.getByRole("button", { name: "Draw a line" }).click();
  await click(0.45, 0.75);
  await click(0.55, 0.85);
  await expect(application).toHaveAttribute("data-sketch", "2");

  await page.getByRole("button", { name: "Undo" }).click();
  await expect(application).toHaveAttribute("data-sketch", "1");
  await expect(page.getByRole("button", { name: "Finish" })).toBeDisabled();

  await page.keyboard.press("Escape");
  await expect(application).toHaveAttribute("data-sketch", "");
  await expect(page.getByRole("button", { name: "Select" })).toHaveAttribute("aria-pressed", "true");
  await expect(application).toHaveAttribute("data-shapes", shapes!);
});

test("a shape drawn elsewhere can be edited, and a route drawn elsewhere is only looked at", async ({ page }) => {
  const { application } = await openMap(page);

  await page.getByRole("button", { name: /^CORDON/ }).click();
  const cordon = page.getByRole("article", { name: "Details for CORDON" });
  await expect(cordon.getByLabel("Name")).toHaveValue("CORDON");
  await expect(application).toHaveAttribute("data-sketch", "4");

  // A device's route names its checkpoints and carries its cues, and a save
  // from here would write it again without them.
  await page.getByRole("button", { name: /^CASEVAC ROUTE/ }).click();
  const route = page.getByRole("article", { name: "Details for CASEVAC ROUTE" });
  await expect(fact(route, "Length")).toHaveText(/km$/);
  await expect(route.getByRole("button", { name: "Save" })).toHaveCount(0);
  await expect(application).toHaveAttribute("data-sketch", "");
});

test("on a phone the tools fit the screen and the list gets out of a drawing's way", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const { application, click } = await openMap(page);

  const toolbar = (await page.getByRole("toolbar", { name: "Map tools" }).boundingBox())!;
  expect(toolbar.x).toBeGreaterThanOrEqual(0);
  expect(toolbar.x + toolbar.width).toBeLessThanOrEqual(390);

  await page.getByRole("button", { name: "Show the list" }).click();
  await page.getByRole("button", { name: "Draw a line" }).click();
  await expect(page.getByRole("button", { name: "Show the list" })).toBeVisible();

  // No double click and no keyboard: the bar's own button finishes it.
  await click(0.3, 0.5);
  await click(0.6, 0.6);
  await page.getByRole("button", { name: "Finish" }).click();

  await expect(page.getByRole("article", { name: /^Details for Line \d+$/ })).toBeVisible();
  await expect(application).toHaveAttribute("data-sketch", "2");
});
