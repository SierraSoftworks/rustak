# CI/CD

rustak's pipeline is a direct port of [`automate`](https://github.com/SierraSoftworks/automate)'s
GitHub Actions setup, adapted for a multi-crate workspace with six release
binaries (`rustak-server` → `rustak`, and the `rustak-plugin-example`,
`rustak-plugin-ais`, `rustak-plugin-adsb`, `rustak-plugin-esb` and `rustak-plugin-firms` sidecars)
instead of one.
See `.claude/plan/design/01-foundations-storage-ci.md` §7 for the design this
implements and `.claude/plan/research/01-automate-architecture.md` §1 for what
was ported from where.

## Job graph (`.github/workflows/rust.yml`)

Runs on every push to `main`, every pull request, and every published release.

```
deduplicate ──┬─ version ─────────────────────────────────────┐
              ├─ lint    (fmt --check, clippy -D warnings, check-file-length.sh, cargo doc -D warnings)
              ├─ test    (cargo nextest run + doctests, coverage → grcov → codecov)
              ├─ ui      (lints + tests rustak-ui; trunk build → ui-dist-e2e; trunk build --release → ui-dist)
              ├─ e2e     (needs ui; cargo build -p rustak-server; Playwright)
              ├─ interop-node-tak  (needs ui; @tak-ps/node-tak contract suite)
              └─ build   (needs version, ui; crate × target matrix, 25 jobs) ─┬─ ci (aggregator, always())
                                                                                ├─ docker-build  (per crate × platform)
                                                                                │     └─ docker-publish (per crate, manifest list)
                                                                                └─ tap (release only)
```

- **`deduplicate`** caches a success marker keyed on the PR's merge-tree hash
  (`git rev-parse HEAD^{tree}`), so re-running CI on a merge tree that already
  passed is a cache hit instead of a rebuild. Every other job (except `ci`
  itself) is skipped on a cache hit.
- **`version`** rewrites the single `version = "…"` line under
  `[workspace.package]` in the root `Cargo.toml` from the release tag (every
  crate inherits it — unlike automate, which rewrites one package's own
  manifest, rustak only ever touches this one line) and uploads the file as
  the `cargofile` artifact; `build` downloads it back to the repository root
  before compiling.
- **`lint`** and **`test`** run once, workspace-wide. `rustak-ui` is excluded
  from the workspace (it targets `wasm32-unknown-unknown`), so it is not
  covered here — it is linted *and tested* inside the `ui` job instead, where
  the wasm32 toolchain and Trunk are already installed. `test` runs under
  `-Cinstrument-coverage`; see [Keeping the test job inside its
  timeout](#keeping-the-test-job-inside-its-timeout) for what that costs and
  what pays for it.
- **Every job carries a `timeout-minutes`** — 90 for `test`, 45 for `build`,
  30 for the four that compile or image something big (`ui`, `e2e`,
  `interop-node-tak`, `docker-build`), 20 for `lint`, `docker-publish` and
  `tap`, 10 for the bookkeeping jobs (`deduplicate`, `version`, `ci`).
  GitHub's own default is six hours, which is not a guard rail — a `test` job
  that had stopped finishing at all ran until it was cancelled by hand. The
  numbers are deliberately loose: a cold cache is the slow case and none of
  these should come near them, so a job that *does* hit its timeout is a bug
  report rather than a number to raise. Loose is not unbounded, though: the
  nightly `interop-eud` job carried 90 minutes against a ~15-minute envelope,
  and when a crashed runner left an orphaned server holding the step's stdout
  open the job sat idle for 82 of them before anyone was told. It is 30 now.

  **`test` is the one exception to "a timeout is a bug report", and it is worth
  knowing why.** The *same* tests cost two to four times more on a slow host —
  `rustak_server`'s library measured 102 s on one run and 341 s on another,
  `stream_routing` 81 s against 309 s. Nothing in the repository changes
  between those; GitHub's two-vCPU hosts simply vary.

  The number has moved three times, and the history is the argument. **Thirty**
  was set against a normal band of 11–16 minutes — the good case dressed up as a
  bound — and on 2026-09-19 the multiplier met a suite that had also grown, so
  the job was cancelled at exactly 30 with four binaries still to run.
  **Forty-five** was set against 15–17. On 2026-09-20 the M9 wave added 184
  tests across four landings and the band became 16–20 minutes, which puts the
  bad case at 20 × 2.8 ≈ 56 — past 45 again. So `test` went to **60**, raised
  deliberately rather than discovered from a cancelled release: `build` needs
  `ui` and `test`, and v0.0.2 published nothing at all after a single job died.

  **Then the band moved again, and the number moved with it.** Fifteen `main`
  runs between 2026-09-20 23:34Z and 2026-09-22 01:34Z: thirteen took
  22m05s–28m35s, one 17m22s, and one slow host 41m30s. So the normal band is
  **22–28 minutes**, not 16–20. The per-binary shape says it is the suite and
  not the hosts: the binaries' own times summed to 660–870 s on 2026-09-20 and
  sum to 1110–1250 s now, and the difference is in what landed —
  `workload_identity` (new; 24 tests since M9-14, 170–215 s, the most
  expensive binary in the job), `sidecar_enrolment` (2 tests → 5, 12 s → 40–60 s),
  `hostile_server_name` and `sidecar_trust` (new, 10–25 s each). By the same
  arithmetic the bad case is 28 × 2.8 ≈ 78 — past 60 — so `test` now carries
  **90**: 78, with margin. The worst whole job actually seen against this band
  is that 41m30s, so the margin is generous, and deliberately: with images gated
  on the test suite a cancelled `test` blocks every image, while a timeout is
  cheap and reversible. The other lever is the suite's own cost, and M10-07
  pulled it: `workload_identity`'s 24 tests each booted a server and enrolled,
  and now its cases share a server per group — seven servers, not 24 — through
  `rustak_server::testing::cases`, which still reports every failing case by
  name. `sidecar_enrolment` went from four servers to two and `sidecar_trust`
  from two to one. **Local figures, not CI's** (an uninstrumented debug build
  on a ten-core laptop that other builds were loading, CPU time of the whole
  binary, two samples each): `workload_identity` 21–23 s of CPU → 7–8 s, and
  16–17 s of wall at `--test-threads=2` → 8–9 s; `sidecar_enrolment` 3.4–4.4 s
  → 2.0 s; `sidecar_trust` 1.8–2.0 s → 1.0–1.2 s. What that buys under
  coverage on a two-vCPU runner has not been measured yet; the band and this
  number should both come back down, and the next steward's samples say by how
  much.

  **A `test` job that hits 90 is a bug report again** — and the first thing to
  check is the per-binary shape, not the total. Cost landing in the binaries a
  change touched is growth; cost spread evenly across binaries nothing touched
  is the host. Reading a single slow run as a trend has produced two wrong
  conclusions in this repository already, so take at least two samples and say
  so when you have not.
