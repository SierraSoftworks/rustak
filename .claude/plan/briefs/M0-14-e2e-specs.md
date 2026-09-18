# M0-14 — e2e specs (Playwright)

**Goal:** complete the `e2e/` harness (skeleton from M0-02) per design 01 §7.1: `start-server.mjs` generating a config with `allow_insecure_http = true`, `[web.public.tls] mode = "none"`, `user_acl`/`admin_acl` `'true'`, and a pre-seeded setup token file; `helpers.ts` (`waitForApp`, `bootstrapAdmin` via setup API + Playwright **virtual authenticator** (CDP `WebAuthn.enable`) to register/login a passkey, `signIn`), specs `smoke`, `setup` (wizard end-to-end incl. passkey registration), `auth` (passkey login, sign-out), `navigation`. Runs against the debug UI bundle + debug server.

**Read first:** conventions; design 01 §7.1; `../automate/e2e/**` (lift patterns); M0-11/M0-13 status files for the exact endpoints/pages. Depends on M0-12 and M0-13.

**Files you own:** everything under `e2e/`. No `git`/`but` writes.

**Exit checks:** `cd rustak-ui && trunk build && cd .. && cargo build -p rustak-server && cd e2e && npm ci && npx playwright install chromium && npx playwright test` green locally; paste the report summary.

**Status file:** `.claude/plan/status/M0-14-e2e-specs.md`.
