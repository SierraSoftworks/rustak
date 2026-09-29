# M10-12: Deploying a plugin is one console step (service-account onboarding). Complete

Brief: `.claude/plan/briefs/M10-12-service-onboarding.md` (wave rules: `M10-00-wave-rules.md`).

## What was built

`POST /api/v1/service-onboarding` is one administrative action that does the following:

1. Creates the service account if it does not exist.
2. Mints a one-time enrolment token for it.
3. Mints a service token when the account holds no live one.

It answers once with both secrets, the `[service]` configuration fragment, the environment lines
(`RUSTAK_ENROLLMENT_TOKEN=…`, `RUSTAK_SERVICE_TOKEN=…`, with the expiry stated in a comment) and a
list of plain sentences saying what it did. Settings → Services has a new **Add a service** card
that calls it. The card's result panel shows the four values with copy buttons and the expiry.

### Is a service token still needed? Yes. What I found

A sidecar that enrols gets a client certificate. That certificate authenticates the CoT stream and
the Marti API. **It cannot reach the control API.** `/api/v1` (which includes `/services/*` and
`/events`) is mounted only on the public listener. `pki::tls::server_config` builds that listener
with `public_server_config`, which has **no client-certificate verifier**, and `on_connect_capture`
is installed only on the Marti server. So no `PeerCertificate` ever reaches `plugins::auth` there;
its own module doc says so. The Marti listener does not mount `/api/v1`. Without a service token, a
sidecar deployed this way would stream, but it could not register, heartbeat, read its
configuration or open the event feed. It would never appear on the Services page. The action
therefore mints a service token, but only when the account has no live one (see "Repeating", below).

**Stale claim elsewhere (not mine to change):** `docs/plugins.md` → "Three credentials" says "the
client certificate authenticates everything (and the control API too, so a sidecar that has enrolled
needs no token at all)". Per the above, that is not true today. The `rustak-client::control` module
doc has the same claim ("lets the certificate speak for itself when one is not"). It needs either a
docs fix or a public-listener change. It is listed under open questions.

## Decisions

- **Path.** The path is `/api/v1/service-onboarding`, not something under `/services`. `/services/*`
  is the control API, mounted *outside* the session gate by `services::routes`, and an
  administrative action has no business being matched there. The new path sits behind `api_auth` and
  takes `Administrative`.
- **Request.** `{ name, account?, rotate_service_token? }`. `name` must parse as a `ServiceName` and
  the account (default: the name) as a `Username`. Both are taken as strings and parsed in the
  service layer, so a bad value is a readable `400`, not serde's text.
- **Refusals.** `400` for an invalid name or account. `409` for:
  - a **person's** account;
  - a **switched-off** service account;
  - a service name **already registered to another account** (the sidecar would otherwise be
    refused at registration with credentials in hand).
  `403` for a non-administrator, and `401` without a session (added to the `PROTECTED` route
  table).
- **Atomicity by compensation, not one transaction.** Argon2 hashing sits between the writes, and
  the existing repos each open their own `db.write`, so one transaction would have meant new SQL in
  repo files outside this brief. Instead:
  - Each credential is recorded as it lands.
  - On any failure, an account this call created is **deleted** (credentials cascade).
  - On an existing account, the credentials this call minted are **revoked**.
  - Undo failures are logged at `error` and recorded on the session. The original failure is what
    the caller gets (`500`).
