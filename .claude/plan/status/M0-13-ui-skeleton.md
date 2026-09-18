# M0-13 — Yew UI skeleton: status

## Summary

`rustak-ui/` is implemented per design 01 §6.4 with the plan's deltas applied (no local
passwords; `AuthMode::{Oidc, Passkey}`; passkey ceremonies through `navigator.credentials`;
`AuthStatus::NeedsSetup`). 47 source files, every one under the 300-functional-line limit
(largest is `pages/setup/server.rs` at 265). The API client, the OIDC popup flow, the
single-flight refresh, the `demo!` macro, the component kit and the stylesheet structure are
lifted from `../automate/ui/` and re-branded; the passkey module, the setup wizard, the
`NeedsSetup` gate and the fixtures are new.

All five exit checks pass, and every page was walked in a browser — in `?demo` against the
fixtures, and in the real (non-demo) code path against a throwaway `/api/v1` stub, so the two
halves of the client are both exercised. Outputs below.

Only files under `rustak-ui/` and this status file were touched. No `git`/`but` commands were run.

## Layout

```
rustak-ui/
├── Cargo.toml          web-sys gains SubtleCrypto (PKCE) and the WebAuthn types
├── Cargo.lock          regenerated (see "Deviations" 4)
├── Trunk.toml          proxies /api/v1, /oauth, /login, /Marti → 127.0.0.1:8446
├── index.html          unchanged from M0-01
├── styles.scss         6 sections, 1 427 lines: tokens · base · primitives · chrome · pages · responsive
└── src/
    ├── main.rs                             entry point and module list
    ├── app.rs                              Route, AuthStatus, AuthHandle, use_auth, switch
    ├── util.rs                             nav_href, urlencode, time formatting, initials
    ├── api/{mod,auth,setup,users,audit,settings,health}.rs
    ├── auth/{mod,oidc,passkey,single_flight}.rs
    ├── fixtures/{mod,data,store}.rs
    ├── components/{mod,admin_shell,app_bar,alert,page_title,status_pill,layout,helpers,secret_input}.rs
    │   └── form/{mod,input,choice,button}.rs
    └── pages/{mod,load,landing,login,auth_callback,dashboard,users,activity,settings,
        not_found,protected,stubs,demo}.rs
        └── setup/{mod,steps,admin,server}.rs
```

### Routes

`Landing "/"`, `AuthCallback "/auth/callback"`, `Setup "/setup"`, `AdminRoot "/admin"`,
`Dashboard "/admin/"`, then `Devices`, `Credentials`, `Users`, `Groups`, `Services`,
`Missions`, `Packages`, `Profiles`, `Activity`, `Settings` (all under `/admin/…`),
`#[cfg(debug_assertions)] DemoControls "/demo/controls"`, and `#[not_found] NotFound "/404"`.

`Route::heading()` gives the shell each page's title and subtitle, so the chrome is not
hard-coded to one page's name.

### Auth model

- `AuthStatus = Loading | NeedsSetup | SignedIn(Box<Me>) | NeedsLogin | Forbidden | Error(String)`.
- `use_auth` finishes any in-flight OIDC callback, then **resolves the setup status before
  `/me`**: a server that has never been set up answers `/me` with a 401 exactly like one whose
  session expired, and sending somebody to a login page they cannot use is worse than useless.
  A `setup/status` that errors falls through to the identity probe, which gives the better
  error of the two.
- `AuthHandle` carries `login` (OIDC popup), `login_passkey`, `signout` and `refresh` — the
  last so the wizard can hand the session it just established to the rest of the app.
- Storage keys are all `rustak.admin.*` in **sessionStorage** (`token`, `refresh`,
  `oidc_state`, `oidc_verifier`); the one `localStorage` slot is `rustak.admin.popup_result`,
  which a sign-in popup hands its tokens back through (popups do not share sessionStorage with
  their opener) and which is cleared the moment it is read.

### Passkeys (`src/auth/passkey.rs`, 186 functional lines)

`register(label, registration_token)` and `login(username)` drive `navigator.credentials`
through `web-sys`. The ceremony payloads stay opaque `serde_json::Value` (as
`rustak_api::passkey` intends); this module only moves them across the JavaScript boundary:

- **In**: `js_sys::JSON::parse` of the server's options, then base64url → `Uint8Array` for
  `challenge`, `user.id`, and the `id` of every entry in `excludeCredentials`/`allowCredentials`.
  `webauthn-rs` wraps its options in `publicKey`; a server that sends the inner dictionary on
  its own is accepted too. The result is cast to `PublicKeyCredential{Creation,Request}Options`
  and set on a `Credential{Creation,Request}Options`.
