# M0-11 — admin web server, `/api/v1`, OIDC + passkeys, setup wizard API — complete

Brief: `.claude/plan/briefs/M0-11-web-api-auth.md`
Read first: `conventions.md`; `plan.md` → Identity & auth model, Listeners, Decisions;
`design/01-foundations-storage-ci.md` §3.4, §6 (all); `design/03-identity-pki-acme-auth.md` §5, §6, §8;
`research/01-automate-architecture.md` §4, §6; `../automate/agent/src/web/**` and
`../automate/agent/src/testing/oidc.rs`; status files `M0-03`, `M0-04`, `M0-06`…`M0-10`.

## What was built

Four module trees plus one `pki` file, replacing the `web/mod.rs` and `identity/mod.rs` stubs.
`lib.rs` was touched once, to add the feature-gated `pub mod testing;` the brief allows.

| File | Functional lines | Contents |
|---|---:|---|
| `web/mod.rs` | 7 | module doc (why TLS is not optional; why there are no cookies) + `mod`/`pub use` |
| `web/server.rs` | 65 | `services(context, limiter)` (the `App` a test and the listener share), `build_public(context, tls)` |
| `web/tls.rs` | 161 | `resolve()` per `TlsMode`, `server_names()`, PEM loading, the ACME refusal, idempotent rustls provider install |
| `web/ui.rs` | 49 | `include_dir!` SPA, `robots`, content types (`application/json`, never with a charset) |
| `web/telemetry.rs` | 192 | `TracingLogger` lifted; query/header redaction extended with `setup_token`, `registration_token`, `code_verifier` |
| `web/helpers/request.rs` | 62 | `client_ip`, `client_address`, `is_https`, `base_url_from`, `request_base_url` |
| `web/helpers/oidc/{mod,discovery,validate,exchange,claims,pkce}.rs` | 10/81/101/109/92/32 | lifted and split; PKCE `S256` pair, `code_verifier` passthrough, nonce checking |
| `web/api/mod.rs` | 77 | `configure()`: the public routes, then the guarded scope |
| `web/api/error.rs` | 112 | `json_ok`, `json_error`, `json_error_code`, `ApiError`, `ApiResult` |
| `web/api/middleware.rs` | 71 | `bearer_token`, `api_auth` (the actix half of `auth::resolve`) |
| `web/api/extract.rs` | 57 | `Identity` (= `auth::resolve::Resolved`), `Authenticated`, `Administrative` |
| `web/api/{health,me,auth,passkey,setup,users,audit,settings}.rs` | 30/14/196/284/212/109/45/14 | the endpoints |
| `auth/acl.rs` | 60 | `AuthRequestFilter`, `AclOutcome`, `evaluate`, `json_to_filter_value` |
| `auth/ratelimit.rs` | 82 | in-memory `RateLimiter` (bucket per address+subject, swept on demand) |
| `auth/resolve.rs` | 85 | `RequestFacts`, `Resolved`, `AuthFailure`, `bearer()` |
| `auth/tokens.rs` | 134 | `issue_session`, `rotate` (family revocation on reuse), `revoke`, `scope_for` |
| `auth/setup.rs` | 192 | setup token (`ensure`/`verify`/`consume`, 0600) and registration token (`issue`/`claim`) |
| `auth/passkeys.rs` | 253 | `Passkeys::{for_base_url, start_registration, finish_registration, start_login, finish_login}`, `to_summary` |
| `auth/passkey_store.rs` | 117 | ceremony state in `auth-state` (one-shot, 5 min), `sweep`, row ⇄ `Passkey`, `user_handle` |
| `identity/{mod,users,groups,settings}.rs` | 4/165/109/96 | provisioning, principal building, channel mapping, wizard settings |
| `pki/server_cert.rs` | 248 | `ServerCertificate`, `load_or_issue` (reissues on name change, CA change or renewal window) |
| `testing/{mod,context,oidc,authenticator}.rs` | 7/96/193/252 | `TestServer`, `TestIdentityProvider`, `SoftAuthenticator` |

**211 unit and endpoint tests** added, each file with its single trailing column-0 `#[cfg(test)] mod tests`.

### Manifest changes

- Root `[workspace.dependencies]`: `webauthn-rs = "0.5.5"` with
  `danger-allow-state-serialisation` (the ceremony state goes in the key/value store),
  `danger-credential-internals` (the `passkeys` columns are filled from the credential), and
  `conditional-ui` (username-less sign-in). 0.5.5 is the current stable; 0.6.1-dev is a pre-release.
  Licence is MPL-2.0 — file-level copyleft, so linking is fine for an MIT project, unlike the GPL
  sources `conventions.md` forbids copying from.
