# M8-01 — rustak as an OpenID Connect provider — complete

Brief: `.claude/plan/briefs/M8-01-oidc-provider.md`
Read first: `conventions.md`; `plan.md` → Identity & auth model, "Our JWT"; `compat/oauth.md`
§1–§4; status `M5-01` (the two authorization codes, `oauth_server/*`, the control/attack table),
`M5-02` (the scope ceiling), `M2-02`/`M2-08` (`users.email`); `auth/{jwt,tokens}.rs`,
`auth/oauth_server/*`, `marti/{oauth,login}.rs`, `config/{auth,validate}.rs`,
`docs/deployment.md` "Browser single sign-on".

## What was built

rustak now answers the five documents a relying party reads — discovery, a key set, an ID token,
userinfo and an end-session endpoint — and accepts **confidential** clients that authenticate with
a secret instead of a proof key. The point is the step after that: the access token the relying
party ends up holding is rustak's own, so it goes straight on to `/Marti/api/tls/config` and
`/Marti/api/tls/signClient/v2` and enrols a certificate. `oidc_provider.rs` walks that whole chain
and finishes with a real mutually authenticated handshake.

| File | Functional lines | Contents |
|---|---:|---|
| `auth/oauth_server/discovery.rs` | 57 | `GET /.well-known/openid-configuration`, the document builder |
| `auth/oauth_server/scopes.rs` | 21 | `openid profile email groups`, `granted`, `grants` |
| `auth/oauth_server/clients.rs` | 81 | `client_secret_post` / `client_secret_basic`, constant-time comparison, redacting `Debug` |
| `auth/oauth_server/claims.rs` | 56 | the one claim set both the ID token and userinfo release |
| `auth/oauth_server/id_token.rs` | 48 | the ID token, signed with a `kid` |
| `auth/oauth_server/userinfo.rs` | 71 | `GET\|POST /oauth/userinfo` |
| `auth/oauth_server/code_grant.rs` | 173 | the `authorization_code` grant, moved out of `marti/oauth.rs` and extended |
| `auth/oauth_server/responses.rs` | 62 | the OAuth body shapes both grant families share |
| `auth/jwt/keys.rs` | 103 | `SigningKey` and the sealed-key helpers, split out of `jwt.rs` |
| `config/oauth.rs` | 145 | `[auth.oauth]`, `OAuthClient` (+ `secret`, `post_logout_redirect_uris`), `admin_group`, and the `--check` rules |
| `migrations/0019_oauth_code_oidc.sql` | — | `oauth_codes` gains `oidc_scope` and `nonce`; `code_challenge` becomes nullable (table rebuild) |
| `rustak-server/tests/oidc_provider.rs` | — | 18 tests; one positive chain, seventeen negatives |
| `interop/node-tak/tests/oidc-provider.test.ts` | — | 4 scenarios through the real `openid-client@6.8.8` |

**Additive edits:**

| File | Change |
|---|---|
| `auth/jwt/mod.rs` (was `auth/jwt.rs`) | `sign_id_token` (RS256 + `kid`); `SigningKey` moved to `keys.rs`. The access-token header is **unchanged**: still the 27 bytes, still no `kid`. |
| `auth/oauth_server/mod.rs` | eight new modules, three re-exports, the control/attack table extended with client authentication, the sign-out URI, our own `nonce` and `aud` |
| `auth/oauth_server/authorize.rs` | reads `scope` and `nonce`; `proof_key` now returns `Result<Option<_>, ()>` so a confidential client may omit one |
| `auth/oauth_server/codes.rs` | `NewCode`/`Redemption` carry `oidc_scope`, `nonce` and `authorized_at`; `redeem` takes `Option<&str>` for the verifier and `proof_key_holds` decides |
| `auth/oauth_server/state.rs` | `PendingKind::AuthorizationCode` carries the granted scopes, the **client's** nonce (distinct from ours) and an optional challenge |
| `auth/oauth_server/session.rs` | `/logout` gains `LogoutQuery` and the registered-only `302` |
| `marti/oauth.rs` | `client_secret` on the form, `TokenForm` loses its `Debug`, `/oauth/jwks` gains `Cache-Control: public, max-age=3600`, the code grant delegates |
| `marti/{mod,login}.rs` | `/oauth/userinfo` (GET+POST) in the `/oauth` scope; `/.well-known/openid-configuration` on the public listener only |
| `config/{auth,mod,validate}.rs` | `[auth.oauth]` moved to `config/oauth.rs`; `validate` calls `config.auth.oauth.validate()` beside `stream.limits` |
| `config.example.toml`, `docs/deployment.md`, `compat/oauth.md` §6, `README.md` | documented |

