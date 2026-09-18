# CI-01 — CI steward: running log

Append newest first. One entry per run I acted on: run id, what failed, what I did,
what remains. Brief: `.claude/plan/briefs/CI-01-ci-steward.md`.

---

## Standing state of CI (2026-09-18, 19:15 UTC+?)

| Workflow | On `main` | Verdict |
|---|---|---|
| `rust.yml` | **red** — `Test` fails on `tests/bootstrap.rs` (2 of 3) | **fix ready and verified**, §F2; everything else in the run is green |
| `nightly.yml` `interop-eud` | **red** — 5 passed, 1 skipped, 3 failed | one known-expected failure, one harness bug (fixed below), one server bug (written up) |
| `security_audit.yml` | **red, and has never been green** (8 of 8 recorded runs failed) | two advisories, neither fixable from this repository today — needs a decision, §S3 |
| `changelog.yml` | green | — |

---

## 2026-09-18 — run 35376447445 (`main`, `b802803`, "fix(test): Cache test key material…")

**The M0-21 landing did what it set out to do.** The `Test` job now **finishes**,
in **12m 06s** (17:48:10 → 18:00:16) against its 30-minute timeout, where it
previously had to be cancelled by hand. The library target ran **1215 tests in
140 s**. `Lint` passed, which confirms M0-21's blind `result_large_err` fix in
`marti/tls.rs` against CI's newer stable clippy. `Build UI`, `Interop: node-tak`,
`End-to-End Tests`, all ten `build` matrix jobs and all four `docker-build` jobs
passed; `docker-publish` ran; `tap` correctly skipped (not a release).

**What failed:** `rustak-server/tests/bootstrap.rs`, 2 of 3.

```
---- a_first_start_serves_its_own_tls_and_walks_an_operator_all_the_way_in ----
panicked at rustak-server/tests/bootstrap.rs:222:9:
/tmp/.tmpg7u7fV/pki/ca.crt was never written

---- the_insecure_development_listener_serves_plaintext_and_still_stops_cleanly ----
panicked at rustak-server/tests/bootstrap.rs:243:9:
the listener never came up: error sending request for url (http://localhost:45039/api/v1/health)
test result: FAILED. 1 passed; 2 failed … finished in 60.30s
```

Both are the file's own `STARTUP_TIMEOUT` (30 s) expiring. This suite is the one
that does **not** go through `TestServer`, so none of M0-21's three levers reach
it: it drives `rustak_server::run` directly, and a first start mints its own
RSA-2048 token-signing key (`auth::jwt::create_key`) and its own RSA-2048
certificate authority (`pki::ca`), plus an RSA server certificate in the TLS
case — none of them the cached `testing::keys::JWT_SIGNING_KEY`. The third test,
`a_listener_that_cannot_bind_reports_it_rather_than_running_without_one`, passes
because it has no deadline of its own: it just awaits `run` to its bind failure.

**Same two tests fail on the newest `main` too** — run 35377904509 (`ce0998d`),
`finished in 34.60s`, `error: 1 target failed: -p rustak-server --test bootstrap`
and nothing else in the job. So it is the file, not the commit.

### F2 — `rustak-server/tests/bootstrap.rs` (landed-ready, verified)

Three changes, all in that one file, which the brief gives me:

1. **`start()` now races the readiness wait against the server task.** This is
   the fix that mattered most, and it is a defect independent of the timeout:
   `start()` spawned `run` on a `JoinHandle` and then polled a file and a socket
   for 30 s *without ever looking at the handle*. A `run` that returned an error
   in the first second therefore produced "`…/pki/ca.crt` was never written" or
   "the listener never came up" — identical text for every possible start-up
   failure. My first local reproduction proved the point: it failed with those
   same two messages, and the real cause was a migration-numbering error from
   another agent's half-landed `0012`, which the third test happened to surface
   and the other two hid. A new `ready()` helper `tokio::select!`s (biased) on
   `&mut handle`, so a server that stopped, returned or panicked is reported with
   *its* error.
