# CI/CD

rustak's pipeline is a direct port of [`automate`](https://github.com/SierraSoftworks/automate)'s
GitHub Actions setup, adapted for a multi-crate workspace with five release
binaries (`rustak-server` → `rustak`, and the `rustak-plugin-example`,
`rustak-plugin-ais`, `rustak-plugin-adsb` and `rustak-plugin-esb` sidecars)
instead of one.
See `.claude/plan/design/01-foundations-storage-ci.md` §7 for the design this
implements and `.claude/plan/research/01-automate-architecture.md` §1 for what
was ported from where.

## Job graph (`.github/workflows/rust.yml`)

Runs on every push to `main`, every pull request, and every published release.

```
deduplicate ──┬─ version ─────────────────────────────────────┐
              ├─ lint    (fmt --check, clippy -D warnings, check-file-length.sh, cargo doc -D warnings)
              ├─ test    (cargo test --workspace, coverage → grcov → codecov)
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
- **Every job carries a `timeout-minutes`** — 60 for `test`, 45 for `build`,
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

  The number has moved twice, and the history is the argument. **Thirty** was
  set against a normal band of 11–16 minutes — the good case dressed up as a
  bound — and on 2026-09-19 the multiplier met a suite that had also grown, so
  the job was cancelled at exactly 30 with four binaries still to run.
  **Forty-five** was set against 15–17. On 2026-09-20 the M9 wave added 184
  tests across four landings and the band became **16–20 minutes**, which puts
  the bad case at 20 × 2.8 ≈ 56 — past 45 again. So `test` now carries **60**,
  raised deliberately rather than discovered from a cancelled release: `build`
  needs `ui` and `test`, and v0.0.2 published nothing at all after a single job
  died. Revisit once the M9 tests settle into a band.

  **A `test` job that hits 60 is a bug report again** — and the first thing to
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
  rustak-plugin-esb}` ×
  `{x86_64-unknown-linux-musl, aarch64-unknown-linux-musl (cross),
  x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-msvc}` — 25
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
  `…/rustak-plugin-example`, `…/rustak-plugin-ais`, `…/rustak-plugin-adsb` and
  `…/rustak-plugin-esb`.
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
  alongside it — 16–20 minutes on a normal runner, and up to an hour on a slow
  one under `test`'s 60-minute bound. A push whose images are wanted sooner has
  to wait; that is the price of the tag meaning something.
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
workflow — it is part of `cargo test --workspace` in the `test` job, since
those suites live under `rustak-server/tests/stream_*.rs`.

## Keeping the test job inside its timeout

The `test` job compiles the whole workspace with `RUSTFLAGS=-Cinstrument-coverage`
and runs the lot on a two-vCPU runner — over 1300 tests in `rustak-server`'s
library target alone, plus eleven integration binaries. Instrumentation applies to *every*
crate in the graph, dependencies included — there is no stable per-package
rustflag — so the arithmetic-heavy dependencies (`rsa`, `num-bigint-dig`,
`argon2`, `blake2`) run with a counter update in their innermost loops. That is
why an RSA-2048 generation that costs a fraction of a second uninstrumented costs
*minutes* of CPU instrumented — measured at roughly two orders of magnitude on
this workspace — and why the job once stopped finishing at all, with individual
tests reported as "has been running for over 60 seconds". `grcov` already discards
everything outside the workspace at *report* time, but that is after the cost
has been paid.

Three things keep it inside 30 minutes, all of them in test-only code:

1. **One token signing key per test process.** `rustak-server/src/testing/keys.rs`
   holds a `LazyLock` RSA-2048 key and `TestServer` adopts it through
   `JwtIssuer::load_or_adopt`, instead of each of the 240-odd test servers
   generating its own. Databases, data directories, secret stores and content
   stores stay per test, so isolation is unchanged; the tests that are *about*
   key generation (rotation, "a token signed by another server") still generate.
2. **A cheaper argon2id cost in tests.** `rustak_core::identity::password::use_testing_params`
   switches the process to `Params::TESTING` (m = 8 MiB, t = 1) and `TestServer`
   calls it. It is compiled out without the `testing` feature, no deployment can
   reach it, and verification is unaffected because a PHC string carries the
   cost its hash was made at.
3. **`opt-level = 3` for the crypto dependencies** (`[profile.dev.package.*]` in
   the workspace `Cargo.toml`: `rsa`, `num-bigint-dig`, `argon2`, `blake2`).
   Dependencies of a test target are built with the `dev` profile, so these
   apply to `cargo test`.

If the job ever creeps back towards its timeout, measure before changing
anything: `cargo test -p rustak-server --features testing` with and without
`RUSTFLAGS=-Cinstrument-coverage` gives the instrumentation multiplier directly,
and `--lib <module>::` narrows it to one suite.

Instrumenting only the workspace is the obvious fourth lever and it is not
available on stable: `-Cinstrument-coverage` is a per-*invocation* flag and
applies to every crate that invocation builds, and the per-package equivalent
(`[profile.dev.package.<dep>] rustflags`) is nightly-only behind
`-Zprofile-rustflags`. `cargo llvm-cov` does not change that — it sets the same
flag through `RUSTFLAGS` and filters at report time, as `grcov` already does.
So the levers are the three above; dropping coverage is not one, because codecov
is a gate.

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
   `…/rustak-plugin-ais`, `…/rustak-plugin-adsb` and `…/rustak-plugin-esb`.
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

# test
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
cargo build --release -p rustak-plugin-ais -p rustak-plugin-adsb -p rustak-plugin-esb

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