## The discovery document, exactly as served

`GET /.well-known/openid-configuration` — public listener only, exactly
`Content-Type: application/json`, `Cache-Control: public, max-age=3600`:

```json
{
  "issuer": "https://tak.example.com",
  "authorization_endpoint": "https://tak.example.com/oauth/authorize",
  "token_endpoint": "https://tak.example.com/oauth/token",
  "userinfo_endpoint": "https://tak.example.com/oauth/userinfo",
  "jwks_uri": "https://tak.example.com/oauth/jwks",
  "end_session_endpoint": "https://tak.example.com/logout",
  "response_types_supported": ["code"],
  "grant_types_supported": ["authorization_code", "refresh_token", "password"],
  "subject_types_supported": ["public"],
  "id_token_signing_alg_values_supported": ["RS256"],
  "scopes_supported": ["openid", "profile", "email", "groups"],
  "token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic", "none"],
  "code_challenge_methods_supported": ["S256"],
  "claims_supported": ["sub", "iss", "aud", "exp", "iat", "auth_time", "nonce",
                       "preferred_username", "name", "email", "groups"]
}
```

`issuer` is `[auth] issuer` (default `[server] base_url`) — the same string the tokens carry as
`iss` — and every endpoint is absolute and built from it. An installation with neither configured
answers `404 {"error":"not_configured"}` rather than building a document from a `Host` header
somebody else chose. `GET /login/.well-known/openid-configuration` is untouched: different path,
different question, still the **upstream** provider's two endpoints in TAK Server's bare shape.

## The security properties, and where each is asserted

| Property | Where |
|---|---|
| A confidential client must authenticate; wrong, empty and missing secrets are one `401 invalid_client` | `clients.rs::a_confidential_client_needs_its_own_secret_and_nothing_else_will_do`; `oidc_provider::client_authentication::{a_wrong_secret_is_invalid_client, a_missing_secret_is_invalid_client, a_basic_header_with_the_wrong_secret_is_refused_the_same_way}` |
| The comparison is constant time, and the secret is never rendered | `clients.rs::a_secret_never_appears_in_a_debug_dump`; `config/oauth.rs::a_secret_never_appears_in_a_debug_dump`; `constant_time_eq` |
| A wrong secret does **not** burn the code the real client holds | `oidc_provider::client_authentication::a_wrong_secret_is_invalid_client` (second half) |
| `client_secret_basic` is accepted, and a header/form client mismatch is refused outright | `clients.rs::{a_basic_header_is_read_and_beats_the_form, a_request_naming_two_different_clients_is_refused_rather_than_resolved}`; `oidc_provider::client_authentication::client_secret_basic_is_accepted_beside_client_secret_post` |
| A **public** client still cannot start a flow without `S256`, and `plain` is never accepted | `authorize.rs::{a_public_client_cannot_start_a_flow_without_a_proof_key, plain_is_not_a_proof_key_this_server_will_register}`; `oidc_provider::client_authentication::a_public_client_still_cannot_skip_its_proof_key`; `oauth_flows::a_public_client_cannot_start_a_flow_without_a_proof_key` |
| A confidential client that **did** send a challenge is held to it | `authorize.rs::a_confidential_client_that_did_send_one_is_still_held_to_it`; `codes.rs::a_code_issued_with_a_proof_key_is_not_redeemed_without_one`; `oidc_provider::client_authentication::a_confidential_client_that_offered_a_proof_key_is_held_to_it` |
| A missing verifier is a refusal, never a waiver | `codes.rs::a_registered_proof_key_is_never_satisfied_by_a_missing_one` |
| `--check` refuses `public = false` with no secret and `public = true` with one | `config/oauth.rs::{a_confidential_client_with_no_secret_is_refused_at_load_time, a_public_client_carrying_a_secret_is_refused_at_load_time}`; `oidc_provider::configuration::a_half_configured_client_is_refused_before_the_server_starts` |
| No `openid`, no `id_token` | `oidc_provider::tokens::a_flow_without_openid_gets_no_id_token` |
| The `nonce` is echoed byte for byte, and absent when none was sent | `id_token.rs::a_flow_that_sent_no_nonce_gets_a_token_with_no_nonce_claim`; `oidc_provider::tokens::{the_id_token_echoes_the_nonce_byte_for_byte, a_flow_with_no_nonce_produces_a_token_with_no_nonce_claim}` |
| An ID token is not a credential for this server | `id_token.rs::an_id_token_is_not_a_credential_for_this_server` |
| `aud` is the one client id, as a string | `id_token.rs::the_token_names_this_server_the_account_and_the_client_it_was_issued_to` |
| The ID token verifies against the published JWKS, key matched by `kid` | `oidc_provider::relying_party::a_relying_party_signs_somebody_in_and_enrols_them` (step 4) |
| JWK `n`/`e` are unpadded base64url, and import into a real verifier | `jwt/keys.rs::the_modulus_and_exponent_are_base64url_with_no_padding`; `interop oidc-provider "publishes a key set an ID-token verification can start from"` |
| Userinfo is bearer-only, never a cookie, `401` + `WWW-Authenticate: Bearer` otherwise | `userinfo.rs::{a_request_with_no_token_is_told_what_to_present, a_session_cookie_is_not_a_credential_here, a_token_this_server_did_not_issue_is_refused_the_same_way, a_revoked_token_stops_describing_anybody}`; `oidc_provider::userinfo_and_sign_out::userinfo_without_a_token_is_refused` |
| An account with no email address is never given one | `claims.rs::an_account_with_no_email_address_has_no_email_claim`; `userinfo.rs::a_live_token_describes_the_account_it_belongs_to` |
| `groups` is channels held, deduplicated, plus the configurable admin marker | `claims.rs::{the_groups_claim_is_the_channels_held_and_nothing_is_listed_twice, an_administrator_carries_the_marker_group_a_relying_party_maps, the_marker_group_is_whatever_the_installation_named_it}` |
| `/logout` redirects **only** to a registered URI, and preserves `state` | `session.rs::{a_registered_sign_out_uri_is_returned_to_with_the_clients_state, everything_else_signs_out_and_redirects_nowhere, a_client_that_registered_no_sign_out_uri_is_never_redirected_to}`; `oidc_provider::userinfo_and_sign_out::*` |
| A post-logout URI is held to the same rules as a redirect URI | `config/oauth.rs::a_post_logout_uri_is_held_to_the_same_rule_as_a_redirect_uri` |
| The rustak scope ceiling is untouched | `oauth_flows::scope_ceiling::*` (unchanged, still green) |
| **The password grant's body is exactly its three keys** | `oidc_provider::tokens::the_password_grant_body_is_exactly_the_three_keys_it_has_always_been`; `enroll_oauth::a_client_password_is_exchanged_for_exactly_the_body_cloudtak_reads`; interop `login.test.ts` |
| The whole chain, ending at a real mTLS handshake | `oidc_provider::relying_party::a_relying_party_signs_somebody_in_and_enrols_them` |