- **`ui`** installs `trunk` pinned to **0.21.14** — downloaded from the project's
  own GitHub release and checked against a SHA-256 pinned beside the version in
  the workflow's `env:` block. 0.22 was still beta at the time this pipeline was
  written; bump the pin deliberately, not via dependabot, which cannot see
  release-asset downloads.
  It builds a **debug** bundle (for `e2e`'s `?demo` fixtures, which are
  compiled out of release) and a **release** bundle (for the `build` matrix to
  embed).

  Both builds run `rustak-ui/scripts/vendor.mjs` as a Trunk hook, which copies
  the map page's JavaScript (MapLibre GL and milsymbol) into
  `dist/vendor`, and derives the two symbol catalogues its pickers search
  (`dist/vendor/symbology/`, from `mil-std-2525`), all from
  the versions locked in `rustak-ui/package-lock.json` — `npm ci` on a cold
  runner, a file copy after that. It uses the Node the runner image ships;
  those two pins *are* dependabot's to bump (`npm`, `/rustak-ui`).

  It also **runs `rustak-ui`'s unit tests**, on the *host* target rather than on
  wasm32. Until M7-04 it only lint-checked them: `cargo clippy --all-targets`
  type-checks a test without running it, so an assertion that would fail was not
  a failing build (found by M2-14). The host target needs no browser, no wasm
  test runner and no tool to install — `wasm-bindgen`'s bindings compile there
  and panic only when called, and nothing in this suite calls one, so all 51
  tests run and none is excluded. A test that does need a DOM gates itself
  behind `#[cfg(target_arch = "wasm32")]`: the clippy step still compiles it and
  this step skips it. The cost is one more dependency graph compiled for the
  host — about a minute cold, seconds once `Swatinem/rust-cache` holds it,
  against a job that takes ~1.5 minutes today.
- **`e2e`** downloads the debug UI bundle, builds `rustak-server` and runs the
  Playwright suite in `e2e/`. See `e2e/README.md`.
- **`interop-node-tak`** downloads the release UI bundle, builds
  `rustak-server` and runs the `@tak-ps/node-tak` contract suite in
  `interop/node-tak` against a throwaway server it starts and bootstraps
  itself. A real gate since M2-03 landed `/oauth/token` and
  `/Marti/api/tls/*`: login, enrollment and the mutually authenticated
  `GET /Marti/api/version` all run. Scenarios whose endpoints are still to come
  report as skips naming the brief that will serve them, and begin running on
  their own when it lands. See `interop/node-tak/README.md`.
- **`build`** is a `crate × target` matrix: `{rustak-server → rustak,
  rustak-plugin-example, rustak-plugin-ais, rustak-plugin-adsb,
  rustak-plugin-esb, rustak-plugin-firms}` ×
  `{x86_64-unknown-linux-musl, aarch64-unknown-linux-musl (cross),
  x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-msvc}` — 30
  jobs. A new plugin crate is three lines: one in each of this matrix and the
  two Docker ones below. **No `protoc` is installed anywhere in this
  workflow**: `rustak-cot` builds its protobuf definitions with `protox`, a
  pure-Rust `protoc` substitute, so neither the native runners nor the `cross`
  Docker image need one. Artifacts and (on a release) release assets are named
  `<bin>-<os>-<arch>[.exe]`.

  The aarch64 leg's `cross` is pinned to **0.2.5** (`cargo binstall cross@0.2.5`)
  — bump the pin deliberately, like `trunk`'s, and for the same reason:
  dependabot cannot see cargo-binstall installs, so an unpinned `cross` would
  let a new upstream release change how that target is built with no commit
  saying so. The binary is **cached** on that version, which is the other half
  of why it is pinned: an unpinned cache would be worse than none, restoring
  whatever was current the day it was first stored, forever. Five of the twenty-five
  jobs use `cross` and share one cache key, so on a cold key four of them log
  `Cache already exists` — a warning, not a failure.