- `rustak-server`: `webauthn-rs` added; `tempfile` and `wiremock` added as **optional**
  dependencies enabled by the `testing` feature, so `src/testing/**` compiles both under
  `cargo test` (where they are dev-dependencies) and under `--features testing`.
- `services/mod.rs`: the two `Late<T>` aliases repointed —
  `ContentStore = crate::store::ContentStore`, `JwtKeys = crate::auth::jwt::JwtIssuer`.
- `config.example.toml`: one comment added under `[server] base_url` saying what that host name
  means for WebAuthn. No new keys.

## Security deltas, point by point

### The public listener always serves TLS

`web::tls::resolve(config, db, secrets, ca)` answers `[web.public] tls.mode`:

| mode | behaviour |
|---|---|
| `internal` (default) | `pki::server_cert::load_or_issue` issues a leaf from our CA for `[server] domains` + the wizard's stored domains + `[acme] domains` + `[pki] server_names`, with `[pki] server_ips` as IP SANs. The key is sealed (`SecretContext::ServerCertKey`) in the `pki` key/value partition. |
| `files` | chain and key read from `cert_file`/`key_file`, each failure naming the path. |
| `acme` | `Kind::User` error: "ACME arrives in M2, so this server cannot obtain a public certificate for itself yet", with both alternatives as advice. |
| `none` | refused unless `allow_insecure_http` is **also** set; when it is, a `warn!` says what is being carried in the clear. |

The certificate is **reissued** when the covered names change, when the CA changes, or inside
`[pki] server_cert_renew_before` — the stored record carries the exact name list it was issued
for. Without that, adding a host name would appear to work and then fail at every handshake with
a name mismatch nobody would blame on a cached certificate.

### No local passwords

Nothing in this change set reads or writes a password. Sign-in is OIDC or passkey; the first
administrator is created by the setup token and immediately registers a passkey.

### The one-time setup token

`auth::setup::ensure(db, path)` runs at start-up (M0-12 calls it, see the handoff below): on an
installation with no administrator and no completed wizard it mints 32 bytes, stores the SHA-256
in `auth-state`, and writes the token to `[auth] setup_token_file` with mode **0600**, returning
it so start-up can log where it is. Restarting does **not** invalidate a token somebody is
half-way through typing; deleting the file mints a fresh one, so an installation can never be left
with no way in. `verify` is a constant-time digest comparison, and an absent token and a wrong one
are refused with the identical message. `consume` removes the record and the file when the wizard
completes.

The registration token that `POST /setup/admin` returns is the same shape with a ten-minute life
and is spent on first use, so it can register exactly one passkey for exactly one account.

### Setup routes answer 410 once completed

Every `/api/v1/setup/*` route calls `open()` first, which reads `settings.server.setup_completed_at`.
Deliberately **not** "does an administrator exist": a test asserts that deleting the last
administrator after completion still gets `410`, because a guard on the account count would hand
whoever emptied the table a way to create one without a credential.

### Bearer only, no cookies

`Set-Cookie` appears nowhere in this change set. The credential is `Authorization: Bearer` and
nothing else, so there is no CSRF surface and no double-submit token. The `/login/*` cookie flow
TAK clients need arrives in M2, scoped to those paths.

### Rate limiting

`auth::ratelimit::RateLimiter` — a bucket per `(client address, subject)`, `[auth.rate_limit]`
attempts/window/lockout, swept when the map exceeds 1024 entries so a flood of distinct addresses
cannot make the sweep quadratic. One instance per process, built in `build_public` **outside** the
`HttpServer::new` factory closure: one built inside would be one bucket per worker thread, and an
attacker would get that many times as many attempts. Applied to `/auth/token`, `/auth/refresh`,
`/setup/admin` and all four passkey ceremony endpoints; `429` carries `code: "rate_limited"`.

## `/api/v1` as built

| Method/path | Who | Notes |
|---|---|---|
| `GET /health` | public | `Health`, plus one `SELECT 1` through a reader. Says nothing about storage. |
| `GET /auth/metadata` | public | `AuthMode::Oidc{…}` from cached discovery, else `AuthMode::Passkey`. A provider that is **down** falls back to passkeys rather than failing — that is what keeps an administrator able to sign in when the directory is out. |
| `POST /auth/token` | public | code + `redirect_uri` + `code_verifier` → exchange → validate → `user_acl` → provision → **our** session. |
| `POST /auth/refresh` | public | rotates; reuse revokes the family. |
| `POST /auth/logout` | user | revokes the `jti` and every refresh token for the account. |
| `GET /me` | user | channels read per request, never from the token. |
| `POST /auth/passkey/register/{start,finish}` | session **or** registration token | `finish` answers with a `TokenResponse` on the bootstrap path and a `PasskeySummary` otherwise. |
| `POST /auth/passkey/login/{start,finish}` | public | named or discoverable. |
| `GET /auth/passkeys`, `DELETE /auth/passkeys/{id}` | user (own only) | delete refuses the caller's **last** passkey when they have no OIDC identity; somebody else's answers `404`, the same as one that does not exist. Both audited. |
| `GET /setup/status` | public | `SetupStatus`. |
| `POST /setup/admin` | setup token | `409` once an administrator exists, `410` once the wizard is done. |
| `POST /setup/server`, `POST /setup/ca`, `POST /setup/complete` | admin | `ca` is `409` if one exists — it is what every enrolled device trusts. |
| `GET /users`, `PATCH /users/{username}` | admin | disabling also revokes refresh tokens; an administrator cannot disable or demote themselves. |
| `GET /audit` | admin | `category`/`subject`/`actor`/`before`/`limit`, capped at 500. |
| `GET /settings` | admin | configuration file wins over the wizard wherever it says something. |

