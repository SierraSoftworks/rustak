# M9-06 — Enrol and authenticate a sidecar with its orchestrator's workload identity

**Status: complete.** Every deliverable in the brief is implemented and every exit check is
green. A sidecar under Nomad or Kubernetes now holds **no rustak secret at all**: the JWT its
orchestrator already gave it buys the client certificate *and* the control-API access token, so
there is nothing to mint, nothing to hand over and nothing to rotate.

Deployments on enrolment tokens are untouched. The two credentials run side by side against one
server, on the same routes, and an assertion this server will not accept leaves the request
exactly as it found it — which is what keeps an enrolment token working on `signClient/v2` when
`[auth.workload]` is configured and the caller is not a workload.

## What landed

| Area | Files |
|---|---|
| `[auth.workload]` schema | `rustak-server/src/config/workload/mod.rs` (new), `config/auth.rs` (one field), `config/mod.rs` (module + re-exports) |
| `--check` rules | `rustak-server/src/config/workload/validate.rs` (new), `config/validate.rs` (one call; `positive` widened to `pub(in crate::config)`) |
| The credential | `rustak-server/src/auth/workload/{mod,verify,keys,claims,rules,request,grant}.rs` (all new) |
| Request wiring | `rustak-server/src/auth/{mod,resolve}.rs` (one `mod` line; one additive arm) |
| `jwt-bearer` grant | `rustak-server/src/auth/workload/grant.rs`, `marti/oauth.rs` (one form field, one match arm) |
| Enrolment | `rustak-server/src/marti/enroll.rs` (`issued_via` override, `enrollment.workload` audit, supersede) |
| PKI | `rustak-server/src/pki/{facade,revoke,mod}.rs` (`IssuedVia::WorkloadIdentity`, `supersede_workload`) |
| Migration | `rustak-server/migrations/0020_workload_identity.sql` (new) |
| Start-up warning | `rustak-server/src/runtime.rs` (one call) |
| Fake issuer | `rustak-server/src/testing/{workload.rs,mod.rs}` |
| Server tests | `rustak-server/tests/workload_identity.rs` (new, 22 tests) |
| Sidecar | `rustak-client/src/sidecar/workload.rs` (new), `sidecar/{config,enrolment,control_link,mod,run}.rs`, `enroll.rs` |
| Control client | `rustak-client/src/control/{mod,register}.rs` (replaceable credential, `401` flag) |
| Sidecar tests | `rustak-server/tests/sidecar_enrolment.rs` (3 new), unit tests throughout |
| Docs | `docs/deployment.md` ("Workload identity"), `docs/plugins.md` ("Three credentials"), `config.example.toml`, the three plugins' `config.example.toml`, `.claude/plan/compat/enrollment.md` §8 |

