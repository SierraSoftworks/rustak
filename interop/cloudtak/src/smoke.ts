/**
 * The Playwright smoke: CloudTAK's own UI, in a browser, against this rustak.
 *
 * Deliberately shallow. The assertions that matter are the API-level ones in
 * `src/steps.ts`; this exists to catch the class of failure they cannot see —
 * a CloudTAK that answers every REST call correctly and still cannot render,
 * because its login page never resolves, its map never initialises, or the
 * Data Sync the API just created is invisible in the menu that lists them.
 *
 * Three screenshots are kept whatever happens, because "the map did not load"
 * is a sentence nobody can act on and a picture of the page is.
 */

import fs from "node:fs";
import path from "node:path";

import { chromium, type Browser, type Page } from "playwright";

/** What the smoke found. */
export interface SmokeResult {
  readonly status: "pass" | "fail" | "skip";
  readonly reasons: readonly string[];
  readonly screenshots: readonly string[];
}

/** What it needs to drive the UI. */
export interface SmokeOptions {
  /** CloudTAK's base URL, as the browser reaches it. */
  readonly base: string;

  readonly username: string;
  readonly password: string;

  /** The Data Sync the API steps created, which the mission menu must list. */
  readonly missionName?: string;

  /** Where the screenshots are kept. */
  readonly artifacts: string;
}

/** How long any one wait is given. CloudTAK's first paint pulls in its map bundle. */
const TIMEOUT_MS = 45_000;

/** Whether a browser has been installed for Playwright to drive. */
export function browserInstalled(): boolean {
  try {
    const executable = chromium.executablePath();

    return executable.length > 0 && fs.existsSync(executable);
  } catch {
    return false;
  }
}

/** Takes one screenshot, and records where it went. */
async function capture(page: Page, into: string, name: string, kept: string[]): Promise<void> {
  const file = path.join(into, `${name}.png`);

  try {
    await page.screenshot({ path: file, fullPage: false });
    kept.push(file);
  } catch {
    // A page that has already gone is not worth failing the smoke over; the
    // reason the caller is about to record is the interesting part.
  }
}

/** Signs in through the form, exactly as a person does. */
async function signIn(page: Page, options: SmokeOptions, kept: string[]): Promise<void> {
  await page.goto(`${options.base}/login`, { waitUntil: "domcontentloaded", timeout: TIMEOUT_MS });

  const password = page.locator('input[type="password"]').first();

  await password.waitFor({ state: "visible", timeout: TIMEOUT_MS });
  await capture(page, options.artifacts, "01-login", kept);

  // The username field is the one text input beside it; CloudTAK's own
  // placeholder is an email address, which is the stable handle on it.
  const username = page.locator('input[type="text"], input[type="email"]').first();

  await username.fill(options.username);
  await password.fill(options.password);

  // `exact` and the precise casing, because CloudTAK's login card carries two
  // buttons whose accessible names both match /sign in/i — the `submit` one
  // ("Sign In") and a secondary SSO one ("Sign in with …"). A loose match is a
  // Playwright strict-mode violation rather than a wrong click, which is how it
  // failed on run 35394055984.
  await page.getByRole("button", { name: "Sign In", exact: true }).click();
}

/** Waits for the map to be a map rather than a spinner. */
async function waitForMap(page: Page, options: SmokeOptions, kept: string[]): Promise<void> {
  await page.waitForURL((url) => !url.pathname.startsWith("/login"), { timeout: TIMEOUT_MS });
  await page.locator("canvas").first().waitFor({ state: "attached", timeout: TIMEOUT_MS });

  // The canvas exists before the first frame is drawn; a beat here is the
  // difference between a screenshot of the map and one of a blank rectangle.
  await page.waitForTimeout(2_000);
  await capture(page, options.artifacts, "02-map", kept);
}

/** Opens the Data Sync menu and looks for the mission the API created. */
async function waitForMissions(page: Page, options: SmokeOptions, kept: string[]): Promise<void> {
  await page.goto(`${options.base}/menu/missions`, { waitUntil: "domcontentloaded", timeout: TIMEOUT_MS });

  if (options.missionName !== undefined) {
    await page
      .getByText(options.missionName, { exact: false })
      .first()
      .waitFor({ state: "visible", timeout: TIMEOUT_MS });
  } else {
    await page.waitForTimeout(3_000);
  }

  await capture(page, options.artifacts, "03-missions", kept);
}

/** Runs the whole smoke, keeping what it saw either way. */
export async function uiSmoke(options: SmokeOptions): Promise<SmokeResult> {
  if (!browserInstalled()) {
    return {
      status: "skip",
      reasons: [
        "no Playwright browser is installed — run `npx playwright install --with-deps chromium` (the nightly job does).",
      ],
      screenshots: [],
    };
  }

  fs.mkdirSync(options.artifacts, { recursive: true });

  const kept: string[] = [];
  const failures: string[] = [];

  let browser: Browser | undefined;
  let page: Page | undefined;

  try {
    browser = await chromium.launch({ args: ["--no-sandbox"] });

    page = await browser.newPage({ viewport: { width: 1440, height: 900 } });

    page.setDefaultTimeout(TIMEOUT_MS);
    page.on("pageerror", (error) => failures.push(`page error: ${error.message}`));

    await signIn(page, options, kept);
    await waitForMap(page, options, kept);
    await waitForMissions(page, options, kept);

    return {
      // A page error during a run that otherwise reached every landmark is
      // worth reporting and not worth failing on: CloudTAK's map logs plenty
      // about tiles this stack deliberately does not serve.
      status: "pass",
      reasons: [
        "the login page rendered, the map canvas initialised",
        options.missionName === undefined
          ? "and the Data Sync menu opened"
          : `and the Data Sync menu lists '${options.missionName}'`,
        ...failures.slice(0, 5).map((failure) => `(noted) ${failure}`),
      ],
      screenshots: kept,
    };
  } catch (error) {
    if (page !== undefined) await capture(page, options.artifacts, "99-failure", kept);

    return {
      status: "fail",
      reasons: [error instanceof Error ? error.message : String(error), ...failures.slice(0, 5)],
      screenshots: kept,
    };
  } finally {
    await browser?.close();
  }
}
