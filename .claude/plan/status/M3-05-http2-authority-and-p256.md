# M3-05 — Base URLs under HTTP/2 (`:authority`), and the p256 0.14 bump — complete

Brief: `.claude/plan/briefs/M3-05-http2-authority-and-p256.md`
Read first: `.claude/plan/status/CI-01-2026-09-18-mission-package-url.md` (the CI steward's
diagnosis, which this implements), `conventions.md`.

## What changed

| File | Functional lines (limit 300) | Change |
|---|---:|---|
| `rustak-server/src/web/helpers/request.rs` | 80 | `scheme_for` extracted; `request_base_url` falls back to the URI authority; new `public_base_url`; 6 new unit tests |
| `rustak-server/src/marti/sync_read.rs` | 185 | `content_url` routes its non-`public_host` branch through `public_base_url` (3 lines + a doc paragraph) |
| `rustak-server/src/testing/authenticator/keys.rs` | 120 | `to_encoded_point(false)` → `to_sec1_point(false)` (one line + a comment) |
| `Cargo.toml` | — | the `p256` line only: `0.13.2` → `0.14.0` |
| `Cargo.lock` | — | `p256 0.14.0` and its new tree, purely additive |
| `rustak-server/tests/sync_contract.rs` | — (`tests/` exempt) | one additive test: a real HTTP/2 upload over a real socket |

Nothing else was touched. In particular `marti/sync.rs`, `web/api/passkey.rs`,
`auth/oauth_server/login.rs`, `config/server.rs` and `identity/settings.rs` needed **no** edit —
see "Call sites" below.

## 1. The authority fix

`request_base_url` used to be `base_url_from(headers…)` and nothing else, and `base_url_from`'s
first act is `header_str(headers, "host")?`. Over HTTP/2 there is no `Host` header: the authority
travels in the `:authority` pseudo-header, which actix puts on the request URI. So the function
returned `None` for **every** h2 client — and our TLS listeners advertise `h2` in ALPN, which is
what ATAK takes. It now reads the URI when the headers say nothing:

```rust
base_url_from(trust_proxy, headers, scheme).or_else(|| {
    let authority = request.uri().authority()?.as_str();
    Some(format!("{}://{authority}", scheme_for(trust_proxy, headers, scheme)))
})
```

Three things worth recording about that shape:

* **The header still wins.** `Host` (or `X-Forwarded-Host` behind a trusted proxy) is consulted
  first, so nothing that worked before changes. RFC 9112 says the absolute-form request target
  outranks `Host`, but actix only ever sets the authority itself, on h2 — so making the URI win
  would change h1 behaviour for no gain.
* **`X-Forwarded-Proto` applies to the new branch too.** The steward's sketch used only
  `app_config().secure()` there; extracting `scheme_for` lets both branches run the same
  `is_https` rule, so a proxied installation does not start advertising `http://` URLs to peers
  that can only reach it over TLS. Test:
  `a_forwarded_scheme_applies_to_the_authority_reading_too`.
* **Still not `connection_info()`.** Its existing doc comment (it consults the forwarding headers
  whether or not a proxy is trusted) is still the reason, and is left in place.

No trust boundary moves. `:authority` over h2 is client-supplied in exactly the way `Host` is over
h1, so the existing warning on `base_url_from` — "a `Host` header is the client's to choose and an
issuer that varies per request is not an issuer" — covers the new branch unchanged, and everything
that needs a stable value (the JWT issuer, the WebAuthn relying party) still prefers the configured
or stored one before asking the request at all.

## 2. The last resort is a URL, not a display name

New `public_base_url(&ServerConfig, &HttpRequest) -> String`:

```
request_base_url  →  config.base_url()  →  format!("https://{name}")
```

`ServerConfig::base_url()` already resolves `[server] base_url` → `https://<first [server] domains
entry>`, which is exactly the order the brief asks for, so the chain is expressed by reusing it
rather than by re-deriving it. `[server] name` survives at the very end only so that a deployment
which has configured neither keeps today's behaviour.

`content_url`'s `[marti] public_host` branch is untouched and still wins over all of it, so
`sync_contract.rs`'s existing `https://tak.example.com/...` assertion holds unchanged.

## 3. Call sites

Grep for `request_base_url|base_url_from` across the workspace gives four users, and only one
needed an edit:

| Caller | Action |
|---|---|
| `marti/sync_read.rs::content_url` | **edited** — now `public_base_url`, which is fix 1 + fix 2 |
| `marti/sync.rs` (missionupload) | none: it already calls `content_url`, so it is fixed by that |
| `web/api/passkey.rs::relying_party` (origin/RP-ID) | none: it is `settings::base_url(...).or_else(request_base_url)`, and `settings::base_url` already resolves the configured/wizard `base_url` then the canonical domain. Fix 1 closes the pre-wizard h2 hole the steward flagged, with no change to the file. `server.name` there is the RP **display** name, which is correct. |
| `auth/oauth_server/login.rs:372` | none, and deliberately: the file belongs to another agent this session, and the same `settings-then-request` shape means fix 1 reaches it without an edit. |

No other place in `src/` builds a URL out of `config.server.name`
(`auth/mission_token.rs` uses it as a `rustak/<name>` identifier, not a host).

## 4. The regression test

`tests/sync_contract.rs::a_package_uploaded_over_http_2_is_advertised_at_the_authority_it_was_sent_to`
binds a real socket, serves `web::server::services(...)` on it with `listen_auto_h2c` (a plain
`listen` speaks HTTP/1 only; the production listeners reach the same h2 dispatcher through ALPN),
and drives it with `reqwest::Client::builder().http2_prior_knowledge()`. It asserts
`response.version() == HTTP_2` first, so the test cannot silently degrade into an h1 test.

The harness is configured exactly like the failing scenario — no `[marti] public_host`, no
`[server] domains`, no `[server] base_url`, and `name = "rustak-interop-eud-mp-download"` — and
asserts the returned URL starts with `http://127.0.0.1:<port>/Marti/sync/content?hash=` and does
not contain the display name. **Verified to fail against the old code**: with the two `or_else`
clauses removed it reports

```
the URL travels to a peer, so it names the authority the upload was addressed to:
https://rustak-interop-eud-mp-download/Marti/sync/content?hash=09d2fd20c146…
```

which is the CI-01 URL character for character.

`server.app()` could not be used to build the factory: it borrows the harness and actix needs the
factory for `'static` (edition 2024 RPIT capture). The test clones `server.context` and
`server.limiter` and calls the same public `web::server::services` that `app()` calls, so it is
still the real application rather than a copy. `testing/context.rs` was **not** modified.

The unit tests on `request.rs` cover the four shapes the brief names — h2 (authority, no `host`),
h1 (`host`), forwarded headers with and without `trust_proxy`, a request that names no host at
all — plus the whole `public_base_url` fallback chain.

## 5. p256 0.14

`Cargo.toml`: `p256 = { version = "0.14.0", default-features = false, features = ["ecdsa", "std"] }`
(only the version changed; both features still exist in 0.14).

The bump broke exactly one line, as the steward predicted: `elliptic-curve` 0.14 renamed
`ToEncodedPoint::to_encoded_point` to `to_sec1_point`, so
`testing/authenticator/keys.rs` now calls `P256_KEY.verifying_key().to_sec1_point(false)`. The
returned point still answers `.x()`/`.y()`, so the COSE key bytes are unchanged. Nothing else in
the workspace uses the `p256` crate — every other `p256` hit is the `ecdsa-p256` key-type string
in `[pki]`, which is ours.

**Duplicates, as asked.** `webauthn_rp` 0.3.0 pins `p256 ^0.13.2` and was left alone, so the tree
carries two:

```
$ cargo tree -i p256@0.13.2        $ cargo tree -i p256@0.14.0
p256 v0.13.2                       p256 v0.14.0
└── webauthn_rp v0.3.0             └── rustak-server (feature `testing`, + dev-dependency)
    └── rustak-server
```

and with them `ecdsa` 0.16.9/0.17.0, `elliptic-curve` 0.13.8/0.14.1, `sec1` 0.7.3/0.8.1,
`signature` 2.2.0/3.0.0, `crypto-bigint` 0.5.5/0.7.5, `ff`/`group` 0.13/0.14. This is harmless
here and costs nothing in a release binary: our `p256` is behind the `testing` feature and the
software authenticator only ever hands `webauthn_rp` **bytes** (a CBOR COSE key and a DER
signature), never a typed key, so the two versions never meet at a type boundary. The duplicates
disappear on their own when `webauthn_rp` moves to p256 0.14 — worth a Dependabot watch, not a
fork.

