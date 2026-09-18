# M5-01 — OAuth2 authorization-code flow and TAK-style OIDC federation — complete

Brief: `.claude/plan/briefs/M5-01-oauth2-authorize-oidc-federation.md`
Read first: `conventions.md`; `design/03-identity-pki-acme-auth.md` §5 (`oauth_server/*`, `oidc/`),
§6 endpoint tables, §10 M5 steps; `compat/oauth.md`, `compat/cloudtak.md`;
`research/06-takserver-http-api-verified.md` §4.5–§4.7; status `M2-03`, `M0-11`, `M2-02`.

## What was built

Two authorization codes now exist in this server and they are **not** the same thing: the
identity provider's, which we redeem with the client secret and a proof key of our own, and
**ours**, which a registered client redeems at `/oauth/token` with the proof key *it* generated.
The module tree is laid out so that confusing the two is hard — `state.rs` holds the first,
`codes.rs` the second — and `auth/oauth_server/mod.rs` opens with the table of which control stops
which attack.

| File | Functional lines | Contents |
|---|---:|---|
| `auth/oauth_server/mod.rs` | 21 | module docs (the control/attack table), re-exports, `constant_time_eq` |
| `auth/oauth_server/authorize.rs` | 199 | `GET /oauth/authorize`, client + redirect validation, `deliver_code`, the redirect/refusal builders |
| `auth/oauth_server/codes.rs` | 133 | `oauth_codes`: `issue`, `redeem` (all four bindings in one transaction), `prune` |
| `auth/oauth_server/state.rs` | 102 | `PendingAuth`/`PendingKind`, `state_hash`, `matches_state`, one-shot `begin`/`claim`, `sweep` |
| `auth/oauth_server/cookies.rs` | 105 | TAK's chunked `access_token_N`, the `state` cookie, `cookies_allowed` |
| `auth/oauth_server/login.rs` | 264 | `GET /login/auth`, `begin_federation`, `GET /login/redirect`, the sign-in and its completion |
| `auth/oauth_server/session.rs` | 131 | `/login/authserver`, `/login/.well-known/openid-configuration`, `/token/access`, `/logout`, the shared refusals |
| `marti/login.rs` | 25 | the routing table, public listener only |
| `migrations/0013_oauth_codes.sql` | — | `oauth_codes`, STRICT, `WITHOUT ROWID` |
| `rustak-server/tests/oauth_flows.rs` | — | 24 endpoint tests, nearly all negative |

**Additive edits to existing files** (each one line or one block):

| File | Change |
|---|---|
| `auth/mod.rs` | `pub mod oauth_server;`, two re-exports, a "What M5 added" paragraph |
| `auth/resolve.rs` | one arm in `resolve_principal`: a cookie is read **only** where `cookies_allowed` says so and **only** when the `Authorization` header carried nothing |
| `marti/mod.rs` | `pub mod login;` and `.configure(login::routes(role))` before every scope |
| `marti/oauth.rs` | `grant_type=authorization_code` (+ four form fields); the password grant's body is untouched |
| `config/auth.rs` | `[auth.oauth]` — `OAuthServerConfig`, `OAuthClient`, both `deny_unknown_fields` |
| `config/validate.rs` | `oauth_clients` + `redirect_uri`: duplicate ids, empty `redirect_uris`, non-absolute URIs, fragments, plaintext non-loopback URIs |
| `config/mod.rs` | one re-export line |
| `runtime.rs` | the ten-minute housekeeping sweep also drops abandoned sign-ins and expired codes |
| `testing/oidc.rs` | `TestIdentityProvider` now records each code's `nonce` and `code_challenge` and enforces both at the token endpoint |
| `config.example.toml` | the `[auth.oauth]` block, documented with its default |
| `docs/deployment.md` | a "Browser single sign-on" section |
| `.claude/plan/compat/oauth.md` | §1 and §4 corrections (below) |

