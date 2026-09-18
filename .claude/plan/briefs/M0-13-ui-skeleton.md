# M0-13 — Yew UI skeleton

**Goal:** `rustak-ui/` per design 01 §6.4: router (`Landing`, `AuthCallback`, `Setup`, `AdminRoot`, `Dashboard`, `Devices`, `Credentials`, `Users`, `Groups`, `Services`, `Missions`, `Packages`, `Profiles`, `Activity`, `Settings`, debug-only `DemoControls`, `NotFound`), `AuthStatus` with `NeedsSetup`, `use_auth` resolving setup status before `/me`; API client lifted from `../automate/ui/src/api.rs` (bearer, refresh-once, `demo!` macro, sessionStorage keys `rustak.admin.*`), `auth.rs` lifted (popup OIDC + PKCE `code_verifier` generation; **passkey login** via `navigator.credentials` through `web-sys` — `PublicKeyCredential` request/creation with the server's start/finish endpoints); fixtures/demo mode; components lifted (`admin_shell`, `app_bar`, `alert`, `form`, `page_title`, `status_pill`, `layout`, `secret_input`, `helpers`); pages: `landing`, `login` (SSO button and/or passkey button per `AuthMetadata`), `auth_callback`, `setup` (wizard: setup token → admin username → register passkey → server name/hostname → CA → done), `dashboard`, `users`, `activity`, `settings`, `not_found`, `protected`, and "arrives in M{n}" stubs for the rest; `styles.scss` sections per design.

**Read first:** conventions; design 01 §6.2–6.4; research 01 §6 (automate UI map); `../automate/ui/src/**` (lift liberally; automate's `styles.scss` structure). Depends on M0-03 DTOs and the M0-11 API (develop against `?demo` fixtures until M0-11 lands; the fixtures must cover every page).

**Files you own:** everything under `rustak-ui/`. Files < 300 functional lines (split pages/components accordingly). No `git`/`but` writes.

**Exit checks:** `cd rustak-ui && trunk build` (debug and `--release`), `cargo clippy --target wasm32-unknown-unknown -- -D warnings`, `cargo fmt --check`, `trunk serve` + `?demo` renders every page (describe what you verified), file-length script.

**Status file:** `.claude/plan/status/M0-13-ui-skeleton.md`.
