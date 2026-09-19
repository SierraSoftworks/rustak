# M5-03 — "Onboard CloudTAK": one action that produces everything CloudTAK's server setup needs — complete

Brief: `.claude/plan/briefs/M5-03-cloudtak-onboarding.md`
Read first: `conventions.md`; `plan.md` → Identity & auth model; `compat/cloudtak.md` §1, §2, §3;
status files `M2-01` (`pki::issue`, `pki::p12`, `p12_legacy`), `M2-02` (`ClientPassword` minting),
`M2-08` (`/api/v1/certificates`), `M3-02` (`web/api/config_packages.rs` — the refusal this carves an
exception next to), `M2-07`/`M3-04` (UI patterns), `M4-03` (`interop/cloudtak`).

## What was built

`POST /api/v1/users/{username}/cloudtak-onboarding` and
`GET /api/v1/cloudtak-onboarding/{id}.p12` — administrator-only, audited at both ends, one-shot,
sealed at rest with a ten-minute TTL — plus the admin-UI action, the demo fixtures, the interop
scenarios and the documentation.

| File | Functional lines | Contents |
|---|---:|---|
| `rustak-api/src/cloudtak.rs` | 79 | `OnboardingCredential`, `CloudTakPorts`, `CloudTakOnboardingRequest`, `CloudTakUrls`, `CloudTakOnboarding` (hand-written `Debug` redacting both secrets), `DEFAULT_CREDENTIAL_LABEL`, `ISSUED_VIA`, `BUNDLE_TTL_MINUTES` |
| `rustak-server/src/identity/cloudtak/mod.rs` | 205 | `Onboarding`, `onboard`, credential resolution (mint / verify existing), the audit entries |
| `rustak-server/src/identity/cloudtak/bundle.rs` | 123 | `Bundle`, `stash`, `take`, `sweep`, the download identifier and its validation |
| `rustak-server/src/identity/cloudtak/urls.rs` | 92 | `compose`, `default_host`, host validation |
| `rustak-server/src/web/api/cloudtak_onboarding.rs` | 81 | the two routes |
| `rustak-server/migrations/0018_cloudtak_onboarding.sql` | — | widens `certificates.issued_via` by rebuilding the table |
| `rustak-ui/src/api/cloudtak.rs` | 36 | `onboard`, `p12` (and the sentence a `410` means here) |
| `rustak-ui/src/fixtures/cloudtak.rs` | 64 | demo hand-over with a one-shot store, so the `410` branch is exercised |
| `rustak-ui/src/pages/panels/cloudtak.rs` | 166 | the panel: the exception notice, the advanced host/port form, the button |
| `rustak-ui/src/pages/panels/cloudtak_result.rs` | 87 | the result: three URLs, two secrets, the one-shot download |
| `rustak-server/tests/cloudtak_onboarding.rs` | — | 19 integration tests |
| `interop/node-tak/tests/cloudtak-onboarding.test.ts` | — | 4 scenarios through `@tak-ps/node-p12` and the mTLS probe |
| `interop/cloudtak/src/onboard.ts` + `tests/onboard.test.ts` | — | the suite now takes its admin identity from this endpoint |
| `e2e/tests/cloudtak.spec.ts` | — | 4 Playwright specs |

Additive elsewhere: `pki/issue.rs` (`GeneratedRequest`, `generate_signing_request`),
`pki/p12.rs` (`P12Options::handover`), `pki/facade.rs` (`IssuedVia::CloudTakOnboarding`),
route lines in `web/api/mod.rs` (+2 in its protected-route table), module lines in
`identity/mod.rs`, `rustak-api/src/lib.rs`, `rustak-ui/src/{api,fixtures,pages/panels}/mod.rs`,
`rustak-ui/src/pages/{user_detail,user_create}.rs`, `styles.scss` §5.11.
Docs: `docs/deployment.md` (new "Onboarding CloudTAK" subsection replacing the manual recipe),
`README.md` (one sentence), `compat/cloudtak.md` §2, `backlog.md` (two entries).

## The maintainer's tightening of item 1 — done, and the brief updated