## The security properties, and where each is asserted

Every one of these is a **negative** test; the positive round trip is asserted once so that the
negatives are not passing because the flow never worked.

| Property | Where |
|---|---|
| PKCE `S256` mandatory for every client; `plain` never accepted | `authorize.rs::proof_key` tests (×3); `oauth_flows::a_public_client_cannot_start_a_flow_without_a_proof_key` |
| A code is worthless without its verifier, and a wrong verifier does **not** burn it | `codes.rs::a_code_without_its_verifier_is_worthless`; `oauth_flows::a_code_is_worthless_without_the_verifier_that_made_it` |
| A code is single-use, decided inside the transaction that spends it | `codes.rs::a_code_is_exchanged_once_and_never_again`; `oauth_flows::a_code_is_spent_the_first_time_it_is_exchanged` |
| A code is bound to its `redirect_uri` and its `client_id`, byte for byte | `codes.rs` ×2; `oauth_flows` ×2 |
| Ten-minute expiry | `codes.rs::an_expired_code_is_refused_and_pruned`; `oauth_flows::an_expired_code_is_refused` |
| `sha256(state cookie) == state`, compared in constant time | `state.rs::the_value_sent_to_the_provider_is_the_digest_of_the_cookie`, `a_state_that_did_not_come_from_this_cookie_is_refused`; `oauth_flows::a_callback_with_a_tampered_state_is_refused` |
| The callback is one-shot even for the browser that made it | `state.rs::a_pending_sign_in_is_claimable_exactly_once`; `oauth_flows::a_callback_cannot_be_replayed` |
| The ID token's `nonce` must be the one this flow issued | `oauth_flows::an_id_token_from_another_flow_is_refused` |
| An unregistered client or redirect URI is refused **without redirecting** | `oauth_flows::an_unregistered_redirect_uri_is_refused_here_rather_than_redirected_to`, `an_unregistered_client_is_refused_without_redirecting` |
| Cookies never authenticate `/api/v1` | `cookies.rs::the_admin_api_is_never_authenticated_by_a_cookie`, `the_token_endpoint_reads_no_cookie`; `oauth_flows::a_session_cookie_is_refused_on_the_admin_api` |
| Cookies carry `HttpOnly`, `Secure`, `SameSite=Lax`, `Path=/`; `state` is `Path=/login` | `cookies.rs` ×2; `oauth_flows::a_browser_signs_in_through_the_provider_and_comes_back_with_a_session_cookie` |
| Signing out revokes the `jti` and the refresh family, not just the cookie | `session.rs::signing_out_stops_the_token_it_was_asked_with`; `oauth_flows::signing_out_clears_every_chunk_and_revokes_the_session` |
| `returnTo` cannot become an open redirect | `login.rs::a_return_to_may_only_be_somewhere_on_this_site` |
| The `/login/*` flow is not served on the mutually authenticated listener | `marti/login.rs::the_mutually_authenticated_listener_serves_no_sign_in_flow` |
| The password grant's body is unchanged | `oauth_flows::the_password_grant_still_answers_exactly_what_cloudtak_parses`, plus the whole of `tests/enroll_oauth.rs` still green |

### Two ordering decisions that are load-bearing

1. **`/oauth/authorize` is registered before the `/oauth` scope.** actix matches services in
   registration order and a scope that matches the prefix answers its own unmatched paths rather
   than letting a later sibling see them, so a later registration would simply never be reached.
   `marti/login.rs::the_authorization_endpoint_is_not_swallowed_by_the_oauth_scope` asserts it.
2. **The sign-in endpoints are outside the `/Marti` and `/oauth` scopes**, because
   `marti_headers::assert_no_redirect` turns any `3xx` into a `500` — correct for the TAK surface,
   fatal for a flow made of redirects.

### The client and redirect URI are validated before anything can redirect

