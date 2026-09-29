# M10-06 — `/oauth/userinfo` releases only what the granted scopes allow — complete

Brief: `.claude/plan/briefs/M10-06-userinfo-scopes.md`. Closes the M8-01 deviation recorded in
`compat/oauth.md` §6.6 and the backlog line about userinfo releasing the full claim set.

## What changed and why

1. **The grant is recorded against the session** (migration `0024_refresh_token_oidc_grant.sql`).
   `refresh_tokens` gains two nullable columns, `jti` (the access token minted beside this refresh
   token) and `oidc_scope` (the granted OpenID scopes), plus a partial unique index on `jti`.
   - Issuance: `tokens::issue_granted_session` (new; `issue_session` delegates to it with `None`)
     writes both in the same `INSERT` that already stored the refresh token — one write per
     issuance, no new table. `code_grant.rs` passes `redemption.oidc_scope`.
   - Refresh: `tokens::rotate` copies `spent.oidc_scope` unchanged onto the new row, with the new
     access token's `jti`. A refresh can neither widen nor narrow the grant.
   - Deletion: the grant lives on the session row, so it goes with it — pruned at refresh expiry,
     cascaded with the account, and ignored once the family is revoked
     (`RefreshTokensRepo::oidc_grant` reads `revoked_at IS NULL AND expires_at > now`).
2. **Userinfo reads it.** `userinfo.rs` looks up the grant by the presented token's `jti` and
   passes it to `claims::released`; no record ⇒ `openid`.
3. **One mapping.** `claims::RELEASED_BY` (claim → scope) now drives `claims::released` (used by
   both the ID token and userinfo) and `discovery::claims_supported()`. `id_token::ENVELOPE` lists
   the non-account claims an ID token carries.
4. **Discovery is truthful.** `scopes_supported` is unchanged (`openid profile email groups`, each
   releases something — unit-tested); `claims_supported` is built from `RELEASED_BY` + `ENVELOPE`
   and is byte-identical to before; a test asserts every claim a full ID token carries is in it.

## Decisions

- **`preferred_username` moved from "always" to `profile`.** The brief says `openid` alone is
  `sub` only; OIDC Core §5.4 puts `preferred_username` under `profile`. Because the mapping is
  shared, an **ID token** for `openid` alone also loses `preferred_username` now. CloudTAK's relying
  party asks for `openid profile email groups` by default (M8-01 brief), so it is unaffected.
- **Refresh-token row, not a new table.** The row already has the session's lifecycle (one write
  per issuance, copied by rotation, revoked with the session, pruned, cascaded with the user); a
  separate table would have to reimplement all four. M8-01's backlog note pointed here too.
- **A revoked session's still-live access token answers `sub` only.** `POST /logout` / "sign out
  everywhere" revokes refresh families but, by existing design, leaves access tokens valid until
  expiry. Such a token is no longer part of a session, so it no longer carries the session's grant
  (test `a_revoked_session_takes_its_grant_with_it`). `GET /logout` revokes the `jti` itself, so
  userinfo refuses it with `401`.
- **A spent (rotated) row keeps its grant** until its family is revoked or it expires, because the
  access token minted beside it is still live.

## Who depended on the full claim set for grant-less tokens (brief item 3)

Searched the whole repository (Rust, `rustak-ui`, `e2e`, `interop/{node-tak,cloudtak,eud}`,
docs) for `userinfo`, `preferred_username` and the endpoint path:
- **`interop/node-tak/tests/oidc-provider.test.ts`** was the only consumer: it called userinfo with a
  password-grant token and asserted `preferred_username` and `groups`. It is a test of this very
  behaviour, not a real consumer; updated to assert the narrowed `{ sub }` body exactly.
