# M2-10 — ACME certificates for the public listener — complete

Brief: `.claude/plan/briefs/M2-10-acme.md`
Read first: `conventions.md`; `design/03-identity-pki-acme-auth.md` §3 `acme/` (lines 265–289), §8
`[acme]`, §10, §11; `config/acme.rs` and `config.example.toml` `[acme]`; `web/tls.rs`;
`pki/tls/resolver.rs`; `crypto/` (`Sealed`, `SecretContext`); `migrations/0003_auth_pki.sql`
(`acme_accounts`, `acme_certificates`); `jobs/`; status files `M2-01-pki-issuance.md`,
`M0-12-runtime-bootstrap.md`.

## What was built

`rustak-server/src/pki/acme/**` on `instant-acme` 0.8.5 (already declared in
`[workspace.dependencies]` and in `rustak-server`; nothing to add), plus the renewal job, the
listener's ACME mode, the admin API and its DTO.

| File | Functional lines | Contents |
|---|---:|---|
| `pki/acme/mod.rs` | 61 | module doc; the process-wide resolver slot (`publish_resolver`/`resolver`); `stored_certificate`; `install` |
| `pki/acme/transport.rs` | 42 | `SharedClient` — `instant_acme::HttpClient` over `Services::http_client` |
| `pki/acme/account.rs` | 123 | `ensure` — register once, reuse after; credentials sealed under `SecretContext::AcmeAccount` |
| `pki/acme/order.rs` | 217 | `place`/`answer_and_finish` — the RFC 8555 state machine; `choose` (preference then fallback); our own CSR; `validity` from the leaf |
| `pki/acme/challenge.rs` | 127 | the process-wide `http-01` token map + `GET /.well-known/acme-challenge/{token}`; `Responder`; the `tls-alpn-01` challenge certificate |
| `pki/acme/store.rs` | 201 | `acme_certificates`: `normalise`, `load`, `reserve`, `store`, `record_failure`, `AcmeCertificateRow::certified` |
| `pki/acme/renew.rs` | 254 | `CertState`, `Decision`, `backoff`, `decide`, `state`, `next_renewal`, `status`, `run` |
| `jobs/acme_renew.rs` | 67 | `AcmeRenewJob`, hourly, arms itself at start-up when `[acme] enabled` |
| `web/tls.rs` | 211 | `mode = "acme"` now builds the listener behind `HotSwapCertResolver` (was: refused) |
| `web/api/settings.rs` | 86 | `GET /api/v1/settings/tls`, `POST /api/v1/settings/tls/renew` (added to the existing file) |
| `rustak-api/src/settings.rs` | 99 | `TlsStatus`, `TlsSource`, `TlsCertificateState` (added) |
| `config/acme.rs` | 156 | `AcmeConfig::unorderable` added; the rest untouched |
| `config/validate.rs` | 293 | `public_names` rule added; the two challenge messages improved |

One-line registrations: `pki/mod.rs` (`pub mod acme;` + one `pub use` + two doc lines),
`jobs/mod.rs` (`pub mod acme_renew;` + one `pub use`), `web/api/mod.rs` (two routes + two entries in
the "nothing answers without a session" table), `web/server.rs` (one `.configure(...)` — see
**Deviations**).

**52 new unit tests** (47 under `pki::acme`/`jobs::acme_renew`, plus `config::acme`,
`config::validate`, `web::api::settings`, `web::tls`, `rustak-api`) and **5 integration tests** in
`rustak-server/tests/acme_directory.rs`.

### Manifest changes

`http = "1.5.0"` added to `[workspace.dependencies]`, and `http` + `bytes` (already a workspace
dependency) added to `rustak-server`. Both are already in the lock file through `reqwest`,
`actix-web` and `instant-acme`; they are named because `instant_acme::HttpClient` is
`fn request(&self, http::Request<BodyWrapper<bytes::Bytes>>) -> … BytesResponse`, and
`pki::acme::transport` implements that trait. `instant-acme` itself needed no change — it was
already declared with `default-features = false, features = ["aws-lc-rs", "hyper-rustls"]`, which
is why `Order::finalize` (behind the `rcgen` feature) is not used and the CSR is built here from
`[pki] key_type`.

## How it fits together

