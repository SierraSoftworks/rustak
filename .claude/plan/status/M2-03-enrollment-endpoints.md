# M2-03 — Enrollment endpoints, `/oauth/token`, the Marti mTLS listener and principal resolution — complete

Brief: `.claude/plan/briefs/M2-03-enrollment-endpoints.md` (including both addenda).
Read first: `conventions.md`; `compat/{enrollment,oauth,cloudtak}.md`; `design/03-identity-pki-acme-auth.md`
§5–§7; `design/04-marti-api-missions-files-profiles.md` §0 (D1–D4); status files `M2-04`, `M2-01`,
`M2-02`, `M0-11`, `M0-12`.

## What was built

Three new files under `auth/`, two under `marti/`, one endpoint on `/api/v1`, one DTO, the second
HTTP listener, and two integration suites.

| File | Functional lines (limit 300) | Contents |
|---|---:|---|
| `auth/basic.rs` | 79 | `BasicCredential`, `basic_credential`, `verify_basic`, `CHALLENGE`/`REALM` |
| `auth/cert.rs` | 104 | `client_cert`: fingerprint → row → account → device-effective channels |
| `auth/resolve.rs` | 200 (was ~85) | `BasicPolicy`, `ListenerAuthPolicy`, `basic_purpose`, `resolve_principal`, `AuthFailure::RateLimited`, `Resolved::claims` now optional |
| `marti/tls.rs` | 208 | the five `/Marti/api/tls/*` + `/device/profile/*` endpoints, `Accept` dispatch, the `ns2:certificateConfig` document, the JSON/XML bodies, the Basic challenge |
| `marti/enroll.rs` | 129 | `SignQuery`, `issue` (parse → CN → device → sign → spend), `internal_error` |
| `marti/oauth.rs` | 213 | `POST /oauth/token` (password + refresh grants), `/oauth/token_key`, `/oauth/jwks` |
| `marti/mod.rs` | 171 (+30) | the `tls`/`device` routes, the new `/oauth` scope, eight rows in `PATHS` |
| `marti/principal.rs` | 107 (+18) | `auth_policy` widened, `AuthPolicy::listener()`, `resolve` delegates |
| `web/server.rs` | 110 (+45) | `marti_services`, `build_marti` |
| `web/api/users.rs` | 174 (+56) | `POST /api/v1/users` |
| `services/mod.rs` | 179 (+20) | the `Late<Pki>` slot: `install_pki`, `pki`, `has_pki` |
| `runtime.rs` | 164 (+20) | the Marti listener spawn, `serve_named` |
| `rustak-api/src/user.rs` | 127 (+24) | `CreateUserRequest` |
| `tests/enroll_flows.rs` | — (`tests/` exempt) | 10 scenarios, including a **real mTLS socket** |
| `tests/enroll_oauth.rs` | — | 10 scenarios over the real `App` |

**1046 lib tests** (M2-04 left 892; the rest are this brief's and the two concurrent ones'),
**20 new integration scenarios**, and **16/25 node-tak interop scenarios green** where 7 were
before. No new dependencies; no manifest change.

### Routes as mounted

Mounted on **both** listeners, in `marti::services`:

| Method | Path | Auth | Answer |
|---|---|---|---|
| GET | `/Marti/api/tls/config` | Basic (enrolment) \| Bearer \| cert | `200 application/xml`, `ns2:certificateConfig`, ≥2 `nameEntry` |
| POST | `/Marti/api/tls/signClient/v2?clientUid=&version=` | same | `200`; JSON `{signedCert, ca0…}` or XML `<enrollment>` by `Accept`; `400` otherwise |
| POST | `/Marti/api/tls/signClient` | same | `200 application/octet-stream`, PKCS#12, aliases `signedCert`/`ca0`… |
| GET | `/Marti/api/tls/profile/enrollment?clientUid=` | anonymous | `204` |
| GET | `/Marti/api/device/profile/connection` | anonymous | `204` |
| POST | `/oauth/token` | the grant carries the credential | `200 application/json` `{access_token, token_type, expires_in}`, `Cache-Control: no-store` |
| GET | `/oauth/token_key` | anonymous | `{"alg":"SHA256withRSA","value":"<PEM>"}` |
| GET | `/oauth/jwks` | anonymous | `{"keys":[…]}` |