The passphrase requirement arrived mid-task and the brief's item 1 and its test list have been
reworded to match. What the implementation does:

* **Generated per bundle.** `generate_token(12)` — sixteen base64url characters, 96 bits, fresh for
  every hand-over. Nothing like the shared `atakatak` every other bundle uses.
* **It is what encrypts the `.p12`.** `P12Options::handover` takes it; `legacy` is forced on there
  rather than read from `p12_legacy`, because CloudTAK's parser reads PBES1/3DES and nothing newer.
* **Returned only in the creation response**, so the UI shows it once beside the download.
* **Stored nowhere.** `bundle::stash` is handed the already-encrypted bytes and has no parameter for
  the passphrase; it is not on the credential row, not on the certificate row, not in either audit
  entry, and not in any log line.
* **The seal is now defence in depth, not the defence.** A database dump plus the installation's
  sealing key yields an encrypted PKCS#12 and no way in.

Two tests were added for it:

* `the_sealing_key_alone_does_not_open_the_bundle` — opens the sealed envelope with the real
  `SecretStore`, then asserts the bytes inside refuse `""`, `"atakatak"` and a wrong passphrase, and
  open only with the one the response carried.
* `the_passphrase_is_in_the_response_and_in_no_row_anywhere` — asserts the value appears in neither
  the key/value stash, the credential row, the certificate row, nor the audit log.

## Decisions worth recording

### A migration, because `issued_via` is a `CHECK`

`certificates.issued_via` is a closed set and SQLite cannot alter a `CHECK` in place, so
`0018_cloudtak_onboarding.sql` rebuilds the table — the pattern `conventions.md` prescribes. Two
foreign keys point at `certificates` with `ON DELETE SET NULL`, and a rebuild is a drop, so both are
handled explicitly: the new table's self-reference is declared against `certificates_migrated` (the
rename rewrites it back), and `devices.last_certificate_id` is copied out and restored.
`the_rebuild_that_made_room_keeps_every_row_and_both_links` proves the rows, both links and
`PRAGMA foreign_key_check` survive; `db::migrations`'
`an_upgraded_database_has_the_same_schema_as_a_fresh_one` already covers the shape.

### The `password` field is optional

A reused client password is stored as an argon2id hash, so nothing can re-emit it. The response
therefore omits `password` rather than returning an empty string, and the UI says why.

### The certificate goes through `Pki::enroll`, not a shortcut

`generate_signing_request` builds a real, self-signed CSR which then goes through `parse_csr`,
`CsrPolicy::validate` and `issue_client_cert` — the same path an enrolling device takes. There is no
second, laxer route into issuance, and the row, the audit entry and the revocation hook are the
ordinary ones.

### The interop probe path is the download, not the POST

`interop/shared/src/probe.ts` assumes a POST-only route answers `405`. It does not here: rustak
answers an unrecognised path with the admin UI's single-page shell, so a `GET` on the POST-only
route is `200 text/html`, which the probe correctly reads as *absent* — and every scenario for this
brief skipped in silence on the first run. Both suites now probe
`GET /api/v1/cloudtak-onboarding/probe.p12`, which is a real `GET` and answers a clean `410`.
`the_surface_the_interop_suites_probe_for_answers_something_they_can_read` locks both halves of that
down.

### `@tak-ps/node-p12` returns PEM with the line breaks removed

Not a bug in the bundle: `convertToPem` runs node-forge's output through `.replace(/\r\n/g, "")`,
which strips the breaks rather than converting them, so Node's TLS refuses the result with
`ERR_OSSL_PEM_NO_START_LINE` whatever produced the file. The scenario re-wraps before use and says
so; the server was not bent to suit it. Recorded in `compat/cloudtak.md` §2.

### The Playwright flow runs against `?demo`

The e2e server runs with `[web.marti] enabled = false` and `[stream.tls] enabled = false`, so
`runtime::listen` installs no certificate authority and the endpoint correctly answers `503`. That
refusal is asserted against the real API — an operator must be told why the button did nothing — and
the rest of the flow uses the demo fixtures, which is what CI's debug UI bundle exists for. The
endpoint itself is covered by the 19 Rust integration tests and, against a real authority and
CloudTAK's own parser, by `interop/node-tak`.