```
start-up ──► web::tls::resolve (mode = "acme")
               │  stored_certificate(db, secrets, domains)          ─► the last issued chain, or
               │  bootstrap(): the internal-CA certificate             none on a fresh install
               │  HotSwapCertResolver::new(Some(that))
               │  pki::acme::publish_resolver(resolver)             ─► the process-wide slot
               └► public_server_config(resolver)   (alpn: h2, http/1.1, acme-tls/1)

job host   ──► AcmeRenewJob::setup   ─► immediate message when [acme] enabled
AcmeRenewJob::handle
               │ re-arm (+1h)
               └► acme::run(services, acme::resolver(), forced)
                     ├ decide(row, renew_before, now) → Wait | BackOff | Order
                     ├ store::reserve(domains)                     ─► the row id = sealing context
                     ├ account::ensure(db, secrets, cfg, http)     ─► acme_accounts, sealed
                     ├ order::place(account, domains, pref, responder, key_type)
                     │    ├ authorizations → choose(preferred, else other, else error)
                     │    ├ Responder::publish  ─► tls-alpn-01: resolver.set_challenge
                     │    │                     ─► http-01:     challenge::publish(token, key auth)
                     │    ├ set_ready → poll_ready → finalize_csr(our CSR) → poll_certificate
                     │    └ Responder::withdraw (always, success or not)
                     ├ store::store(chain, sealed key, validity, challenge)
                     ├ acme::install(…)  ─► resolver.install(certified)   ← no restart
                     └ audit "acme.issued" / on error: record_failure + audit "acme.renew.failed"
```

### Decisions worth knowing

- **The resolver is a process-wide slot.** `web::tls::resolve` builds it and the renewal job needs
  it, and the job holds nothing but `Services`. A `Late<AcmeManager>` slot on `AppContext` would
  have been tidier but `services/mod.rs` belongs to another brief; the slot is read only by the
  renewal and the admin API, and both treat its absence (a listener not in ACME mode) as "nothing
  to swap", not as a failure. If `services/mod.rs` is ever free, this is a ten-line change.
- **The `http-01` token map is a static too**, and for a stronger reason: actix builds one `App`
  per worker and the handler is a plain function with nothing but its request, so a challenge
  published by a background job has no per-application state to reach. There is one public
  listener and one order in flight per process.
- **Rows are written twice.** Both `acme_accounts.credentials_sealed` and
  `acme_certificates.key_sealed` are sealed against the row's own id
  (`SecretContext::{AcmeAccount, ServerCertKey}`), which SQLite only assigns on insert. `reserve`
  inserts a placeholder (`'{}'`, so the `json_valid` check passes) and `store` fills it in;
  `is_issued()` reports a placeholder row as "no certificate", and `store.rs` reads `key_sealed`
  as `Option<Sealed>` for the same reason. A ciphertext moved between rows does not decrypt —
  there is a test for exactly that.
- **`ServerCertKey` slot collision:** the internal server certificate seals under
  `CertificateId::new(0)` (`server_cert.rs`), and SQLite's `INTEGER PRIMARY KEY` starts at 1, so
  `acme_certificates` ids never collide with it.
- **The `tls-alpn-01` challenge certificate is built with `CertifiedKey::new`, not `from_der`.**
  `from_der` checks the key against the certificate by parsing it with webpki, which refuses an
  unknown *critical* extension — and `acmeIdentifier` (RFC 8737) is exactly that. The key is
  loaded through the provider and paired directly; it was generated three lines earlier, so there
  is nothing for the check to find. It is always ECDSA P-256 whatever `[pki] key_type` says:
  nothing verifies its signature, and an RSA key would put hundreds of milliseconds inside a
  challenge window.
- **A failed order is not a failed job.** `run` records the failure on the row, audits it and logs
  it with the authority's own words, then the job returns `Ok`. Returning the error would hand the
  retry to the job host's back-off, which knows nothing about certificate-authority rate limits.
  Ours is 1 h → 4 h → 24 h, held in `acme_certificates.attempts`/`last_attempt_at` so it survives a
  restart — which is when an operator is most likely to be retrying by hand.
- **`POST /settings/tls/renew` queues.** An order waits for an authority to reach this server, which
  is seconds at best; the route answers `202` with the status as it stands and the caller polls
  `GET /settings/tls`. Repeated calls collapse onto one queued order under
  `ACME_RENEW_FORCED_KEY`, because each one spends real rate limit.