That ordering is the difference between an authorization server and an open redirector. Only once
both are known good does a refusal go back to the client as `?error=…&state=…`; before that it is a
`400` answered here. `error=` in a query string does not make an attacker-chosen bounce off a
trusted host any less of one.

## Deviations from the brief and the designs, and why

1. **`session.rs` and `cookies.rs` are extra files** beyond the brief's
   `{authorize,login,state,codes}.rs`. `login.rs` is at 264 functional lines with the federation
   round trip alone; the cookie mechanics and the four small session endpoints would have put it
   well over 300. Both new files are inside `auth/oauth_server/**`, which the brief owns.

2. **`auth/resolve.rs` was edited**, which the brief does not list. Cookie acceptance has to happen
   where credentials are resolved, and the alternative — duplicating bearer resolution inside the
   `/login/*` handlers — would have been a second code path that could drift from the first. The
   change is one arm of ~12 lines plus a doc paragraph, gated on
   `oauth_server::cookies_allowed(path)`, so the policy itself still lives in a file this brief
   owns. No other agent was listed against `auth/**`.

3. **`runtime.rs` was edited** (five lines in `sweep_ceremonies`) so the existing ten-minute
   housekeeping tick also drops abandoned sign-ins and expired codes. Housekeeping only — both are
   refused on their own merits whether or not they have been swept — but without it two tables grow
   without limit on a server people keep closing tabs on.

4. **`testing/oidc.rs` was extended.** The brief says to use `TestIdentityProvider` for the
   federation round trip, and the provider as it stood echoed no `nonce` and checked no
   `code_challenge` — so the two controls the brief calls out could not have been tested against it
   at all. It now records both per issued code and enforces the challenge with its **own** SHA-256
   rather than calling `pkce::challenge_for`, so a provider that agreed with a broken implementation
   would still fail. Existing behaviour is preserved: the constant `CODE` is still redeemable with
   no nonce, which is what `web/api/auth.rs`'s tests use.

5. **`oauth_codes` is a new table rather than the unused `oauth_tokens` from `0003`.** That table
   was designed as one generic row for codes *and* pending provider states, with the bindings in a
   JSON `data` blob. Every binding a code carries is a reason to refuse a redemption, and a column
   makes forgetting one a compile error where a JSON key makes it a silent widening. **`oauth_tokens`
   is now provably dead** — nothing in the tree reads or writes it — and should be dropped in a
   later migration; not done here because migrations are append-only and a `DROP TABLE` landing
   between two other agents' in-flight migrations is not a change to make unannounced.

6. **`/login/refresh` is not implemented.** TAK Server's version renews from the refresh token it
   keeps in the servlet HTTP session; rustak has no such session and does not put a refresh token in
   a cookie at all, so there would be nothing for the endpoint to read. Clients renew with
   `POST /oauth/token` `grant_type=refresh_token`, which already existed. Recorded in
   `compat/oauth.md` §4.

7. **A failed callback is a `400`, not a `302` to `/login/error.html`.** Design 03 §5 has the
   redirect; there is no such page in the UI, and a redirect off a failure is one more place a
   half-finished flow can be steered. Every cause answers the same generic JSON body with the
   `state` cookie cleared, so the endpoint is not an oracle for which control was hit.

8. **`/logout` answers `204`, not TAK Server's `301` to `/webtak/index.html`.** Design 03 §5 says
   `204`; there is no `/webtak/index.html` here.

9. **`SameSite=Lax`, not TAK Server's `Strict`.** `Strict` would withhold the cookie on the
   top-level navigations `/login/redirect` and `/oauth/authorize` *are*, so the flow could not
   complete. `Lax` still withholds it from every cross-site `POST` and `fetch`.

10. **`Secure` is set unconditionally** rather than only when the request arrived over TLS. A cookie
    that silently drops `Secure` because a proxy forwarded the request as HTTP is a session handed
    to whoever is watching. Documented in `docs/deployment.md`: this flow needs TLS.

