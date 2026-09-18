# M5-02 — Fix the security review findings (R-01)

**Brief:** `.claude/plan/briefs/M5-02-security-review-fixes.md`
**Review:** `.claude/plan/reviews/R-01-security-review.md`

Every **High** and every **Medium** except M1 (M4-04's) is implemented, each with at least one
negative test. Ten of the sixteen **Low** items are done; the six that are not are listed with a
reason. No check was weakened to make a test pass: where an existing test encoded the behaviour a
finding describes, the test was rewritten to assert the new contract and says which finding it is
about.

## Disposition, finding by finding

### High

| Finding | Disposition |
|---|---|
| **H1** — `scope` issued everywhere, enforced nowhere | **Fixed.** `users::principal` ANDs `AuthMethod::Bearer`'s granted scope into `is_admin`, so the request is answered under the narrower of "what the account may do" and "what this credential was granted". A credential that carries no scope (certificate, Basic, passkey, setup token) is bounded by the account alone. The password grant now mints `api` and never `admin`. Tests: `identity::users::tests::{a_token_granted_no_administrative_scope_administers_nothing, a_scope_that_merely_starts_with_admin_is_not_the_admin_scope, a_credential_that_carries_no_scope_is_bounded_by_the_account_alone}`, `auth::resolve::tests::{an_expression_cannot_widen_a_token_that_was_granted_no_admin_scope, an_expression_still_takes_administrative_access_away_per_request}`, `oauth_flows::scope_ceiling::*`, `enroll_oauth::a_password_grant_never_issues_an_administrative_token`. |
| **H2** — `/oauth/authorize` accepts a raw bearer | **Fixed.** The endpoint resolves a caller from the `access_token_N` **cookies only** (`authorize::browser_session`); an `Authorization` header establishes nobody there. A password-grant token is therefore not a credential for it, and with H1 it could not buy an administrative family even if transplanted into a cookie. Tests: `oauth_flows::scope_ceiling::{a_bearer_header_mints_no_authorization_code, the_same_session_as_a_cookie_still_mints_one}`. |
| **H3** — `GET /api/v1/events` is an unfiltered firehose | **Fixed.** Every publish now names an `Audience` (`plugins::visibility`), and the feed filters each frame against a `Subscriber` resolved from the caller: channel reachability (`can_reach`) for client events, the mission's channels for mission events, `Viewer::can_read_groups` for packages, the account itself for `channel.changed`, and the service itself for `service.status`. Tests: `plugins::visibility::tests::*`, `web::api::events::tests::{a_service_is_not_shown_a_package_it_could_not_download, an_administrator_is_shown_the_same_package, a_service_is_not_told_whose_channels_changed, a_service_still_hears_about_its_own_account}`. |
| **H4** — the feed never re-checks authorization | **Fixed.** An open feed re-runs `plugins::auth::caller` every 60 s *and* immediately on an invalidation pushed by `ServerEvents::invalidate`, rebuilds the subscriber from the answer, and ends the response on failure. Test: `web::api::events::tests::an_open_feed_ends_when_its_account_is_switched_off` (no shutdown cancel — the response ends because the credential was re-checked). |
| **H5** — disabling an account does not end its live session | **Fixed.** `identity::sessions::end_all` revokes the refresh family, closes the account's `:8089` connections through the new `LiveState::disconnect_by_user`, and invalidates its open feeds. Called from `PATCH /api/v1/users/{u} {"disabled":true}` and from `DELETE /api/v1/credentials/{id}`. Tests: `stream_session::switching_an_account_off_ends_the_session_it_already_had`, `identity::sessions::tests::*`. |

### Medium

| Finding | Disposition |
|---|---|
| **M1** — mission token matches name **or** guid | **Not mine** — M4-04 owns `marti/missions/**`. Untouched. |
| **M2** — cookies reach destructive `/Marti` `GET`s | **Fixed.** `cookies_allowed` refuses `/Marti/sync/delete` and `/Marti/api/repeater/remove/*` outright, whatever the method; the TAK-compatible verbs are unchanged and a header still works. `GET /logout` now revokes only the presented session (`tokens::revoke_session`) while `POST /logout` still ends every family. Tests: `cookies::tests::a_cookie_never_authenticates_a_marti_path_that_deletes`, `oauth_flows::cookie_scope::{a_session_cookie_never_reaches_a_marti_path_that_deletes, a_cross_site_logout_link_does_not_end_the_accounts_other_sessions}`. |
| **M3** — enrolment token is check-then-act | **Fixed.** `credentials.claim_single_use` is one conditional `UPDATE` that counts the use, spends the row and reports whether this caller won; `marti::enroll::issue` claims **before** signing and releases the claim if issuance then fails. Tests: `enroll_flows::{one_enrolment_token_produces_exactly_one_certificate, a_spent_enrolment_token_cannot_be_presented_again}`. |
| **M4** — any account can re-bind another's device row | **Fixed.** `devices::upsert_seen` refuses a uid held by a different account (`Kind::User`), enrolment turns that into a `403`, and the escape hatch is the existing administrative `DELETE /api/v1/devices/{uid}`. Tests: `identity::devices::tests::{a_uid_belongs_to_the_account_that_first_enrolled_it, an_administrator_releasing_the_row_lets_the_uid_be_claimed_again}`, `enroll_flows::a_device_uid_cannot_be_taken_from_the_account_that_enrolled_it`. |
| **M5** — service tokens bypass `user_acl` | **Fixed.** The service-token arm builds an `AuthRequestFilter` and evaluates it before answering. The related half is fixed too: `plugins::auth::caller` now tries the explicit header **first** and falls back to a peer certificate only when the header established nobody, so an ambient certificate cannot outrank a bearer on the control API. Tests: `plugins::auth::tests::{a_service_token_is_refused_where_user_acl_refuses_the_request, a_service_token_still_works_where_the_expression_allows_it, an_explicit_header_outranks_an_ambient_certificate}`. |
| **M6** — scope widening on refresh | **Fixed.** `tokens::rotate` clamps the recomputed decision by `spent.scope`. Tests: `auth::tokens::tests::{a_renewal_cannot_widen_the_scope_its_family_was_minted_with, a_renewal_drops_administrative_scope_the_account_has_lost}`, `oauth_flows::scope_ceiling::a_code_minted_for_an_ordinary_session_stays_ordinary`. |
| **M7** — `state` cookie has no `__Host-` prefix | **Fixed.** A second, independent binding: 256 bits in `__Host-rustak_login` at `Path=/`, stored in `PendingAuth` as a digest and required on the callback, failing closed on an absent cookie or an older record. `state` keeps its TAK name and rule. Tests: `state::tests::a_callback_without_this_browsers_host_binding_is_refused`, `cookies::tests::{the_binding_cookie_carries_the_attributes_its_prefix_requires, signing_out_clears_the_binding_as_well_as_the_state}`, `oauth_flows::federation::a_callback_with_a_tampered_state_is_refused` (extended with three binding cases). |
| **M8** — one sweep aborts on another record shape | **Fixed.** Four partitions: `auth-state` (setup token), `auth-registration`, `auth-ceremony`, `auth-login`. The setup token keeps the original name so an installation upgraded mid-wizard does not lose it. Test: `passkey_store::tests::a_sweep_is_not_stopped_by_a_record_of_another_shape` (writes a `setup-token` row and asserts the sweep still deletes). |
| **M9** — `passkey/login/start` records no limiter outcome | **Fixed.** A miss records a failure; a hit records nothing, so a probe cannot reset its own budget by guessing a real name. Test: `web::api::passkey::tests::probing_for_accounts_by_name_runs_into_the_limiter`. |
| **M10** — WebAuthn RP identity not pinned | **Fixed.** The RP id is recorded in `Ceremony` and required by both finishes; `relying_party` reads `[server] base_url`/`domains` and **never** the `Host` header, refusing outright when neither is configured. Tests: `passkey_store::tests::a_ceremony_cannot_be_finished_under_another_relying_party`, `web::api::passkey::tests::{a_ceremony_needs_a_configured_base_url_rather_than_the_host_header, a_ceremony_records_the_relying_party_it_was_started_under}`. |
| **M11** — `services.enabled` is a kill switch nothing reads | **Fixed.** `plugins::auth::service_token` refuses a token whose account holds a disabled registration; an account with no registration is unaffected, so registering still works. Test: `plugins::auth::tests::a_service_an_operator_switched_off_stops_authenticating`. |
| **M12** — 128 MiB pinned, and a lock held while cloning | **Fixed.** A heartbeat message is truncated at 512 characters on the bus, the ring and the broadcast carry `Arc<PublishedEvent>` (a resume or a lagged refill copies pointers), open feeds are capped at 64, and a 256 KiB `JsonConfig` is installed on `/api/v1`. |
| **M13** — one-shot claims are read-then-delete | **Fixed.** `KeyValueStore::take` is one write transaction doing `DELETE … RETURNING value`; the ceremony, registration-token and pending-sign-in claims all use it. Tests: `db::kv::tests::a_value_can_be_claimed_exactly_once`, `passkey_store::tests::a_ceremony_can_be_claimed_by_exactly_one_of_two_racing_callers`, `state::tests::two_callbacks_racing_on_one_state_cannot_both_win`. |
| **M14** — `client.disconnected` leaks incognito clients | **Fixed.** `ConnectionSummary` carries `incognito`, and an incognito connection is announced to administrators only — which matches `GET /api/v1/clients`, where an administrator sees it too. |
| **M15** — username-claim fallback is an account-selection primitive | **Fixed.** The fall-through to `preferred_username`/`sub` applies only while `username_claim` is at its default; a configured claim that is absent is a refusal. Tests: `oidc::claims::tests::{a_configured_claim_that_is_absent_is_a_refusal_rather_than_a_downgrade, the_default_claim_still_falls_through_to_the_subject}`. |
| **M16** — service-registration existence oracle | **Fixed.** A non-administrator gets the same `404` — same status *and* same body — for a name that is not registered and for one that is somebody else's. `404` rather than `403` because it is the answer a sidecar acts on ("register again") and it tells nobody anything. Test: `web::api::services::tests::one_service_may_not_read_or_change_another`. |

### Low

| Finding | Disposition |
|---|---|
| **L1** — `rate_limit` keyed on `(ip, subject)` | **Not done.** A per-account counter is a change to `auth/ratelimit.rs`, which M1-09 owns this round. Every secret behind the limiter is 100–256 bits of server-generated entropy, so it is not exploitable today; worth doing the day a human-chosen secret is added. |
| **L2** — the passkey limiter's subject is constant | **Not done, deliberately.** Making the subject per-username would give each guessed name its own bucket and undo M9's throttle on enumeration, which is the sharper of the two. The NAT lockout stays; a narrower fix needs the per-account counter of L1. |
| **L3** — `register_finish` does not re-check `disabled` | **Fixed.** The account is re-read and a disabled one is refused before the bootstrap session is issued, as `login_finish` already did. |
| **L4** — adding an authenticator needs no recent authentication | **Not done.** Step-up authentication is a feature rather than a fix (it needs a re-authentication ceremony and a UI for it), and H1 has since given the narrow token it was worried about a narrow scope. Worth a brief of its own. |
| **L5** — no minimum curve strength for an ECDSA CSR | **Fixed.** `MIN_ECDSA_BITS = 256`, beside `min_rsa_bits`. Test: `pki::csr::tests::a_curve_below_the_floor_is_refused_the_way_a_short_rsa_key_is`. |
| **L6** — revocation cache fails **open** on a poisoned lock | **Fixed.** `contains` and `replace` step over the poison with `PoisonError::into_inner`, as `RateLimiter::lock` already does, so a poisoned `revoked` set is read rather than treated as empty. No test: poisoning the lock needs a panic inside a private method of a private field, and the fix removes the branch rather than changing one. |
| **L7** — service-token uses are never recorded | **Fixed.** `plugins::auth::service_token` records the use, which is what makes `max_uses` and `last_used_at` mean anything. Test: `plugins::auth::tests::a_service_token_minted_for_one_use_works_once`. |
| **L8** — `azp` is never checked | **Fixed.** `require_authorized_party` refuses a multi-audience ID token that does not name us as the authorized party (OIDC Core §3.1.3.7). |
| **L9** — the ID-token algorithm check is a deny-list | **Fixed.** `ACCEPTED_ALGORITHMS` is an allow-list of RS/PS/ES/EdDSA. |
| **L10** — rate limiting covers only the password grant | **Not done.** It is write amplification rather than a guessing risk, and the unbounded-KV half of it is closed by M8's partitions plus M13's atomic claims. A limiter on `/login/auth` belongs with L1's per-account counter. |
| **L11** — refresh tokens are not bound to their client | **Not done.** Every client here is a public bearer holder, so this is audit value rather than a control; binding it changes `/api/v1/auth/refresh`'s contract and wants its own brief. |
| **L12** — `[auth.oidc] endpoint` and `jwks_uri` are unchecked | **Not done here.** M1-09's brief covers the outbound HTTP client (`web/helpers/oidc/{discovery,exchange}.rs`, request budgets and body caps); a scheme/origin check belongs with that work rather than split across two agents. |
| **L13** — unauthenticated `POST /Marti/ErrorLog` writes | **Not mine** — `marti/util.rs` is outside this brief's file list. |
| **L14** — a lagged consumer receives history it did not ask for | **Fixed.** `seen` is seeded from `events.latest_id()` when no resume id was supplied. Test: `web::api::events::tests::a_consumer_that_asks_for_nothing_is_not_replayed_the_ring_when_it_lags`. |
| **L15** — check-then-act in `registry::register` | **Not done.** The race needs a name to be momentarily absent and two registrations to collide on it; the `500` half is M4-04-adjacent (`plugins/registry.rs` upsert semantics) and a `UNIQUE` violation mapped to a message is a small brief of its own. |
| **L16** — no request-body limit on the ceremony endpoints | **Fixed.** 256 KiB `JsonConfig` on `/api/v1` (uploads are multipart and unaffected). |

### Info

Four of the Info items were cheap and are done: the `docs/plugins.md` line about what a service token
authenticates, the stale "logged once" claim in `auth::setup`'s module docs, the stale "M2" comment
in `web::api::auth`, and the `anon_group_default`/`allow_access_token_retrieval` trades, which are
now written out in `docs/deployment.md` → **Security notes worth knowing about** together with the
passkey single-domain rule the same section of the review asks for. The user-handle shape and
`dynamic_state_of`'s hardcoded `user_verified` are left as the review describes them: neither is
exploitable today and both are `webauthn_rp` interface decisions rather than one-line changes.

## Deviations worth knowing about

1. **`PackageEvent` did not gain `groups`.** The review suggested carrying the channels on the wire
   so a consumer could self-filter. The bus carries a server-side `Audience` beside each event
   instead, so the event a subscriber may not see never reaches its socket and the wire shape is
   unchanged — strictly narrower than the suggestion, and it does not tell a reader which channels
   they are *not* in.
2. **The feed's subscriber uses the *effective* group set**, not the raw memberships
   `users::principal` carries. A connection announces itself with the set `stream::resolver` gave it
   (memberships narrowed by active state, widened by `__ANON__` when `anon_group_default` is on);
   comparing that against a raw set is not a rule. `services_flow`'s sidecar test now runs with
   `anon_group_default = true`, which is the shipped default, and says why.
3. **A promotion now takes effect at the next sign-in** for bearer sessions, because the scope is a
   ceiling. Demotion, disabling and channel changes stay immediate. `auth::tokens::tests` records
   both directions and `docs/deployment.md` says so.
4. **H2 took the cookie route, not the token-marking route.** Marking a password-grant token would
   need a new JWT claim, and `compat/oauth.md` §2 pins the payload CloudTAK's parser reads. The
   brief asked for the cookie requirement; the narrow scope closes the privilege half.
5. **Files edited outside the brief's list, none of them another agent's:** `web/api/users.rs` (the
   disable path H5 names), `web/api/subject.rs` (a test helper that hard-coded `scope: "api"` for
   administrators), `web/api/mod.rs` (the JSON limit), `db/repos/{credentials,services}.rs`
   (`claim_single_use`, `release_single_use`, `get_by_user`), `plugins/{events,health,mod}.rs` and
   the new `plugins/visibility.rs` (H3's audiences), `identity/{devices,credentials}.rs`,
   `pki/{csr,revoke}.rs` (L5, L6), `stream/live.rs` (additive, as the brief allows) and
   `docs/plugins.md`. `config.example.toml` was **not** touched: this brief added no configuration
   keys, and another agent was editing that file throughout.
6. **`docs/deployment.md`'s new section is already in `HEAD`.** Another agent committed that file
   (`0fed4b0`) while my edit was in the working tree, so the section is in their commit rather than
   uncommitted. No `git`/`but` write was made from this session.

## Exit checks

All green. Every command below was re-run after the last edit, on a quiet machine.

```
$ cargo fmt --all --check                                    # clean
$ cargo clippy --workspace --all-targets \
      --features rustak-server/testing -- -D warnings        # clean
$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
                                                             # clean
$ bash scripts/check-file-length.sh                          # clean
$ cargo test -p rustak-server --features testing --lib
test result: ok. 1682 passed; 0 failed; 2 ignored
$ cargo test --workspace --features rustak-server/testing
   every binary green; run three times, no failures
$ cd interop/node-tak && npm test
ℹ tests 25   ℹ pass 25   ℹ fail 0
$ cd e2e && npm run typecheck                                # clean
$ cd e2e && npx playwright test
   cannot run here — see below
```

Each server integration binary, run one at a time (`-- --test-threads=1`):

```
acme_directory 5/5       api_v1_live 14/14      api_v1_packages 9/9    bootstrap 3/3
enroll_flows 14/14       enroll_oauth 11/11     marti_channels 11/11   marti_contract 14/14
marti_cot 11/11          mission_dest 11/11     mission_squash 4/4     missions_authz 7/7
missions_extras 14/14    missions_flow 14/14    oauth_flows 31/31      profiles_contract 14/14
services_flow 2/2        stream_channel_state 3/3                      stream_routing 12/12
stream_session 11/11     stream_store 8/8       sync_contract 15/15
```

### One flake seen along the way, now green

While three other briefs were building and testing in this working tree, `enroll_flows` failed on
roughly four runs in five at exactly the two tests that bind a real mutually authenticated socket —
`a_cloudtak_shaped_enrolment_produces_a_certificate_the_marti_listener_accepts` and
`a_revoked_certificate_cannot_complete_the_handshake` — both as "connection closed" during the TLS
handshake. It is green on a quiet machine, including three consecutive full workspace runs, so it is
not a correctness problem; it is a window CI's parallel run could still hit. What was established:

- Not caused by anything added here: the same two tests fail with this brief's three new
  `enroll_flows` tests marked `#[ignore]`, and those three pass in every run.
- Not the certificate policy: the enrolment answers `200` with a `signedCert` before the handshake
  is attempted, so `CsrPolicy` — L5's new curve floor included — has already passed.
- The same mutually authenticated path is green against the **real binary** in `interop/node-tak`
  ("answers an enrolled client certificate on the mutually authenticated listener").

The window is in `enroll_flows.rs`'s own `try_harness`, which reserves a port, drops the reservation
and then binds by address; `harness()` retries a bind *failure*, which is not what happens. Flagged
for the CI steward rather than fixed here, since the brief reserves tests and CI infrastructure.

### What stands in for the Playwright run

`npx playwright test` cannot run in this sandbox: the browser is not installed and
`npx playwright install chromium` fails to download it (connection timeout, retried three times with
`PLAYWRIGHT_DOWNLOAD_CONNECTION_TIMEOUT=180000`). The server-side half of the UI sign-in was verified
through the tests that drive the same code:

- `rustak-server/tests/bootstrap.rs` walks the whole first run over real sockets — setup token →
  `POST /api/v1/setup/admin` → `register/start` and `register/finish` against the software
  authenticator → the session that comes back → `GET /api/v1/me` answering `is_admin: true`. That is
  the path M10 (the relying party) and H1 (the scope) could have broken, and it is green.
- `web::api::passkey`'s unit tests cover both ceremonies end to end, including the new relying-party
  pinning and the refusal when no base URL is configured.
- `e2e/scripts/start-server.mjs` sets `[server] base_url`, so the `Host` fallback M10 removed was
  never on that suite's path.
