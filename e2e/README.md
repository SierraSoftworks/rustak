# End-to-end tests

Playwright tests that drive the Yew admin UI in a real browser, against a real
`rustak` server, with a real SQLite database behind it. They cover the things
that only show up when those three are put together: that the UI was embedded
into the binary, that the SPA fallback makes deep links work, and that the
server is genuinely routing rather than serving `index.html` to everything.

This harness is a direct port of `../automate`'s `e2e/` (see
`.claude/plan/research/01-automate-architecture.md` §7), adapted for rustak's
config schema and its own admin API. The traps below are automate's, verified
against rustak's equivalents.

## Running them

The server is **not** built by the test run, and the order below matters:

```bash
cd rustak-ui && trunk build     # takes a few minutes the first time
cd .. && cargo build -p rustak-server

cd e2e
npm install
npx playwright install chromium
npx playwright test
```

`trunk build` has to come first. The UI is embedded into the server binary at
compile time by `include_dir!("$CARGO_MANIFEST_DIR/../rustak-ui/dist")`, so a
server built while `rustak-ui/dist` was empty compiles and runs perfectly well
and then answers `GET /` with a 500. `rustak-server/build.rs` declares
`cargo:rerun-if-changed=../rustak-ui/dist`, so rebuilding the UI does trigger a
rebuild of the server — you just have to do it in that order.

Useful variants:

```bash
npx playwright test --ui                  # the interactive runner
npx playwright test tests/smoke.spec.ts
npx playwright test --headed --debug
npx playwright show-report
```

## How the server under test is started

`playwright.config.ts` runs `scripts/start-server.mjs`, which makes a
throwaway directory under the system temp directory, writes a minimal
`config.toml` into it, and starts the already-built binary there. Everything
the server writes — the SQLite database, its encryption key file, the
content-addressed store — stays in that directory, and the directory is
removed when the run ends. Your own `config.toml` and `data/` directory are
never touched.

The generated configuration disables authentication entirely (`user_acl` and
`admin_acl` of `true` admit everybody, exactly as automate's does), and turns
off every listener this suite does not exercise: `[web.marti]` and
`[stream.tls]` are both `enabled = false`, and there is no `[stream.tcp]`
section at all (that config section does not exist in rustak — see
plan.md's "no plaintext TCP stream listener" decision). `[web.public]` serves
plain HTTP with `[web.public.tls] mode = "none"` and
`allow_insecure_http = true`, since rustak otherwise refuses to bind a public
listener without TLS — this suite is testing the admin UI, not certificate
handling.

The server listens on **18446**, not rustak's default public port (8446) and
not automate's e2e port (8099), so a run cannot point itself at a rustak
instance you already have running with real devices and missions behind it.
Override with `RUSTAK_E2E_PORT`, or point the suite somewhere else entirely
with `RUSTAK_E2E_BASE_URL`. `RUSTAK_E2E_BINARY` selects a specific binary.

## Two traps worth knowing about (inherited from automate)

**The repository's root `.env` is a named pipe.** `rustak_core::config`'s env
loader is expected to guard against this itself (`load_env_file` only reads a
path that `Path::is_file()` says is a regular file — a FIFO fails that check,
unlike the bare `Path::exists()` automate's loader used, which is *true* for a
pipe and blocks forever reading it). This launcher still always passes `--env`
a path that cannot exist and runs the server from its scratch directory,
belt-and-braces on top of that guard rather than a substitute for it. Do not
remove either. (For the same reason: never `cat` that file, and never run a
recursive `grep` from the repository root.)

**A debug build may print nothing.** rustak's telemetry bootstrap
(`rustak-core::telemetry`) is built on the same `tracing-batteries` crate
automate uses, which disables its stdout output under `debug_assertions` —
verify this still holds once `rustak-core::telemetry` lands, since a server
that has started successfully can otherwise look exactly like one that has
hung. Readiness is therefore established by polling `GET /robots.txt` —
registered ahead of the SPA catch-all, so a 200 proves the server is genuinely
routing rather than just serving `index.html` to everything. Never gate
readiness on log output.

## Writing tests here

- Wait for the application with `waitForApp`/`gotoApp` from `tests/helpers.ts`,
  which watch for the `TrunkApplicationStarted` event the Trunk-built bundle
  dispatches once wasm boots. Not `networkidle`: the bundle is megabytes and
  the app keeps polling `/api/v1` after it has painted.
- **The SPA fallback answers 200 with `index.html` for any unknown path**, so
  a wrong URL never produces a 404. Do not write assertions that rely on one.
- Prefer `getByRole`, `getByLabel` and `getByText` over CSS selectors or
  `data-testid` hooks, matching automate's UI conventions.
- Every test shares one server process and one database, so name anything you
  create with `uniqueName()` and clean it up again (an `afterEach` purge, the
  same shape as automate's `purgeWorkflowsNamed`/`purgeConnectionsNamed`, is
  the natural place once device/credential/mission fixtures exist).
- `bootstrapAdmin`/`signIn` in `tests/helpers.ts` are ahead of what this brief
  builds: they target `/api/v1/setup/admin`, `/api/v1/auth/local` and the
  `rustak.admin.token` session-storage key design 01 §6.2/§6.4 specify, so a
  later `setup.spec.ts`/`auth.spec.ts` does not have to re-derive the
  sequence. `smoke.spec.ts` does not use them.
- Name a test as a sentence describing the behaviour and why it matters, to
  match the Rust tests in this repository.

## Not done here

This brief ships the harness skeleton only: `package.json`, `playwright.config.ts`,
`scripts/start-server.mjs`, `tests/helpers.ts` and `tests/smoke.spec.ts`. It
does not run `npm install` (no `package-lock.json` is committed by this
change — the first `npm install` after a crate-owning brief lands should
commit the resulting lockfile), and it does not add `setup.spec.ts`,
`auth.spec.ts` or any device/mission specs — those follow the admin API and UI
pages they exercise, per design 01 §7.1 and plan.md's M0 step 14.