Plus `POST /api/v1/users` (admin, `201`, `409` on a duplicate, audited as `user.created`).

## Decisions worth recording

### `Resolved.claims` is an `Option`, and that is the shape of the whole change

A certificate and a Basic credential carry no token claims. The alternative — synthesising an
`AccessClaims` for them — would put a `jti` in the audit log that revocation could never match, and
an `exp` nothing had promised. So `claims` became `Option<AccessClaims>` with a `Resolved::token()`
accessor, which rippled to exactly three places: `web/api/auth.rs::logout` (which now returns `204`
without revoking anything when there is no `jti` — a `/api/v1` session is always a bearer token, so
this is unreachable in practice and an honest answer if it ever is not) and the two test fixtures in
`web/api/{extract,subject}.rs`.

### Resolution order is certificate, bearer, Basic — and a skipped arm is never read

`resolve_principal` tries a client certificate first because it is the strongest thing a caller can
present and the only one that cannot be replayed from a log; a request carrying both a certificate
and a header is answered as the certificate. An arm the policy does not allow is **not read at all**
rather than read and refused: `ListenerAuthPolicy::basic_purpose` returns `None` for every path but
`/Marti/api/tls/**` and `/oauth/token`, so a stray Basic header on `/Marti/api/groups/all` cannot be
turned into a password-guessing oracle. A test enumerates both the two paths that answer and five
that must not, including `/Marti/api/tls`, which is close enough to the prefix to look right.

### A bearer token that does not verify falls through rather than refusing

Design 04 D2: the same header will carry mission tokens in M4. `resolve_principal` therefore treats
a failed bearer verification as "no identity from this header" and carries on to Basic, and only a
read failure (`AuthFailure::Unavailable`) short-circuits. This is the same rule M2-04 established in
`marti::principal`, applied one level down so that every listener gets it.

### The Basic arm fails closed when a listener installed no rate limiter

`resolve_principal` reads the `RateLimiter` out of the request's application data. A listener that
did not install one gets an `error!` and a refusal rather than an unlimited password endpoint —
asserted by a test, because "the limiter was forgotten" is exactly the wiring mistake that produces
a working server nobody notices is wrong.

### A one-time token is spent by `signClient`, never by `tls/config`

`verify_basic` records an **ordinary** use (`consumed = false`), which `credentials::record_use`
ignores for a single-use credential by design. The consuming call is in `marti::enroll::spend`,
after the certificate exists and its row is written. A test asserts that two consecutive
`verify_basic` calls both succeed, and an integration scenario asserts the second `signClient/v2`
with the same token is a `401`.

### The enrolment refusals are plain text with a challenge, not the Marti envelope

Every other Marti route answers `{status, code, message}` JSON. These three answer
`401 text/plain "Unauthorized"` with `WWW-Authenticate: Basic realm="rustak"`, because they are the
only paths where a client is *expected* to retry with credentials and the challenge is how it is
told so. `/oauth/token` answers OAuth error objects (`{"error", "error_description"}`) for the same
reason: its callers parse that shape and not ours.

### The CN check is done twice, on purpose

`Pki::enroll` validates the common name and returns `Kind::User` for that and for a malformed
request alike. A client cannot tell `400` from `403` apart if both arrive as `400`, and "retry with
a different credential" and "fix your request" are different instructions. So `marti::enroll::issue`
parses the request itself first, compares the common name, and answers `403`; everything else falls
through to `Pki::enroll` and becomes a `400`. The second parse costs one pass over ~1 KB.

### `Pki` is a `Late` slot on `AppContext`, not a `Services` method

