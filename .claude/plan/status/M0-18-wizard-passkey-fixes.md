# M0-18 — the wizard's CA step and resident passkeys — complete

Brief: `.claude/plan/briefs/M0-18-wizard-passkey-fixes.md`
Read first: `conventions.md`; status files `M0-11` (passkeys + setup API), `M0-12` (where the CA is
created), `M0-13` (wizard/login pages), `M0-14` §"Things that had to be worked around" (the two
defects and the exact work-arounds removed here).

No `git`/`but` commands were run.

## The two defects, and what each one is now

### 1. The wizard's authority step dead-ended on every installation

`runtime::listen` calls `pki::load_or_create_root_ca` before it binds anything — it has to, because
the certificate the public listener presents is issued by that authority — so `has_ca` is already
true the first time a browser can reach `/setup`. `POST /setup/ca` answered `409` for that, and the
UI only advanced on `Ok`, so **the linear walk could not be completed by clicking through it**.

Fixed by making the step idempotent rather than by moving the CA's creation, which is M0-12's
(option (b) of the three M0-14 suggested, plus (c) in the UI):

- `POST /api/v1/setup/ca` no longer refuses. It calls `pki::load_or_create_root_ca`, which already
  adopts an existing record, and answers `200` with the `CaSummary` either way. The requested
  common name and key type therefore apply **only** when there is nothing to adopt — it will not
  replace the authority every enrolled device trusts, it just stops pretending that being asked is
  an error. The audit action is `setup.ca` when it created one and `setup.ca.adopted` when it did
  not, because those are different facts to whoever reads the log.
- **New: `GET /api/v1/setup/ca`** (administrator, in the guarded scope, `410` once the wizard is
  closed like every other `/setup/*` route, `404` when there is genuinely no authority). The step
  has to be able to *show* what is there before offering to make anything, and a `POST` whose only
  purpose was to read would be a lie about what the button does.
- `CaSummary` gained `certificate_pem: Option<String>` (`skip_serializing_if`, so the wire is
  unchanged for a caller that does not get one). It is the public half — it is handed to every
  device that enrols — and it is what an operator actually needs at that moment.
- The UI's authority step (`rustak-ui/src/pages/setup/ca.rs`, split out of `server.rs`) asks
  `GET /setup/ca` on mount. With an authority it shows the subject, the **SHA-256 fingerprint** an
  operator is meant to compare, the validity window, a `download="ca.crt"` link built from the PEM,
  and **Continue**. With none it shows the create form exactly as before. A failed read is a
  warning above the create form rather than a dead end, because the `POST` behind it is idempotent
  and will hand back whatever is really there.

### 2. A freshly registered passkey could not sign in

`webauthn-rs` 0.5's `start_passkey_registration` hard-codes `require_resident_key(false)` →
`residentKey: "discouraged"`, and the sign-in prompt only ever ran the **discoverable** ceremony
(empty `allowCredentials`), because it has no username field. An authenticator that honours the
flag — a security key, or Chromium's virtual authenticator — therefore held a credential the prompt
could never find.

- `auth::passkeys::require_discoverable` rewrites the emitted options to
  `residentKey: "required"` / `requireResidentKey: true`. `required` rather than `preferred`
  because the two are identical in every major browser and `required` is the one that fails at
  *creation* rather than producing a credential that cannot sign in.
- The login page keeps the username-less ceremony as the primary button and gains a folded-away
  **"Sign in with a username instead"** (`NamedPasskey` in `rustak-ui/src/pages/login.rs`), which
  runs `POST /auth/passkey/login/start` with a `username`. The server side of that already existed
  (M0-11); nothing was needed in `rustak-api`.
- `docs/deployment.md`'s "First-run setup and sign-in" gained two paragraphs: what
  `residentKey: "required"` means for a security key (PIN set, a free credential slot, and an
  outright refusal rather than an unusable passkey when there is none), and what the username
  fallback is for.

## Deviations from the brief, and why

