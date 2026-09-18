import os from "node:os";
import path from "node:path";

import { defineConfig, devices } from "@playwright/test";

/**
 * The port the server under test listens on.
 *
 * Deliberately not rustak's default public port (8446), nor automate's e2e
 * port (8099). A developer running this suite very likely has their own
 * rustak instance running on the default port with a real database and real
 * TAK clients behind it, and a test run that quietly pointed at it would
 * create devices, credentials and missions in — and delete them from — that
 * real deployment.
 */
const port = Number(process.env.RUSTAK_E2E_PORT ?? 18446);

/**
 * The host the suite addresses the server by.
 *
 * `localhost`, never `127.0.0.1`: WebAuthn identifies a relying party by
 * domain, so an address cannot be one at all, and every passkey ceremony in
 * this suite would be refused by the browser before it reached the server.
 * `localhost` is also the one name a browser treats as a secure context over
 * plain HTTP, which is what lets this suite serve `[web.public.tls] mode =
 * "none"` and still run real ceremonies.
 */
const host = process.env.RUSTAK_E2E_HOST ?? "localhost";
const baseURL = process.env.RUSTAK_E2E_BASE_URL ?? `http://${host}:${port}`;

/**
 * Where the server under test keeps everything it writes.
 *
 * Derived from the port rather than randomly generated, and exported into the
 * environment here so that `scripts/start-server.mjs` and the test workers
 * agree on it without one having to tell the other: this file is loaded in
 * both processes. The tests need it because a first-run installation's only
 * credential is the setup token the server writes into that directory.
 */
process.env.RUSTAK_E2E_WORKSPACE ??= path.join(
  os.tmpdir(),
  `rustak-e2e-${port}`,
);
const workspace = process.env.RUSTAK_E2E_WORKSPACE;

export default defineConfig({
  testDir: "./tests",

  // Every test talks to one server process backed by one SQLite database, so
  // running them concurrently would have them reading each other's devices,
  // credentials and missions. Names are unique per test as a second line of
  // defence, but serial execution is what makes a failure mean what it says.
  fullyParallel: false,
  workers: 1,

  // A `.only` left in a spec file silently reduces CI to that one test.
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,

  // The wasm bundle is several megabytes and is compiled by the browser on
  // first load, so the first navigation of a run is far slower than the rest.
  timeout: 60_000,
  expect: { timeout: 15_000 },

  reporter: process.env.CI ? [["github"], ["html", { open: "never" }]] : [["list"], ["html", { open: "never" }]],

  use: {
    baseURL,
    trace: "on-first-retry",
    screenshot: "only-on-failure",
    video: "off",
    actionTimeout: 15_000,
    navigationTimeout: 30_000,

    // An escape hatch for a machine that cannot reach Playwright's browser
    // CDN: point this at a Chrome or Chromium already installed here. CI runs
    // `npx playwright install chromium` and leaves it unset.
    launchOptions: {
      executablePath: process.env.RUSTAK_E2E_CHROMIUM || undefined,
    },
  },

  projects: [
    // The first-run wizard is a *one-way door*: it closes itself for good, and
    // `POST /setup/admin` answers `409` ever after. So the spec that walks it
    // has to be the first thing that touches the server, and no file-name
    // ordering says that ("auth" sorts before "setup"). A project dependency
    // does, in as many words.
    //
    // Chromium only, on purpose. The UI is one wasm bundle rendered by Yew
    // rather than a stack of browser-specific CSS and DOM workarounds, so a
    // second engine would re-run the same assertions against the same code
    // for roughly triple the wall-clock time. Add a browser here when a bug
    // is found that only one of them has.
    {
      name: "setup",
      testMatch: /setup\.spec\.ts/,
      use: { ...devices["Desktop Chrome"] },
      // A retry would run against a server that has already been set up, so
      // the second attempt could only fail differently. Fail once, clearly.
      retries: 0,
    },
    {
      name: "chromium",
      testIgnore: /setup\.spec\.ts/,
      dependencies: ["setup"],
      use: { ...devices["Desktop Chrome"] },
    },
  ],

  webServer: {
    command: "node scripts/start-server.mjs",
    // `/robots.txt` is registered before the SPA catch-all, so a 200 here
    // means the server is genuinely routing. Almost any other path would
    // answer 200 with `index.html` whether the routes were wired up or not.
    url: `${baseURL}/robots.txt`,
    // Not reused, even locally. Everything this suite asserts about the first
    // run is one-shot, so a server left over from an earlier run — with its
    // wizard already completed and its setup token already deleted — is not a
    // server these specs can describe. Playwright reports the port as taken,
    // which is the honest failure.
    reuseExistingServer: false,
    timeout: 120_000,
    // The server logs one INFO span per request with every header in it, which
    // buries the test report it is interleaved with. `RUSTAK_E2E_SERVER_LOG=1`
    // puts it back when a failure needs it; failures on the way *up* still
    // surface, because they go to stderr.
    stdout: process.env.RUSTAK_E2E_SERVER_LOG ? "pipe" : "ignore",
    stderr: "pipe",
    // Playwright SIGKILLs the server's process group unless asked otherwise,
    // which would leave the scratch directory — database, key file, content
    // store and all — behind after every run. A signal the launcher can catch
    // lets it remove what it made.
    //
    // The timeout is longer than the server's own budget on purpose:
    // `[server] shutdown_timeout` defaults to 8 s of draining and the WAL
    // checkpoint that follows it is allowed 2 s more, so a 5 s wait here used
    // to SIGKILL the server in the middle of the checkpoint it had been asked
    // to make. 15 s leaves both of them room and still fails fast.
    gracefulShutdown: { signal: "SIGTERM", timeout: 15_000 },
    env: {
      RUSTAK_E2E_PORT: String(port),
      RUSTAK_E2E_HOST: host,
      RUSTAK_E2E_WORKSPACE: workspace,
    },
  },
});