2. **`config.pki.key_type = KeyType::EcdsaP256` in `start()`.** `[pki] key_type`
   governs both the authority and the server certificate, so this removes two of
   the three RSA-2048 generations a first start does in the TLS test and one of
   two in the plaintext test. This suite is about the wiring — an authority that
   did not exist a second ago, a certificate issued from it, a client that trusts
   it — and not the key algorithm; `pki::ca` still asserts `Rsa2048` is the
   shipped default and that a default CA gets one (`ca.rs:675`, `:681`), and
   `pki::keys` exercises all three types, so nothing is left uncovered.
   `pki::testing::TestAuthority` has been P-256 for this reason since M0-21 §5.1.
3. **`STARTUP_TIMEOUT` 30 s → 120 s**, for the one RSA-2048 token-signing key a
   test file cannot avoid: `auth::jwt` is RS256 with `KEY_BITS = 2048` hard-coded
   and `load_or_create` has no seam a test can reach without editing `src/`. With
   (1) in place a *failed* start-up no longer waits this out, so the number now
   bounds only the honest slow case.

**Verified** with the CI configuration on a filtered target:

```
$ RUSTFLAGS=-Cinstrument-coverage cargo test -p rustak-server --features testing     --test bootstrap -- --test-threads=2
running 3 tests
test a_listener_that_cannot_bind_reports_it_rather_than_running_without_one ... ok
test a_first_start_serves_its_own_tls_and_walks_an_operator_all_the_way_in ... ok
test the_insecure_development_listener_serves_plaintext_and_still_stops_cleanly ... ok
test result: ok. 3 passed; 0 failed … finished in 26.49s

$ rustfmt --edition 2024 --check rustak-server/tests/bootstrap.rs      → clean
$ cargo clippy -p rustak-server --features testing --test bootstrap -- -D warnings
                                                                       → clean
```

(That 26.49 s is on this machine with `--test-threads=2`; CI's two vCPUs are
slower, which is what the 120 s headroom is for. The runner had not finished
start-up inside 30 s even with the RSA cost it no longer pays.)

**Later runs on `main`:** 35377781744 (`5429eeb`) and 35377904509 (`ce0998d`) were
still in `Test`/`build` at the time of writing, with `Lint`, `Build UI`,
`Interop: node-tak` and `End-to-End Tests` already green on `ce0998d`.

---

## 2026-09-18 — nightly 35376450618 (`main`, `b802803`) — first EUD scenario run

**`5 passed, 1 skipped, 3 failed`**, against M2-09 §4's predictions:

| Scenario | M2-09 expected | Actual | Verdict |
|---|---|---|---|
| `enroll-basic` | runs; pass plausible | **pass** | ATAK's own enrollment code works against rustak first time — the PKCS#12 truststore shape, the subject order and the Basic-auth path all held |
| `two-eud-routing` | pass | **pass** | as predicted |
| `negotiate-refused` | pass | **pass** | denial wording held |
| `negotiate-silent` | pass | **pass** | 60 s timeout line held |
| `mp-upload` | uncertain — the `!ECDH` gate | **pass** | the `[ports]` pinning was right |
| `enroll-revoked` | runs | **skipped** | *correct*: `certificateRevocation` probes `/api/v1/certificates`, which M2-08 landed **after** `b802803`. Not a regression — the probe doing its job. It will run on the next nightly. |
| `chat-direct` | **fail — expected, and the point** | **fail** | exactly as written up: `[alpha] commo-xml.txt has no event with type=b-t-f-s`. The delivery half passed (31 SA events + a `t-x-d-d` received). Standing failure until `M1-08-chat-bounce` lands. |
| `disconnect` | pass | **fail** | **the server half passed** — bravo's `t-x-d-d` with `link_uid = EUD-GONE-ALPHA` was asserted and found. Both failures were client-log expectations the harness had wrong. Fixed, §F1. |
| `mp-download` | uncertain | **fail** | **a real server bug.** Written up in `CI-01-2026-09-18-mission-package-url.md`. |