1. **`userVerification` stays `required`.** The brief asked for `preferred`. Both sign-in
   ceremonies in `webauthn-rs` 0.5 — `start_passkey_authentication` and
   `start_discoverable_authentication` — pin `UserVerificationPolicy::Required`, and
   `webauthn-rs-core`'s assertion check refuses an unverified assertion when *either* the
   registration or the authentication policy was `Required`. Registering with `preferred` would let
   an authenticator that does no user verification register happily and then be refused at every
   attempt to use the credential — a worse failure than the one being fixed, and one that only
   shows up later. Changing it properly means changing the authentication policy too, which is a
   security decision beyond this brief.

2. **`residentKey` is patched on the serialised options, not asked for.** `webauthn-rs` 0.5's
   high-level `Webauthn` exposes no way to set it (only
   `start_google_passkey_in_google_password_manager_only_registration`, which is feature-gated and
   also forces `authenticatorAttachment: "platform"`), and `webauthn-rs-core`'s builder is behind a
   private field. Patching the JSON is safe *because* the stored ceremony state carries the same
   flag and nothing at verification time reads it — `webauthn-rs-core`'s `register_credential`
   destructures `require_resident_key` as `_` — so the two cannot disagree about anything that
   matters. Both facts are written down at the function.

3. **Two files outside the brief's list were touched**, both unavoidable and both uncontested:
   `rustak-server/src/web/api/mod.rs` (one route for `GET /setup/ca`, plus its row in the
   `PROTECTED` table the "nothing behind the gate answers without a session" test walks) and
   `rustak-ui/src/{api/setup.rs,fixtures/{data,store}.rs}` (the client for that route, and the demo
   store's `ca()` — the demo `init_ca` is now idempotent too, so demo mode and the server agree).
   `rustak-ui/styles.scss` gained one `.auth-card__aside` block for the fallback's disclosure.

4. **`Step::resume_from` was left alone.** It still skips the authority step on a *resume* when
   `has_ca` is true. That is about resuming where the server got to, not about the linear walk,
   which now passes through the step and shows the fingerprint.

## The work-arounds that were deleted

| Where | What it was | What replaced it |
|---|---|---|
| `e2e/tests/setup.spec.ts` | Click "Create the authority", tolerate the `409` alert, `page.reload()` so `resume_from` skipped the step | Asserts there is **no** create button, that the existing authority is shown, that the fingerprint is 64 hex characters, that the download link is `download="ca.crt"` and its `data:` URI contains a PEM, then clicks **Continue** |
| `e2e/tests/helpers.ts` | `POST /setup/ca` fired and its status ignored ("tolerated, not asserted") | Asserted `200` |
| `e2e/tests/webauthn.ts` | `makeCredentialsDiscoverable()` — CDP `WebAuthn.addCredential` re-storing the credential with `isResidentCredential: true` | Removed. The inverse, `makeCredentialsUndiscoverable()`, took its place: it **manufactures** the situation the username fallback exists for, which is the only way to reach that path from a browser whose registrations are all discoverable |
| `e2e/tests/auth.spec.ts` | `expect(await makeCredentialsDiscoverable()).toBeGreaterThan(0)` before the sign-in | `expect(held.every(c => c.isResidentCredential)).toBe(true)` — nothing is done to the credential between registering it and signing in with it |

**New spec**: `a passkey the browser cannot offer on its own is reached by naming the account` —
registers a passkey, makes it non-discoverable through CDP, signs out, asserts the login page shows
**no** username field until asked, opens the fallback, types the username, and lands on the
dashboard. It also pins the property that matters: a username field is never the first thing the
sign-in prompt offers.

## Exit checks

```
$ cargo test -p rustak-server --features testing --lib -- \
      web::api::setup web::api::passkey auth::passkeys web::api::tests
running 33 tests
test auth::passkeys::tests::registration_asks_the_authenticator_to_store_the_credential ... ok
test auth::passkeys::tests::options_without_an_authenticator_selection_are_left_alone ... ok
test web::api::passkey::tests::a_registration_asks_for_a_credential_the_authenticator_will_keep ... ok
test web::api::setup::tests::the_wizard_creates_the_authority_once_and_adopts_it_afterwards ... ok
test web::api::setup::tests::the_authority_can_be_read_back_before_the_wizard_offers_to_make_one ... ok
test web::api::setup::tests::every_wizard_route_is_gone_once_it_has_been_completed ... ok
test web::api::tests::nothing_behind_the_gate_answers_without_a_session ... ok
…
test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 944 filtered out; finished in 0.87s

$ cargo test -p rustak-server --features testing        # whole crate
test result: FAILED. 974 passed; 1 failed; 2 ignored
    cot_store::writer::tests::control_traffic_never_reaches_the_history_segments
# Not this brief's: `rustak-server/src/cot_store/**` is another agent's in-flight tree.
# See "Blocked on concurrent work" below — a later run of the same command could not
# build the test binary at all, for the same reason.

$ cargo test -p rustak-api
test result: ok. 93 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cd rustak-ui && trunk build
2026-09-18T13:49:51 INFO 📦 starting build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.35s
2026-09-18T13:49:54 INFO ✅ success

$ cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.50s

$ cd e2e && npm run typecheck
> tsc --noEmit
(no output, exit 0)

$ RUSTAK_E2E_CHROMIUM="…/chromium-1234/…/Google Chrome for Testing" npx playwright test
Running 13 tests using 1 worker
  ✓   1 [setup] › setup.spec.ts:29 › the first-run wizard turns a token on disk into an administrator who can sign in (1.1s)
  ✓   2 [setup] › setup.spec.ts:124 › the wizard closes itself for good once it has been completed (5.1s)
  ✓   3 [chromium] › auth.spec.ts:31 › a browser holding no passkey for this server cannot sign in, and is not told why (502ms)
  ✓   4 [chromium] › auth.spec.ts:50 › a passkey registered for one host is refused at another (558ms)
  ✓   5 [chromium] › auth.spec.ts:86 › an administrator signs in with a passkey, and signing out ends the session (478ms)
  ✓   6 [chromium] › auth.spec.ts:140 › a passkey the browser cannot offer on its own is reached by naming the account (515ms)
  ✓   7 [chromium] › navigation.spec.ts:44 › every destination in the navigation strip opens the page it names (1.2s)
  ✓   8 [chromium] › navigation.spec.ts:54 › a deep link into the console is served by the single-page fallback (326ms)
  ✓   9 [chromium] › navigation.spec.ts:67 › an address nothing matches reaches the application's own not-found page (344ms)
  ✓  10 [chromium] › navigation.spec.ts:81 › the landing page gets out of the way of somebody already signed in (507ms)
  ✓  11 [chromium] › smoke.spec.ts:15 › robots.txt is served before the SPA catch-all (15ms)
  ✓  12 [chromium] › smoke.spec.ts:27 › the API reports its own health, and says nothing about the storage behind it (6ms)
  ✓  13 [chromium] › smoke.spec.ts:42 › the application boots and renders (299ms)

  13 passed (14.4s)
```

`npx playwright install chromium` still cannot reach `cdn.playwright.dev` from this machine
(M0-14 §"Exit checks"), so the run above used the cached Chrome for Testing through the
`RUSTAK_E2E_CHROMIUM` knob M0-14 added for exactly that. **CI leaves it unset.**

```
$ ./scripts/check-file-length.sh
(no output, exit 0)

# `check-file-length.sh` only walks git-tracked files, so the one new file was
# checked by hand with the same awk:
rustak-ui/src/pages/setup/ca.rs: 197 functional lines

# And every file this brief touched, all under the 300 limit:
rustak-api/src/setup.rs: 105          rustak-ui/src/pages/setup/server.rs: 162   (was 265)
rustak-server/src/auth/passkeys.rs: 265   rustak-ui/src/pages/setup/mod.rs: 200
rustak-server/src/web/api/setup.rs: 229   rustak-ui/src/pages/login.rs: 173
rustak-server/src/web/api/mod.rs: 103     rustak-ui/src/api/setup.rs: 36
rustak-server/src/web/api/passkey.rs: 284

$ cargo fmt -p rustak-api --check
(no output)

$ rustfmt --edition 2024 --check <every file this brief touched>
(no output — `cargo fmt -p rustak-server --check` reports `auth/{basic,cert}.rs`,
 which are another agent's new files, so it was run per file instead)

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-api --no-deps
    Finished `dev` profile; Generated target/doc/rustak_api/index.html

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server --no-deps --features testing
error: … marti/principal.rs:23, marti/principal.rs:67, jobs/retention.rs:5, stream/hub.rs:3
# Four rustdoc errors, all in other agents' in-flight trees. None in this brief's files —
# the one it introduced (`passkeys.rs` linking a private item from a public module doc) was
# fixed before this run.

$ cargo clippy -p rustak-api -p rustak-server --all-targets --features rustak-server/testing -- -D warnings
error: … cot_store/history.rs:151, web/api/extract.rs:142, web/api/subject.rs:146
# Same story: a `collapsible_if` in `cot_store`, and two test modules that stopped compiling
# when another agent changed `auth::resolve::Resolved::claims` to `Option<AccessClaims>` at
# 14:43. Nothing in this brief's files.
```

## Blocked on concurrent work (nothing here is this brief's to fix)

Three things in other agents' uncommitted trees stood between this brief and a clean
whole-workspace run. All are recorded so the orchestrator can tell them apart from anything M0-18
did.

1. **The e2e launcher cannot start the server** as the tree stands. `runtime::listen` now calls a
   new `stream_pki()` **unconditionally**, before checking whether the stream listener is enabled,
   and it issues a server certificate — so a configuration with `[stream.tls] enabled = false`,
   `[web.marti] enabled = false`, `[web.public.tls] mode = "none"` and no `[server] domains` (which
   is exactly `e2e/scripts/start-server.mjs`'s, and deliberately so: an empty `domains` is what
   makes the wizard's "Server name" step a step) dies at start-up with *"We cannot issue this
   server's own certificate without knowing what host name it is reached on."*

   The Playwright run above was obtained by adding `[pki] server_names = ["localhost"]` to the
   generated config — which satisfies `web::tls::server_names` without feeding
   `identity::settings::resolve().domains`, so `has_server_name` stays false and the wizard is
   unchanged — and that line was **reverted afterwards**, because papering over another brief's
   defect in this one's harness is the wrong place for it. Whoever owns `runtime.rs` should gate
   `stream_pki` on the stream listener being enabled; if instead the intent is that every
   installation issues a stream certificate at start-up, then `start-server.mjs` needs that line
   permanently and it is a one-line follow-up here.

2. **`cargo test -p rustak-server` cannot build its test binary.** `auth::resolve::Resolved::claims`
   became `Option<AccessClaims>`; `web/api/extract.rs:142` and `web/api/subject.rs:146` still
   construct it bare. Both files belong to the brief making that change. The 33-test run above was
   taken before it landed and re-taken after every change in this brief; the whole-crate run
   (`974 passed; 1 failed`) is from the same window.

3. **`cot_store::writer::tests::control_traffic_never_reaches_the_history_segments`** fails and
   `cot_store/history.rs` has a `collapsible_if`. That tree is another agent's.

## Notes for the orchestrator and the briefs that follow

- **`GET /api/v1/setup/ca` is wizard-scoped and goes away with the wizard**, like every other
  `/setup/*` route. The settings/PKI page that M2 will want — showing the authority, its
  fingerprint and its certificate *after* setup — needs its own route outside that scope.
  `summarise()` in `web/api/setup.rs` is the two lines it will want to share.
- **`CaSummary::certificate_pem` is `Option`** precisely so that a listing endpoint can leave it
  out; only the two wizard routes fill it in today.
- **The stream and Marti listeners will want the same resident-key decision applied to any other
  credential ceremony they add.** `require_discoverable` is the one place that knowledge lives, and
  it is a patch on `webauthn-rs`'s output — worth revisiting when `webauthn-rs` 0.6 stabilises,
  since its builder path is expected to expose `residentKey` directly.
- **M0-14's `.claude/plan/status/M0-14-e2e-specs.md` §§1–2** describe defects that no longer exist.
  The file is a record of what that brief found, so it has been left as written; this one is the
  answer to it.