`Cargo.lock` is updated as a build artefact; `rustak-client/Cargo.toml` gained one line
(`base64`, to read the `iss` out of the sidecar's *own* token for the start-up line). No `git`
or `but` command was run.

## The exact configuration the Dublin deployment needs

### rustak (`config.toml`)

```toml
[auth.workload]
revoke_previous = true

[[auth.workload.issuers]]
name = "nomad"
# The `iss` the tokens carry, because the servers have `oidc_issuer` set.
issuer = "https://nomad.raptor-perch.ts.net"
# Where the keys are fetched from — deliberately NOT the name above. The
# TLS-terminated tailnet name hairpins unreliably from inside a container on
# that node, so the key set comes off the same node's tailnet address over
# plain HTTP, which needs the opt-in below and is logged at every start-up.
jwks_url = "http://<nomad-node-tailnet-ip>:4646/.well-known/jwks.json"
allow_insecure_jwks = true
audience = "rustak"
algorithms = ["RS256", "EdDSA"]
clock_skew = "30s"
jwks_refresh = "1h"

# The reference rule: `rustak-plugin-ais` in `default` is the account `ais`,
# `rustak-plugin-adsb` is `adsb`. Both accounts must already exist, as kind
# `service` and enabled — the rule names one, it never creates one.
[[auth.workload.rules]]
issuer = "nomad"
namespace_claim = "nomad_namespace"
namespace = "default"
subject_claim = "nomad_job_id"
subject_prefix = "rustak-plugin-"
account = "strip-prefix"
```

**Check `[auth] user_acl` before deploying.** It gates this credential like every other, and one
written for an identity provider (`claims.groups contains "…"`) refuses every workload enrolment,
because the claims it is judged against are Nomad's. Widen it:

```toml
user_acl = 'source == "service" || claims.groups contains "tak-users"'
```

A refusal for that reason is logged at `warn` with the expression that caused it, so it is a
`grep user_acl` rather than an afternoon.

### The jobspec (each sidecar task)

```hcl
identity {
  name        = "rustak"
  aud         = ["rustak"]
  ttl         = "1h"
  env         = true
  file        = true
  change_mode = "noop"
}
```

Exactly the block the packs already carry, plus `change_mode = "noop"` (optional; the sidecar
re-reads the token on every use, so a renewal needs no restart).

### The sidecar (`plugin.toml`)

**Nothing.** `workload_identity` is left out and auto-detection finds `NOMAD_TOKEN_rustak`.
`[service] token`, `[service] enrollment_token`, `RUSTAK_SERVICE_TOKEN` and
`RUSTAK_ENROLLMENT_TOKEN` all come out of the job. Keep `[service] pki_dir = "/data"` (or leave
the files beside the configuration file) and `[service] account` if the account is not the
service's own name.

### Nomad variables

Only the AIS key: `AISSTREAM_API_KEY`. Nothing else. Remove `RUSTAK_ENROLLMENT_TOKEN` and
`RUSTAK_SERVICE_TOKEN` once the volume holds the three PEMs — or empty the volume and let the
first start enrol with the assertion.

### Handing over

Nothing to do. A sidecar that already has its PEMs does not enrol, so adding the server section
and the `identity` block changes nothing until the volume is emptied. Grep the sidecar's log for
`Identity:` — one line per start says which of the four credentials it used, who signed it, and
the account rustak bound it to.

## How it behaves

1. **The order of the checks is the design.** Algorithm (an allow-list, so a token cannot pick
   `HS256` and sign itself with a published modulus), `kid` (required), signature, `iss`, `aud`,
   `exp`, `nbf`, `iat` — and only then a claim that decides anything. `aud` and `iss` are
   **required by name**, because `jsonwebtoken` compares them only against a token that carries
   them and a token that simply leaves one out was otherwise accepted where one naming the wrong
   value was refused.
2. **Which issuer judges a token** is selected by its *unverified* `iss`, as a routing hint only;
   the entry then re-checks `iss` after the signature. An entry with no `issuer` exists for a
   Nomad cluster with no `oidc_issuer`, and a token that *does* carry one is refused by it rather
   than matched loosely.
3. **Binding.** All rules for the issuer are evaluated, not the first that matches: a token two
   rules disagree about is refused rather than resolved by the order the file happens to be in.
   Two rules landing on the same account is fine. Every comparison is exact and case sensitive —
   `filt_rs`'s `==` folds case and is deliberately *not* what these use. `startswith_cs` and
   friends do exist in `filt_rs` 1.1.3, which is what a rule's optional `match` expression can
   reach for; it narrows a rule and can never widen one.
4. **`kubernetes.io` is one key.** A dotted claim path tries the whole path as a literal key
   first and then the longest head that exists as one, so `kubernetes.io.serviceaccount.name` and
   plain `nomad_job_id` are read by the same rule with no special case for either orchestrator.
5. **Keys.** Cached for `jwks_refresh` (default 1h); a token naming a `kid` we do not hold
   refetches **on the spot**, at most once a minute per issuer, because that refetch is a request
   to somebody else's control plane that any caller can provoke. The maintainer's Nomad serves six
   rotating keys, which is exactly the shape that compromise is for.
6. **The account** must exist, be kind `service`, be enabled and pass `user_acl`. A person's
   account is a `403` with a `warn` naming it. `users::principal` is told this is not an
   administrative grant (`scope_grants_admin` answers `false` for `AuthMethod::Workload`), so a
   service account somebody once flagged as an administrator does not become one by presenting a
   job token.
7. **Rate limiting**, two keys: the address alone before anything is verified (a caller who sends
   rubbish never reaches an account), and the account once one has been named.
8. **Superseding.** `revoke_previous = true` takes back the account's earlier
   `workload_identity` certificates when a new one is issued — through `pki::revoke`, so the
   hooks fire and a live CoT stream on the old certificate is dropped, not just refused at the
   next handshake. Certificates from an ordinary enrolment are never touched. An orchestrator
   reschedules and nothing else would ever revoke the certificate on the old node's volume,
   because nothing was spent to get it.
9. **Both headers.** `Authorization: Bearer` and — on the enrolment routes only — Basic with the
   JWT as the password, for `commoncommo`-shaped clients. The Basic *username* is ignored: the
   rules decide the account. The sidecar sends Bearer.
10. **A refusal falls through.** `auth::workload::from_request` answers `None` for a token it does
    not claim, so the request carries on to the Basic credential it always had. Only an assertion
    that verified and was then refused for a reason of ours ends the request there. This is also
    why a JWT is tried *before* the Basic path: a 900-byte assertion in a password field would
    otherwise be argon2-verified against every credential the named account holds, at a cost the
    caller chose.
11. **The token is never logged.** Not at `debug`, not in an error, not in the audit trail. What
    `enrollment.workload` records is issuer, `iss`, namespace, subject, `jti`, `sub`, fingerprint
    and serial — enough to find the run of the task that enrolled, and useless to replay.
    Asserted by `the_enrolment_is_audited_with_the_run_that_made_it_and_never_the_token`.
12. **The sidecar re-reads its token every time it is used**, because both orchestrators rotate
    it; a copy held from start-up would work for an hour and then stop, which is the worst shape
    a failure can have. The rustak access token bought with it is cached until 60 s before
    `expires_in` and exchanged again on a `401`.

## The `[auth.workload]` keys, as implemented

**Issuer** (`[[auth.workload.issuers]]`): `name` (required), `issuer` (optional; required in
practice), exactly one of `jwks_url` / `discovery_url` / `jwks_file`, `audience` (required),
`algorithms` (default `["RS256", "ES256", "EdDSA"]`), `clock_skew` (default `"30s"`),
`jwks_refresh` (default `"1h"`), `allow_insecure_jwks` (default `false`).

**Rule** (`[[auth.workload.rules]]`): `issuer`, `namespace_claim`, `namespace`, `subject_claim`
(all required), `subject_prefix` (optional; required when `account = "strip-prefix"`), `account`
(required — the literal `"strip-prefix"` or an account name in full), `match` (optional
`filt_rs` expression).

**Section**: `revoke_previous` (default `true`).

**Sidecar** (`[service]`): `workload_identity = { env = "…" }` or `{ file = "…" }` — exactly one,
and naming both or neither is refused. Left out, three places are tried in order:
`NOMAD_TOKEN_rustak`, `${NOMAD_SECRETS_DIR}/nomad_rustak.jwt`, `/var/run/secrets/tokens/rustak`.

**What `--check` refuses**: an issuer with no `audience`; an issuer with no key source or with two;
two issuers of one name; a rule naming an issuer nobody registered; a rule with an empty
`namespace_claim`/`namespace`/`subject_claim`; a `strip-prefix` rule with no prefix; two rules
over the same claims in the same namespace whose prefixes overlap and whose accounts differ; a
non-positive `clock_skew`/`jwks_refresh`; and a `jwks_url` over plain `http://` to a host that is
not a loopback one without `allow_insecure_jwks = true`. An `allow_insecure_jwks` that *is* set
logs a `warn` at every start-up.

## Decisions worth naming

- **A new `AuthMethod::Workload { issuer, subject }`** in `rustak-core`, rather than reusing
  `AuthMethod::Bearer` with the assertion's `jti`. Putting a foreign `jti` where sign-out looks
  for one of ours would be a revocation that could never match, and the audit label
  (`"workload"`) would have been a lie. `rustak-core/src/identity/principal.rs` and
  `rustak-server/src/identity/users.rs` are the two files outside the brief's list that this
  touched, both additively (a variant, a `label()` arm, a `via_of` arm, and a `scope_grants_admin`
  arm that makes a workload assertion a non-administrative grant).
- **`rustak-client/src/control/mod.rs` and `register.rs` are also outside the list**, and had to
  be: a control-API credential that expires cannot be a `Option<Secret>` fixed at construction.
  The change is small and additive — the token moved into a shared `RwLock` slot with a
  `set_credential` setter, and a `401` is recorded so the harness can exchange again and retry
  once rather than going quiet for an hour. Every clone of the client (the plugin's, the
  harness's, the event feed's) sees the new token the moment it lands.
- **`config/workload.rs` is a directory module**, `config/workload/{mod,validate}.rs`. The schema
  and the cross-rule checks together are over 300 functional lines, and `config/validate.rs` was
  already at 231.
- **A migration for one CHECK value.** `certificates.issued_via` is constrained, SQLite cannot
  alter a constraint in place, and `revoke_previous` matches the column exactly — so
  `workload_identity` is a stable token rather than the issuer's name folded in. `0020` repeats
  `0018`'s rebuild verbatim, including the two foreign keys it has to work around.
- **`user_acl` is enforced**, as the brief asked, with a loud, specific `warn` when it refuses —
  because an installation whose ACL was written for a directory would otherwise see every workload
  enrolment fail with nothing to go on. Documented in `config.example.toml` and
  `docs/deployment.md` with the expression to widen it to.
- **`Enrolment` gained a `credential: Presentation` field** (`Basic` or `Bearer`) rather than a
  second function. Three call sites and the doc example were updated; `Presentation::default()` is
  `Basic`, so the meaning of an unchanged call is unchanged.

## Tests

**`rustak-server/tests/workload_identity.rs`** (22) — a real key set over HTTP, real RS256
assertions, the real routes over a real actix listener, and a real mutually authenticated
handshake for the certificate that comes out. The refusals are first in the file as well as in
the head:

- wrong audience; audience omitted entirely; expired; `iss` mismatch; signature forged with an
  unpublished key; wrong namespace; job outside the prefix; account of kind `person` (a `403`);
  account switched off; account that does not exist; no issuers configured at all; `user_acl`
  refuses (a `403`).
- `a_rotated_key_is_picked_up_by_the_first_assertion_that_needs_it` — both halves: a key
  published *after* the set was cached is picked up with no restart and exactly one refetch, and
  three presentations of a `kid` that will never exist do not become three refetches.
- the Nomad reference rule maps `rustak-plugin-ais` to `ais`, the certificate completes a
  handshake on `:8089`, and the row records `issued_via = workload_identity`.
- a Kubernetes-shaped token through the nested `kubernetes.io` claims.
- the enrolment is audited as `enrollment.workload` with the run that made it and never the token.
- re-enrolment supersedes: the new certificate works, the old one no longer completes a
  handshake, and `certificate.superseded` is in the audit log. `revoke_previous = false` keeps
  both.
- the Basic compatibility form, with a username that is *not* the account.
- `jwt-bearer` answers `application/json` with exactly `access_token`/`token_type`/`expires_in`
  and no `refresh_token`; the token reaches `POST /api/v1/services/register` and is refused on
  `/api/v1/users`; a forged assertion and rubbish are both `400 invalid_grant`.
- **the password grant answers exactly what it always did.**

**`rustak-server/tests/sidecar_enrolment.rs`** (+3) — the same `serve` start-up sequence a
container runs:

- `a_sidecar_under_an_orchestrator_enrols_and_reports_with_no_rustak_secret_at_all`: the
  configuration file holds no token of any kind. `--enroll` writes the three PEMs (key `0600`)
  from the assertion in a file, the orchestrator **rotates the token between the two uses**, and
  the running start connects the stream with the certificate and registers over the control API
  with an access token bought from the *rotated* assertion. This is the end-to-end proof of the
  whole brief.
- the environment form, delivered through `--env` (under a variable auto-detection never looks
  for, because the process environment is shared by every test in that binary).
- `--check` on a pre-enrolment file that will use a workload identity: valid, and writes nothing.

**Unit tests** (~90 new across the two crates — 61 under `workload` in `rustak-server`, 19 under
`workload` in `rustak-client`, plus the sidecar enrolment, config and `enroll` cases): the `[auth.workload]` schema and all nine
`--check` refusals; dotted claim lookup including `kubernetes.io`; the binding engine
(ambiguity, agreement, `match`, case sensitivity, both orchestrators' claim shapes); the key
cache, the discovery indirection, the throttle and a key set on disk; the refusal vocabulary and
that every refusal a caller can provoke looks the same on the wire; the request-side header
handling and the path guard; the `jwt-bearer` grant's three early refusals; the sidecar's source
detection order, token reading and trimming, unverified issuer, exchange, cache margin, re-read
and refusal wording; the sidecar's credential precedence, the Bearer presentation, and the two
`--check` cases.

## Exit checks

```
$ cargo fmt --all --check
ok

$ cargo clippy --workspace --all-targets -- -D warnings
    Checking rustak-plugin-example v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-example)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 17.37s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.54s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 8 other files

$ ./scripts/check-file-length.sh
no file over the limit
  (config/workload/mod.rs 200, config/workload/validate.rs 223, auth/workload/mod.rs 213,
   auth/workload/verify.rs 156, auth/workload/keys.rs 163, marti/enroll.rs 261,
   pki/revoke.rs 272, auth/resolve.rs 258, sidecar/workload.rs 223, sidecar/enrolment.rs 260)

$ cargo test --workspace
test result: ok. 3282 passed; 0 failed; 2 ignored   (every suite, 0 failing suites)

     Running tests/workload_identity.rs
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.47s
     Running tests/sidecar_enrolment.rs
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.07s
     Running unittests src/lib.rs (rustak-server)
test result: ok. 1904 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 41.26s
     Running unittests src/lib.rs (rustak-client)
test result: ok. 212 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.04s
```

## Re-verified against the vendors' documentation (2026-09-21)

- **Nomad `identity` block** — parameters are exactly `name`, `aud`, `change_mode`,
  `change_signal`, `env`, `file`, `filepath`, `ttl`, `extra_claims`; `change_mode` takes
  `"noop"`, `"restart"`, `"signal"`. The default identity is `NOMAD_TOKEN` /
  `secrets/nomad_token`, and a **named** identity follows `NOMAD_TOKEN_${name}` /
  `secrets/nomad_${name}.jwt` — which is what the sidecar's auto-detection looks for
  (`NOMAD_TOKEN_rustak`, `${NOMAD_SECRETS_DIR}/nomad_rustak.jwt`).
- **Kubernetes** — `/.well-known/openid-configuration` and `/openid/v1/jwks`, and the
  `system:service-account-issuer-discovery` ClusterRole. One correction to what the brief implied:
  a default binding already gives that role to the `system:serviceaccounts` group, which does
  **not** cover an anonymous caller — and rustak is not a pod, so it fetches anonymously. The
  documented binding is therefore to `system:unauthenticated`, with the alternative (run rustak in
  the cluster and give its own service account the role) noted beside it.

## What I could not verify

- **Against the Dublin cluster, or any real orchestrator.** Everything is exercised against the
  in-process harness and a `wiremock`-served key set. Nothing was run against Nomad, against
  Kubernetes, or against a rustak server behind a Let's Encrypt certificate. In particular the
  plain-HTTP tailnet `jwks_url` is untested in anger, and the claim shapes are the ones the brief
  records rather than ones I saw come out of that cluster.
- **Nomad's `sub` format** (`global:<namespace>:<job>:<group>:<task>:<identity>`) is taken from
  the brief; nothing in the implementation parses it, it is only recorded in the audit trail, so a
  different shape there would change a log line and nothing else.
- **Kubernetes' `expirationSeconds` minimum of 600** is the brief's; I could not find it stated on
  the pages I fetched. It appears only in the documented pod snippet, which uses 3600.
- **The `profile/enrollment` route with an assertion** is reachable — it is under the same
  `/Marti/api/tls/` prefix and `resolve_principal` resolves it the same way — but there is no test
  that fetches a device profile with a workload assertion. Nothing about that flow is
  workload-specific; the grace window it exists for is about *spent one-time tokens*, and a
  workload assertion is never spent.
- **A second issuer with a different `iss`.** The integration fixture registers two issuers
  (a Nomad rule set and a Kubernetes one) over one mock server, so they share an `iss` and both
  are tried for every token. That is a realistic shape and it exercises the candidate loop, but
  it is not the same as two genuinely separate clusters — the refetch throttle test had to be
  written as "at most one refetch per issuer" because of it.
- **Non-Unix key permissions**, unchanged from M9-05: `write_pem` sets `0600` only on Unix.
