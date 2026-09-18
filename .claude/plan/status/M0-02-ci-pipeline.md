# M0-02 — CI/CD pipeline: status

## What was created

- `.github/workflows/rust.yml` — job graph exactly per design 01 §7.3:
  `deduplicate` → `version` (rewrites the single `^version =` line under
  `[workspace.package]` in the root `Cargo.toml`) → `lint` (fmt, clippy
  `--workspace --all-targets -D warnings`, `scripts/check-file-length.sh`,
  `cargo doc -D warnings`) → `test` (coverage → grcov → codecov) → `ui`
  (trunk pinned to 0.21.14, debug bundle for e2e + release bundle, plus
  fmt/clippy of `rustak-ui` for `wasm32-unknown-unknown` since it's excluded
  from the workspace) → `e2e` (Playwright) → `build` (crate × target matrix,
  10 jobs: `{rustak-server→rustak, rustak-plugin-example→rustak-plugin-example}`
  × `{x86_64-unknown-linux-musl, aarch64-unknown-linux-musl (cross),
  x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-msvc}`) → `ci`
  aggregator → `docker-build`/`docker-publish` (per crate, multi-arch, to
  `ghcr.io/sierrasoftworks/<bin>`, on `main` pushes **and** releases) → `tap`
  (release only, formula inferred as `rustak`). Placeholder `interop-node-tak`
  job (PR-triggered, `if: false`, comment pointing at M2). **No `protoc`
  installation anywhere** — verified by grepping the file for
  `protoc`/`setup-protoc`: zero matches.
- `.github/workflows/nightly.yml` — new (not in automate): schedule
  (04:00 UTC daily) + `workflow_dispatch`, with `interop-cloudtak` and
  `interop-eud` placeholder jobs, both `if: false` with comments pointing at
  M4 and the M1 EUD-harness exploration brief respectively.
- `.github/workflows/changelog.yml`, `.github/workflows/security_audit.yml` —
  lifted from automate essentially verbatim. `security_audit.yml` additionally
  runs on `push` to `main` touching `Cargo.lock`/`rustak-ui/Cargo.lock`, per
  design 01 §7.3's explicit delta over automate.
- `.github/release-drafter.yml` — lifted verbatim, with the docs autolabeler
  glob widened to also match `docs/**` (automate only had `*.md`).
- `.github/dependabot.yml` — `cargo /` (workspace root; groups
  `opentelemetry`, `protobuf`, `rustls`, `actix`), `cargo /rustak-ui` (group
  `yew`), `github-actions /`, `npm /e2e` — all daily, as specified.
- `Cross.toml` — minimal; no pre-build steps by default (no protoc, no
  libssl-dev — rustak uses protox and rustls/aws-lc-rs respectively). The
  `cmake` fallback for `aarch64-unknown-linux-musl` is present but commented
  out, as instructed.
- `rustak-server/Dockerfile`, `rustak-plugin-example/Dockerfile` — created
  under freshly `mkdir -p`'d crate directories (neither existed yet; the
  concurrent M0-01 agent had not created them at the time this brief ran).
  Content matches design 01 §7.3 exactly for `rustak-server/Dockerfile`; the
  plugin Dockerfile mirrors it with only the binary name and
  `--config /data/plugin.toml` changed, per the design's "is the same with
  its binary and `--config /data/plugin.toml`" instruction — see Deviations
  below re: whether the `EXPOSE`/`VOLUME` lines make sense for a sidecar.