Every response body goes through `json_ok`/`json_error`, which set `Content-Type:
application/json` with **no** charset parameter.

## Deviations from the brief and the designs, and why

1. **`user_acl` is the sign-in gate, not the per-request gate.** Design 01 §6.2 has `api_auth`
   evaluate `user_acl` over `AdminRequestFilter` on every request. But both filters *deny by
   default*, and on a bearer request there are no identity-provider claims to satisfy them with —
   so an installation that had never written one would refuse every request, including after a
   successful passkey sign-in. As built: `user_acl` is evaluated in `POST /auth/token`, where the
   claims exist, and denies by default there (a federated installation that has not said who may
   in lets nobody in, which is the intended posture). On an authenticated request the gate is the
   account existing and not being disabled, **plus** the expression when one *is* configured — so
   a change to it still takes effect on the next request, which was the property §6.2 wanted. The
   same rule applies to `admin_acl`, ORed with `users.admin_override ?? users.is_admin`.
   Passkey sign-in is not gated by `user_acl` at all: the passkey was registered against an
   account that already exists here, so the decision was made when it was registered.

2. **The whole serialised credential goes in `passkeys.public_key`, not a bare COSE key.**
   `webauthn-rs` verifies an assertion against a `Passkey`, which carries the public key, the
   counter, the backup flags, the registered extensions and the attestation. Rebuilding one from
   columns by hand means reconstructing all of that, and any mistake is a verification that
   quietly stops meaning what it should. `credential_id`, `sign_count`, `transports`,
   `backup_eligible` and `backup_state` are still mirrored into their own columns for indexing and
   for the UI. The column's doc comment in `db/repos/passkeys.rs` says "COSE public key" and is now
   slightly narrower than the truth — worth a one-line amendment when that file is next touched.

3. **No `auth/principal.rs`.** `rustak_core::identity::Principal` (M0-04) is the type, and the two
   places that build one — `identity::users::principal` and `auth::resolve::bearer` — are where it
   belongs. A third file that only re-exported would have said nothing.

4. **`identity/{members,devices,credentials}.rs` were not written.** The brief says "as needed";
   nothing in M0 needs them, and the repositories are already thin.

5. **`web/principal.rs` folded into `web/api/extract.rs`.** Design 01 §6.1 puts the extractors in
   `web/principal.rs`; they are five lines each and belong beside the `ApiError` they return.

6. **The listener refuses an *address* as a WebAuthn relying party.** WebAuthn identifies a relying
   party by domain, so `https://192.0.2.10` cannot register a passkey whatever else is configured;
   `Passkeys::for_base_url` says so rather than letting browsers refuse every prompt silently. The
   development relaxation (`allow_any_port`) therefore applies to `localhost` and `*.localhost`,
   **not** `127.0.0.1`, which cannot be a relying party at all.

7. **`webauthn-authenticator-rs` was not used for the round-trip test.** Its `softpasskey` feature
   requires `openssl`/`openssl-sys`, a system library this workspace has avoided everywhere
   (rustls over `aws-lc-rs`, `protox` instead of `protoc`), and one that would have to exist on
   every cross-compilation image for `cargo test` to run. `testing/authenticator.rs` performs the
   real ceremony in about 250 lines using `rsa`, `sha2` and `jsonwebtoken`, all already here. It is
   a genuine authenticator: real key pair, real CBOR, real signature over
   `authData || sha256(clientDataJSON)`. **No gap to record** — the ceremony is tested end to end.

8. **`config.example.toml` was edited.** One comment under `[server] base_url`, because the host
   name is now load-bearing for passkeys and `conventions.md` requires every key to be documented
   with its consequences. No keys added or changed.

## Notes for the orchestrator and the briefs that follow