## Decisions worth knowing about

1. **`code_challenge` became nullable, by rebuilding the table.** The alternative — an empty string
   meaning "no proof key" — is a sentinel a redemption could read as a challenge that trivially
   matches. `0019` copies the rows (ten-minute codes, so tens of them at most) and `oauth_codes` is
   nobody's parent, so the drop fires no cascade. A NULL challenge is only reachable for a
   confidential client, because `authorize.rs` refuses to issue a public one a code without one.

2. **The OpenID scopes are a second string, never the rustak scope.** `oidc_scope` on the code row
   says what may be *said* about the account; `scope` still says what the token may *do* and is
   still clamped by `tokens::scope_for` at redemption. Conflating them would turn "tell me this
   person's email address" into administrative access.

3. **The token response's `scope` is conditional.** The granted OpenID scopes when the request
   asked for any, and the rustak scope (`api`, `api admin`) otherwise — which is what this grant has
   always answered, so nothing that reads it today changes. Both halves are asserted
   (`oidc_provider::tokens::a_request_that_asked_for_no_scope_still_reports_the_rustak_one`).

4. **The client rate-limit subject is namespaced.** `oauth-client:<id>`, because the limiter is
   keyed by (address, subject) and the password grant's subject is a *username*: an unnamespaced
   client identifier equal to somebody's username would let a request needing no credential clear
   the failures of one that does. Only a confidential client is counted, because only a confidential
   client authenticates.

5. **`id_token_hint` is not read at `/logout`.** Taking the client from an attacker-supplied token's
   `aud` would mean parsing that token before deciding where to send a browser. `client_id` is
   required instead, which costs a relying party one query parameter.

6. **Two files were split rather than grown.** `auth/jwt.rs` (294 lines) → `auth/jwt/{mod,keys}.rs`;
   `[auth.oauth]` out of `config/auth.rs` into `config/oauth.rs` with its own `--check` rules beside
   the schema, the way `stream.limits.validate()` already does it. `marti/oauth.rs` (289) shed the
   code grant and the shared response shapes. Every file is comfortably under 300 again; the largest
   touched is `authorize.rs` at 232.

## Deviations from the brief, and why