- `e2e/package.json`, `e2e/tsconfig.json`, `e2e/playwright.config.ts`,
  `e2e/scripts/start-server.mjs`, `e2e/tests/helpers.ts`,
  `e2e/tests/smoke.spec.ts`, `e2e/README.md` — harness skeleton, ported from
  automate's `e2e/` (research 01 §7). Port at 18446 (not rustak's default
  8446, not automate's 8099). Generated config uses
  `[web.public.tls] mode = "none"` + `allow_insecure_http = true`, no
  `[stream.tcp]` section at all (per plan.md's "no plaintext TCP stream
  listener" decision), `[web.marti]`/`[stream.tls]` both `enabled = false`.
  `npm install` was **not** run (per brief); no `package-lock.json` is
  committed.
- `docs/ci.md` — job graph, release/tag flow, required secrets/vars, local
  reproduction commands for every CI check.

## Exit checks — results

All four ran successfully; outputs below.

### actionlint

Not preinstalled and `brew install actionlint` failed (Homebrew itself needs
the Xcode Command Line Tools license accepted via `sudo xcodebuild -license`,
which this agent cannot run). Rather than fetch a prebuilt binary from
GitHub Releases, I built actionlint v1.7.12 from source via
`go install github.com/rhysd/actionlint/cmd/actionlint@latest` (Go 1.27 was
already on this machine) and ran it against every workflow file:

```
$ actionlint .github/workflows/*.yml
```

Findings (all pre-existing in automate's own workflows too — verified by
running the identical binary against `../automate/.github/workflows/*.yml`,
which produces the exact same class of findings and also exits 1):

- `shellcheck reported issue ... SC1083` on `echo "tree=$(git rev-parse HEAD^{tree})"`
  in `rust.yml` and `changelog.yml` — literal `{`/`}` in the git plumbing
  command; present verbatim in automate's own workflow (same line, same
  warning) and not something this port introduced.
- `shellcheck ... SC2086`/`SC2046` (unquoted expansions) in the `docker-build`/
  `docker-publish` `run:` blocks — same shell fragments automate uses
  (`platform=${{ matrix.platform }}`, the `printf '...@sha256:%s '` /
  `jq -cr ...` digest-collection idiom); pre-existing in automate, not new.
- `if-cond`: "constant expression `false` in condition, remove the `if:`
  section" on the three placeholder jobs (`interop-node-tak`,
  `interop-cloudtak`, `interop-eud`). This is the intended, brief-specified
  shape (`if: false` "until M2 ... with a comment") — actionlint's own style
  opinion, not a defect. Flip these to real conditions when each interop
  suite lands.

No workflow-authoring errors (bad expression syntax, unknown context/property
references, matrix/needs graph errors, missing required action inputs, or
job-name collisions) were reported for either `rust.yml`'s 10× build matrix,
the docker jobs, or the `ci` aggregator's `needs`/`always()` handling.

### `node --check e2e/scripts/start-server.mjs`

```
$ node --check e2e/scripts/start-server.mjs
```
Exit 0, no output — valid syntax.

### Action version parity with automate

```
$ grep -hoE 'uses: [A-Za-z0-9_.\/-]+@[A-Za-z0-9_.-]+' .github/workflows/*.yml | sort -u
```
Every third-party action rustak's workflows reference is pinned to the exact
same tag automate uses (`actions/checkout@v7`, `actions/cache/{restore,save}@v6`,
`actions/{upload,download}-artifact@v{7,8}`, `actions/setup-node@v7`,
`dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`,
`SierraSoftworks/setup-grcov@v1`, `codecov/codecov-action@v7.0.0`,
`cargo-bins/cargo-binstall@main` — automate itself leaves this one unpinned to
a version tag, per the action's own recommendation, since it installs
whatever the latest `cargo-binstall` release is; kept as-is for parity —
`docker/setup-buildx-action@v4`, `docker/login-action@v4.6.0`,
`docker/metadata-action@v6`, `docker/build-push-action@v7`,
`SierraSoftworks/gh-releases@v1.0.10`, `SierraSoftworks/actions-tap@v1`,
`release-drafter/release-drafter@v7.7.0`, `rustsec/audit-check@v2.0.0`). The
only action in automate's set that rustak's workflows do **not** use is
`SierraSoftworks/setup-protoc@v3.0.1` — correctly dropped, since no leg of
this pipeline installs `protoc`.

### YAML parses

```
$ python3 -c 'import yaml,sys;[yaml.safe_load(open(f)) for f in sys.argv[1:]]' .github/workflows/*.yml
$ python3 -c 'import yaml,sys;[yaml.safe_load(open(f)) for f in sys.argv[1:]]' .github/dependabot.yml .github/release-drafter.yml
```
(PyYAML was not preinstalled; installed into a scratch venv rather than the
system/Homebrew Python, which refuses global installs — PEP 668 — to avoid
touching anything outside this task.) Both commands exited 0 with no errors
for all 6 workflow files plus `dependabot.yml` and `release-drafter.yml`.