The brief's `build_marti(ctx)` takes only the context, and the enrolment handlers need the same
authority the listener's TLS configuration came from — so it has to live on the context. It is a
`Late<Pki>` beside the content store and the signing keys, for the reason `services::late`'s own
documentation gives: loading it reads the database through the secret store the context already
carries, so it cannot exist before the context does. It is an **inherent** method on `AppContext`
rather than a `Services` trait method, because widening the trait would make every hand-written
stand-in implement a capability none of them can provide, and every caller holds the concrete type.

`runtime::listen` already installed it — the stream agent added that line while this brief was in
flight, with a comment naming the enrolment endpoints; nothing here had to change it.

### `build_marti` serves the TAK surface and nothing else

No `/api/v1`, no single-page shell, and a default service that answers our own JSON `404`. A browser
has no business on `:8443` and the admin UI has no business answering a device, so those routes are
simply not mounted rather than mounted and refused. The rate limiter is shared with the public
listener, so an attacker cannot double their allowance by alternating ports. `on_connect_capture` is
registered there and only there — without it a handler has no way to reach the certificate rustls
verified and `:8443` would authenticate nobody.

A disabled `[web.marti]` returns `Ok(None)`, and `runtime::serve_marti` then waits on the shutdown
token rather than returning: `stopping_on_exit` cancels the token however its future ends, so an
immediate `Ok(())` would stop the whole server.

### The `tls/config` document pads to two `nameEntry` elements

`[pki]`'s default `subject_entries()` is one entry (`O=rustak`). CloudTAK's `xml-js` compact mode
collapses a one-element array to a bare object and its `for (… of nameEntries.nameEntry)` then
throws on a non-iterable, so `certificate_config` appends `OU=""` when there are fewer than two.
An empty value is better than a broken client. Attribute values are XML-escaped (design 04 D14);
TAK Server does not, and ATAK parses this with a real parser.

### `expires_in` is clamped at zero

`u64::try_from(exp - now)` on a token that expired between signing and serialisation would otherwise
become 18 quintillion seconds. A test asserts both directions.

## Deviations from the brief

1. **`marti/enroll.rs` is a sixth file the brief did not name.** `marti/tls.rs` with the issuance
   pipeline in it came to **330** functional lines, over `conventions.md`'s hard 300 limit. Split by
   responsibility: `tls.rs` is the wire contract (which paths exist, what `Accept` means, what the
   documents look like) and `enroll.rs` is what the server does when somebody enrols. 208 and 129.
2. **`marti/principal.rs` was edited**, which the orchestrator's instruction asked me to avoid
   beyond registering routes. Three changes, all of them the seam M2-04 documented for this brief:
   `auth_policy` now returns `client_cert: true` for `ListenerRole::Marti` and `basic: true` for
   both; a new `AuthPolicy::listener()` converts it to `ListenerAuthPolicy`; and `resolve`'s two
   `TODO(M2-03)` branches became one call to `auth::resolve_principal`. The test that pinned the old
   answer was updated rather than deleted. Without these the brief's item 1 would have had no effect
   on any route, and an mTLS caller on `:8443` would have resolved to anonymous. Nothing else in the
   file changed, and no other M2-04 file was touched.
3. **`services/mod.rs` was edited** (the `Late<Pki>` slot) — see above; `build_marti(ctx)` cannot
   work without it.
4. **`web/api/{auth,extract,subject,middleware}.rs` were edited**, each by a few lines, as the
   unavoidable ripple of `Resolved::claims` becoming optional and `AuthFailure` gaining a variant.
5. **`docs/ci.md` was corrected** (three sentences): it described `interop-node-tak` as an
   `if: false` placeholder the aggregator excuses, which this brief makes untrue.