- **Out**: `ArrayBuffer` → base64url for `rawId`, `clientDataJSON`, `attestationObject`,
  `authenticatorData`, `signature`, `userHandle`, plus `getTransports()` — which is the exact
  JSON shape `webauthn-rs`'s `RegisterPublicKeyCredential` / `PublicKeyCredential` deserialise.
- Failures collapse to one message ("the passkey prompt could not … — it may have been
  dismissed"). The browser deliberately reports "cancelled" and "no matching credential"
  identically, because telling them apart would say whether an account has a passkey; repeating
  a guess here would undo that.

### Demo mode

`?demo` routes every call to `src/fixtures/`, through a `demo!` macro invoked as the first line
of each API function *and* of each passkey ceremony — so no page has a demo branch and none can
forget one. The store is mutable (`thread_local! RefCell`), so suspending a user, registering a
passkey and walking the wizard all stick for the tab. Everything below `is_demo()` is
`#[cfg(debug_assertions)]`; `is_demo()` is `const false` in release. Verified: no fixture string
(`Weather sidecar`, `Blue Team`, `MacBook Touch ID`, `demo-challenge`, `0.1.0-demo`) appears in
the release `.wasm`.

## Exit checks

```
$ cd rustak-ui && trunk build
2026-09-18T11:07:44.121705Z  INFO 🚀 Starting trunk 0.21.14
2026-09-18T11:07:44.122037Z  INFO 📦 starting build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.10s
2026-09-18T11:07:44.872536Z  INFO applying new distribution
2026-09-18T11:07:44.873591Z  INFO ✅ success

$ trunk build --release
2026-09-18T11:07:44.884037Z  INFO 🚀 Starting trunk 0.21.14
2026-09-18T11:07:44.884363Z  INFO 📦 starting build
    Finished `release` profile [optimized] target(s) in 0.09s
2026-09-18T11:07:46.752166Z  INFO applying new distribution
2026-09-18T11:07:46.753046Z  INFO ✅ success

$ cargo clippy --target wasm32-unknown-unknown -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.17s

$ cargo fmt --check
(clean, no output)
```

`cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings` and
`cargo build --release --target wasm32-unknown-unknown` are also clean — the release build
matters separately because the demo branches vanish there, which is what first produced nine
unused-import warnings (now fixed by importing the `demo!` macro and the `fixtures` module
separately).

Release bundle: `rustak-ui-*_bg.wasm` 1 441 826 B, `styles-*.css` 20 319 B, `*.js` 56 595 B.

### File lengths

The repository script iterates `git ls-files`, so it does not yet see this brief's files (they
are untracked until the orchestrator commits). Running its own `awk` over every file in the
crate reports nothing over the limit; the longest twelve:

```
rustak-ui/src/pages/setup/server.rs      265
rustak-ui/src/pages/setup/admin.rs       244
rustak-ui/src/pages/settings.rs          226
rustak-ui/src/app.rs                     213
rustak-ui/src/pages/demo.rs              209
rustak-ui/src/pages/setup/mod.rs         198
rustak-ui/src/auth/passkey.rs            186
rustak-ui/src/components/form/input.rs   159
rustak-ui/src/pages/users.rs             152
rustak-ui/src/auth/oidc.rs               150
rustak-ui/src/pages/dashboard.rs         139
rustak-ui/src/api/mod.rs                 138
```