- **`--check` now refuses a name no public authority could issue for** — an address literal, a bare
  label, or anything under `.local`/`.lan`/`.internal`/`.home.arpa`/`.localhost`/`.invalid`/`.test`.
  Discovering that at run time costs a failed-validation slot (Let's Encrypt allows five an hour).
  The classification is `AcmeConfig::unorderable` in `config/acme.rs`; `validate.rs` turns it into
  the error, which also kept that file under the 300-line limit.

## Testing

- `pki/acme/store.rs` — canonical domain key (case, trailing dot, order), reserve/store/reload,
  failure counting and its reset, a key sealed for one row refusing to open as another, `Debug`
  never rendering a key.
- `pki/acme/renew.rs` — the decision table: nothing stored, a reserved-but-never-issued row, a
  fresh certificate, one inside its window, the inclusive boundary, an expired one, back-off after
  a recent failure and its expiry, the back-off ladder, a failure reported while the old
  certificate still serves, `next_renewal`, and `status` for non-ACME/first-order/failed.
- `pki/acme/challenge.rs` — the `http-01` route answers exactly the key authorization and nothing
  else, 404 before publish and after withdraw; the resolver is armed under the lower-cased name and
  disarmed; the challenge certificate carries a **critical** `acmeIdentifier` (1.3.6.1.5.5.7.1.31);
  every challenge kind round-trips to the wire spelling.
- `pki/acme/order.rs` — the leaf (not the issuer) decides the validity window; an empty or
  unparseable chain is refused rather than stored.
- `pki/acme/account.rs` — unaccepted terms stop the order before any request; one row per
  (directory, contact); a contact is never logged whole.
- `pki/acme/transport.rs` — method, headers and body survive the trip (wiremock); an unreachable
  directory is an error, not a panic.
- `jobs/acme_renew.rs` — no schedule without ACME; the first order is armed immediately; an order
  that cannot reach the authority re-arms and does not fail the job; the failure is recorded; a
  forced run does not re-arm.
- `web/tls.rs` — an ACME listener binds on the internal certificate before its first order and
  publishes a ready resolver; it serves the last issued certificate after a restart (byte-compared);
  an ACME mode with no names says which keys to set.
- `web/api/settings.rs` — an administrator is told what the listener presents; a non-ACME
  installation gets `409` from `renew`; an ACME one gets `202` and a `forced` message on the queue.
- `rustak-api/src/settings.rs` — `TlsStatus` round-trips and omits what it does not know; only an
  ACME installation can need attention; the sources are spelled as the configuration file spells
  them.
- `config/acme.rs`, `config/validate.rs` — orderable and unorderable names, each with its reason.
- **`rustak-server/tests/acme_directory.rs`** — the real `instant-acme` client against a `wiremock`
  directory. Pebble is a Go binary this build cannot fetch, so the authority's side is faked, but
  the client, the JWS signing, the state machine and the JSON are real — and `/finalize` **parses
  and verifies the CSR rustak sent and signs a certificate over its public key**, so the key rustak
  sealed and the chain it stored have to go together. Five tests: an account registered once and
  reused; one `http-01` order end to end (including the fallback from the configured `tls-alpn-01`,
  which that authority does not offer) with the answer checked at the moment `set_ready` arrives and
  checked gone afterwards, the stored chain byte-compared, the issuer present, and the resolver
  swapped; a second run placing no order; a forced run placing one; a refusal recorded, backed off,
  and surfaced with the authority's own words.

The manual staging check is written up in `docs/deployment.md` → **ACME certificates** →
"Manual check against staging before production" (seven steps, `openssl s_client`, the
`renew` round trip, and how to move to production without disturbing the staging account).

## Exit checks

Run on 2026-09-18 against the whole workspace, once the concurrent agents' in-progress edits had
settled. All green.

```
$ cargo fmt --all -- --check
(clean — no output)

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.44s
(clean)

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
(clean)

$ MAX_FUNCTIONAL_LINES=300 bash scripts/check-file-length.sh
(clean — exit 0)

$ cargo test --workspace --no-fail-fast
test result: ok. 1569 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 19.60s
   (rustak-server lib — includes the 47 new pki::acme / jobs::acme_renew tests)
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.14s
   (tests/acme_directory.rs)
test result: ok. 138 passed; 0 failed; …   (rustak-api)
… 38 test binaries, every one `ok`, 0 failed anywhere.

$ cargo test -p rustak-server --test acme_directory
running 5 tests
test an_account_is_registered_once_and_reused_afterwards ... ok
test an_authority_that_refuses_the_order_is_recorded_rather_than_retried_at_once ... ok
test one_http_01_order_produces_a_certificate_the_listener_can_serve ... ok
test a_second_run_against_a_fresh_certificate_places_no_order ... ok
test a_forced_run_orders_again_even_though_nothing_is_due ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.14s

$ cargo run -p rustak-server -- --config config.example.toml --check
config.example.toml is valid: rustak would listen on 0.0.0.0:8446, with data in ./data.

$ ./target/debug/rustak --config acme-alpn.toml --check          # rc=0
#   [server] domains = ["tak.example.com"]; [web.public] listen = [":443", ":8446"];
#   [web.public.tls] mode = "acme"; [acme] enabled = true, accept_tos = true,
#   directory = "letsencrypt-staging", challenge = "tls-alpn-01", renew_before = "30d"
acme-alpn.toml is valid: rustak would listen on 0.0.0.0:443, 0.0.0.0:8446,
with data in /tmp/rustak-acme-check.

$ ./target/debug/rustak --config acme-http01.toml --check        # rc=0
#   listen = [":8446"], plain_bind = ":80", challenge = "http-01"
acme-http01.toml is valid: rustak would listen on 0.0.0.0:8446,
with data in /tmp/rustak-acme-check.

$ ./target/debug/rustak --config acme-bad-name.toml --check      # rc=1, as intended
#   domains = ["tak.lan"]
error(usr):  `tak.lan` cannot be ordered from a certificate authority: it is
             under a suffix reserved for private networks, which
             no public authority can validate
 • A private or made-up name cannot be validated by a public authority; use
   `[web.public.tls] mode = "internal"` for a LAN deployment.
 • List the fully qualified names this server answers to on the public internet, for example
   `domains = ["tak.example.com"]`.
```

Local toolchain is rustc 1.96.0; CI's stable is newer and remains the authority for new lints.
Nothing here can be verified against a real certificate authority offline — see the staging
checklist in `docs/deployment.md`.

## Deviations from the brief

1. **One line in `web/server.rs`**, which the brief does not list as mine:
   `.configure(crate::pki::acme::http01_routes)` in `services()`. Without it the
   `/.well-known/acme-challenge/{token}` route the brief asks for is never mounted — the public
   listener's catch-all would answer the authority with the single-page shell and a `200`, which
   fails an `http-01` order with no useful message. The line is additive, sits ahead of the
   catch-all beside the two existing "ahead of the catch-all" registrations, and `web/server.rs`
   was not named as contended.
2. **Five lines of comment in `config.example.toml`**, also not listed as mine: the `[acme]
   challenge` comment said `http-01` requires `plain_bind`, which is not true now that the route is
   served on every `[web.public] listen` address — and `plain_bind` is still not bound by anything
   (see 3). Only the `[acme]` block was touched.
3. **`[web.public] plain_bind` is still not bound by any listener.** It was not bound before this
   brief either — nothing in `web/server.rs` or `runtime.rs` reads it — so an `http-01` deployment
   needs port 80 to reach a `[web.public] listen` address, directly or through a proxy.
   `config/validate.rs` accepts `plain_bind` as evidence that a proxy exists (it did before, and
   only the operator knows), and `docs/deployment.md` now says plainly which it is. Binding
   `plain_bind` (plaintext, the challenge route, `301` for everything else) belongs to whoever owns
   `web/server.rs` and `runtime.rs` next — **added to the backlog below.**
4. **`runtime.rs` was not touched at all.** Nothing needed it: `web::tls::resolve` publishes the
   resolver on its way out and `AcmeRenewJob::setup` places the first order, both of which
   `runtime::listen` already calls. The consequence is that the first order happens once the job
   host is up rather than during `listen`, which is a second or two later and is where it belongs.
5. **`.claude/plan/backlog.md`**, not listed as mine either: its "ACME for the public listener —
   no implementation brief yet" line was this brief, and is now stale. It was replaced with the four
   follow-ups listed below.
6. **A sixth and seventh file under `pki/acme/`** beyond the `{mod,account,order,challenge,renew}`
   the brief names: `store.rs` (the `acme_certificates` SQL, which would otherwise have pushed
   `renew.rs` past 300 lines, and which does not belong in `db/repos/` because that would mean
   editing `db/repos/mod.rs`) and `transport.rs` (the HTTP client). Both are inside
   `pki/acme/**`, which the brief gives me.

## Backlog items this leaves

- **`[web.public] plain_bind` is parsed and validated but never bound.** An `http-01` ACME
  deployment, and the `80 → 443` redirect design 03 §3 describes, both want a plaintext listener on
  it serving `pki::acme::http01_routes` and `301`ing everything else. (`web/server.rs`,
  `runtime.rs`.) Found by M2-10.
- **The ACME resolver and the `http-01` token map are process-wide statics.** Both become ordinary
  handles the moment `services/mod.rs` can take a `Late<AcmeState>` slot; the resolver is a
  ten-line change, the token map needs the slot to reach an actix handler
  (`web::Data`). (`services/mod.rs`, `pki/acme/mod.rs`, `pki/acme/challenge.rs`.) Found by M2-10.
- **The admin UI has no TLS panel.** `GET /api/v1/settings/tls` and
  `POST /api/v1/settings/tls/renew` exist and are administrator-only; nothing in `rustak-ui`
  reads them, so a failed renewal is visible only in the audit log and the API. `TlsStatus`
  carries `needs_attention()` for exactly that banner. Found by M2-10.
- **Wildcard names are accepted by `--check` but cannot be validated by either challenge rustak
  implements** — a wildcard needs `dns-01`, which needs a DNS provider integration. An order for
  `*.example.com` will reach the authority and be refused. Worth either implementing `dns-01` or
  refusing a wildcard at `--check`. Found by M2-10.