6. **`/Marti/api/tls/profile/enrollment` and `/Marti/api/device/profile/connection` are
   anonymous.** Design 03 §6 lists the first as authenticated. Both answer `204` with an empty body
   to everybody, so there is nothing to leak, and ATAK fetches the enrolment profile
   *unconditionally* straight after enrolling (`compat/enrollment.md` §6) — a `401` there would be a
   confusing first impression of a server that had just worked. M3, which gives them something to
   send, should add the credential check with the content.
7. **`/oauth/token` answers `401` for `invalid_grant`**, per the brief and design 03 §5.
   `compat/oauth.md` §1 says `400`; both clients accept either and node-tak's own assertion is
   `[400, 401].includes(status)`. `401` is the honest status for "that credential is not accepted".
8. **The `refresh_token` grant does return a `refresh_token`.** The password grant does not, which is
   the CloudTAK-compatibility rule; rotation is the entire point of the other grant.

## The addenda

### `POST /api/v1/users`

`CreateUserRequest {username, display_name, email, kind}` in `rustak-api` (defaults to `person`),
`POST /api/v1/users` in `web/api/users.rs`: administrative, `201` with the `User` DTO, `409` on a
duplicate (checked first *and* caught from the unique index, because two administrators can ask at
the same moment), audited as `user.created`, and the account joins the default channel when
`[auth] anon_group_default` is on. It hands out **nothing**: there is no local password to set and
no field on the request or the response that could carry one — asserted by a test that lists the
serialised field names, the same guard `rustak-api`'s `User` already has.

### The `interop-node-tak` CI job

Flipped. `if: false` became the condition every other job carries
(`github.event_name != 'pull_request' || needs.deduplicate.outputs.cache-hit != 'true'`), the
placeholder comment was replaced with what the job now proves, and `INTEROP_NODE_TAK_RESULT` was
folded into the `ci` aggregator's loop — the special case that excused a `skipped` result is gone.
`actionlint` reports no new findings (the two pre-existing shellcheck notes on unrelated steps are
unchanged, and the `if-cond` warning the `if: false` used to draw has gone).

**Not done, and left for whoever owns `interop/`**: `src/bootstrap.ts` still mints the client
password for the *administrator*, because `interop/node-tak/src/**` is M2-05's and outside this
brief's file list. The endpoint that closes M2-05's "gap this suite cannot close" now exists, so the
ten-line change it describes — create an ordinary account with `POST /api/v1/users` and mint the
password against it — is available whenever that suite is next touched. Until then the suite does
not prove that an ordinary account enrols the same way an administrator does.

## Exit checks

Run against the working tree. Two other agents were editing throughout; see the note at the end.