1. **Userinfo releases the full claim set for every live token, not just for password-grant
   tokens.** The brief asked for it to be narrowed to the OIDC scopes granted at the authorization
   endpoint. Those scopes are recorded against the **code**, which is spent in seconds: there is
   nowhere on a rustak access token to carry them (the 27-byte header and flat-claims rules leave no
   room, and `tokens::rotate` rebuilds the scope from the account anyway) and no table that
   remembers them. Inventing one would be new state with its own lifetime and its own way to go
   wrong. It is also not a widening: every token that reaches userinfo carries rustak's `api` scope,
   and `GET /api/v1/me` already answers the same four facts to the same token. The **ID token** *is*
   narrowed by scope, which is where a relying party reads claims from anyway
   (`oidc_provider::tokens::an_id_token_only_carries_the_claims_its_scopes_asked_for`). Recorded in
   `compat/oauth.md` §6.6 and in the backlog note below.

2. **`compat/oauth.md` gained a §6, not a §5.** §5 was already "Group-claim mapping (OIDC groups →
   rustak groups)" — the other direction — and renumbering an existing section other documents cite
   would be worse than the off-by-one. §6 opens with a note saying so.

3. **`config/mod.rs` was edited** (one `pub mod oauth;` and two re-export lines) although it is not
   in "Files you own". It is the unavoidable consequence of splitting `config/auth.rs`, and M5-01
   made the same one-line edit for the same reason.

4. **One existing assertion changed.** `oauth_flows::a_code_with_no_verifier_at_all_is_refused` now
   expects `invalid_grant` rather than `invalid_request`: the verifier is required of a *public
   client* rather than of the form, so its absence is a binding that did not hold. RFC 7636 §4.6
   says `invalid_grant` for exactly this. The test carries that note.

## What I could not verify

- **Nothing was tested against a real CloudTAK.** Upstream has not shipped its relying-party back
  end (dfpc-coe/CloudTAK#661) and TAK.NZ's fork was not run. What *is* verified is that a real
  relying-party library (`openid-client@6.8.8`, pinned) accepts the discovery document, the key set
  and the userinfo response, and that a hand-written relying party completes the whole flow
  including enrolment. The claim mapping in `docs/deployment.md` is from the brief's summary of the
  fork, not from running it.
- **No browser sign-in in the interop suite.** That harness has no upstream identity provider, so
  the code exchange stays in the Rust suite, which has both halves in one process. The interop
  scenario covers discovery, JWKS and userinfo with a password-grant token.
- **`client_secret_basic` with a secret containing `+` or `%`.** The identifier is form-decoded and
  the secret is taken raw, which is the interoperable choice for clients that do not encode (a `+`
  would otherwise decode to a space and fail invisibly) and the wrong one for a conforming client
  with such a secret. `client_secret_post` is unaffected either way. Documented in `clients.rs`.
- **Key rotation while a relying party holds a cached key set.** The JWKS publishes every retired
  key, and libraries refetch on an unknown `kid`, so this should be seamless — but no test rotates
  the key mid-flow.

## For the backlog

- **Record the granted OIDC scopes against the access token**, so `/oauth/userinfo` can narrow its
  claim set the way the specification describes. Needs somewhere to put them that survives a
  refresh; the refresh-token family row is the obvious candidate, since it already carries a scope.
- **Dynamic client registration** is not implemented and probably should not be: every client here
  is first-party by construction, which is also why there is no consent screen.
- **`prompt`, `max_age`, `login_hint` and `acr_values`** on `/oauth/authorize` are ignored. None is
  needed by CloudTAK; `max_age` in particular would need a re-authentication ceremony
  (see M5-02's L4 note on step-up authentication).
- **An e2e spec for the sign-out redirect.** The Rust and interop suites cover it; Playwright does
  not.

## Exit checks

```
$ cargo fmt --check
(no output)

$ cargo clippy --workspace --all-targets -- -D warnings
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.32s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 11.87s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 6 other files

$ ./scripts/check-file-length.sh
(no output)

$ cargo test -p rustak-server
test result: ok. 1842 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 34.76s
... 28 further suites, every one `ok`; oidc_provider: 18 passed, oauth_flows: 31 passed,
enroll_oauth: 14 passed, enroll_flows: 20 passed

$ cd interop/node-tak && npm run typecheck
> tsc --noEmit
(no output)

$ npm test
ℹ tests 34
ℹ pass 34
ℹ fail 0
ℹ skipped 0
```

`check-file-length.sh` reads `git ls-files`, so the new (untracked) files are not covered by it yet;
each was measured by hand with the same `awk` and the largest is `code_grant.rs` at 173.
