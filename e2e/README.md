# End-to-end tests

Playwright tests that drive the Yew admin UI in a real browser, against a real
`rustak` server, with a real SQLite database behind it. They cover the things
that only show up when those three are put together: that the UI was embedded
into the binary, that the SPA fallback makes deep links work, that the setup
wizard turns a token on disk into an administrator who can sign in, and that a
passkey is bound to the host it was registered against.

This harness is a direct port of `../automate`'s `e2e/` (see
`.claude/plan/research/01-automate-architecture.md` §7), adapted for rustak's
config schema, its own admin API, and the fact that rustak has **no
passwords** — so every session in this suite comes from a WebAuthn ceremony.

## Running them

The server is **not** built by the test run, and the order below matters:

```bash
cd rustak-ui && trunk build     # takes a few minutes the first time
cd .. && cargo build -p rustak-server

cd e2e
npm ci
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
npx playwright test tests/auth.spec.ts
npx playwright test --headed --debug
npx playwright show-report
npm run typecheck                         # tsc --noEmit; Playwright does not type-check
```

### Environment knobs

| Variable | What it does |
|---|---|
| `RUSTAK_E2E_PORT` | The port the server binds (default `18446`). |
| `RUSTAK_E2E_HOST` | The name the suite addresses it by (default `localhost`). It is also the WebAuthn relying party — see below. |
| `RUSTAK_E2E_BIND` | The address the listener binds (default `127.0.0.1`). |
| `RUSTAK_E2E_BASE_URL` | Point the whole suite somewhere else entirely. |
| `RUSTAK_E2E_BINARY` | A specific `rustak` binary, instead of the newer of `target/{debug,release}/rustak`. |
| `RUSTAK_E2E_WORKSPACE` | The scratch directory. Normally derived from the port and exported by `playwright.config.ts`. |
| `RUSTAK_E2E_KEEP` | Leave the scratch directory (database, CA, log) behind after the run. |
| `RUSTAK_E2E_SERVER_LOG` | Pipe the server's own log into the test output. Off by default: it logs every request header at `INFO` and buries the report. |
| `RUSTAK_E2E_CHROMIUM` | A Chrome/Chromium already on this machine, for somewhere that cannot reach Playwright's browser CDN. |

## How the server under test is started

`playwright.config.ts` runs `scripts/start-server.mjs`, which empties a scratch
directory, writes a minimal `config.toml` into it, and starts the already-built
binary there. Everything the server writes — the SQLite database, its
encryption key file, the certificate authority it generates, the content store,
the setup token — stays in that directory, and the directory is removed when
the run ends. Your own `config.toml` and `data/` directory are never touched.

The generated configuration disables authentication policy entirely (`user_acl`
and `admin_acl` of `true` admit everybody, exactly as automate's does), and
turns off every listener this suite does not exercise: `[web.marti]` and
`[stream.tls]` are both `enabled = false`, and there is no `[stream.tcp]`
section at all (that config section does not exist in rustak — see plan.md's
"no plaintext TCP stream listener" decision). `[web.public]` serves plain HTTP
with `[web.public.tls] mode = "none"` and `allow_insecure_http = true` — note
that `allow_insecure_http` lives on `[web.public]`, *beside* the
`[web.public.tls]` table rather than inside it, and every config struct is
`deny_unknown_fields`, so putting it in the wrong table is a start-up failure.

The scratch directory's path is **derived from the port** (`rustak-e2e-<port>`
under the system temp directory) rather than randomly generated, because the
tests have to read the setup token out of it. It is emptied at the start of
every run: almost everything this suite asserts about the first run is
one-shot, so a database inherited from a previous run would be a different
server from the one the specs describe. For the same reason
`webServer.reuseExistingServer` is **false**, even locally.

The server listens on **18446**, not rustak's default public port (8446) and
not automate's e2e port (8099), so a run cannot point itself at a rustak
instance you already have running with real devices and missions behind it.

## Passkeys, and why the host name is `localhost`

rustak has no local passwords. The first administrator is created by a one-time
setup token and immediately registers a passkey, and every sign-in after that is
a WebAuthn ceremony — so a suite that could not run one could not sign in, and
could not test anything behind a session.

Two things make that work here:

- **The relying party is a name, not an address.** WebAuthn identifies a relying
  party by domain, so `http://127.0.0.1:18446` cannot register a passkey *at
  all* — `Passkeys::for_base_url` refuses it in as many words. The generated
  config therefore sets `[server] base_url = "http://localhost:18446"`, which is
  what `identity::settings::base_url` returns (in preference to anything the
  wizard later stores) and therefore what the relying party is derived from.
  `localhost` is also the one name a browser treats as a secure context over
  plain HTTP, which is what lets this suite skip TLS and still run real
  ceremonies. `auth.spec.ts` uses the difference deliberately: the same
  credential, offered at `127.0.0.1`, is refused by the browser.