11. **No consent screen.** Every registered client comes from this server's own configuration file,
    which makes them all first-party; a dialogue asking somebody to approve the administrator's own
    admin UI teaches people to click through dialogues.

12. **`config/mod.rs` gained one re-export line**, which the brief does not list — `OAuthClient` and
    `OAuthServerConfig` have to be nameable from `crate::config` like every other config type, and
    the integration suite needs them.

13. **Proof key for code exchange is required of `public = false` clients too.** The specification
    lets a confidential client lean on its secret instead — but `[auth.oauth]` has no `secret` field
    and nothing here accepts client authentication, so `public = false` would otherwise register a
    client that can start a flow and never finish it: a configuration that looks like it works.
    `public` is parsed and stored so that adding client authentication later is not a change to the
    shape of the config file, and both `config.example.toml` and `docs/deployment.md` say plainly
    that it changes nothing today.

14. **The `authorization_code` grant derives `is_admin` from the account, not from the code.** The
    scope recorded when the code was issued is the *ceiling* — a code minted for an ordinary session
    cannot become an administrative one — but the decision itself is
    `user.is_effective_admin() && <the code said admin>`, so somebody demoted inside the code's
    ten-minute life is not handed the scope their code still remembers. This is what
    `tokens::rotate` already does, and `auth::resolve::bearer` re-evaluates `admin_acl` per request
    regardless, so the scope string is not what gates anything on `/api/v1`.

## `compat/oauth.md` corrections

- **§1** now says `grant_type=password` is the only grant *TAK Server* serves there, and records
  that rustak additionally serves `refresh_token` and `authorization_code` at the same path —
  additively, with the password grant's request and response byte-for-byte unchanged. `client_id`
  is required for `authorization_code` and stays a no-op for the other two.
- **§4** gained "What M5-01 built, and where it deliberately differs from TAK Server": a
  nine-row table of every difference (state, PKCE, nonce, both cookies, refresh, `/logout`, failure
  shape, and port-gating versus path-gating), the two TAK endpoints not implemented and why, and
  what `GET /oauth/authorize` — which has no TAK counterpart — validates.
- **Gotchas** gained the two that bite: the other grants *do* return a `refresh_token` and only the
  password grant's body is pinned, and the cookies are `Lax` rather than `Strict` with the reason.

## Notes for the orchestrator and later briefs

- **`oauth_tokens` (migration `0003`) is dead.** Deviation 5. A later migration should drop it.
- **The UI has no sign-in page for this flow yet.** `GET /login/auth` sends somebody to the
  provider and `/login/redirect` puts them back at `/` (or `?returnTo=`), where the SPA shell is
  served. A rustak-ui brief that wants the WebTAK-style "read my token from `/token/access`" page
  has everything it needs; nothing here depends on it existing.
- **`[auth.oauth] clients` is empty by default**, so the authorization-code flow is off until an
  operator registers a client. The wizard (`/api/v1/setup/settings`) does not manage it; a UI brief
  could add it, and the validation in `config/validate.rs` is the shape to reuse.
- **Rate limiting.** `/oauth/authorize` and `/login/*` are not rate limited: neither accepts a
  guessable secret (a code is 256 bits, a state is 256 bits, and the verifier check is a hash
  comparison), so the limiter's buckets would only add a way to lock a legitimate browser out. The
  credential endpoints behind them — `/oauth/token`'s password grant, `/api/v1/auth/*` — are limited
  as before.
- **`/login/*` on the Marti listener** is deliberately absent (`marti/login.rs`). If a deployment
  ever needs a browser on `:8443`, that is a change to `routes(role)` and its test, not to anything
  else.

## Exit checks