- **`ci`** is the required check: `always()`-gated, it fails the run if any
  dependency did not succeed, then saves the merge-tree success marker for
  `deduplicate` to find next time. **`tap` is one of those dependencies**, and
  is the only one whose *skip* is the normal outcome — it selects
  `github.event_name == 'release'`, so `ci` accepts `success` or `skipped` from
  it and fails on anything else. It was added after the v0.0.1 release
  published ten assets and four image tags, failed to write the Homebrew
  formula, and still reported a green `CI`: a release that did not publish the
  formula is not a successful release. Nothing depends on `ci`, so waiting for
  `tap` delays only the aggregator and never the images.
- **`docker-build`**/**`docker-publish`** build and publish one multi-arch
  (`linux/amd64` + `linux/arm64`) image per crate to
  `ghcr.io/sierrasoftworks/<bin>` — `ghcr.io/sierrasoftworks/rustak`,
  `…/rustak-plugin-example`, `…/rustak-plugin-ais`, `…/rustak-plugin-adsb`,
  `…/rustak-plugin-esb` and `…/rustak-plugin-firms`.
  Unlike automate (which only publishes on a GitHub release), this also runs on
  every push to `main`,
  because rustak's M0 exit criterion is a multi-arch
  `ghcr.io/sierrasoftworks/rustak:latest` on a green `main`.

  **`docker-build` needs `lint`, `test`, `e2e` and `interop-node-tak` as well
  as `build`, so `:latest` means "passed CI" rather than "compiled".** It used
  to need `build` alone, which was fine while nothing pulled `:latest`
  automatically — a deployment that does cannot tell a green run from a red one
  otherwise, because `build` only proves the crates compile. The correctness
  jobs are listed by name rather than depending on the `ci` aggregator: `ci`
  also needs `tap`, and a Homebrew formula that failed to publish should not
  stop images reaching a deployment (v0.0.1 published its images with a red
  `tap`, which was the right outcome). `ui` is not listed because `build`,
  `e2e` and `interop-node-tak` all need it already.

  **The trade-off is delay.** Images now publish after `test` rather than
  alongside it — 22–28 minutes on a normal runner, and as long as `test`'s
  90-minute bound allows on a slow one. A push whose images are wanted sooner
  has to wait; that is the price of the tag meaning something.
  **Floating tags move forward only.** On a push to `main`, `docker-publish`
  reads `org.opencontainers.image.revision` off the current `:latest` and moves
  `:latest` and `:main` **only if that revision is an ancestor of the commit
  being published** (`git merge-base --is-ancestor`); otherwise it logs a
  `::notice::` and leaves them. The per-commit `:sha-<full sha>` tag is pushed
  either way, so every build stays addressable. Release events are unaffected —
  version tags are the point of a release, not a race.

  Why it exists: two pushes to `main` publish concurrently and **race** for
  `:latest`, and the one that finishes *second* wins the tag regardless of which
  commit is newer. On 2026-09-22 a fix for a three-day production outage and the
  commit stacked on top of it were in flight together; whichever landed second
  would have decided what the deployment pulled.

  Why ancestry and not "is this the tip of `main`": images are gated on the test
  suite, so a newer commit that fails publishes nothing. Under a tip test,
  `:latest` would then be pinned behind an older *good* build that had correctly
  declined to move it, and nothing would ever move it again.

  If the revision cannot be read, or names a commit not in the checkout, the
  tags are **not** moved and the job says so with a `::warning::` — a publish
  that cannot prove it is newer does not get the benefit of the doubt. That is
  also why the job checks out with `fetch-depth: 0`: a shallow clone cannot
  answer an ancestry question, and a wrong answer here silently refuses to
  publish.

- **`tap`** updates the `SierraSoftworks` Homebrew tap with the `rustak`
  formula (aliased as `major`/`minor`) on a published release.

## Nightly interop (`.github/workflows/nightly.yml`)

New relative to automate — plan.md's "Interop in CI" table calls for
`interop/cloudtak` and `interop/eud` to run nightly (plus on demand via
`workflow_dispatch`) because they are heavy (a full docker-compose stack, or an
ATAK-side harness). The workflow has two schedules and three jobs; only one
job is active today. See [`docs/interop.md`](interop.md) for what each suite
actually covers — this section only describes the workflow's shape.

```
schedule: "17 4 * * *"  (nightly) ──┬─ interop-cloudtak   active
                                    └─ interop-eud        active
schedule: "17 3 * * 1"  (weekly) ──── interop-eud-image   active
workflow_dispatch                ──── all three            active (manual)
pull_request + label run-cloudtak ─── interop-cloudtak     active
```

**Both crons are deliberately at :17 rather than on the hour.** GitHub queues
every repository's top-of-the-hour schedules together and delays or drops them
under that load. Measured here on 2026-09-19: the `0 0 * * *` security audit ran
**1h57m** late and the `0 4 * * *` nightly ran **4h19m** late, arriving in the
middle of the next working day rather than before it. A minute nobody else picks
costs nothing, and is GitHub's own documented advice.

Each job selects its own cron with `github.event.schedule`, so **a cron and the
`if:` that names it must change together** — there are three such expressions in
`nightly.yml`, and a mismatch stops a job selecting itself silently rather than
failing.

- **`interop-eud-image`** — **active.** Builds `interop/eud/Dockerfile`
  (ATAK's own `commoncommo` networking core and its stock `commotest` CLI,
  compiled from a pinned `atak-civ` commit — nothing GPL vendored into this
  repository) and pushes it to
  `ghcr.io/sierrasoftworks/rustak-interop-commoncommo`, tagged
  `atak-civ-<upstream sha>` (full and short) and `latest`, then smoke-tests
  the published image by asserting `commotest -h` lists its expected command
  set. Runs on the Monday 03:00 UTC schedule and on `workflow_dispatch` — not
  on the nightly 04:00 UTC schedule, since a cold build is ~25–45 minutes and
  the result only changes when the upstream pin or the build recipe does; the
  scenario job below consumes the published tag and never rebuilds it. Needs
  `packages: write`, which the default `GITHUB_TOKEN` already has (see
  **Required repository secrets and variables** below) — no new secret
  required. See `.claude/plan/status/M1-00-eud-interop-harness-exploration.md`
  and `.claude/plan/status/M1-04-interop-eud-image.md`.
- **`interop-eud`** — `if: false` placeholder for the scenario suite that
  drives the image above against a live rustak (enrollment, TLS, TAK
  Protocol v1 negotiation, ping/pong, SA/chat routing, mission-package
  transfer), asserting on `commotest`'s own log output. Lands in M2, once
  rustak exposes the Basic-auth enrollment listener the harness needs.
- **`interop-cloudtak`** — `if: false` placeholder for CloudTAK's own
  docker-compose stack driven through its REST API. Lands in M4 with the
  mission API.

`interop/rust` (the `rustak-client` fake-EUD suites) is **not** a separate
workflow — it is part of the workspace test run in the `test` job, since
those suites live under `rustak-server/tests/stream_*.rs`.

## Keeping the test job inside its timeout

The `test` job compiles the whole workspace with `RUSTFLAGS=-Cinstrument-coverage`
and runs the lot — about 3,800 tests, 2,000 of them in `rustak-server`'s library
target — on a GitHub-hosted `ubuntu-latest` runner, which for a public
repository has **four** vCPUs (GitHub's published runner table, checked
2026-09-29; older notes in this repository say two). Instrumentation applies to
*every* crate in the graph, dependencies included — there is no stable
per-package rustflag — so the arithmetic-heavy dependencies (`rsa`,
`num-bigint-dig`, `argon2`, `blake2`) run with a counter update in their
innermost loops. `grcov` discards everything outside the workspace at *report*
time, but that is after the cost has been paid.

**Most of that cost was not instrumentation; it was tests sharing a process.**
The coverage counters are process-global, so tests running on several threads
of one process update the same memory and fight over it. Measured on
2026-09-29 (instrumented, same machine, same tests): `stream_session` spent
**15 s of CPU on one thread and 493 s on four**, and the `pki::` tests of the
library 9 s against 145 s; uninstrumented, the same pairs cost the same either
way. An RSA-2048 generation in a process of its own costs 1–2 s instrumented
(0.2–0.3 s without), not the minutes it cost inside `cargo test`. That is why
the job once stopped finishing at all, with individual tests reported as "has
been running for over 60 seconds" — and why the tests now run under
[nextest](#why-the-tests-run-under-nextest), one process each.

Four things in test-only code kept it inside its bound before that, and still
help:

1. **One set of test keys per build, shared by every test process.**
   `rustak-server/src/testing/keys.rs` makes each RSA-2048 key the helpers
   need — the token signing key, the identity provider's two, the
   orchestrator's three, the software authenticator's one — the first time any
   process asks for it, and leaves it in `target/<profile>/rustak-test-keys/`
   for every later process to read (see
   [Test keys shared across processes](#test-keys-shared-across-processes)).
   `TestServer` adopts the signing key through `JwtIssuer::load_or_adopt`, and
   so does the stream harness, before `build_context` would generate one.
   Databases, data directories, secret stores and content stores stay per
   test, so isolation is unchanged; the tests that are *about* key generation
   (rotation, "a token signed by another server", a first start) still
   generate.
2. **A cheaper argon2id cost in tests.** `rustak_core::identity::password::use_testing_params`
   switches the process to `Params::TESTING` (m = 8 MiB, t = 1) and `TestServer`
   calls it. It is compiled out without the `testing` feature, no deployment can
   reach it, and verification is unaffected because a PHC string carries the
   cost its hash was made at.
3. **`opt-level = 3` for the crypto dependencies** (`[profile.dev.package.*]` in
   the workspace `Cargo.toml`: `rsa`, `num-bigint-dig`, `argon2`, `blake2`).
   Dependencies of a test target are built with the `dev` profile, so these
   apply to `cargo test`.
4. **One server per group of cases, not per assertion**, in the integration
   suites where a server is most of the cost (`workload_identity`,
   `sidecar_enrolment`, `sidecar_trust`). Cases that cannot see one another —
   distinct accounts, distinct service names — run concurrently against one
   deployment through `rustak_server::testing::cases::run`, which reports each
   failing case by name; a test that changes the server's configuration keeps
   a server of its own.

The first of these was once per *process*, and under nextest a process holds
one test, so for the first two `main` runs under nextest every test that built
a server generated its own keys — the cost the binaries that got slower under
nextest paid (below). M10-17 made it once per build.

If the job ever creeps back towards its timeout, measure before changing
anything: the `Run tests` log has one line per test with its duration, and
`cargo nextest run -p rustak-server --profile ci` with and without
`RUSTFLAGS=-Cinstrument-coverage` gives the instrumentation multiplier directly;
`-E 'binary(<suite>)'` narrows it to one suite.

Instrumenting only the workspace is the obvious fourth lever and it is not
available on stable: `-Cinstrument-coverage` is a per-*invocation* flag and
applies to every crate that invocation builds, and the per-package equivalent
(`[profile.dev.package.<dep>] rustflags`) is nightly-only behind
`-Zprofile-rustflags`. `cargo llvm-cov` does not change that — it sets the same
flag through `RUSTFLAGS` and filters at report time, as `grcov` already does.
So the levers are the four above and process isolation; dropping coverage is
not one, because codecov is a gate.

### Why the tests run under nextest

Adopted 2026-09-29 (brief M10-16; the full figures are in
`.claude/plan/status/M10-16-nextest-evaluation.md`). `cargo test` runs one
binary at a time and that binary's tests on threads of one process; nextest
runs every test in a process of its own, across binaries at once.

**Measured locally**, a 10-core machine shared with other builds, every run
reported, tests already built:

| | `cargo test` | nextest + doctests |
|---|---|---|
| Uninstrumented, all cores | 140, 122, 128 s (median 128) | 84, 72, 70 s (median 72) |
| Uninstrumented, 4 threads | 150, 134, 135 s (median 135) | 142, 142, 133 s (median 142) |
| **Instrumented, 4 threads** | **1708, 1366 s** | **343, 336 s** |

Uninstrumented at the runner's four threads the two are level — process start
costs about what cross-binary scheduling saves. Instrumented, which is what CI
runs, nextest is **four to five times faster**, and the reason is the contention
described above, not scheduling.

**Measured in CI.** The first two `main` runs under nextest, against 21–26
minutes of `Run tests` for `cargo test` on the six normal runs before them:

| Run | `Run tests` | of which compiling | running (3,866 tests) | test time summed | `Run doctests` | `grcov` |
|---|---|---|---|---|---|---|
| 36648789737 (`03b09f0e`) | **18m17s** | 3m30s | 881 s | 3,464 s | 55 s | 69 s |
| 36648792932 (`2eeb9915`), a slow host | 45m05s | 3m40s | 2,479 s | 9,796 s | 52 s | 67 s |

The first job took 20m56s, about four and a half minutes less than `cargo test`
— the pessimistic end of M10-16's projection of 9–21 minutes. The second ran
every test about 2.8 times slower on the same code (`2eeb9915` changes only
plan documents; the per-binary shape is the same), which is the slow-host band this job has always had (53
minutes once under `cargo test`), not a regression. Four slots were busy
throughout (3,464 s ≈ 4 × 866 s), so what was left was CPU, and the first
run's log said where:

| Binary | Tests | Summed time (run 1 / run 2) | Per test (run 1) |
|---|---|---|---|
| `rustak-server` (library) | 2,056 | 1,556 / 4,683 s | 0.76 s |
| `oidc_provider` | 24 | 192 / 540 s | 8.0 s |
| `workload_identity` | 7 | 150 / 358 s | 21 s |
| `cloudtak_onboarding` | 20 | 134 / 442 s | 6.7 s |
| `oauth_flows` | 31 | 131 / 454 s | 4.2 s |
| `api_v1_map` | 16 | 95 / 171 s | 5.9 s |

248 tests took five seconds or more. Most of it was every test process
generating the RSA keys a `cargo test` binary used to generate once — which is
what [the next section](#test-keys-shared-across-processes) removed — and one
library test, `a_flood_of_lockouts_does_not_make_every_later_check_pay_for_it`,
took 42 s on its own. Leave `timeout-minutes` at 90 until two normal runs after
M10-17 agree, then bring it down with the band.

### Test keys shared across processes

M10-17, 2026-09-30. Every RSA key the test helpers use — the token signing key
`TestServer` and the stream harness adopt, the identity provider's advertised
and forged keys, the orchestrator's three, the software authenticator's RS256
key — is now `rustak_server::testing::keys::rsa("<name>")`. The first process
to ask for a name generates the key and writes it to
`target/<profile>/rustak-test-keys/<name>.pk8` (a temporary name, then a
rename, so a reader sees a whole file or none); every other process reads and
parses it. On a cold build the first process takes `<name>.lock` and the rest
wait for its file rather than each generating the same key; a lock older than
a minute is a dead process's and is taken over. A file that does not parse is
replaced. Why beside the binaries and not in `CARGO_TARGET_TMPDIR`: Cargo sets
that only when compiling integration tests, and the keys are made in the
library. The keys protect nothing, live and die with `target/`, are never
checked in, and nothing a deployment runs reads them — the module doc of
`testing/keys.rs` has the threat model.

`stream_support::Harness` (and so `feed_support` and `workload_support`) also
stops generating a signing key per harness: `testing::keys::adopt_signing_key`
stores the shared one in the harness's database before `build_context` opens
it, so start-up loads a key instead of making one. Databases, data directories,
secret stores and content stores are still per test. And the flood test
overran the rate limiter's ceiling by 5,000 entries, each of which pruned the
whole 100,000-entry map; it now overruns it by five, which proves the same
ceiling and still counts every full scan a check makes.

In CI a run is expected to start with no key files — rust-cache prunes a
profile directory to its `build`, `deps` and `.fingerprint` before saving it,
though no run has confirmed it yet — so a run pays seven generations in all
instead of one or more per test; a run that does find them pays none. Locally
the files persist between runs until `cargo clean`.

**Measured locally** (10-core Apple silicon, `cargo nextest run --workspace
--profile ci -j 4`, tests already built, before and after interleaved, the key
directory deleted before every run so each starts cold as CI does; load
average 4–9 throughout):

| | before | after |
|---|---|---|
| **Instrumented** (`-Cinstrument-coverage`, `LLVM_PROFILE_FILE=<dir>/%m.profraw`) | 412, 383, 342 s (median **383 s**) | 124, 120, 113 s (median **120 s**, −69%) |
| CPU (user) instrumented | 1,284–1,552 s | 224–230 s |
| Uninstrumented | 129, 124, 121 s (median 124 s) | 93, 88 s (−27%) |

| Binary, instrumented | summed before | summed after |
|---|---|---|
| `rustak-server` (library) | 613–635 s | 209–212 s |
| `oidc_provider` | 49–50 s | 4.2–4.4 s |
| `workload_identity` | 157–248 s | 51–67 s |
| `cloudtak_onboarding` | 35–40 s | 17–22 s |
| `oauth_flows` | 44–53 s | 4.2–4.4 s |
| `api_v1_map` | 20–24 s | 2.9–3.2 s |
| every test | 1,347–1,450 s | 425–450 s |

The flood test: 13.9 s instrumented and 13.0 s uninstrumented before, 0.2 s
after. What remains in `workload_identity` is the cold start: its first few
tests wait while the orchestrator's three keys are generated, ~10 s each
instrumented; later tests read them and take about a second. Applied to the
first CI run's 3,464 s of test time, the local ratio (0.3) would put the
running part near five minutes on four slots instead of fifteen — a
projection, not a measurement; the next `main` run is the measurement.

**Coverage is unchanged.** Left alone, nextest's ~3,800 processes would each
write a `default_<sig>_<pid>.profraw`: ~3,800 files and ~35 GB measured
locally, against 14 GB of runner disk. So the job sets
`LLVM_PROFILE_FILE=<workspace>/coverage/%m.profraw` — outside `target/`, which
rust-cache saves. `%m` without `%p` makes every process of one binary merge
into one file as it exits (the profiling runtime locks the file). That left 89 files, 339 MB — `cargo test` wrote 387 files,
461 MB — and grcov read it in 71–78 s against 79–81 s, with the same 95,580
lines found and 91,134–91,136 hit against 91,131–91,132. `cargo-llvm-cov` is not
needed.

**The config** is `.config/nextest.toml`: profile `ci` has **no retries** (a
flaky test here is fixed, not retried), `fail-fast = false`, and a
`slow-timeout` of 120 s that terminates a test after ten minutes — a hung-wait
guard that names the test, not a bound on how long a test may take. It prints
one line per test with its duration, and the slow ones and failures again at
the end. No JUnit file: nothing reads one yet.

**Doctests** are not run by nextest; `Run doctests` runs them with the same
flags, so nothing rebuilds, and the same profile path.

**Installing it** follows trunk and cross: one pinned release asset,
`NEXTEST_VERSION`/`NEXTEST_SHA256` in `env:`, curl with
`--retry-all-errors`, `sha256sum --check`, and an `actions/cache` entry keyed on
the version in front. The asset is 12 MB, so the cache entry is too — nothing
against the 10 GB quota. The checksum is GitHub's recorded digest for the
asset; it has not yet been exercised in-workflow, and the first run will
download it (the cache is empty).

**What nextest gives besides speed:** a duration for every test, a SLOW line
for any test past two minutes, a named failure instead of a stuck job when a
test hangs, and every failure repeated at the end of the log.

## Caching, and why the binaries are fetched the way they are

Two things in this pipeline are downloaded rather than built: `trunk` (the `ui`
job and both nightly jobs) and `cross` (the four aarch64 build jobs). Between
2026-09-19 and 2026-09-21 their downloads failed **five** times, and one of
those failures published an **empty v0.0.2** — `Build UI` died, `build` needs
`ui`, and the entire release pipeline behind it skipped. How they are fetched is
therefore not an implementation detail.

**Not `cargo binstall`.** It resolves through a chain of fetchers (QuickInstall,
crate metadata, the release itself) and when that chain times out it fails with
no fallback, because `--disable-strategies compile` is set — building `trunk`
from source has broken before on transitive `cssparser`/`lightningcss`
mismatches, so a slow success is not a better outcome than a fast failure.
Retrying around it helped but could not cover a runner degraded for four
minutes.

**Instead:** one URL, `curl --fail --location --retry 5 --retry-all-errors
--retry-delay 5`, then a **SHA-256 check against a value pinned in `env:`**.
`--retry-all-errors` matters — plain `--retry` ignores connection resets and
5xx, which is most of what we saw. The checksum is not ceremony: `binstall`
verified nothing we could inspect, and a truncated download over a flaky network
is otherwise a mysterious build failure rather than a loud one.

To recompute a checksum when a pin moves:

```
curl -sL -o /tmp/trunk.tar.gz \
  https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-x86_64-unknown-linux-gnu.tar.gz
sha256sum /tmp/trunk.tar.gz          # shasum -a 256 on macOS
```

and the same for `cross-rs/cross`. The `trunk` pin has been exercised
in-workflow — `Build UI` on `c9118d9` met an evicted cache, downloaded the asset
and passed `sha256sum --check`. The `cross` pin was **verified out-of-band on
2026-09-21**, a fresh download matching `642375d1bcf3…`, because all four
aarch64 jobs hit the cache and skipped the install; it will be exercised
in-workflow on the next cache miss. Note that `cross-rs/cross` publishes no
`.sha256` sidecar, unlike `trunk-rs/trunk`, so this repository's pinned value is
the only recorded checksum for that asset.

Both archives are flat — `trunk` contains one
binary, `cross` contains `cross` and `cross-util`, and **both** of cross's must
be extracted or a later cache hit restores half a toolchain.

### Artifact uploads are retried once

Every `actions/upload-artifact` step **on the critical path** runs twice if it
has to: the first attempt carries `continue-on-error: true` and an `id`, a
`sleep 15` follows, and a second identical upload runs
`if: steps.<id>.outcome == 'failure'` with `overwrite: true`. The **second**
attempt is the one that fails the job.

The five covered are the ones something downstream needs — `cargofile`,
`ui-dist-e2e`, `ui-dist`, the build-matrix binaries and the image digests. The
Playwright report is deliberately **not** covered: it uploads only on
`if: failure()`, so retrying it would add noise to runs that are already failing
for a reason we have.

Why: GitHub's artifact service failed twice in three days, each time after the
work was done and each time fatally. On 2026-09-19 a darwin build died with
`Failed to CreateArtifact: … ENOTFOUND`; on 2026-09-22 `linux-arm64-rustak-plugin-ais`
died with `Failed to FinalizeArtifact: … (403) Forbidden: Error from
intermediary`. Neither had anything to do with this repository, and the second
one mattered more than the first: with images gated on the test suite, **one
failed upload now blocks all four images**, so `9c6f6f4` — a fix for a
three-day production outage — published nothing at all.

This is the same reasoning as the `trunk` and `cross` download retries, applied
to the other end of the pipeline: a network step that fails after the expensive
work is finished should be retried, not allowed to discard the work.

### The cache quota is a shared, finite resource

An `actions/cache` step sits in front of each download, so a normal run does not
touch the network for them at all. That only works while the entries survive.

On 2026-09-21 this repository sat at **9.94 GB of GitHub's 10 GB** cache limit,
**9.86 GB of it in 19 `rust-cache` entries** — the largest were 707 MB (`e2e`),
697 MB (`interop-eud`) and three separate 649 MB copies of the darwin build.
GitHub evicts least-recently-used entries when a repository is over quota, so
the **7 MB trunk and 2 MB cross caches were being deleted by their 700 MB
neighbours**. That is what failed `Build UI` on `6fac5bb`: not a network fault
first, but a cache that had been evicted, leaving the download to face a bad
network alone.

So every `Swatinem/rust-cache` step carries:

```yaml
    with:
      save-if: ${{ github.ref == 'refs/heads/main' }}
```

Pull requests and Dependabot branches **restore** from the cache and never
write to it. They were the churn — each wrote entries nobody reused — and
restoring without saving keeps their speed while leaving the quota to `main`.

Do not prune caches by hand: LRU and the 7-day expiry do it, and a hand-deleted
entry is simply rebuilt by the next run that needs it. If the quota is tight
again, look first at *how many distinct keys* the build matrix produces, not at
the size of any one entry.

## Other workflows

- **`changelog.yml`** — `release-drafter/release-drafter@v7.7.0` drafts
  release notes from merged PR labels, with the same merge-tree dedup cache
  pattern as `rust.yml`. Category/label mapping lives in
  `.github/release-drafter.yml`.
- **`security_audit.yml`** — `rustsec/audit-check@v2.0.0` nightly, plus (a
  deviation from automate, per design 01 §7.3) on every push to `main` that
  touches `Cargo.lock` or `rustak-ui/Cargo.lock`, so a vulnerable dependency
  bump is caught the same day rather than up to 24 hours later. Two advisories
  with no available fix are ignored through `.cargo/audit.toml` — see below.

### Ignored advisories (`.cargo/audit.toml`)

`cargo audit` reads `.cargo/audit.toml`, and so does the workflow. Only advisories
with no version to move to are listed there, each with the date and the reason, so
the job still fails on anything new:

- **`RUSTSEC-2023-0071` — `rsa`, the Marvin timing attack.** The advisory carries
  `patched = []` deliberately. rustak signs tokens and issues certificates with
  this crate; it never performs the PKCS#1 v1.5 *decryption* the side channel
  targets. Decision recorded 2026-09-19.
- **`RUSTSEC-2026-0258` — `h2 0.3`, unbounded empty DATA frames.** Patched only on
  the `0.4` line; `cargo tree -i h2@0.3` gives exactly one path, `actix-http` →
  `actix-web 4`, which pins `h2 ^0.3`. Revisit at actix-web 5. Decision recorded
  2026-09-19.

Remove an entry the day a fixed version becomes reachable. Security findings that
need tracking are raised through GitHub's security interface (Dependabot alerts,
code scanning), not as pull-request comments.

## Release / tag flow

1. Tag a GitHub Release (e.g. `v0.1.0`) → the `release: published` event fires
   `rust.yml`.
2. `version` rewrites `Cargo.toml`'s workspace version to `0.1.0` (the tag
   name with its leading `v` stripped) and uploads it.
3. `build` downloads that manifest, compiles all 25 crate×target
   combinations, and uploads each binary both as a GitHub Actions artifact and
   (via `SierraSoftworks/gh-releases@v1.0.10`) as a release asset named
   `<bin>-<os>-<arch>[.exe]`.
4. `docker-build` builds and pushes per-platform image digests for every
   crate; `docker-publish` combines them into multi-arch manifest lists
   tagged `latest`, `<major>.<minor>.<patch>`, `<major>.<minor>` and `<major>`
   at `ghcr.io/sierrasoftworks/rustak`, `…/rustak-plugin-example`,
   `…/rustak-plugin-ais`, `…/rustak-plugin-adsb`, `…/rustak-plugin-esb` and
   `…/rustak-plugin-firms`.
5. `tap` pushes an updated `rustak` formula (aliased `major`/`minor`) to the
   `SierraSoftworks` Homebrew tap.

A push to `main` (no release) runs the same pipeline minus `version`'s
tag-derived rewrite (the workspace version stays whatever is committed),
`build`'s release-asset upload, and `tap` — but **does** run `docker-build`/
`docker-publish`, so `ghcr.io/sierrasoftworks/rustak:latest` always tracks the
tip of `main`.

## Required repository secrets and variables

Configure these in the GitHub repository's Settings → Secrets and variables
before the pipeline can fully pass:

| Name | Used by | Notes |
|---|---|---|
| `GITHUB_TOKEN` | `test` (grcov download), `build` (release assets), `docker-build`/`docker-publish` (`ghcr.io` login), `tap`, `security_audit.yml`, `changelog.yml` | Provided automatically by GitHub Actions. For `docker-build`/`docker-publish` it needs `packages: write` — already requested in the workflow's `permissions:` block; no repository setting is required beyond the default `GITHUB_TOKEN` having package write enabled (Settings → Actions → General → Workflow permissions → "Read and write permissions"). |
| `CODECOV_TOKEN` | `test` (codecov upload) | From [codecov.io](https://codecov.io) once the repository is added there. Optional for a public repository on Codecov (uploads can work tokenless), but set it to avoid rate-limited/flaky uploads. |
| `TAP_APP_ID` | `tap` | GitHub App ID for the bot that pushes to the `SierraSoftworks` Homebrew tap repository. Same secret name and purpose as automate's. |
| `TAP_APP_PRIVATE_KEY` | `tap` | Private key (PEM) for that GitHub App. Same secret name as automate's. |

No repository *variables* (as distinct from secrets) are required — `REGISTRY`
(`ghcr.io`) and `ORG` (`sierrasoftworks`) are hardcoded `env:` values in
`rust.yml`, matching automate's own pattern of hardcoding `REGISTRY` there.

## Running the checks locally

```bash
# lint
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
scripts/check-file-length.sh
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# rustak-ui lint and tests (separate: excluded from the workspace)
cd rustak-ui
cargo fmt --all --check
cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings
cargo test   # host target: nothing in the suite needs a DOM, and CI has no wasm runner
cd ..

# test (what CI runs, without coverage; needs cargo-nextest — see CONTRIBUTING.md)
cargo nextest run --workspace --profile ci
cargo test --workspace --doc
# or, with no extra tool:
cargo test --workspace --no-fail-fast

# ui bundles
cd rustak-ui
trunk build            # debug, for e2e
trunk build --release  # release, for embedding
cd ..

# e2e (after a debug UI build, per rustak-ui step above)
cargo build -p rustak-server
cd e2e
npm install
npx playwright install chromium
npx playwright test
cd ..

# a single build-matrix leg, e.g. the native target
cargo build --release -p rustak-server
cargo build --release -p rustak-plugin-example
cargo build --release -p rustak-plugin-ais -p rustak-plugin-adsb -p rustak-plugin-esb -p rustak-plugin-firms

# a cross-compiled leg (needs `cross`: cargo binstall cross@0.2.5 — the same
# pin the build matrix uses)
cross build --release --target aarch64-unknown-linux-musl -p rustak-server

# docker image (after building the binary for the host platform)
cp target/release/rustak .
docker build -f rustak-server/Dockerfile -t rustak:dev .
rm rustak

# dependency audit (what security_audit.yml runs nightly)
cargo install cargo-audit --locked   # once
cargo audit
```

`actionlint` (not installed in this environment when this brief was written —
see `.claude/plan/status/M0-02-ci-pipeline.md`) can additionally lint the
workflow YAML itself:

```bash
actionlint .github/workflows/*.yml
```