### No scheduled sweep

`identity::cloudtak::sweep` runs when the next hand-over is prepared, and the TTL is enforced on
read, so a stale bundle is never served. A `jobs/` entry would also clear the last one on an
installation that onboards CloudTAK once; `jobs/mod.rs`, the job host and `config/jobs.rs` were
outside this brief's files, so it is recorded in `backlog.md` instead.

## Exit checks

```
$ cargo fmt --all --check
(clean)

$ scripts/check-file-length.sh
(clean)

$ cargo clippy --workspace --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s)

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
(clean)

$ cargo test --workspace --no-fail-fast
41 test binaries, "test result: ok." in every one, 0 failed
  rustak-api                                150 passed; 0 failed
  rustak-server (lib)                      1754 passed; 0 failed; 2 ignored
  rustak-server tests/cloudtak_onboarding    19 passed; 0 failed

$ cd rustak-ui && trunk build
2026-09-19T13:33:30Z INFO ✅ success

$ cd rustak-ui && cargo fmt --all --check
(clean)

$ cd rustak-ui && cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s)

$ cd e2e && npm run typecheck
(clean)

$ cd e2e && RUSTAK_E2E_CHROMIUM="…/chromium-1234/…/Google Chrome for Testing" npx playwright test
31 passed, 2 failed (3.1m)
  ✔ all four tests/cloudtak.spec.ts specs
  ✘ tests/auth.spec.ts:31 "a browser holding no passkey for this server cannot sign in…"
  ✘ tests/auth.spec.ts:50 "a passkey registered for one host is refused at another"

$ cd interop/node-tak && npm run typecheck && npm test
ℹ tests 30   ℹ pass 30   ℹ fail 0   ℹ skipped 0
  ✔ hands CloudTAK a keystore its own parser can open, and a certificate the mTLS listener accepts
  ✔ hands the keystore over exactly once
  ✔ echoes back the host and ports a published deployment actually uses
  ✔ refuses a hand-over to anything but an administrator

$ cd interop/cloudtak && npm run typecheck && npm run test:unit
ℹ pass 47   ℹ fail 0
```

### The two Playwright failures are not this brief's

`tests/auth.spec.ts:31` and `:50` fail on the passkey-refusal path
(`getByText('We could not check your session')` no longer matches). Both come from the OIDC-claims
work that landed while this brief was in progress — `ed5ed61 fix: Offer sign-out when the admin
console refuses an account` changed exactly that refusal. Nothing in this brief touches
`auth/**`, `web/api/{auth,me}.rs`, `auth/resolve.rs` or any passkey path, and `git diff --name-only`
confirms it. Flagged for whoever owns those commits.

## Concurrency: a second agent was working in this repository

The brief says "no other implementation agent is running". That was not true: an OIDC-claims brief
was editing `rustak-api/src/auth.rs`, `rustak-server/src/{identity/users.rs, auth/resolve.rs,
db/repos/users*, web/api/{auth,me}.rs, web/helpers/oidc/claims.rs}` throughout, and it took
migration number **0017** (`0017_user_claims.sql`) roughly a minute after this brief created its own
`0017_cloudtak_onboarding.sql`. Two files with the same number make `db::migrations::load()` fail
outright, so this brief deferred and renumbered its own to **0018**; that agent's work has since
landed as `ed5ed61`/`abba577`/`ec7320f` with `0017_user_claims.sql` committed, so the numbering is
now correct with no gap. Nothing of theirs was edited.

Because the rebuild test has to seed the schema *before* this brief's migration, it finds its own
number by filename rather than hard-coding one — so a later renumbering by anybody cannot silently
turn it into a test of the wrong thing.

One side effect worth naming: `docs/deployment.md` shows as **staged** in `git status`. This brief
runs no `git`/`but` commands; it was staged by the other session's tooling. It is this brief's
content and wants committing with the rest of it.
