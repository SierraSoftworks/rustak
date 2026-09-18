# M0-14 — e2e specs (Playwright) — complete

Brief: `.claude/plan/briefs/M0-14-e2e-specs.md`
Read first: `conventions.md`; `design/01-foundations-storage-ci.md` §7.1; `docs/ci.md`; the
M0-02 skeleton under `e2e/**`; status files `M0-06`, `M0-11`, `M0-12`, `M0-13`.

Only files under `e2e/` and this status file were touched. No `git`/`but` commands were run.

## What was built

| File | Contents |
|---|---|
| `e2e/scripts/start-server.mjs` | Rewritten. Port-derived scratch directory, emptied per run; config with `[server] base_url = http://localhost:<port>`, `[web.public] allow_insecure_http`, `[web.public.tls] mode = "none"`, listeners off, `user_acl`/`admin_acl` `'true'`, an explicit `setup_token_file`; banner on stderr; `RUSTAK_E2E_KEEP`. |
| `e2e/playwright.config.ts` | Rewritten. `baseURL` on **`localhost`**; two projects (`setup` → `chromium` via `dependencies`); `reuseExistingServer: false`; `RUSTAK_E2E_CHROMIUM` and `RUSTAK_E2E_SERVER_LOG` escape hatches; `RUSTAK_E2E_WORKSPACE` exported from the config so launcher and workers agree. |
| `e2e/tests/webauthn.ts` | New. `attachAuthenticator` (CDP `WebAuthn.enable` + `addVirtualAuthenticator`, `hasResidentKey`/`hasUserVerification`/`isUserVerified`), `registerPasskey` (the ceremony, run inside the page), `makeCredentialsDiscoverable`. |
| `e2e/tests/helpers.ts` | Rewritten. `waitForApp`/`gotoApp`/`uniqueName` kept; `serverWorkspace`, `readSetupToken`, `bootstrapAdmin`, `signIn`, `storedSession`, `readCachedSession`/`cacheSession`, `ADMIN`. |
| `e2e/tests/setup.spec.ts` | New. The wizard end to end through the UI, then that it has closed itself. |
| `e2e/tests/auth.spec.ts` | New. Passkey sign-in and sign-out; no-passkey failure; wrong-origin failure. |
| `e2e/tests/navigation.spec.ts` | New. Every nav destination, the SPA deep-link fallback, the not-found page, the landing redirect. |
| `e2e/tests/smoke.spec.ts` | Extended with `/api/v1/health`. |
| `e2e/README.md` | Rewritten for the harness as built: the environment knobs, the passkey/`localhost` story, the spec inventory and the project ordering. |
| `e2e/package.json`, `e2e/package-lock.json` | `typescript` added as a devDependency with an `npm run typecheck` script — Playwright transpiles TypeScript with esbuild and never type-checks it, so nothing else would have caught a type error in these files. |

**12 tests**, all green. No files outside `e2e/` were changed.

## Decisions

### The relying party is `localhost`, so the whole suite is

`[server] base_url = "http://localhost:18446"` is the load-bearing line in the generated config.
`identity::settings::base_url` prefers it over anything the wizard stores, and
`auth::passkeys::Passkeys::for_base_url` derives the WebAuthn relying party from it — so this is
what makes the relying party `localhost` and keeps it `localhost` after the wizard's "Server
name" step. The listener still **binds `127.0.0.1`**: binding `localhost` would make start-up
depend on whether the host resolves it to `::1`, `127.0.0.1` or both, and a bind of an address
family the host does not have is a failure rather than a fallback.

`127.0.0.1` cannot be a relying party at all (M0-11 deviation 6), and `localhost` is also the one
name a browser treats as a secure context over plain HTTP — which is what lets the suite serve
`mode = "none"` and still run real ceremonies. `auth.spec.ts` turns the difference into an
assertion: the same credential, the same authenticator, offered at `127.0.0.1`, is refused.

### `setup.spec.ts` is its own project, not just another file

The wizard is a one-way door — `POST /setup/admin` answers `409` once an administrator exists and
every `/setup/*` route answers `410` once it is finished — so the spec that walks it has to touch
the server first. File-name order does not say that ("auth" sorts before "setup"), so
`playwright.config.ts` declares `projects: [setup, chromium]` with `dependencies: ["setup"]`. The
`setup` project also sets `retries: 0`: a retry would run against a server that has already been
set up, so the second attempt could only fail differently.