(`src/fixtures/*` is exempt by the script's own rule; the largest there is `data.rs` at 303.)

## Walkthrough

`trunk serve` on `:8081`, driven in a real browser at 1280×900 and again at 375×812.

### `?demo` — every page rendered

| Page | What rendered |
|---|---|
| `/?demo` | Landing redirects straight into `/admin` (demo starts signed in), as designed. |
| `/admin?demo` | **Dashboard**: three cards — Server (green "Healthy" + "Database: Healthy" pills, version `0.1.0-demo`, uptime `1d`), Identity (name, host names, base URL, node id), People (4 accounts / 1 administrator / 1 suspended, with "Cannot sign in or connect."); then Recent activity, six audit rows with outcome pills, and a "See everything" link. Refresh button in the title row. |
| `/admin/users?demo` | Four rows — Avery Quinn (Administrator, both buttons **disabled**, titled "You cannot suspend your own account."), Bhavna Rao (Active, Promote/Suspend), Cormac Doyle (Suspended, Promote/Restore), Weather sidecar (service, Promote disabled). Clicking Suspend writes through the demo store and the list reloads. |
| `/admin/activity?demo` | Eleven category chips (Everything + all ten `AuditCategory` values) over eight rows: relative time, category, monospace action, actor → subject, message, outcome pill. The `Denied` row is amber, the `Failed` row red. |
| `/admin/settings?demo` | "This server" definition list (name, host names, base URL, node id, set-up date) and "Your passkeys" — two rows with last-used, a Remove per row, and a label field + "Register a passkey". Clicking it added a third ("This device") through the demo ceremony. |
| `/admin/devices`, `/credentials`, `/groups`, `/missions`, `/packages`, `/profiles`, `/services` | Stubs: the route's own title and subtitle, then "Arrives in M2/M2/M2/M4/M3/M3/M6" and a sentence saying what will be there. |
| `/setup?demo` | Wizard. Opens on "This server is set up" (410 equivalent) with "Open the console"; the debug-only "Start the wizard again" resets the demo store. Then walked all six steps — see below. |
| Login | Reached by Sign out: the shell keeps its bar and nav, and the card offers **both** "Sign in with single sign-on" and "Sign in with a passkey" (fixture metadata is `Oidc` + `passkeys_enabled`). The passkey button signed the demo session back in. |
| `/demo/controls?demo` | Gallery: eight button variants + a group; every `AuditOutcome` and `ComponentStatus` pill; four alerts (one dismissible with an action); Field/TextInput/invalid TextInput/TextArea/NumberInput/Select/SecretInput/Switch/disabled; Stats; LoadingNote and EmptyState; a centred auth card. |
| `/nope` | "Page not found" with "Back to the start". |

**Wizard, end to end (demo):** step 1 accepts a masked setup token → step 2 refuses
`Not A Username!` inline with `rustak-api`'s own message ("A username cannot contain ' '…") and
keeps the submit disabled; submitting with a too-short token showed "That setup token was not
accepted…" and a "Change the setup token" button back to step 1 → step 3 registered a passkey
for `avery` → step 4 saved "Example TAK" / `tak.example.com, 203.0.113.24` → step 5 created the
CA (RSA 2048 selected) → step 6 "Finish setup" → "This server is set up" + "Open the console",
which landed on the dashboard.

### Non-demo — the real client against a stub `/api/v1`

A throwaway Python stub on `:8446` (scratchpad only, not in the repo) answered the M0-11 routes
so the network paths could be walked too:

- `GET /` → **Landing** with an enabled "Sign in" (`setup/status` said complete, `/me` said 401
  → `NeedsLogin`). Request log confirms `GET /api/v1/setup/status` then `GET /api/v1/me`.
- The first 401 triggered `POST /api/v1/auth/refresh` **once** (a stale `rustak.admin.refresh`
  from the demo run was still in sessionStorage); the stub 404'd it, the session was cleared and
  the original 401 surfaced — the refresh-once path, exactly as designed.
- `/admin` → **Login** card; "Sign in with a passkey" fetched
  `POST /api/v1/auth/passkey/login/start`, converted the options, called
  `navigator.credentials.get()`, and — with no authenticator attached — surfaced "the passkey
  prompt could not complete — it may have been dismissed". So the whole conversion path runs
  without throwing.
- With the stub switched to `needs_setup: true`: `/admin` shows "This server has not been set
  up yet" with "Open the setup wizard", and `/` redirects to `/setup`, which opens on step 1
  with no demo banner.
- `/auth/callback` with no `?code` resolves and continues into the console, as it should.

### Responsive

At 375×812 the nav wraps to three rows, the brand name and user meta hide, the title row
stacks, and user rows collapse to name / meta / pill / buttons. One fix came out of this: a
`.status-pill` is a grid item, so it stretched to the full column and read as a banner —
`justify-self: start` in the mobile block.

## Deviations, and why

1. **`components/form/` is a directory** (`mod.rs` + `input.rs` + `choice.rs` + `button.rs`).
   automate's `form.rs` is 528 lines; splitting it by what the control *is* keeps every file
   well inside the limit without dropping a control from the kit.
2. **`Button`/`ButtonGroup` take `children: Html`, not `Children`.** Yew reads a component
   whose body is a single braced expression as "this expression *is* the children prop", so
   `<Button>{ if disabled { "Restore" } else { "Suspend" } }</Button>` — which is what a button
   whose label depends on state has to write — does not type-check against `Children`.
3. **`authenticatorAttachment` is read with `js_sys::Reflect`.** `web-sys` only exposes its
   accessor behind `--cfg=web_sys_unstable_apis`, and turning that on for the whole crate is a
   large door to open for one optional string. `PublicKeyCredential::toJSON` and
   `parse*OptionsFromJSON` are gated the same way, which is why the conversions are written out
   rather than delegated to the browser.
4. **`Cargo.lock` lost `getrandom 0.4.3` and `r-efi`.** Not a change made here: M0-03 dropped
   `uuid`'s `v4` feature from `rustak-api`, and this crate's lock had not been regenerated
   since. The first build in this brief did it.
5. **The demo reset does not reload the page.** The demo store lives exactly as long as the
   page does, so reloading after `reset_setup()` would put back the installation it had just
   cleared. The wizard re-reads the status through a generation counter instead.
6. **The wizard's admin step has a "Change the setup token" button.** The token step and the
   admin form feed one request, so a token the server refuses is only discovered on step 2;
   without a way back the only remedy was reloading the page.
7. **The navigation is a strip below the app bar**, not beside the brand. Eleven destinations
   do not fit in a 60px row alongside a user chip, and pushing the chip off the end is worse
   than a second row.
8. **`/api/v1/auth/passkey/register/finish` is read loosely.** If the response parses as a
   `TokenResponse` the wizard uses it; if not, it runs a login ceremony to get one. M0-11 does
   not say which the endpoint returns, and both are defensible — this way either works.

## Notes for M0-11

The client calls exactly these paths, all relative to `/api/v1`:

| Method | Path | Body / notes |
|---|---|---|
| `GET` | `/health` | `Health` |
| `GET` | `/auth/metadata` | `AuthMetadata`; the login page reads `passkeys_enabled`, not `mode`, to decide whether to offer the passkey button |
| `POST` | `/auth/token` | `TokenExchangeRequest` (with `code_verifier`) → `TokenResponse` |
| `POST` | `/auth/refresh` | `{"refresh_token": …}` → `TokenResponse` |
| `POST` | `/auth/logout` | no body; failure is ignored |
| `GET` | `/me` | `Me`; **200 or 401**, never 204 |
| `POST` | `/auth/passkey/register/{start,finish}` | `PasskeyRegistrationStart` → `PasskeyChallenge`; `PasskeyRegistrationFinish` → `TokenResponse` *or* anything else (deviation 8) |
| `POST` | `/auth/passkey/login/{start,finish}` | `PasskeyLoginStart` → `PasskeyChallenge`; `PasskeyLoginFinish` → `TokenResponse` |
| `GET` | `/auth/passkeys` | `Vec<PasskeySummary>` — **not in the M0-11 brief**; the settings page lists the signed-in user's own passkeys |
| `DELETE` | `/auth/passkeys/{id}` | **not in the M0-11 brief**; removes one |
| `GET` | `/setup/status` | `SetupStatus`, public |
| `POST` | `/setup/{admin,server,ca,complete}` | `CreateAdminRequest`→`AdminCreated`, `ServerSettingsRequest`→`ServerSettings`, `InitCaRequest`→`CaSummary`, `{}`→204 |
| `GET` | `/users`, `PATCH /users/{username}` | `Vec<User>`, `UserPatch`→`User` |
| `GET` | `/audit?limit&category` | `Vec<AuditRecord>`, newest first; `category` is the `AuditCategory` wire string |
| `GET` | `/settings` | `ServerSettings` |

Other expectations:

- Errors are `{"error": …}`; **410** is mapped to its own `ApiError::Gone` so the wizard can say
  "this server has already been set up" rather than showing a generic failure.
- The `challenge` and the user/credential ids inside `PasskeyChallenge.options` must be
  **base64url strings**, which is what `webauthn-rs` serialises; the client decodes them.
  A `publicKey` wrapper is expected but not required.
- The WebAuthn relying-party id must match the host the console is served from. In development
  that is `127.0.0.1:8081` behind Trunk's proxy, so a server configured for
  `tak.example.com` will refuse a dev-server ceremony — worth a line in the dev docs.

## Open items

1. `GET /auth/passkeys` and `DELETE /auth/passkeys/{id}` are used by the settings page but are
   not in the M0-11 brief. Either add them there or the card degrades to an error (it does so
   gracefully: "We could not list your passkeys").
2. `uuid = { version = "1", features = ["js"] }` in this crate's manifest is currently **inert**,
   because M0-03 dropped `v4` from `rustak-api`. It is kept so that enabling `v4` later cannot
   silently reach for OS randomness that wasm does not have. Worth revisiting with M0-03's own
   open item 1.
3. The e2e suite (M0-14) can rely on: `?demo` making every page render with no server, a
   stable `TrunkApplicationStarted` event, and the debug-only `/demo/controls` gallery. The
   wizard's passkey step needs Playwright's virtual authenticator, as plan.md anticipates.
4. `styles.scss` has no dark theme. automate has none either; it is worth a brief of its own
   rather than a half-done pass here.