M2-09 §4's stated risk for `disconnect` — "`remiface:0` … if it is wrong ALPHA
never disconnects and both halves fail together" — did **not** happen: ALPHA
disconnected, BRAVO was told, and the `<link>` named the right device.

### F1 — harness fixes for `disconnect` and `chat-direct` (landed-ready)

Files changed, all mine under the brief:

- **`interop/eud/scenarios/disconnect.toml`** — ALPHA asserted `Interface Down`
  after `remiface:0`. `remiface` takes the interface *out of the stack* rather
  than bringing it down, and by `quit` there is no interface left to take down
  either, so that line is never logged. ALPHA does log
  `Contact Removed: EUD-GONE-BRAVO`, which is the client-side evidence its stream
  went away, so that is what it asserts now. BRAVO asserted
  `log_any = ["Contact Added", "Contact Removed"]`; `log_any` means "in any
  order", not "any one of", so this asked BRAVO to age ALPHA out of its contact
  list within the 30 s it stays up — `commotest`'s own staleness timer, which
  this scenario does not control. Now `["Contact Added"]`. **What the scenario
  proves is unchanged**: BRAVO's `xml_present = [{ type = "t-x-d-d", link_uid =
  "EUD-GONE-ALPHA" }]` is the assertion this scenario exists for, and it passed.
- **`interop/eud/src/expect.ts`** — `checkRuntime` measured `min_runtime_seconds`
  against the **shortest**-running EUD. `chat-direct` scripts BRAVO to quit at
  T+35 *on purpose*, so that ALPHA can speak into the gap at T+50 — and the guard
  read that deliberate departure as a run cut short
  (`the EUDs ran for 37s, short of the 60s…`) on top of the real `b-t-f-s`
  failure. Now measured against the longest, which is the scenario's own clock.
  Nothing is lost: an EUD that died before its script finished is caught by the
  timeout kill (`docker.ts` → `timedOut`) and by its own per-EUD expectations.
- **`interop/eud/tests/expect.test.ts`** — a new case,
  `an EUD scripted to leave early does not make the scenario a short run`.
- **`interop/eud/README.md`** — the two semantics now documented in the schema
  section: `log_any` relaxes order, not count; `min_runtime_seconds` is the
  scenario's clock.

Verified: `npm run typecheck` clean, `npm run test:unit` **42 pass, 0 fail**.

After these, the nightly's expected steady state is **7 passed, 2 failed**
(`chat-direct`'s bounce half, `mp-download`'s URL) until `M1-08-chat-bounce` and
the write-up's fix land, and 8 once `enroll-revoked` starts running.

---

## S3 — `security_audit.yml` has never been green, and needs a decision

Every recorded run has failed (35337851102 through 35377904545). Two advisories,
and **neither is fixable from this repository today**:

- **`RUSTSEC-2023-0071` — `rsa 0.9.10`, the Marvin timing attack.** The advisory
  itself records `patched = []` and says so explicitly: "Still affected as of
  2026-09-12: rsa 0.9.10 (latest stable) and rsa 0.10.0-rc.18 (latest).
  `patched = []` is intentional." rustak signs tokens and issues certificates with
  it; there is no version to move to.
- **`RUSTSEC-2026-0258` — `h2 0.3.27`, unbounded empty DATA frames.** Patched in
  `0.4.16`, which is a different major line. `cargo tree -i h2@0.3.27` gives one
  path and only one: `actix-http 3.13.6` → `actix-web 4.15.0` (and
  `actix-multipart`, `actix-ws`). actix-web 4 pins `h2 ^0.3`; moving off it is an
  actix major upgrade, not a dependency bump.

So the workflow as configured can only ever be red, which means it reports
nothing: a genuinely new advisory would land in a job that was already failing.
**I have not changed it** — suppressing a security gate is the user's call, not
mine. The two shapes of fix, for whoever decides:

1. `rustsec/audit-check@v2.0.0` takes an `ignore:` input; or `.cargo/audit.toml`
   with `[advisories] ignore = ["RUSTSEC-2023-0071", "RUSTSEC-2026-0258"]`, which
   `cargo audit` reads and which keeps the rationale in the repository next to
   the list. Either ignores **those two ids only** — any new advisory still fails
   the job.
2. Leave it red and treat the job as a report rather than a gate — in which case
   `docs/ci.md` should say so, because right now it reads as a gate.

My recommendation is (1) with a dated comment per advisory and a re-check note,
plus a line in `docs/ci.md`. Say the word and I will write it.

---

## S4 — Dependabot PRs #1–#3 (evaluated; **not merged**, per the brief)

**#1 — `codecov/codecov-action` 7.0.0 → 7.1.0. Recommend: merge after a re-run.**
The diff is `dist/codecov.sh` (+41/-15), `action.yml` (+5), `src/version`,
`README.md` — the uploader wrapper, no input removed or renamed, so `rust.yml`'s
`with:` block is unaffected. Its checks show `End-to-End Tests` and the two
`darwin-arm64-*` jobs failing, and **both are stale-base artefacts, not this
bump**: the run is 35329781507 from 09:32, and the `darwin-arm64-rustak` job log
shows `sudo apt update && sudo apt install -y musl-tools` → `sudo: apt: command
not found` on a **Linux** runner under a job named `darwin-arm64-rustak` — an old
`build` matrix where `run_on`/`setup` merged onto the wrong entries. The matrix on
`main` today is correct and every darwin job passes there. Re-run against current
`main` and it should be green.

**#2 — `typescript` 5.9.3 → 7.0.2 in `e2e`. Recommend: rebase, then merge.**
No code change needed: `End-to-End Tests` and `Interop: node-tak` both **passed**,
so TypeScript 7 compiles and runs the suite. The only red check is `Lint`, and it
is not about TypeScript — `error: the Err-variant returned from this function is
very large`, i.e. the newer stable's `clippy::result_large_err` that M0-21 fixed
in `marti/tls.rs`. The PR branched at 17:34, that fix landed at 17:47. A rebase
clears it.

**#3 — `p256` 0.13.2 → 0.14.0. Needs a one-line code change first.** `Lint` and
`Test` both fail to compile, in one place:

```
error[E0599]: no method named `to_encoded_point` found for reference
              `&ecdsa::verifying::VerifyingKey<NistP256>`
  --> rustak-server/src/testing/authenticator/keys.rs:97:54
help: there is a method `to_sec1_point` with a similar name
```

`to_encoded_point` → `to_sec1_point` at
`rustak-server/src/testing/authenticator/keys.rs:97`. The `.x()` / `.y()` calls on
the result are unchanged. Everything else builds: all ten `build` matrix jobs,
`e2e` and `interop-node-tak` are green on the PR. I have **not pushed to the
Dependabot branch**; the change belongs on `main` (or in a follow-up commit on the
PR by whoever merges it).

---

## S5 — Slow spots

Nothing over 15 minutes on `main` today. The longest jobs on 35376447445:
`Test` 12m 06s, `windows-amd64-rustak` 10m 38s, `darwin-amd64-rustak` 7m 32s,
`linux-arm64-rustak` 6m 30s. The nightly's `Interop: EUD harness` ran **13m 20s**
for nine scenarios — under M2-09 §4's own 20–25 minute estimate and well inside
its 90-minute timeout. No cuts proposed.

## S6 — Concurrency note

`interop/shared/src/probe.ts` was edited by another agent while I was reading it
(a retry window on the stream probe, so a listener still binding is not read as
absent). Left alone — it is a real improvement and does not touch anything I
changed.