### The scratch directory is derived from the port, and emptied

`mkdtemp` was replaced by `<tmp>/rustak-e2e-<port>`, wiped at the start of every run. The tests
have to **read the setup token** out of the server's data directory — that file is the entire
first-run trust model, and there is no other way to create the first administrator — so its path
has to be knowable from both processes. `playwright.config.ts` computes it and puts it in
`RUSTAK_E2E_WORKSPACE`; the config is loaded in the launcher's process and in every worker, so
both read the same value rather than one telling the other. `reuseExistingServer` is `false` for
the same reason: a server left over from a previous run has already spent its wizard.

### The session is cached in the server's own scratch directory

`setup.spec.ts` leaves the session it established in `<workspace>/e2e-session.json`, and
`bootstrapAdmin` reads it. Inside the workspace on purpose: the session belongs to that
installation's database and both are deleted together, so a stale cache can never outlive the
account it names. When a spec is run on its own against a fresh server, `bootstrapAdmin` drives
the same sequence through the API instead (setup token → `/setup/admin` → passkey ceremony in the
page → `/setup/server` → `/setup/ca` → `/setup/complete`).

`auth.spec.ts`'s sign-out test puts a **fresh** session back with `cacheSession`, because
`POST /api/v1/auth/logout` revokes the `jti` *and* every refresh token for the account — so
signing out would otherwise invalidate the token `navigation.spec.ts` was going to use.

### `bootstrapAdmin` takes a `Page`, not an `APIRequestContext`

The brief's signature was `bootstrapAdmin(request)`. `navigator.credentials` exists in a page and
nowhere else, and only works for that page's own origin, so the passkey step cannot be driven from
Node without reimplementing the parts of WebAuthn that make it worth having. The ceremony runs in
the page; everything around it uses `page.request`, which shares the page's `baseURL`.

### The server's log is not piped by default

`TracingLogger` emits one `INFO` span per request with **every header in it**, which buried the
test report. `webServer.stdout` is `"ignore"` unless `RUSTAK_E2E_SERVER_LOG` is set; `stderr`
stays piped, and `start-server.mjs`'s own four-line banner was moved to stderr so a run that will
not start still says why. Readiness is still `GET /robots.txt` and never a log line.

## Things about the server and the UI that had to be worked around

Each of these is a defect in something M0-14 does not own. They are worked around in the specs,
with the work-around commented at the call site and pointing back here, so that fixing the defect
makes the work-around dead code rather than breaking a test.

### 1. The wizard's "certificate authority" step can never succeed — `POST /setup/ca` always 409s

`runtime::listen` (M0-12) calls `pki::load_or_create_root_ca` **before** it binds anything, so a
brand-new installation already has a root authority the first time it answers a request:

```
$ curl -s http://localhost:18447/api/v1/setup/status
{"needs_setup":true,"has_admin":false,"has_ca":true,…}
```

`web::api::setup::ca` refuses to replace one ("it is what every enrolled device trusts"), so
step 5 of the wizard answers `409 This server already has a certificate authority.` every time,
for every installation. The UI only advances on `Ok`, so the linear walk **dead-ends there** and
the wizard cannot be completed by clicking through it.

It is reachable in practice only because `Step::resume_from` skips a step the server says is done:
reloading `/setup` after the "Server name" step resumes at "Finish". `setup.spec.ts` does exactly
that, conditionally, so the spec keeps passing once this is fixed.

M0-12's `tests/bootstrap.rs` did not catch it because it walks
`setup/status → /setup/admin → passkey → /me` and never calls `/setup/ca`.

