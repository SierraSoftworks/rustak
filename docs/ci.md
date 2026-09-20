# CI/CD

rustak's pipeline is a direct port of [`automate`](https://github.com/SierraSoftworks/automate)'s
GitHub Actions setup, adapted for a multi-crate workspace with four release
binaries (`rustak-server` → `rustak`, and the `rustak-plugin-example`,
`rustak-plugin-ais` and `rustak-plugin-adsb` sidecars) instead of one.
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
              └─ build   (needs version, ui; crate × target matrix, 20 jobs) ─┬─ ci (aggregator, always())
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
- **Every job carries a `timeout-minutes`** — 45 for `build` and for `test`,
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
  knowing why.** Its normal wall time measured 11m40s to 16m12s across six
  runs, but the *same* tests cost two to four times more on a slow host —
  `rustak_server`'s library was 102 s on one run and 341 s on another,
  `stream_routing` 81 s against 309 s. Nothing in the repository changes
  between those; GitHub's two-vCPU hosts simply vary. On 2026-09-19 that
  multiplier met a suite that had also grown and the job was cancelled at
  exactly 30 minutes with four binaries still to run. So `test` carries **45**,
  and the extra fifteen minutes buy variance rather than the suite's own cost.
  A `test` job that hits *45* is a bug report again.
- **`ui`** installs `trunk` pinned to **0.21.14** (`cargo binstall trunk@0.21.14`;
  0.22 was still beta at the time this pipeline was written — bump the pin
  deliberately, not via dependabot, which cannot see cargo-binstall installs).
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
  rustak-plugin-example, rustak-plugin-ais, rustak-plugin-adsb}` ×
  `{x86_64-unknown-linux-musl, aarch64-unknown-linux-musl (cross),
  x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-msvc}` — 20
  jobs. A new plugin crate is three lines: one in each of this matrix and the
  two Docker ones below. **No `protoc` is installed anywhere in this
  workflow**: `rustak-cot` builds its protobuf definitions with `protox`, a
  pure-Rust `protoc` substitute, so neither the native runners nor the `cross`
  Docker image need one. Artifacts and (on a release) release assets are named
  `<bin>-<os>-<arch>[.exe]`.
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
  `…/rustak-plugin-example`, `…/rustak-plugin-ais` and `…/rustak-plugin-adsb`.
  Unlike automate (which only publishes on a GitHub release), this also runs on
  every push to `main`,
  because rustak's M0 exit criterion is a multi-arch
  `ghcr.io/sierrasoftworks/rustak:latest` on a green `main`.
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
3. `build` downloads that manifest, compiles all 20 crate×target
   combinations, and uploads each binary both as a GitHub Actions artifact and
   (via `SierraSoftworks/gh-releases@v1.0.10`) as a release asset named
   `<bin>-<os>-<arch>[.exe]`.
4. `docker-build` builds and pushes per-platform image digests for every
   crate; `docker-publish` combines them into multi-arch manifest lists
   tagged `latest`, `<major>.<minor>.<patch>`, `<major>.<minor>` and `<major>`
   at `ghcr.io/sierrasoftworks/rustak`, `…/rustak-plugin-example`,
   `…/rustak-plugin-ais` and `…/rustak-plugin-adsb`.
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
cargo build --release -p rustak-plugin-ais -p rustak-plugin-adsb

# a cross-compiled leg (needs `cross`: cargo binstall cross)
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