- **`rustak-ui`** uses `GET /api/v1/me`, never userinfo.
- **CloudTAK compose scenario** (`interop/cloudtak`): no reference to userinfo. Stock CloudTAK uses
  the password grant and reads its token locally; its OIDC relying party is unshipped
  (dfpc-coe/CloudTAK#661), and will ask for `profile email groups` anyway.
- Nothing else. So the grant-less case was changed to `sub` only as the brief asks; no case was
  kept as before, and nothing needs an orchestrator decision.

## Tests

| Test | What it proves |
|---|---|
| `tests/oidc_provider.rs::userinfo_narrowing::each_grant_releases_exactly_what_its_scopes_cover` | exact bodies for `openid`, `openid profile`, `openid email`, `openid groups`, all four; `application/json`, `no-store` |
| `…::a_refresh_carries_the_grant_unchanged` | two rotations; all three access tokens answer `{sub, email}` |
| `…::after_sign_out_the_token_is_refused` | `401`, exact body, `WWW-Authenticate: Bearer error="invalid_token"`, `no-store` |
| `…::a_revoked_session_takes_its_grant_with_it` | families revoked, access token still live ⇒ `{sub}` |
| `…::a_token_with_no_recorded_grant_is_told_sub_alone` | a code flow with no OpenID scope; a bare `jwt.issue` token (jwt-bearer/service shape) |
| `…::a_password_grant_token_is_told_sub_alone` | CloudTAK's current credential ⇒ `{sub}` |
| `userinfo.rs::{a_session_with_no_recorded_grant_is_told_sub_alone, a_granted_session_is_told_what_its_scopes_cover}` | handler-level, exact bodies |
| `claims.rs::{openid_alone_releases_sub_and_nothing_else, every_scope_this_server_grants_releases_something_it_names}` | the mapping |
| `id_token.rs::{every_claim_a_token_can_carry_is_one_discovery_advertises, openid_alone_names_the_account_by_sub_and_nothing_else}` | ID token ↔ discovery; ID token narrowing |
| `discovery.rs::the_scopes_and_claims_advertised_are_exactly_what_can_be_released` | exact `scopes_supported`/`claims_supported` |
| `refresh_tokens.rs::an_oidc_grant_is_found_by_its_access_tokens_jti_while_the_session_lives` | lookup, spent row still readable, revoked family not |

**On a host ten times slower:** none of these tests has a time bound. They assert bodies, statuses
and database state; the only timeouts are the existing generous ones in the shared harness
(`TIMEOUT` = 10 s per network exchange with the fake identity provider, which only fails a hung
wait). Access-token lifetime defaults are far longer than any of these flows would take.

## Files

Added:
- `rustak-server/migrations/0024_refresh_token_oidc_grant.sql`
- `.claude/plan/status/M10-06-userinfo-scopes.md`

Changed:
- `rustak-server/src/auth/oauth_server/{claims,userinfo,discovery,id_token,code_grant}.rs`
- `rustak-server/src/auth/tokens.rs` (the session/token issuance the recording needs)
- `rustak-server/src/db/repos/refresh_tokens.rs` (the session store)
- `rustak-server/tests/oidc_provider.rs`
- `interop/node-tak/tests/oidc-provider.test.ts`
- `docs/deployment.md` ("rustak as CloudTAK's identity provider": claim/scope table and the rule)
- `.claude/plan/compat/oauth.md` §6.5–6.6 — **outside the brief's file list**; it documented the
  M8-01 deviation this brief removes, and leaving it would contradict the code. Smallest change:
  the §6.6 table and a paragraph replacing the deviation note.

## Checks

`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` (and additionally
`-p rustak-server --features testing`), `RUSTDOCFLAGS="-D warnings" cargo doc --workspace
--no-deps`, `./scripts/check-file-length.sh`, `cargo test -p rustak-server` (and with
`--features testing`): all pass. `interop/node-tak`: `npm ci` + `npm run typecheck` pass; the suite
itself was **not run** (needs a live server session); left to CI.

## Open

- **Migration number.** `0024` assumes no other M10 brief also takes `0024`; if one does, the
  orchestrator renumbers one of them (the runner refuses duplicates/gaps).
- **`backlog.md`** still carries the userinfo line; not edited per the wave rules — the
  orchestrator can remove it.
- A relying party granted OpenID scopes **without** `openid` (e.g. `profile email`) gets no ID token
  but userinfo still releases per those scopes. OIDC does not strictly define this case; left as is.