`p384` 0.13.1 is `webauthn_rp`'s alone and was not touched.

## Exit checks

Seven other agents were editing `rustak-server` throughout. The whole-workspace commands are
therefore reported twice: what they said, and which of that is mine. **Nothing in any of them
points at a file this brief owns.**

```
$ cargo test -p rustak-server --features testing --test sync_contract
running 14 tests
test a_package_uploaded_over_http_2_is_advertised_at_the_authority_it_was_sent_to ... ok
[… 13 more …]
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.51s

$ cargo test -p rustak-server --features testing --lib web::helpers::request
running 13 tests   (7 that were there before, 6 new)
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 1552 filtered out

$ cargo test -p rustak-server --features testing --test api_v1_packages --test enroll_flows \
      --test marti_contract
test result: ok. 9 passed | ok. 10 passed | ok. 14 passed   (0 failed)

$ cargo test -p rustak-cot -p rustak-api -p rustak-core
test result: ok. 138 | 140 | 253 | 9 | 49 | 9 | 0 | 14 | 3 passed; 0 failed across all nine targets

$ cd interop/node-tak && npm test
ℹ tests 25
ℹ pass 25
ℹ fail 0
ℹ duration_ms 2744.128            (the brief's floor was 24/25)

$ ./scripts/check-file-length.sh
(no output, exit 0)

$ cargo fmt --all --check
Diff in rustak-client/src/sidecar/mod.rs:190
Diff in rustak-client/src/sidecar/mod.rs:197
(another agent's file; the four files here were formatted and re-checked clean)

$ cargo clippy -p rustak-server --features testing --lib --test sync_contract
rustak-server/src/plugins/auth.rs:205:5: warning: this boolean expression can be simplified
rustak-server/src/plugins/auth.rs:206:12: warning: this boolean expression can be simplified
    Finished `dev` profile
(another agent's file; the lib target covers all three source files here and the `sync_contract`
 target covers the new test — neither produced a lint)

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server --no-deps --features testing
jobs/acme_renew.rs, web/api/services.rs ×4, pki/acme/{account,store}.rs, plugins/events.rs ×2
(all other agents' modules; `request.rs`, `sync_read.rs` and `keys.rs` are clean — the one
 unresolved link this brief introduced, `[`public_base_url`]` in `content_url`'s doc comment,
 was given an explicit `crate::web::helpers::request::public_base_url` target)
```

### The whole-workspace runs that concurrent edits blocked

`cargo test -p rustak-server --features testing` (the full suite), `cargo test --workspace` and
`cargo clippy --workspace --all-targets -- -D warnings` could not be made to complete (the scoped
clippy above is what ran instead): the
`rustak-server` **lib test** target and `rustak-client` were broken by in-flight edits in other
agents' files for the whole session, in several different states as they worked —
`pki/acme/{challenge,account,renew}.rs`, `config/validate.rs`
(`cannot find function challenge_is_reachable`), `plugins/auth.rs`, `jobs/{queue,service_health}.rs`,
`rustak-client/src/{http,marti}` against reqwest 0.13. Retried on a timer for ~25 minutes each.

One window did open, and the whole lib suite ran:

```
$ cargo test -p rustak-server --features testing        # lib target, one clean moment
running 1565 tests
test result: FAILED. 1555 passed; 8 failed; 2 ignored
  pki::acme::challenge::tests ×2, pki::acme::renew::tests ×2, plugins::auth::tests ×2,
  web::api::services::tests ×1, web::api::tests::nothing_behind_the_gate_answers_without_a_session
```

All eight are other agents' modules; none touches Enterprise Sync, the request helpers or the
authenticator. Re-running the last of them on its own a few minutes later
(`web::api::tests::nothing_behind_the_gate_answers_without_a_session`) passed, so they were
transient states of work in progress rather than standing failures. **The orchestrator should
re-run the three whole-workspace commands once the tree is quiet.**

## Notes for the orchestrator

* Nothing here changes a wire contract except the one that was wrong: a package URL derived from
  an h2 request now names the host the client actually used. `interop/eud/scenarios/mp-download.toml`
  should go green without the `public_host` workaround the steward declined to apply.
* The workspace `Cargo.toml` is being edited concurrently (`instant-acme`/`http`); only the
  `p256` line here is mine, and `Cargo.lock`'s diff is purely the p256 0.14 subtree.
