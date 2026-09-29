# M10-07 — Make the expensive integration suites cheaper

**Done.** `workload_identity` boots 7 servers instead of 24, `sidecar_enrolment`
2 instead of 4, `sidecar_trust` 1 instead of 2. Every assertion is kept, several are
stronger, and each case of a shared server is still reported by its old test name
when it fails. `hostile_server_name` is left alone, and the reason is below.

All figures here are **local**: an uninstrumented debug build on a ten-core
machine that several other agents' builds were loading at the time (the load
average is given beside every sample). CI's band under coverage on a two-vCPU
runner has not been measured and is the orchestrator's to re-take after landing.

## Where the time goes

Per server, not per assertion. In the original `workload_identity` each test's
wall time was 1.0–1.7 s at `--test-threads=2` whatever it asserted (a three-line
refusal cost the same as a full enrolment and handshake), and the binary's CPU
time divided by its servers is ≈ 0.9 s. The issuer's three RSA keys were already
generated once per binary behind `LazyLock`s, as the brief asked.

The single biggest item per server is probably not in these suites at all:
`stream_support::Harness` builds its context through `rustak_server::build_context`,
which calls `JwtIssuer::load_or_create` and so **generates a fresh RSA-2048
access-token key for every harness**. `TestServer` avoids exactly this by adopting
`testing::keys::JWT_SIGNING_KEY` (`JwtIssuer::load_or_adopt`); the harness does
not. A `hostile_server_name` test on a `TestServer` took 0.1–0.6 s in these
samples (0.9 once, under load); the one on a `Harness` took 1.1–1.6 s. Under coverage the comment in `auth/tokens.rs` puts
one generation at "a minute apiece". Both files are outside this brief
(`stream_support/` is M10-08's, `build_context` is `src/` outside `testing/`),
so it is an open item below rather than a change here — and it is the lever
that would help every `stream_*` suite too.

## What changed

### `workload_identity` (24 tests → 7)

- **`the_refusals`** — one deployment, twelve cases run concurrently through
  `testing::cases::run`. Its accounts: `ais` (the service every refused assertion
  would otherwise be good for), `somebody-else`, `person` (a person),
  `off` (disabled), `acl-refused`; `nobody` is deliberately never created. Its
  access-control expression is `username != "acl-refused"`, which refuses
  exactly one account, so no other case can be refused by it.
- **`what_is_accepted`** — one deployment, seven cases, one account each (`ais`,
  `audited`, `adsb`, `moved`, `granted`, `cloudtak`): an enrolment supersedes the
  account's earlier certificates, so no two cases may share one. Callsigns are
  distinct too, because the live registry is what a handshake is judged by.
- **Kept on a server of their own**, because each changes the configuration or
  the issuer: the clock-skew boundary (skew 600 s), token-file renewal
  (`access_token_ttl` 30 s), no issuers, superseding off, and key rotation (it
  rotates its issuer and counts that issuer's key-set fetches, which any
  neighbour would disturb).
- The fixture moved to `tests/workload_support/mod.rs` (the `Deployment`), with
  `nomad_for(job)` (the reference rule strips `rustak-plugin-`, so a job names
  its own account), `refused(...)` and `register(...)` added.

### The rate limiter, which sharing would otherwise have broken silently

Every workload refusal from one address counts against one limiter key
(`workload-identity`), ten per minute, then a fifteen-minute lockout. A dozen
refusals on one server would lock it out — and **a lockout is a refusal**, so the
later refusal cases would have passed for the wrong reason. So `Deployment`
raises `[auth.rate_limit] attempts` to 10 000 for every deployment in the suite,
and every refusal now goes through `Deployment::refused`, which asserts the
status was **not** `429`. That second half also closes the hole for the original
per-test servers, where it had never been checked.

### `testing::cases` (new, `rustak-server/src/testing/cases.rs`)

`run(Vec<(&'static str, LocalBoxFuture<()>)>)`: runs every case to completion
concurrently on the caller's runtime, catches a panic per case, and fails once at
the end with `N of M cases failed:` and each failing case's name and message.
Refuses two cases with one name. Unit-tested in the lib: a failure does not stop
the next case, each failure is named with its message, a passing case is not
listed, duplicate names are refused.

### `TestWorkloadIssuer::warm` (`src/testing/workload.rs`)

Generates its three RSA keys on three scoped threads instead of in sequence; the
first deployment in a binary waits for all three. Also fixed a misplaced doc
comment: `rotate`'s docs had been attached to `warm`.

### `sidecar_enrolment` (5 tests → 3; 4 servers → 2)

- `a_deployment_that_enrols_with_a_one_time_token` runs the two one-time-token
  cases on one server; the refused-token case now uses `svc.refused` / `refused`.
- `a_deployment_under_an_orchestrator` runs the file-form and environment-form
  cases on one server and one issuer. The environment case now enrols through a
  second fixed-account rule (namespace `from-env` → `svc.from-env`, service
  `from-env`), because under one rule both would be `svc.enrolling` and the second
  enrolment would supersede the certificate the first is about to connect with.
- `check_names_the_workload_identity_a_first_start_would_use` starts **no**
  server: its stream address is the discard port on loopback. It was booting a
  harness only to borrow an address, and `--check` promises not to touch the
  network, so a check that did would now fail rather than pass.
- The start-up waits use a local `START_UP` of 30 s instead of the harness's 5 s
  `EXPECT` (see "Flakiness" below).

### `sidecar_trust` (2 tests → 1; 2 servers → 1)

`a_deployment_whose_public_listener_holds_another_authority`: one harness and one
public listener, two cases. The internal-roots-only case now uses
`svc.unpinned` / `unpinned`; it fails in the TLS handshake before reaching the
server, so it cannot touch the pinned case's rows. `START_UP` as above.

### `hostile_server_name` — unchanged, deliberately

Four tests: one pure, one on a `Harness`, two on `TestServer`s. The `TestServer`s
adopt the process-wide signing key and cost well under a second each; the two share
nothing but would collide on the `blue` channel and the `ada` account, and
merging them saves about a third of a second. The `Harness` test is the only
server of its kind. Nothing here pays.

### `docs/ci.md`

The test count (22 → 24 since M9-14), the paragraph that said grouping was on the
backlog (now: what landed, with the local figures marked as local), and a fourth
lever in "Keeping the test job inside its timeout". The job's time bound and the
workflow are untouched.

## Measurements

Two interleaved samples of each suite before and after, the original built from
`HEAD` beside the new one so both saw the same load. `user` is the CPU time of
the whole binary; it is the stable figure under load.

| Suite | Servers | CPU before | CPU after | Wall `--test-threads=2` before | after |
|---|---|---|---|---|---|
| `workload_identity` | 24 → 7 | 20.9–22.8 s | 6.8–7.8 s | 16.1–17.3 s | 7.6–8.9 s |
| `sidecar_enrolment` | 4 → 2 | 3.4–4.4 s | 1.9–2.0 s | 2.3–4.2 s | 1.5–1.7 s |
| `sidecar_trust` | 2 → 1 | 1.8–2.0 s | 1.0–1.2 s | 1.4–1.7 s | 1.5 s |
| `hostile_server_name` | unchanged | 0.5–0.8 s | — | 1.1–1.3 s | — |

Load averages (1 min) during those samples: 25–71. The original
`workload_identity` in those pairs already had the parallel `warm`; the true
original, measured first at load 7–8, took 21.6 s CPU and 16.1 s wall at two
threads, so `warm` alone is within noise locally (uninstrumented RSA is cheap; it
is under coverage that it is seconds).

### Per test, before (original, `--test-threads=2`, load 7.6)

`workload_identity`, seconds: audience 1.13, no audience 1.09, expired 1.03,
clock skew 1.02, token-file renewal 1.69, another issuer 1.13, unpublished key
1.14, namespace 1.09, prefix 1.65, person 1.18, switched off 1.12, no account
1.24, no issuers 1.16, ACL 1.02, rotation 1.07, Nomad handshake 1.25, audited
1.12, Kubernetes 1.41, supersede 3.48, superseding off 1.11, compatibility form
1.13, jwt-bearer grant 1.70, invalid grant 1.13, password grant 1.38 — binary
16.1 s.
`sidecar_enrolment`: 1.18 / 1.26 / 1.33 / 1.23 / 0.38 (check) — 2.8 s.
`sidecar_trust`: 1.20 / 1.11 — 1.2 s. `hostile_server_name`: 0.00 / 0.09 / 0.26 /
1.07 — 1.1 s.

### Per test, after (`--test-threads=2`, load 32)

`workload_identity`: `the_refusals` 1.41, `what_is_accepted` 3.59, clock skew
1.19, token-file renewal 2.03, no issuers 1.06, superseding off 1.14, rotation
1.65 — binary 7.6 s. `sidecar_enrolment`: one-time token 1.35, orchestrator 1.51,
check 0.00 — 1.5 s. `sidecar_trust`: 1.52 s.

## Old test names → new

Every old name survives as a case name, which is what a failure reports.

| Old test | Now |
|---|---|
| `an_assertion_for_another_audience_is_refused` | case in `the_refusals` (same name) |
| `an_assertion_that_leaves_its_audience_out_altogether_is_refused_too` | case in `the_refusals` |
| `an_expired_assertion_is_refused_wherever_it_is_presented` | case in `the_refusals` |
| `an_assertion_from_another_issuer_is_refused` | case in `the_refusals` |
| `an_assertion_signed_by_a_key_the_issuer_does_not_publish_is_refused` | case in `the_refusals` |
| `an_assertion_from_another_namespace_is_refused` | case in `the_refusals` |
| `a_job_outside_the_prefix_is_refused` | case in `the_refusals` |
| `an_assertion_that_resolves_to_a_persons_account_is_refused` | case in `the_refusals` (account `person`, job `rustak-plugin-person`) |
| `an_assertion_for_an_account_that_is_switched_off_is_refused` | case in `the_refusals` (account `off`) |
| `an_assertion_for_an_account_that_does_not_exist_is_refused` | case in `the_refusals` (account `nobody`) |
| `an_access_control_expression_that_refuses_the_account_stops_the_enrolment` | case in `the_refusals` (`username != "acl-refused"`) |
| `a_jwt_bearer_grant_with_an_assertion_we_would_not_accept_is_invalid_grant` | case in `the_refusals` |
| `a_nomad_job_enrols_and_the_certificate_it_gets_completes_a_handshake` | case in `what_is_accepted` |
| `the_enrolment_is_audited_with_the_run_that_made_it_and_never_the_token` | case in `what_is_accepted` (account `audited`; audit read by subject) |
| `a_kubernetes_pod_enrols_through_the_nested_claims` | case in `what_is_accepted` |
| `a_second_enrolment_supersedes_the_first_and_the_old_certificate_stops_working` | case in `what_is_accepted` (account `moved`; audit read by subject) |
| `the_compatibility_form_puts_the_assertion_where_a_password_would_go` | case in `what_is_accepted` |
| `a_jwt_bearer_grant_answers_a_token_that_reaches_the_control_api_and_nothing_more` | case in `what_is_accepted` (account and service `granted`) |
| `the_password_grant_answers_exactly_what_it_always_did` | case in `what_is_accepted` |
| `the_clock_skew_allowance_is_a_boundary_and_not_a_door` | unchanged test |
| `a_sidecar_whose_token_file_is_renewed_keeps_its_control_link` | unchanged test |
| `an_installation_with_no_issuers_accepts_no_assertion_at_all` | unchanged test |
| `a_rotated_key_is_picked_up_by_the_first_assertion_that_needs_it` | unchanged test |
| `an_installation_that_turns_superseding_off_keeps_both_certificates` | unchanged test |
| `sidecar_enrolment::a_sidecar_enrols_on_its_first_start_and_uses_what_it_wrote_on_the_next_one` | case in `a_deployment_that_enrols_with_a_one_time_token` |
| `sidecar_enrolment::a_token_the_server_refuses_stops_start_up_rather_than_running_half_identified` | case in the same (account `svc.refused`) |
| `sidecar_enrolment::a_sidecar_under_an_orchestrator_enrols_and_reports_with_no_rustak_secret_at_all` | case in `a_deployment_under_an_orchestrator` |
| `sidecar_enrolment::a_workload_identity_can_come_from_the_environment_as_nomad_also_offers_it` | case in the same (namespace `from-env`, account `svc.from-env`) |
| `sidecar_enrolment::check_names_the_workload_identity_a_first_start_would_use` | unchanged name, no server |
| `sidecar_trust::a_sidecar_trusts_the_public_listener_and_the_stream_with_different_roots` | case in `a_deployment_whose_public_listener_holds_another_authority` |
| `sidecar_trust::a_sidecar_that_trusts_only_the_internal_ca_cannot_reach_the_public_listener` | case in the same (account `svc.unpinned`) |

What moved inside an assertion, so a reviewer need not diff for it:

- Refusals that asserted `is_err()` now assert an error **that is not `429`**
  (stronger). This includes the clock-skew "outside", no-issuers, and the three
  never-existing-`kid` presentations in the rotation test.
- The audit reads were `recent(20)` over the whole server; on a shared server
  they are `about(<account>, 20)` in the same category, so the entry found is
  the case's own. The supersede check still asserts a `certificate.superseded`
  entry exists, now one about `moved`.
- The audited case asserts the detail names *its* job (`rustak-plugin-audited`)
  rather than `rustak-plugin-ais`.
- `Deployment::handshake` polls the registry for up to 5 s rather than 2 s.

## Isolation

Each shared server is local to one `#[test]` function; nothing is shared across
test functions, so libtest's order and parallelism cannot interleave them. Inside
a function the cases are concurrent by design, and each keeps to its own
accounts, services and callsigns. Verified: every changed binary three times each
at `--test-threads=1`, `2` and `16`, and every test alone with `--exact`, all
passing; then new and original copies side by side, repeatedly, at a load average
of 130–140: `workload_identity` 10/10 (original 10/10), `sidecar_enrolment` 20/20
(20/20), `sidecar_trust` 30/30 (30/30).

### Flakiness seen

Once, `sidecar_trust` failed at `--test-threads=16` while the machine was loaded:
the pinned case's 5-second wait for the stream to come up (`stream_support::EXPECT`)
expired. It did not recur in 55 further runs, nor did the original fail in 30
runs beside it, so I cannot say whether sharing the server made it likelier. The
wait bounds a hang, not a speed, so both sidecar suites now use a local 30-second
`START_UP` for their start-up waits; `EXPECT` itself is M10-08's.

## On a host ten times slower

No new upper bound on anything. The cases of a group take longer together
because they share one process's CPU, but none is timed against another. The
waits are hang guards: `START_UP` 30 s for a sidecar start (≈ 1.4 s here), 5 s
for a handshake to reach the registry (tens of milliseconds here), and
`await_serving`'s 20 s. `testing::cases` unit tests have no waits at all.

## Files

- `rustak-server/src/testing/cases.rs` — new: the case runner and its tests.
- `rustak-server/src/testing/mod.rs` — `pub mod cases`.
- `rustak-server/src/testing/workload.rs` — parallel `warm`; doc comment fix.
- `rustak-server/tests/workload_support/mod.rs` — new: the `Deployment` fixture.
- `rustak-server/tests/workload_identity.rs` — rewritten around the groups.
- `rustak-server/tests/sidecar_enrolment.rs` — two groups, `--check` serverless.
- `rustak-server/tests/sidecar_trust.rs` — one group.
- `docs/ci.md` — count, local timing note, fourth lever.
- `.claude/plan/status/M10-07-cheaper-identity-suites.md` — this note.

`stream_support/` is used but not changed.

## Exit checks

All run in this worktree, after the last source change:

1. `cargo fmt --check` — pass.
2. `cargo clippy --workspace --all-targets -- -D warnings` — pass (the first run
   caught a needless borrow in `workload_identity.rs`, fixed).
3. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` — pass.
4. `./scripts/check-file-length.sh` — pass.
5. `cargo test -p rustak-server` — pass: lib 2016 passed / 2 ignored (including
   the three `testing::cases` tests), every integration binary passed, doctests
   5 passed. Run while the machine was loaded by other agents' builds.

## Open

1. **The per-harness RSA key** (above): `stream_support::Harness` should adopt
   `testing::keys::JWT_SIGNING_KEY` as `TestServer` does — either by the harness
   building its context the way `TestServer` does, or by a `testing`-gated
   variant of `build_context` that calls `load_or_adopt`. Every `stream_*`,
   `sidecar_*`, `feed_sidecars` and `workload_identity` server pays it today.
   Not done here: both places belong to someone else this wave.
2. The coverage-instrumented cost of the four suites is unmeasured; CI's next
   samples will say what the local 3× in `workload_identity` is worth there.