## Secrets and variables the user must configure

| Name | Kind | Used by | Notes |
|---|---|---|---|
| `GITHUB_TOKEN` | secret (automatic) | most jobs; `ghcr.io` login for `docker-build`/`docker-publish` | Provided by Actions automatically. Needs the repository's default token to have package write access: **Settings → Actions → General → Workflow permissions → "Read and write permissions"**. No PAT needs creating. |
| `CODECOV_TOKEN` | secret | `test` job, codecov upload | From codecov.io once the repo is added there. Optional for a public repo (uploads can work tokenless) but recommended to avoid rate-limiting. |
| `TAP_APP_ID` | secret | `tap` job | GitHub App ID for the bot that pushes to the `SierraSoftworks` Homebrew tap repo — same secret name/purpose automate uses. |
| `TAP_APP_PRIVATE_KEY` | secret | `tap` job | Private key (PEM) for that GitHub App — same secret name automate uses. |

No repository *variables* are required; `REGISTRY` (`ghcr.io`) and `ORG`
(`sierrasoftworks`) are hardcoded `env:` in `rust.yml`, matching automate's
own pattern of hardcoding its `REGISTRY`.

## Deviations from automate (and why)

1. **`docker-build`/`docker-publish` run on `main` pushes too, not just
   releases.** Automate's are release-only. rustak's M0 exit criterion
   (plan.md) requires `ghcr.io/sierrasoftworks/rustak:latest` to exist on a
   green `main`, so the `if:` was widened to
   `github.event_name == 'release' || (github.event_name == 'push' && github.ref == 'refs/heads/main')`.
2. **Docker/build matrices are now `crate × …`, not just `…`.** Two binaries
   (`rustak-server`→`rustak`, `rustak-plugin-example`→`rustak-plugin-example`)
   instead of automate's one (`automate`). Artifact names, `Dockerfile`
   selection (`file: ${{ matrix.crate.name }}/Dockerfile`), and image names
   (`ghcr.io/sierrasoftworks/<bin>`) all key off the crate dimension.
3. **`version` rewrites the workspace root `Cargo.toml`, not a per-package
   manifest.** rustak's crates all inherit their version from
   `[workspace.package]` (design 01 §1.1), so one rewrite covers every binary
   in the `build` matrix — automate rewrites `agent/Cargo.toml` because its
   `api`/`ui` crates aren't versioned from a shared workspace table.
4. **No `openssl-vendored` cargo feature / flag anywhere.** automate's build
   matrix passes `--features openssl-vendored` on every non-Windows leg
   (`reqwest`'s TLS backend there is OpenSSL-vendored for the release
   binary). rustak has no such feature — TLS is rustls/aws-lc-rs throughout —
   so the `build` job's cargo invocation is simply
   `${builder} build --release --target ${target} -p ${crate.name}`.
5. **No `protoc`/`setup-protoc` step anywhere**, confirmed by grep. automate
   installs `protobuf-compiler` (native) or `SierraSoftworks/setup-protoc`
   (build matrix) in `test`, `e2e`, and every non-cross `build` leg, because
   `tracing-batteries`' OpenTelemetry stack needs it there. rustak's
   `rustak-cot` build-dependency is `protox` (pure Rust), so none of that
   carries over — per design 01 §0 and the brief.
6. **`Cross.toml` has no pre-build steps by default**, vs. automate's
   `protobuf-compiler`/`libssl-dev` installs for `aarch64-unknown-linux-gnu`,
   `x86_64-unknown-linux-gnu`, and `aarch64-unknown-linux-musl`. rustak's
   `build` matrix also only has **one** cross target
   (`aarch64-unknown-linux-musl`, no `-gnu` targets at all), matching design
   01 §7.3's target list. A commented `cmake` fallback is included per the
   brief, for if `aws-lc-sys` fails to build without it.
7. **`security_audit.yml` also runs on relevant `push`es to `main`**, not
   schedule-only like automate's — an explicit design 01 §7.3 delta.
8. **New `nightly.yml` workflow** (`interop-cloudtak`, `interop-eud`
   placeholders) and a new `interop-node-tak` placeholder job inside
   `rust.yml` — automate has no interop-suite concept at all; these exist
   because rustak's compatibility story (plan.md "Interop in CI") is CI-gated
   from day one, unlike automate's domain.