- **M0-12 (`run()`)** must, in this order: install the rustls provider; open the database and the
  secret store; `pki::load_or_create_root_ca`; `JwtIssuer::load_or_create` → `install_jwt`;
  `ContentStore::new(...).prepare()` → `install_content`; `auth::setup::ensure(db,
  &config.setup_token_file())` and **log the returned path** (`Setup required: open
  https://<host>/setup and enter the token from <file>`); `web::tls::resolve(config, db, secrets,
  Some(&ca))`; `web::build_public(context, tls)`. `build_public` returns the unstarted `Server`
  with signals disabled and a ten-second shutdown timeout, so `server.handle().stop(true)` is what
  the shutdown token drives.
- **Jobs M0-12 should schedule:** `auth::passkey_store::sweep(db)` and `RateLimiter::sweep()` every
  ten minutes, `db.revoked_jtis().prune()` hourly. None are load-bearing for correctness — an
  expired ceremony is refused whether or not it has been swept — so they are housekeeping.
- **M0-14 (e2e) and any `tests/` suite** should run with `--features testing`, which exposes
  `rustak_server::testing::{TestServer, TestIdentityProvider, SoftAuthenticator}`. The whole of
  `cargo test -p rustak-server --all-features` is green.
- **M2 (`auth/`)** adds `basic.rs`, `stream_auth.rs` and the client-certificate arm beside
  `auth::resolve::bearer`, all returning the same `Resolved`; `ListenerAuthPolicy` from design 03
  §5 belongs there. `validate_token` already takes the `expected_nonce` the `/login/*` flow will
  supply, and `pkce_pair()` is ready for the server-driven authorize.
- **M2 (`pki/`)** gets `server_cert::load_or_issue` for the Marti and stream listeners as well;
  the record already reissues on a CA change. `server_certificate_path` is exported for an export
  M2 may want.
- **The file-length script only checks git-tracked files** (`git ls-files '*.rs'`), so the new
  files were checked by hand with the same `awk` until they are committed. The largest is
  `web/api/passkey.rs` at 284.
- **`db/repos/passkeys.rs`'s `public_key` doc comment** should be amended per deviation 2.

## Exit checks

```
$ cargo test -p rustak-server
     Running unittests src/lib.rs (target/debug/deps/rustak_server-...)
running 599 tests
test result: ok. 598 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 5.20s
     Running unittests src/main.rs (target/debug/deps/rustak-...)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
   Doc-tests rustak_server
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p rustak-server --all-features
test result: ok. 597 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test --workspace
871 passed, 0 failed   (12 binaries)

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ cargo clippy --workspace --all-targets --all-features -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s)
   Generated target/doc/rustak_api/index.html and 6 other files

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ cargo fmt -p rustak-server --check
(no output)

# `cargo fmt --all --check` was clean when this brief's work finished; a
# concurrent brief has since added unformatted files under `rustak-cot/src/**`,
# so the workspace-wide check now reports those and not these. Nothing in this
# change set was formatted by this brief's runs of `cargo fmt --all` — the
# rustak-cot files landed afterwards.

$ ./scripts/check-file-length.sh
(no output, exit 0 — and every new file checked by hand with the same awk; largest 284)
```

### What the endpoint tests cover

- **OIDC forgery suite** (`web/helpers/oidc/validate.rs`, 14 tests), driven against a real
  provider serving real discovery, a real key set and real RS256 tokens over HTTP: a genuine token
  is accepted and its claims come back intact (the positive control); a token signed by a key the
  provider does not publish is refused; a token signed by the provider but labelled with an
  unpublished `kid` is refused; expired, wrong-audience and **missing**-audience tokens are
  refused; a symmetric algorithm is refused before verification; a nonce from another flow is
  refused; the key set is fetched once and an unknown key forces exactly one refetch.
- **The whole browser flow** (`web/api/auth.rs`): metadata → the published authorization endpoint
  → the redirect back → the code exchange → one of our own sessions, with the `state` returned
  untouched. A provider that insists on `code_verifier` proves we send it.
- **Passkey ceremony round trip** (`web/api/passkey.rs`, 13 tests) against the software
  authenticator: register then sign in; sign in without naming an account (discoverable); a
  challenge is good for exactly one attempt; an assertion produced at another origin is refused
  (the same credential, the same key, the same challenge — only the page differs); a **cloned**
  authenticator is caught by its counter; the wizard's registration token signs the new
  administrator in; a registration token is spent on first use; an account with no passkey and one
  that does not exist answer identically; nobody can remove their last way in or somebody else's
  passkey; a disabled account cannot sign in with a passkey it still holds.
- **Setup gating** (`web/api/setup.rs`, 9 tests): a wrong token creates nothing; guessing is rate
  limited; a second administrator is `409`; the authority is created once; **every** wizard route
  answers `410` afterwards and the token file is deleted; deleting the last administrator does not
  reopen it; an ordinary account cannot drive it.
- **The gate** (`web/api/mod.rs`): every one of the eleven protected routes answers `401` without a
  session, and the three public ones answer without one.
