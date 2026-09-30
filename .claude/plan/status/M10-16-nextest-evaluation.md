# M10-16 — cargo-nextest: measured, and adopted for the `Test` job

**Recommendation: adopt in CI.** Under `-Cinstrument-coverage` — which is what
the `Test` job runs — nextest ran the workspace **4–5× faster** than
`cargo test` at the runner's four-way parallelism (339 s against 1,537 s,
medians of two), because tests sharing a process fight over its coverage
counters and nextest gives each test a process of its own. Projected for CI:
the job's 22–28 minutes become roughly **9–21**. Threshold used: adopt only if
the projected saving on the normal band is at least five minutes *and* the
lower bound of the projection is still a saving, since a pinned tool is a
permanent cost; a minute or two would not have been enough. Uninstrumented, at
four threads, the two runners are level, so local use is a convenience (1.8×
quicker on ten cores), not the reason.

The change is ready to land and **cannot be proven locally**: see
[What only the first CI run can show](#what-only-the-first-ci-run-can-show).

---

## Environment and method

- Local: 10-core Apple-silicon Mac, 32 GB, shared with other agents' builds.
  Every figure below is reported with the 1-minute load average at start and
  end of the run. Builds with `CARGO_BUILD_JOBS=4` and line-tables debug info,
  per the wave rules; test runs unrestricted except where "4 threads" is
  stated (`--test-threads 4` for `cargo test`, `-j 4` for nextest).
- Runner: GitHub's published table (checked 2026-09-29) gives a **public**
  repository's `ubuntu-latest` **4 vCPU, 16 GB RAM, 14 GB SSD**. `docs/ci.md`,
  the workflow comments and `rustak-server/src/testing/keys.rs` still say
  "two-vCPU" in places (see Open questions).
- nextest **0.9.146** (current release, 2026-09-21), installed with
  `cargo install --locked --root` into a scratch directory in the worktree.
  The installed **0.9.51 cannot run this workspace**: it finds zero test
  binaries and reports success ("Starting 0 tests across 0 binaries").
- grcov 0.10.7 from crates.io (CI's `setup-grcov` fetched 0.10.8), with the
  `llvm-tools-preview` component added to the local stable toolchain (it was
  missing; this is a change to `~/.rustup`, shared with other checkouts).
- Instrumented builds went to `target/cov` (`--target-dir`), so they did not
  thrash the plain `target/debug`.
- "A" = `cargo test --workspace --no-fail-fast` (includes doctests).
  "B" = `cargo nextest run --workspace --no-fail-fast` then
  `cargo test --workspace --doc`. Tests built first (`--no-run`); A and B
  interleaved.

## 1. Local wall time

| Run | A (cargo test) | B (nextest + doctests) | load at start → end |
|---|---:|---:|---|
| all cores, uninstrumented, pair 1 | 140.2 s | 83.8 s | 56 → 19 / 19 → 18 |
| pair 2 | 121.7 s | 71.6 s | 18 → 13 / 13 → 15 |
| pair 3 | 127.5 s | 70.3 s | 15 → 11 / 11 → 16 |
| **median** | **127.5 s** | **71.6 s** (−44%) | |
| 4 threads, uninstrumented, pair 1 | 149.7 s | 142.3 s | 16 → 5 / 5 → 7 |
| pair 2 | 134.2 s | 142.2 s | 7 → 4 / 4 → 10 |
| pair 3 | 134.5 s | 132.6 s | 10 → 5 / 5 → 8 |
| **median** | **134.5 s** | **142.2 s** (+6%, within noise) | |
| 4 threads, **instrumented**, pair 1 | 1708.0 s | 343.2 s | 13 → 4 / 8 → 4 |
| pair 2 | 1365.8 s | 335.5 s | 8 → 4 / 10 → 4 |
| **median of two** | **1536.9 s** | **339.4 s** (−78%) | |

CPU time (children's rusage) tells the same story: instrumented A spent
4,616–5,824 CPU-seconds, B 1,143–1,151; uninstrumented at four threads A spent
244–291, B 344–358 (B pays process start-up and per-process key generation).

Why uninstrumented four-thread runs are level: the sum of per-test times under
nextest is 518 s, i.e. ~130 s of work per slot, and the library's long tests
(one 13–16 s rate-limit flood, eight 10 s handshake waits — see §3) fill the
tail either way. A list-scheduling model over the JUnit per-test times predicts
130 s against the measured 122–130 s nextest phase, so the model is sound.

### The mechanism, measured directly

Same binary, same tests, one thread against four (`cargo test`, 2026-09-29):

| | 1 thread: wall / CPU | 4 threads: wall / CPU |
|---|---:|---:|
| `stream_session`, instrumented | 18.9 s / 15.0 s | 134.0 s / **492.9 s** |
| `stream_session`, uninstrumented | 6.4 s / 2.7 s | 2.2 s / 3.1 s |
| library `pki::`, instrumented | 88.9 s / 8.6 s | 59.7 s / **144.8 s** |
| library `pki::`, uninstrumented | 84.6 s / 4.3 s | 23.5 s / 9.6 s |

Instrumented, four threads cost **33×** (and 17×) the CPU of one for the same
work; uninstrumented they cost about the same. The coverage counters are
process-global, so threads running different tests in one process contend on
the same memory. An RSA-2048 generation alone in a process costs 0.9–1.8 s
instrumented (0.2–0.3 s without) — not the "minutes" `docs/ci.md` attributed
to instrumentation of the bignum inner loops. Those minutes were contention.
`docs/ci.md` is corrected.

## 2. Per binary: who gains and who loses

Instrumented, four threads. "cargo" is the binary's wall time in A; "nextest"
is that binary's per-test times scheduled alone on four slots (modelled from
JUnit; the model matched the measured whole-run times within 1%).

| Binary | cargo (run 1 / run 2) | nextest sum of tests | nextest on 4 slots |
|---|---:|---:|---:|
| `rustak_server` lib (2035) | 345.9 / 153.7 s | 592 s | 149 s |
| `workload_identity` (24) | 238.2 / 208.8 s | 133–135 s | 36–38 s |
| `stream_session` (11) | 163.9 / 117.4 s | 14–15 s | 4.1–4.6 s |
| `stream_routing` (12) | 143.9 / 107.5 s | 21 s | 6.7–6.8 s |
| `cloudtak_onboarding` (20) | 136.5 / 128.4 s | 32–41 s | 9–11 s |
| `marti_channels` (11) | 106.9 / 127.7 s | 19–22 s | 6–7 s |
| `feed_sidecars` (8) | 87.0 / 78.3 s | 19–20 s | 6–7 s |
| `sidecar_enrolment` (5) | 72.8 / 45.7 s | 11 s | 3.2–3.6 s |
| `stream_store` (9) | 66.1 / 70.9 s | 16–17 s | 4.5–4.8 s |
| `stream_idle` (5) | 52.5 / 54.7 s | 9–14 s | 5–6 s |
| `bootstrap` (3) | 46.8 / 40.8 s | 7–9 s | 3.0–3.4 s |
| `mission_dest` (11) | 44.0 / 34.0 s | 6–9 s | 2.5–2.9 s |
| **losers** | | | |
| `oidc_provider` (24) | 5.5 / 3.6 s | 54–59 s | 15–16 s |
| `oauth_flows` (31) | 5.3 / 3.9 s | 46 s | 13 s |
| `missions_flow` (15) | 4.3 / 1.9 s | 22–24 s | 7–9 s |
| `api_v1_map` (16) | 4.2 s | 26.5 s | 8.1 s |
| `enroll_flows` (18) | 8.9 / 6.7 s | 35 s | 10–11 s |
| `marti_cot`, `missions_extras`, `profiles_contract`, `sync_contract`, `missions_authz`, `marti_contract`, `api_v1_live` | 3–5 s each | 13–23 s each | 4.5–7 s each |

Uninstrumented at four threads the picture inverts mildly: almost everything
is 1.0–1.7× slower as a process per test, `oidc_provider` 2.25×.

**Suites built on in-process sharing** (every `LazyLock`/`OnceLock` whose cost
is now paid per test rather than per binary), on `main` today:

- `rustak-server/src/testing/keys.rs` `JWT_SIGNING_KEY` — an RSA-2048 key per
  process for every `TestServer`. This is the cost behind every loser above
  (~1.7 s per test instrumented).
- `rustak-server/src/testing/oidc.rs` `PROVIDER_KEY`, `UNADVERTISED_KEY` — two
  more RSA-2048 keys per process: `oidc_provider`, `enroll_oauth`,
  `oidc_channels`, `oauth_flows` (the worst losers).
- `rustak-server/src/testing/workload.rs` `PRIMARY`, `ROTATED`,
  `UNADVERTISED` — three RSA-2048 keys: `workload_identity`. It still gains
  (6×) because its contention cost was larger.
- `rustak-server/src/testing/authenticator/keys.rs` `RSA_KEY` — WebAuthn tests.
- `rustak-core/src/identity/password.rs` `DUMMY` (one argon2 hash per process),
  `rustak-server/src/identity/secret_cache.rs` `SHARED`,
  `rustak-server/src/profiles/package.rs` `CACHE` — cheap caches; no test
  depends on a warm one (§3).

No suite shares a *server* across tests on `main` today, so nothing breaks;
it only costs.

**M10-07's design under nextest.** M10-07 (worktree read, not integrated)
groups `workload_identity`'s cases into a few `#[test]`s that each boot one
deployment and run their cases concurrently through
`rustak_server::testing::cases::run`, with the issuer keys generated in
parallel by `warm()`. Sharing *inside* one test survives process-per-test, so
the design is compatible: nextest gets fewer, longer tests from that binary,
and each group pays the three issuer keys once instead of per case. Two things
to watch once both land: the longest group becomes the binary's floor on the
nextest schedule, and cases run concurrently inside one instrumented process
contend exactly as threads of `cargo test` did — so a group whose cases run on
several threads may be slower per case than it looks locally uninstrumented.

**Mixed setup considered and rejected.** Running the losers under `cargo test`
would save about half a minute on four slots (modelled) while doubling the
coverage plumbing and the places a test can hide. A cheaper fix, if the half
minute ever matters: a pre-generated test signing key read from a fixture (our
own), so no process generates one. Not done here — it is outside this brief's
files and the tests that are *about* key generation must keep generating.

## 3. Correctness under nextest

Twelve uninstrumented full-workspace nextest runs in a row (default
parallelism, no retries), plus three instrumented ones (two timed, one
dry-run of the CI step):

- Run 2 failed one test:
  `rustak-client feed::symbol::tests::a_feed_that_says_nothing_leaves_the_event_alone`.
  It built two events a moment apart and compared them; each carries
  `time`/`start`/`stale` stamped "now", so whenever the millisecond ticked
  between the two builds they differed (`…50.916Z` against `…50.917Z` in the
  failure). **Not a nextest problem** — it can fail identically under
  `cargo test`; the load on the machine made the tick more likely. Fixed in
  test code: build once, compare with a clone. Host ten times slower: no
  effect; the test no longer reads the clock twice.
- Runs 3–12 (**ten in a row, after the fix**): 3837 passed, 2 skipped (the
  `#[ignore]`d ones `cargo test` also skips), 0 failed, no SLOW, LEAK or
  TIMEOUT. The three instrumented runs: 3837/3837.
- No test depended on another test running first in the same process, on
  ordering, on a fixed port or on the working directory (nextest runs each
  binary from its package root, as `cargo test` does).

**Other finding (pre-existing, both runners, not fixed):** eight library tests
in `rustak-server/src/pki/tls/mod.rs` and `…/tls/resolver.rs` — every
*refused*-handshake case — take **10.05–10.12 s** each, which is the test
helper's own `TIMEOUT` (10 s socket read timeout). The refusal is detected by
waiting out the hung-wait timeout rather than by the connection closing,
uninstrumented and alone (`cargo test … a_client_with_no_certificate…`:
10.14 s). That is a timeout paid on the success path, against the wave's rule.
~80 s of test time per run; worth its own small brief.

## 4. Coverage

| | profraw files | on disk | grcov | lines found | lines hit |
|---|---:|---:|---:|---:|---:|
| A, run 1 (CI's command, `LLVM_PROFILE_FILE` unset) | 387 | 461 MB | 81 s | 95,580 | 91,131 |
| A, run 2 | 387 | 461 MB | 79 s | 95,580 | 91,132 |
| B, run 1 (`LLVM_PROFILE_FILE=<dir>/%m.profraw`) | 89 | 339 MB | 71 s | 95,580 | 91,134 |
| B, run 2 | 89 | 339 MB | 71 s | 95,580 | 91,135 |
| dry run of the new CI steps, `grcov .` as CI | 89 | 339 MB | 78 s | 95,580 | 91,136 |

- **With the default profile-file pattern nextest would break the job.** Each
  process writes `default_<sig>_<pid>.profraw`; one `rustak_server` library
  process writes 13.9 MB, and the whole run would write **~3,800 files,
  ~35 GB** (each binary's per-process size, measured, times its test count) —
  more than the runner's 14 GB of disk.
- `%m` without `%p` enables LLVM's online merging: every process of one binary
  merges its counters into one file on exit, under a file lock. Measured: 290
  processes of the library → one 13.9 MB file; whole run → 89 files, fewer
  and smaller than `cargo test`'s.
- Same lines found; lines hit within five (and higher, not lower — timing-
  dependent branches). grcov found the files in a `coverage/` directory under
  the workspace root with the unchanged `grcov .` command (verified twice: in
  `target/coverage`, then moved to `coverage/`).
- **`cargo-llvm-cov` is not needed.**

## 5. Cost in CI

- **Install**: the trunk/cross pattern exactly — `NEXTEST_VERSION` 0.9.146 and
  `NEXTEST_SHA256` in `env:`; `actions/cache@v6` on `~/.cargo/bin/cargo-nextest`
  keyed `nextest-<version>-<os>`; on a miss, one `curl --fail --location
  --retry 5 --retry-all-errors --retry-delay 5` of
  `cargo-nextest-0.9.146-x86_64-unknown-linux-gnu.tar.gz` (12,055,550 bytes),
  `sha256sum --check --strict`, extract the single `cargo-nextest` binary. No
  `cargo-binstall`.
- **Checksum source**: GitHub's recorded asset digest
  (`682c21b7…695428`, from the releases API). I did not download the asset;
  the first CI run exercises it.
- **Time**: not measurable locally. Comparable steps in the same workflow:
  `Cache Trunk` restores in ~1 s, and `cargo install grcov` (a release-asset
  download of similar size) takes 1–2 s. Expect ~1 s cached, a few seconds
  cold.
- **Cache**: one ~12 MB entry per OS (plus a PR-scoped copy on a PR's cache
  miss) against the 10 GB quota — negligible, and it sits in front of the
  download like trunk's.
- **Retries stay off** (`retries = 0` in both profiles, with a comment).
- **What it gives for free**: a duration for every test in the log
  (`status-level = "pass"`), a SLOW marker past 120 s, a hung test terminated
  and *named* after ten minutes instead of a job stuck until its 90-minute
  bound, every failure repeated at the end. JUnit is available but not
  enabled: nothing reads it yet (codecov's test-results upload could).

## 6. Estimate for the `Test` job

From the six normal `main` runs of 2026-09-28/29 (36461724163, 36462467903,
36462636086, 36464093390, 36609485358, 36626529401) plus one slow host
(36462850383):

| | normal band | slow host |
|---|---|---|
| job | 23m18s – 28m24s | 53m07s |
| `Run tests` step | 21m01s – 26m12s | 50m42s |
| compile part | 197 – 270 s | 263 s |
| binaries (sum of wall per binary) | 1,064 – 1,313 s | 2,778 s |
| grcov | 65 – 76 s | 72 s |

The shape matches local instrumented `cargo test` (library, then
`workload_identity`, then the stream/Marti suites, in the same order), at
roughly 1.3× the local figures.

**Projection for the running part under nextest: 210–860 s (4–14 minutes)**,
against 1,064–1,313 s today, so **the job at about 9–21 minutes** instead of
23–28. Assumptions:

- Low end: the runner contends as much as the Mac did, so the local ratio
  carries over (0.20–0.25 × 1,064–1,313 s = 210–330 s).
- High end: none of the contention ratio carries over and only nextest's
  per-test cost does, on cores 1.5–2.5× slower than the Mac's
  (336–343 s × 1.5–2.5 = 504–858 s).
- The runner does contend: `stream_session` takes 52–101 s there for eleven
  tests needing 15 s of CPU here in separate processes; no difference in core
  speed explains ten-fold.
- Compile, grcov (±10 s) and setup are unchanged; the doctest step adds ~10 s
  (it was inside `cargo test` before); the nextest install ~1 s cached.
- A slow host may gain proportionally more (contention is what a noisy
  neighbour amplifies) — a hypothesis, not a measurement.

The `timeout-minutes: 90` is **left as it is**: lower it only once two runs
show the new band.

## What changed

- `.config/nextest.toml` (new): `nextest-version = { required = "0.9.146" }`;
  `default` profile (no retries, 60 s SLOW / 10-minute terminate); `ci` profile
  (no retries, `fail-fast = false`, 120 s SLOW / terminate after 5 periods =
  10 minutes, a status line per test, slow tests and failures repeated at the
  end, no JUnit).
- `.github/workflows/rust.yml`, `Test` job only (plus two `env:` pins):
  `Cache nextest` + `Install nextest`; `Run tests` is now
  `cargo nextest run --workspace --profile ci` with
  `LLVM_PROFILE_FILE=${{ github.workspace }}/coverage/%m.profraw`; new
  `Run doctests` (`cargo test --workspace --doc --no-fail-fast`, same env,
  runs on `success() || failure()`); grcov and codecov unchanged.
- `docs/ci.md`: job-graph line; the "Keeping the test job inside its timeout"
  explanation corrected (contention, four vCPUs); new "Why the tests run under
  nextest" section with the figures, projection and what to compare the first
  run against; the local test commands. M10-07 edits counts and timings
  higher in the same file — expect a textual merge, not a conflict of fact.
- `CONTRIBUTING.md`: the local test commands; a "Running the tests with
  nextest" section (install, the zero-tests trap of old versions, the ci
  profile, doctests, per-test `LazyLock` cost, no retries).
- `rustak-client/src/feed/symbol.rs`: the clock-tick flake fix above (test
  module only).

## What was verified, and what only the first CI run can show

Verified locally: the exact new step commands (`cargo nextest run --workspace
--profile ci` then `cargo test --workspace --doc --no-fail-fast`, instrumented,
`LLVM_PROFILE_FILE` set as in the workflow, apart from `--target-dir`), 3837
passed; `grcov .` with CI's arguments over the result; the config parsed by
nextest 0.9.146 (`show-config version`: ok); `actionlint` reports nothing in
the `Test` job (its findings are all pre-existing, in other jobs).

Only CI can show: the asset download and checksum; the cache round trip; the
online-merge file locking on the runner's filesystem (standard Linux `flock`,
expected fine); the real wall time on x86. **Compare the first `main` run
against** `Run tests` 21–26 min (expect 4–14 min plus ~3.5–4.5 min compile),
the codecov line totals of the previous `main` commit (same lines found, hit
within noise), and the per-test lines against §2.

## Exit checks (this worktree)

1. `cargo fmt --check` — pass.
2. `cargo clippy --workspace --all-targets -- -D warnings` — pass.
3. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` — pass.
4. `./scripts/check-file-length.sh` — pass.
5. `cargo test -p rustak-client` — pass. Workspace both ways after the fix:
   `cargo test` instrumented ×2 (pass), nextest ten uninstrumented + three
   instrumented (pass).

## Open questions

- **The first CI run is the real measurement.** If the job lands above ~18
  minutes, the contention is smaller on x86 than here; if below ~10, lower
  `timeout-minutes` once a second run agrees.
- **"Two-vCPU" is stale** in `docs/ci.md`'s sizing history, the `Test` job's
  timeout comment, `testing/keys.rs` and `testing/workload.rs` doc comments.
  I corrected it only in the section I rewrote; the history paragraphs are
  M10-07's to touch this wave.
- **The 10 s refused-handshake waits** in `pki::tls` (§3) — a small brief.
- **Per-process key generation** is what the losers pay; a checked-in test key
  fixture would remove it if it ever matters.
- JUnit + codecov test results would give per-test history across runs; not
  enabled because nothing reads it yet.
- `llvm-tools-preview` was added to the local stable toolchain in `~/.rustup`
  for grcov; remove it if unwanted (`rustup component remove
  llvm-tools-preview`).