9. **`ci` aggregator tolerates a `skipped` result specifically for
   `interop-node-tak`.** Every other dependency job must be `success`;
   `interop-node-tak` is allowed `skipped` (its `if: false` state) so the
   placeholder doesn't block merges before M2, while a real failure once it's
   live still would.
10. **`ui` job also lints `rustak-ui` for `wasm32-unknown-unknown`.** automate
    has no equivalent (its `ui/` isn't linted in CI at all, per what's visible
    in `rust.yml`); added because `rustak-ui` is excluded from the workspace
    (§7.3's `ui` job description in design 01 explicitly calls for
    "cargo fmt/clippy in rustak-ui for wasm32"), so the workspace-wide `lint`
    job never reaches it.
11. **`release-drafter.yml`'s `docs` autolabeler also matches `docs/**`**, not
    just `*.md` — rustak's `docs/` tree (per design 01 §1.5) is larger than
    automate's flatter doc layout.
12. **Dependabot's `cargo` ecosystem is split into two directory entries**
    (`/` and `/rustak-ui`), vs. automate's single `/` entry — because
    `rustak-ui` is excluded from the workspace and carries its own
    `Cargo.lock` (design 01 §1.2), exactly as the brief specifies.

## Open items for the orchestrator

- **actionlint could not be installed via Homebrew** in this sandbox (Xcode
  license gate needing `sudo`). I built it from source with `go install`
  instead and ran it successfully (see above) rather than downloading a
  prebuilt binary from GitHub Releases, which felt like the wrong kind of
  "download and run an executable" for an agent to do unprompted. If the
  target CI/dev environment also lacks Homebrew's Xcode license acceptance,
  the same `go install github.com/rhysd/actionlint/cmd/actionlint@latest`
  path works there too.
- **`rustak-plugin-example/Dockerfile`'s `EXPOSE`/`VOLUME` lines** are copied
  verbatim from `rustak-server/Dockerfile` (`EXPOSE 443 8446 8443 8089 8087`,
  `VOLUME /data`) per design 01 §7.3's literal "is the same with its binary
  and `--config /data/plugin.toml`" — but a sidecar plugin is a TAK *client*,
  not a listener (see plan.md's plugin/sidecar contract: it connects out to
  `:8089` and the control API, it doesn't bind ports). These lines are inert
  (`EXPOSE` is documentation-only) but may be worth trimming once
  `rustak-plugin-example` actually exists and its real needs (if any — maybe
  a health-check port in M6) are known. Flagged rather than silently
  decided.
- **This brief could not verify against real crate contents**: `rustak-server`
  and `rustak-plugin-example` did not exist yet when this brief ran (M0-01 was
  concurrent and had not created them), so none of `cargo fmt`, `cargo
  clippy`, `cargo test`, `cargo doc`, `trunk build`, or an actual `docker
  build`/`cross build` could be exercised — only YAML validity, actionlint,
  and `node --check` were run. The orchestrator should re-run the full
  pipeline (or at minimum `cargo build --workspace`, `trunk build` from
  `rustak-ui/`, and one `docker build -f rustak-server/Dockerfile .`) once
  M0-01's crates land, to catch anything the static checks here can't (e.g.
  whether `rustak-server`'s actual binary name is exactly `rustak`, whether
  `rustak-ui`'s `<title>` genuinely matches `/rustak/i` as `smoke.spec.ts`
  assumes).
- **`e2e/package.json` has no committed lockfile** (`npm install` was
  deliberately not run, per the brief) — `npm ci` in the `e2e` CI job will
  fail until a `package-lock.json` is committed. Whoever first runs
  `npm install` in `e2e/` (this agent, per the brief, did not) should commit
  the resulting lockfile in the same change that first exercises the `e2e`
  job for real.
- **`tap` job's Homebrew formula name** is inferred from the repository name
  by `SierraSoftworks/actions-tap@v1` (no explicit formula-name input was
  added, since automate's own workflow doesn't pass one either and I could
  not find documentation confirming such an input exists). Since this
  repository actually is named `rustak` (unlike automate/`automate-rs`), the
  inferred name should already be correct — worth a one-time check on the
  first real release rather than assuming it silently.