```
$ cargo test -p rustak-server --features testing
     Running unittests src/lib.rs (target/debug/deps/rustak_server-…)
test result: ok. 1046 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 16.81s
     Running unittests src/main.rs
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/bootstrap.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.93s
     Running tests/enroll_flows.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.42s
     Running tests/enroll_oauth.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.40s
     Running tests/marti_contract.rs
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.68s
     Running tests/stream_routing.rs
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.57s
     Running tests/stream_session.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.26s
     Running tests/stream_store.rs
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.28s
   Doc-tests rustak_server
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
# The whole crate, the concurrent stream work included. No `--skip` was needed.

$ cargo test -p rustak-api
test result: ok. 95 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
# 93 before this brief, plus the two CreateUserRequest tests.

$ cargo test -p rustak-server --lib --features testing -- auth::
test result: ok. 129 passed; 0 failed; 0 ignored; 0 measured; 924 filtered out

$ cargo test -p rustak-server --lib --features testing -- marti::
test result: ok. 111 passed; 0 failed; 0 ignored; 0 measured; 942 filtered out

$ cargo clippy --workspace --all-targets --all-features -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)
# Clean across the workspace, rustak-client and the concurrent stream work included.

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server -p rustak-api --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s)
   Generated target/doc/rustak_server/index.html and 2 other files
# Two rustdoc findings this brief introduced (a private-item link and a
# redundant explicit link, both in `marti/principal.rs`) were reported by the
# orchestrator mid-brief and are fixed. The remaining findings in the tree at
# the time were `jobs/retention.rs` and `stream/hub.rs`, both the stream
# agent's and both since resolved.

$ cargo fmt -p rustak-server -p rustak-api --check
(no output)
# Files were formatted individually with `rustfmt --edition 2024` rather than by
# running `cargo fmt -p rustak-server`, which would have reformatted another
# agent's half-landed files mid-edit.

$ ./scripts/check-file-length.sh
(no output, exit 0)

$ cargo run -p rustak-server -- --config config.example.toml --check
config.example.toml is valid: rustak would listen on 0.0.0.0:8446, with data in ./data.
exit=0

$ cd interop/node-tak && npm test
✔ the certificate config is the ns2 document CloudTAK's parser requires
✔ signs a CSR into a client certificate for the authenticated account
✔ accepts a second enrollment for the same identity
✔ exchanges a client password for a token node-tak can parse
✔ issues exactly the body CloudTAK's client expects
✔ issues a JWT whose header and claims survive CloudTAK's parser
✔ refuses a password that is not the one minted
✔ answers a bearer token with a plain-text version string
✔ answers an enrolled client certificate on the mutually authenticated listener
✔ never answers a Marti route with a redirect
✔ answers a ping with t-x-c-t-r over a mutually authenticated connection
… (the four bootstrap scenarios and the two `/files/api/config` ones)
ℹ tests 25
ℹ pass 16
ℹ fail 0
ℹ skipped 9
# 16 where M2-05 recorded 7. Every scenario this brief owns — the four in
# `login.test.ts`, the three in `enrollment.test.ts` and the mTLS one in
# `version.test.ts` — runs and passes. The nine remaining skips are M2-06 (4),
# M3 (3) and M4 (3)... the three mission ones and `clientEndPoints`.
```

`check-file-length.sh` reads `git ls-files`, so it does not yet see this brief's untracked files.
They were measured with the script's own `awk`; the table at the top of this file is that
measurement, and the largest is `marti/oauth.rs` at 213 against a limit of 300. `marti/tls.rs` was
330 before the split described under Deviations.

### What the integration suites actually assert

`tests/enroll_flows.rs` runs a **real Marti listener on a real socket** (a reserved port, TLS from
`Pki::marti_server_config(true)`, `on_connect_capture` registered) and drives it with `reqwest` over
mTLS, with a 10-second timeout on every exchange so that a refused handshake reports the error
rustls gave rather than hanging — the failure mode M2-01 recorded and fixed the same way.

- **CloudTAK's shape, end to end**: Bearer on `tls/config`, Basic with a **PEM** body and
  `Accept: application/json` on `signClient/v2?clientUid=ada (ETL)&version=3`, reassemble the PEM
  from `signedCert`, then `GET /Marti/api/version` over mTLS on `:8443` → `200` containing
  `TAK Server`. Asserts `signedCert` carries no armour and the status is `200`, never `201`.
- **ATAK's shape**: a one-time token, `Accept: application/xml`,
  `Content-Type: application/octet-stream`, a **bare base64** body → `<enrollment>` with
  `<signedCert>` and at least one `<ca>`, none of them armoured; the device row exists; and the
  **second** use of the token is a `401` carrying `WWW-Authenticate: Basic realm="rustak"`.
- **CN spoof** → `403`, and nothing is issued (the `certificates` table is checked).
- **Legacy v1** → `application/octet-stream` that `p12-keystore` opens with `atakatak` and that
  carries the `signedCert` alias.
- **Revocation** → the same certificate that worked a moment ago fails the handshake, through a
  **fresh** client so it is a new connection rather than a pooled one.
- **No certificate at all** → the handshake fails, not the request: `client_cert = "required"`.
- **Re-enrolment** → two different serials for one identity, which is what CloudTAK's seven-day
  renewal window depends on.
- **An `Accept` we cannot answer** → `400 application/json`, rather than a guess.