- **Repeating.** Asking again for an existing service account:
  - It mints a fresh enrolment token and says so (`account_created: false`, and a note saying "…
    already existed … Its certificates and any earlier enrolment token were left alone").
  - It **keeps** the live service token (`service_token_outcome: "kept"`, no `service_token` in the
    body, and the environment says to keep the existing one).
  - It revokes nothing.
  - Replacing the token needs the explicit `rotate_service_token: true`. That mints a new token,
    then revokes the old ones (`"rotated"`, with `revoked_service_tokens` listing them). It also
    invalidates the account's open event feeds. It deliberately does **not** call
    `sessions::end_all`, because that would also drop the sidecar's CoT stream, which its untouched
    certificate still entitles it to.
- **Rate limiting.** The neighbouring credential-minting actions (`POST /api/v1/credentials`,
  `POST /users/{u}/cloudtak-onboarding`) have no limiter of their own. Their only bound is the
  administrator session, whose issuing routes are rate limited. This action matches them exactly,
  and the route's doc comment says so. The shared `RateLimiter` counts failures for guessing and
  does not throttle successful actions, so bolting it on would not have meant "like the neighbours".
- **Secrets.** Both secrets are shown once and stored only as argon2id hashes through the existing
  `credentials::mint`. `ServiceOnboarding`'s `Debug` redacts both tokens and the environment block.
  The audit entry `service.onboarding.created` (category Administration, actor and subject set)
  records:
  - the service;
  - whether the account was created;
  - the enrolment-token row;
  - the service-token outcome and row;
  - the revoked rows.

  It never records a secret, and a test asserts that.
- **Fragment and environment are composed in `rustak-api`.** They are pure string functions, so the
  server and the UI's demo fixture produce the same text. The fragment writes `account` only when it
  differs from the name. It writes `token = "${{ env.RUSTAK_SERVICE_TOKEN }}"` and never a value,
  and has a commented `pki_dir` line. It deliberately has no `[server]` section: the action cannot
  know which host and ports a deployment reaches the server on.

## Files

Added:
- `rustak-api/src/service_onboarding.rs`: `ServiceOnboardingRequest`, `ServiceTokenOutcome`,
  `ServiceOnboarding` (redacting `Debug`), `config_fragment`, `environment`, and the env-var names.
- `rustak-server/src/identity/service_onboarding/mod.rs` (252 functional lines): the flow, the
  refusals, and the undo.
- `rustak-server/src/identity/service_onboarding/report.rs` (67): the notes and the audit entry.
- `rustak-server/src/web/api/service_onboarding.rs` (22): the route.
- `rustak-server/tests/service_onboarding.rs`: 11 integration tests.
- `rustak-ui/src/api/service_onboarding.rs`: the client call.
- `rustak-ui/src/pages/service_onboarding.rs` (186): the `AddService` card and the result panel,
  plus 2 unit tests.
- `e2e/tests/service-onboarding.spec.ts`: 3 Playwright specs against the **real** API. No CA is
  needed, so no `?demo`.
- this status note.

Changed. The **shared registries are one added line each**, for the merge with M10-11:
- `rustak-api/src/lib.rs`: `pub mod` + one `pub use` line.
- `rustak-server/src/identity/mod.rs`: one `pub mod` line.
- `rustak-server/src/web/api/mod.rs`: one `pub mod` line, one `.configure(...)` line, and one
  `PROTECTED` entry.
- `rustak-ui/src/api/mod.rs`: one `pub mod` line.
- `rustak-ui/src/pages/mod.rs`: one `mod` line.
- `rustak-ui/src/pages/services.rs`: an import, `<AddService />` above the list, and the
  empty-state sentence (it said there was "nothing to add here by hand").
- `rustak-ui/src/fixtures/services.rs`: a `service_onboarding` demo fixture (thread-local, so a
  second ask shows "kept"). This is not a shared registry; it is appended before the tests.
- `rustak-ui/styles.scss`: a `.service-onboarding` block inside §5.12 (Services), not at the end of
  the file.
- `docs/plugins.md`: a new "Deploying one: one step in the console" subsection under "Running one",
  before "The first start". It gives the one-step path first, then "By hand" (the manual steps).
  Nothing else in the file was touched.

## Tests, and how they behave on a host ten times slower

`rustak-server/tests/service_onboarding.rs`:
- `an_administrator_adds_a_service_in_one_step` checks:
  - the `200` and the exact JSON key set;
  - that the account is kind `service` and holds exactly one enrolment token and one service
    token;
  - that the hashes do not contain the secrets;
  - that both secrets verify for their purpose (`ServiceApi`, `Enrollment`);
  - the fragment and environment contents, including the expiry;
  - that the audit row names the actor and contains no secret.
- `only_an_administrator_may_add_one`: `403`, and nothing is created.
- `a_persons_account_is_refused_and_given_nothing`: `409` whether the person's name is the service
  name or the `account`.
- `a_name_that_cannot_be_a_service_is_a_bad_request`: five bad names and one bad account.
- `a_switched_off_service_account_is_refused`: `409`.
- `a_name_registered_to_another_account_is_refused`: `409`.
- `asking_again_mints_a_fresh_enrolment_token_and_revokes_nothing`.
- `replacing_the_service_token_is_explicit_and_revokes_the_old_one`: the old token fails
  verification and the new one passes.
- `a_failure_minting_leaves_no_account_behind`: a SQLite trigger refuses (a) every credential insert
  and (b) only the service-token insert, which is the *second* mint. Either way the answer is `500`
  and no account remains.
- `a_failure_on_an_existing_account_takes_back_what_this_call_minted`: the account survives, and
  the enrolment token minted before the failure is revoked.
- `a_sidecar_enrols_and_registers_with_exactly_what_the_action_returned`, the end-to-end harness
  test. It covers:
  - the action is called **over HTTP** with an administrator session;
  - the returned fragment is written verbatim, plus a `[server]` section;
  - the returned environment text is written verbatim as the `.env`;
  - `rustak_client::sidecar::serve` then enrols (the certificate is written), connects to the real
    mTLS stream, and **registers with the service token**, and the registration's `user_id` is the
    onboarded account.

Timing: none of these asserts an upper time bound. Every assertion is about state (rows, statuses,
files). The waits are the shared `EXPECT` (5 s) used only to fail a hung wait, as in
`sidecar_enrolment`. On a host ten times slower, the enrolment and registration of the end-to-end
test take longer. The 20 ms poll keeps polling up to `EXPECT`, which is the same margin
`sidecar_enrolment` already relies on. If that ever proves tight on CI, it is `stream_support::EXPECT`
to raise, not an assertion to relax.

UI unit tests (`pages::service_onboarding`, 2): the request builder (a blank account means none)
and the token-outcome wording. e2e (3): the full flow with the copy buttons, the expiry, the
fragment/environment contents, and dismissal clearing the secrets and the form; the repeat saying
"already existed" and "kept" with no new service token; and a person's account refused with the
reason shown.

## Exit checks (all run in this worktree unless noted)

```
cargo fmt --check                                           ok
cargo clippy --workspace --all-targets -- -D warnings       ok
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps  ok
./scripts/check-file-length.sh                              ok (new files are untracked, so the
    script's `git ls-files` skips them; checked by hand with the same awk: 93 / 252 / 67 / 22 / 9 / 186)
cargo test -p rustak-api                                    172 passed
cargo test -p rustak-server --no-fail-fast                  every binary ok: lib 2013 passed (2 ignored),
    service_onboarding 11 passed, sidecar_enrolment 5, services_flow 4, cloudtak_onboarding 20, …
```

UI checks. The worktree is nested inside the main checkout, so cargo inside `rustak-ui` resolves the
**main** checkout's `Cargo.toml` as its workspace and refuses to build. I therefore ran the UI checks
on an rsync copy of this worktree in the session scratchpad; the sources were identical:
```
rustak-ui: cargo fmt --all --check                                          ok
rustak-ui: cargo clippy --all-targets --target wasm32-unknown-unknown -D warnings  ok
rustak-ui: cargo test                                                        151 passed
rustak-ui: trunk build                                                       success
e2e: npm run typecheck                                                       ok
e2e: npx playwright test (whole suite; server = this worktree's
     `cargo build -p rustak-server` with that UI bundle, RUSTAK_E2E_BINARY)  71 passed
```
`e2e/node_modules` was a symlink to the main checkout's installed copy, in the scratch copy only.

## Open questions

1. **`docs/plugins.md` "Three credentials" and the `rustak-client::control` module doc** say an
   enrolled sidecar needs no token for the control API. That is not true while the public listener
   asks for no client certificate. Either fix the docs, or make the public listener request an
   optional certificate (`RustakClientVerifier` with `mandatory = false`) and capture it the way
   `[web.marti]` does. The second option would let this action drop the service token. It is outside
   this brief's files either way.
2. **Rotation is not undone if revoking the old tokens fails part-way.** The new tokens are revoked
   (nothing new is left behind), but an old token already revoked stays revoked. The caller asked
   for it to be revoked, so I judged this acceptable, but it is not strictly all-or-nothing.
3. **No per-action rate limit**, matching the neighbours (see Decisions). If the maintainer wants
   credential minting throttled per administrator, that belongs across all three actions, probably
   next to M10-11's limiter work.