- **A virtual authenticator.** `tests/webauthn.ts` attaches Chromium's CDP
  `WebAuthn` virtual authenticator to the browser context: a real CTAP2
  implementation in software, with real key pairs and real assertions. The
  browser's own origin checks, user verification and signature counter are
  unchanged, so what the specs exercise is the ceremony rather than a stub of
  it.
- **Every passkey is discoverable.** The server registers with
  `residentKey: "required"`, so the credential the authenticator creates is one
  the browser can offer to a ceremony that names nobody — which is the only kind
  the sign-in prompt runs. Nothing in the suite adjusts a credential to make a
  sign-in work. `makeCredentialsUndiscoverable` exists for the opposite reason:
  one spec needs a credential the prompt *cannot* find, to reach the
  username-assisted fallback.

## The specs, and the order they run in

| Spec | What it covers |
|---|---|
| `setup.spec.ts` | The first-run wizard, through the UI: token → administrator → passkey → server name → authority (which already exists, so the step shows its fingerprint and offers the certificate) → finish, and then that every `/setup/*` route answers `410` and the token file is gone. |
| `auth.spec.ts` | Passkey sign-in and sign-out; a browser with no passkey; a passkey offered at the wrong host; the username-assisted fallback for a passkey the browser cannot offer on its own. |
| `navigation.spec.ts` | Every destination in the navigation sidebar, the SPA deep-link fallback, the not-found page, and the landing page getting out of the way. |
| `map.spec.ts` | The map, against the demo fixtures and with every tile request refused: the libraries load from `/vendor`, what the API returns reaches the map, the roster searches by callsign and type, picking a row opens its pop-over, and a click on two things at once opens the chooser. |
| `preferences.spec.ts` | An account's own preferences, against the real server: the edition of MIL-STD-2525 chosen under Account is stored and survives a full navigation. |
| `smoke.spec.ts` | `robots.txt` ahead of the catch-all, `/api/v1/health`, and the bundle booting. |

`setup.spec.ts` is its **own Playwright project**, which the `chromium` project
declares a dependency on, because the wizard is a one-way door and file-name
order does not put "setup" before "auth". It leaves the session it established
in `<workspace>/e2e-session.json`; `bootstrapAdmin` in `tests/helpers.ts` reads
it, and drives the same sequence through the API when a spec is run on its own
against a fresh server.

## Traps worth knowing about

**The repository's root `.env` is a named pipe.** `rustak_core::config`'s env
loader guards against this itself (`load_env_file` only reads a path that
`Path::is_file()` says is a regular file — a FIFO fails that check, unlike the
bare `Path::exists()` automate's loader used, which is *true* for a pipe and
blocks forever reading it). This launcher still always passes `--env` a path
that cannot exist and runs the server from its scratch directory, belt-and-
braces on top of that guard rather than a substitute for it. Do not remove
either. (For the same reason: never `cat` that file, and never run a recursive
`grep` from the repository root.)

**Readiness is a route, never a log line.** `tracing-batteries` used to suppress
stdout under `debug_assertions`; M0-12 turned that back on, but a server that
has started successfully must still never be told apart from one that has hung
by watching its output. Readiness is `GET /robots.txt` — registered ahead of the
SPA catch-all, so a 200 proves the server is genuinely routing rather than just
serving `index.html` to everything.

**The SPA fallback answers 200 with `index.html` for any unknown path**, so a
wrong URL never produces a 404. Do not write assertions that rely on one;
`navigation.spec.ts` asserts the application's own not-found page instead.

## Writing tests here

- Wait for the application with `waitForApp`/`gotoApp` from `tests/helpers.ts`,
  which watch for the `TrunkApplicationStarted` event the Trunk-built bundle
  dispatches once wasm boots. Not `networkidle`: the bundle is megabytes and
  the app keeps polling `/api/v1` after it has painted.
- Get a session with `bootstrapAdmin` + `signIn`. Run the ceremony yourself
  only when the ceremony is what you are testing.
- Prefer `getByRole`, `getByLabel` and `getByText` over CSS selectors or
  `data-testid` hooks, matching automate's UI conventions.
- Every test shares one server process and one database, so name anything you
  create with `uniqueName()` and clean it up again (an `afterEach` purge, the
  same shape as automate's `purgeWorkflowsNamed`/`purgeConnectionsNamed`, is
  the natural place once device/credential/mission fixtures exist).
- A test that signs out revokes the session every later spec was going to use —
  `POST /api/v1/auth/logout` revokes the `jti` *and* every refresh token for
  the account. Put a fresh one back with `cacheSession` before finishing, as
  `auth.spec.ts` does.
- Name a test as a sentence describing the behaviour and why it matters, to
  match the Rust tests in this repository.