`tests/enroll_oauth.rs` asserts the byte-level contract: exactly `application/json` with no
parameter, `Cache-Control: no-store`, no `refresh_token` and no `scope` in the password-grant body,
the header segment's length being a multiple of four, every claim a flat scalar, exactly one `}` in
the payload, `Bad credentials` present in the refusal, a `429` with `Retry-After` after the
configured attempts, `token_key`/`jwks` shapes (and that no private exponent appears in a key set),
and that five spellings of an `/oauth` path never produce a `3xx`.

## Notes for the briefs that follow

- **M2-06 (channels, contacts, `clientEndPoints`)**: `MartiPrincipal` now resolves a client
  certificate on `:8443`, so `require()` works for an mTLS caller and
  `principal.device` carries the `DeviceUid` when the certificate names one. The channel set on a
  certificate principal is the **device-effective** one (`members::effective_for_device`), not the
  account's, so a channel a device switched off is already excluded before a handler sees it.
- **M3 (device profiles)**: `marti::tls::{enrollment_profile, connection_profile}` are the two `204`
  stubs to fill in, and both should gain the credential check when they gain content (deviation 6).
- **M4 (mission tokens)**: `resolve_principal` already falls through a bearer token it cannot
  verify, so `MissionAuthorization`-then-`Authorization` resolution can be layered on top without
  touching the identity path.
- **`auth::ListenerAuthPolicy` is the one place a listener's credentials are named.**
  `BasicPolicy::All` exists and nothing selects it; an installation that wants Basic across the
  Marti surface is a configuration change plus one line in `marti::auth_policy`, not a code change
  in any handler.
- **`AppContext::pki()`** is how anything reaches the authority now; `has_pki()` lets an endpoint
  answer "enrolment is not available on this installation" rather than a bare `500`.
- **`POST /api/v1/users` exists**, so the node-tak bootstrap's "gap this suite cannot close" can be
  closed whenever `interop/node-tak/src/bootstrap.ts` is next edited.
- **The admin UI has no "add user" page.** The endpoint is there; `rustak-ui` was another agent's
  file list this round.

## Concurrency note

Two other agents were editing the same tree throughout: one owning
`rustak-server/src/{stream,cot_store}/**`, `config/stream.rs`, `jobs/retention.rs` and the stream
spawn in `runtime.rs`; one owning `web/api/setup.rs`, `auth/passkeys.rs`, `rustak-ui/**` and `e2e/**`.

- The orchestrator relayed one finding mid-brief — that making `Resolved::claims` optional broke the
  test fixtures in `web/api/{extract,subject}.rs`, and that `marti/` had rustdoc findings. Both were
  of this brief's own making and both are fixed above; every exit check was re-run afterwards.
- `runtime.rs` already contained `context.install_pki(…)` when this brief came to add the listener
  spawn — the stream agent had added it against the slot this brief put on `AppContext`. Nothing had
  to be reconciled; the Marti spawn is four lines beside the stream one plus `serve_named`.
- After this brief's work was finished, the passkey agent began migrating `webauthn-rs` →
  `webauthn_rp` and restructuring `auth/passkeys.rs` into a directory. The crate was repeatedly
  uncompilable for stretches of that, always **only** in their files —
  `auth/{passkeys,passkey_store}.rs`, then `auth/passkeys/register.rs`, then
  `testing/{mod,authenticator}.rs` — which `cargo check -p rustak-server --lib` was used to confirm
  each time. The full test run above was taken during a window when the tree compiled; the clippy,
  fmt, doc and file-length checks were taken shortly before that migration started. At the last
  reading, `cargo clippy --workspace --all-targets --all-features` and `cargo doc -D warnings`
  reported findings only in `testing/mod.rs` and `auth/passkeys/mod.rs`, both theirs and both
  mid-edit. Nothing of theirs was edited here, and `cargo fmt --check` should be re-run over the
  whole crate once that migration settles.