```
$ cargo fmt -p rustak-server --check
(no output)
# `cargo fmt --all --check` reports hunks in rustak-client/src/sidecar/mod.rs and
# rustak-server/tests/services_flow.rs, which belong to other briefs in flight;
# nothing in this change set is unformatted.

$ ./scripts/check-file-length.sh
(no output, exit 0)
# It only walks git-tracked files, so every file created here was checked by
# hand with the same awk. Largest created: auth/oauth_server/login.rs at 264.
# Files edited rather than created: marti/oauth.rs 290, config/validate.rs 275,
# marti/mod.rs 269, auth/resolve.rs 216, config/auth.rs 205.

$ cargo clippy -p rustak-server --all-targets --all-features -- -D warnings
(clean over every file this brief touched — verified by sweeping the whole
 crate's lint output and grepping for each of them; the only findings in the
 run belonged to plugins/auth.rs and tests/acme_directory.rs, both other briefs,
 and both have since cleared)

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server --no-deps --all-features
(clean; one ambiguous intra-doc link in auth/oauth_server/mod.rs was fixed here)

$ cargo test -p rustak-server --features testing
     Running unittests src/lib.rs
test result: ok. 1569 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 20.01s
     Running unittests src/main.rs      test result: ok. 0 passed
     Running tests/acme_directory.rs    test result: ok. 5 passed
     Running tests/api_v1_live.rs       test result: ok. 12 passed
     Running tests/api_v1_packages.rs   test result: ok. 9 passed
     Running tests/bootstrap.rs         test result: ok. 3 passed
     Running tests/enroll_flows.rs      test result: ok. 10 passed   (see below)
     Running tests/enroll_oauth.rs      test result: ok. 10 passed
     Running tests/marti_channels.rs    test result: ok. 9 passed
     Running tests/marti_contract.rs    test result: ok. 14 passed
     Running tests/mission_dest.rs      test result: ok. 10 passed
     Running tests/mission_squash.rs    test result: ok. 4 passed
     Running tests/missions_extras.rs   test result: ok. 12 passed
     Running tests/missions_flow.rs     test result: ok. 14 passed
     Running tests/oauth_flows.rs       test result: ok. 24 passed   <-- this brief
     Running tests/profiles_contract.rs test result: ok. 14 passed
     Running tests/services_flow.rs     test result: ok. 2 passed
     Running tests/stream_channel_state.rs test result: ok. 3 passed
     Running tests/stream_routing.rs    test result: ok. 12 passed
     Running tests/stream_session.rs    test result: ok. 10 passed
     Running tests/stream_store.rs      test result: ok. 8 passed
     Running tests/sync_contract.rs     test result: ok. 14 passed
   Doc-tests rustak_server             test result: ok. 5 passed
```

### One flake seen, and why it is not this brief's

`tests/enroll_flows.rs` failed twice in one run —
`a_cloudtak_shaped_enrolment_produces_a_certificate_the_marti_listener_accepts` and
`a_revoked_certificate_cannot_complete_the_handshake`, both with a broken pipe during the **TLS
handshake** on the Marti listener — and passed on the next run with nothing changed. It drives real
sockets while six other briefs were compiling on the same machine. Nothing here can reach it: the
failure is below HTTP routing entirely, the routes this brief adds are mounted on the public
listener only (`marti/login.rs::the_mutually_authenticated_listener_serves_no_sign_in_flow`), and
the one arm added to `resolve_principal` runs *after* the client-certificate arm has already
returned. Worth watching if it recurs on a quiet runner.

### On the concurrent runs

The workspace-wide checks were run repeatedly while six other briefs were editing the same tree.
Transient failures were observed in `rustak-client/src/{http,marti}`, `src/plugins/**`,
`src/jobs/service_health.rs`, `src/pki/acme/**`, `src/web/api/services.rs`, `src/config/validate.rs`
(a function another brief removed and restored between two of its own writes) and
`tests/acme_directory.rs`, and each cleared on its own as those briefs landed. Nothing in this
change set was implicated in any of them, and the run recorded above is the one where the whole
`rustak-server` suite was green at once.