**Suggested fix (someone else's to make):** one of —
(a) `runtime::listen` only loads an existing authority when the wizard is still open, and lets the
wizard create it (TLS mode `internal` would then need the wizard finished first, which is already
true of any installation that can serve a browser);
(b) `POST /setup/ca` adopts the start-up-created authority when its parameters match, and only
`409`s on a genuine second attempt;
(c) the UI skips the step when `SetupStatus::has_ca` is already true, which is the smallest change
and makes the wizard honest about what it can still decide.

### 2. The sign-in prompt cannot use a passkey this server registers

`webauthn-rs`'s `start_passkey_registration` asks for `residentKey: "discouraged"` /
`requireResidentKey: false`, so the credential is **not discoverable**:

```
"authenticatorSelection": {"requireResidentKey": false, "residentKey": "discouraged", "userVerification": "required"}
```

The admin UI's only passkey sign-in is `AuthHandle::login_passkey` → `auth::passkey::login(None)`,
which is the **discoverable** ceremony (`start_discoverable_authentication`, empty
`allowCredentials`) — there is no username field on the login page. Against a browser that honours
the flag, that ceremony finds nothing:

```
discoverable: threw: NotAllowedError: The operation either timed out or was not allowed.
named ("username": "avery"): finish 200 {"token":"eyJ…"}
```

So: named sign-in works, discoverable sign-in does not, and the UI only offers the latter. Many
real platform authenticators (iCloud Keychain, Chrome's own profile passkeys) store every
credential discoverably regardless of the flag and would therefore work, which is presumably why
this has not been noticed — but a security key, or Chromium's virtual authenticator, honours it.
M0-11's Rust round-trip test passes because `testing::SoftAuthenticator` answers an empty
`allowCredentials` list with the credential it holds.

`tests/webauthn.ts` bridges it with `makeCredentialsDiscoverable()`, which re-stores the
credential through CDP `WebAuthn.addCredential` with `isResidentCredential: true`. The spec
asserts that at least one credential *had* to be converted, so when the defect is fixed the
assertion fails and the work-around can be deleted rather than quietly rotting.

**Suggested fix:** register with `residentKey: "preferred"` (or `"required"`) — `webauthn-rs`
0.5's `start_passkey_registration_with_extensions`/`AttestationCaList` builder path exposes it —
**or** give the login page a "sign in as…" affordance that uses the named ceremony when the
discoverable one finds nothing. The first is the smaller change and keeps the username-less flow
the design wanted.

### 3. `allow_insecure_http` is on `[web.public]`, not `[web.public.tls]`

The M0-02 skeleton's generated config put it inside the `[web.public.tls]` table, which every
config struct's `deny_unknown_fields` would have refused at start-up. Fixed here; design 01 §7.1's
sketch has the same ambiguity and is worth a one-line correction.

### 4. The server is sent two `SIGTERM`s on shutdown

Playwright's `gracefulShutdown` signals the launcher's whole process group, and the launcher also
forwards the signal to its child, so the server logs `Received a second shutdown signal; exiting
immediately.` and skips the WAL `TRUNCATE` that `runtime::run_all` exists to guarantee. Harmless
here — the scratch directory is deleted immediately afterwards — and the alternative (spawning the
child `detached`) risks orphaning a server on port 18446 when a run is `SIGKILL`ed, which would
break every subsequent run. Left as is, recorded so nobody reads that log line as a bug in the
server.

### 5. Minor, no work-around needed

- `PasskeyChallenge.options` is the **inner** WebAuthn dictionary, not `webauthn-rs`'s
  `{"publicKey": …}` wrapper. `tests/webauthn.ts` accepts either, as the UI does.
- `POST /auth/passkey/register/finish` on the bootstrap path answers with a `TokenResponse`, so
  the wizard's passkey step never needs the second ceremony M0-13 deviation 8 allows for. Worth
  settling in the API docs, since both halves currently hedge.

## Exit checks

```
$ cd rustak-ui && trunk build
2026-09-18T13:24:18.323185Z  INFO 🚀 Starting trunk 0.21.14
2026-09-18T13:24:18.327014Z  INFO 📦 starting build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.13s
2026-09-18T13:24:19.138158Z  INFO applying new distribution
2026-09-18T13:24:19.139117Z  INFO ✅ success

$ cd .. && cargo build -p rustak-server
   Compiling rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.32s

$ cd e2e && npm ci
added 6 packages, and audited 7 packages in 779ms
found 0 vulnerabilities

$ npx playwright install chromium
Downloading Chrome for Testing 153.0.8010.12 (playwright chromium v1243) from
https://cdn.playwright.dev/builds/cft/153.0.8010.12/mac-arm64/chrome-mac-arm64.zip
Error: Request to https://cdn.playwright.dev/… timed out after 600000ms
Failed to install browsers
```

**`npx playwright install chromium` cannot run in this environment**: `cdn.playwright.dev` and
`storage.googleapis.com` are both unreachable from here (`curl -sI` returns nothing; the npm
registry over the same path returns `200`), with and without the sandbox. It is not a change this
brief made and it will work in CI, which reaches the CDN. The run below therefore used the
Chrome for Testing 151 already in this machine's `ms-playwright` cache, through the
`RUSTAK_E2E_CHROMIUM` knob added to `playwright.config.ts` for exactly this case — Playwright
1.63 drove it without complaint, including the whole CDP `WebAuthn` domain. **CI leaves that
variable unset and uses Playwright's own browser.**

```
$ RUSTAK_E2E_CHROMIUM="…/chromium-1234/…/Google Chrome for Testing" npx playwright test
[WebServer] [e2e] server binary: /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/debug/rustak
[WebServer] [e2e] workspace:     /var/folders/…/T/rustak-e2e-18446
[WebServer] [e2e] setup token:   /var/folders/…/T/rustak-e2e-18446/setup-token
[WebServer] [e2e] listening on:  http://localhost:18446 (bound 127.0.0.1:18446)

Running 12 tests using 1 worker

  ✓   1 [setup] › tests/setup.spec.ts:29:1 › the first-run wizard turns a token on disk into an administrator who can sign in (1.1s)
  ✓   2 [setup] › tests/setup.spec.ts:122:1 › the wizard closes itself for good once it has been completed (12.2s)
  ✓   3 [chromium] › tests/auth.spec.ts:30:1 › a browser holding no passkey for this server cannot sign in, and is not told why (502ms)
  ✓   4 [chromium] › tests/auth.spec.ts:49:1 › a passkey registered for one host is refused at another (2.4s)
  ✓   5 [chromium] › tests/auth.spec.ts:86:1 › an administrator signs in with a passkey, and signing out ends the session (3.3s)
  ✓   6 [chromium] › tests/navigation.spec.ts:44:1 › every destination in the navigation strip opens the page it names (4.1s)
  ✓   7 [chromium] › tests/navigation.spec.ts:54:1 › a deep link into the console is served by the single-page fallback (4.4s)
  ✓   8 [chromium] › tests/navigation.spec.ts:67:1 › an address nothing matches reaches the application's own not-found page (4.2s)
  ✓   9 [chromium] › tests/navigation.spec.ts:81:1 › the landing page gets out of the way of somebody already signed in (1.2s)
  ✓  10 [chromium] › tests/smoke.spec.ts:15:1 › robots.txt is served before the SPA catch-all (20ms)
  ✓  11 [chromium] › tests/smoke.spec.ts:27:1 › the API reports its own health, and says nothing about the storage behind it (6ms)
  ✓  12 [chromium] › tests/smoke.spec.ts:42:1 › the application boots and renders (1.1s)

  12 passed (1.1m)

$ npm run typecheck
> tsc --noEmit
(no output, exit 0)
```

The scratch directory is gone afterwards (`ls "$TMPDIR"rustak-e2e-*` → no matches), so the run
leaves no database, no encryption key and no CA private key behind.

That the wizard spec genuinely drives the server, rather than passing on a page that happened to
render, was confirmed against the server's own log (`RUSTAK_E2E_SERVER_LOG=1`):

```
The first-run wizard created the first administrator. username=avery
auth.passkeys.register.finish: Registered a passkey. passkey=1
Recorded the server's identity from the setup wizard. domains=["localhost"]
The first-run wizard has been completed; its routes are closed.
```

## Notes for the orchestrator and the briefs that follow

- **Two server-side defects are waiting on owners**: the wizard's CA step (§1) and discoverable
  passkey sign-in (§2). Both are worked around here with the work-around asserted, so a fix is
  visible as a failing e2e test rather than as silence.
- **`tests/helpers.ts` is the seam for M2 fixtures.** `bootstrapAdmin`/`signIn` give any new spec
  an administrator session in two lines; `uniqueName` is there for the `afterEach` purge that
  device, credential and mission specs will need once those pages exist.
- **`e2e` now has a `typecheck` script.** It is not wired into `.github/workflows/rust.yml`
  (that file is not this brief's); adding `npm run typecheck` to the `e2e` job is a one-line
  follow-up and would have caught the one mistake this brief made that the runner did not.
- **The `chromium` project depends on `setup`.** A spec added to `e2e/tests/` is picked up by the
  `chromium` project automatically; a second one-shot spec would need its own project, or to be
  folded into `setup.spec.ts`.
- **`docs/ci.md`'s "Running the checks locally" block says `npm install`**; the workflow and this
  README both say `npm ci`. Worth aligning when `docs/` is next touched.
