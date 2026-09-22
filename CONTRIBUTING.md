# Contributing to rustak

rustak is a from-scratch, MIT-licensed TAK server. This document covers the
workspace layout, how to build and test it locally, the conventions every
change follows, and how changes are proposed. For the project's goals,
architecture and milestone plan, start at
[`.claude/plan/plan.md`](.claude/plan/plan.md); the day-to-day standards
referenced throughout this document live in
[`.claude/plan/conventions.md`](.claude/plan/conventions.md).

## Workspace layout

A flat Cargo workspace, one top-level folder per crate:

| Crate | What it is |
|---|---|
| `rustak-cot` | CoT XML model, clean-room TAK Protocol v1 (prost), frame codecs. A leaf: no I/O, no server dependencies. |
| `rustak-api` | serde DTOs shared between the server and the UI. wasm-safe: `serde`/`chrono`/`uuid` only, no `tokio`/`rusqlite`/`tracing`. |
| `rustak-core` | Shared foundation for every binary: config loading + `${{ env.X }}` interpolation, telemetry bootstrap, the `human-errors` prelude, credential/identity primitives. |
| `rustak-client` | The sidecar SDK: a TAK stream client, a typed Marti API client, a control-API client, and the `Sidecar` trait/harness. Also used by the server's own integration tests. |
| `rustak-server` | Library + the `rustak` binary: listeners, SQLite, PKI/ACME, the Marti API, OAuth2, the admin API, the embedded UI. `src/lib.rs` exposes the app for in-process integration tests; `src/main.rs` is thin. |
| `rustak-ui` | The Yew admin SPA, built with Trunk. **Excluded from the Cargo workspace** (it targets `wasm32-unknown-unknown` and carries its own `Cargo.lock`) and embedded into `rustak-server` at compile time via `include_dir!`. |
| `rustak-plugin-example` | A minimal sidecar template — copy it to start a new plugin (`rustak-plugin-<name>`); see [`docs/plugins.md`](docs/plugins.md). |

Dependency direction is one-way: `rustak-api ← rustak-core ← rustak-client ←
rustak-plugin-*`; `rustak-cot` is a leaf; `rustak-server` depends on
`rustak-api`, `rustak-core` and `rustak-cot` (plus `rustak-client` as a
dev-dependency, for its own integration tests). All crate versions come from
`[workspace.package]`, all third-party versions from
`[workspace.dependencies]` (every crate manifest uses `dep = { workspace =
true }`), and lints from `[workspace.lints]`.

Besides the crates: `e2e/` (Playwright, driving the real UI against a real
server), `docs/` (this repository's own documentation, plus
`.claude/plan/compat/*.md` for wire contracts), `interop/` (automated
compatibility suites against real TAK-ecosystem client code — see
[`docs/interop.md`](docs/interop.md)), `scripts/` (CI-equivalent local
tooling), and `.github/workflows/` (CI/CD — see [`docs/ci.md`](docs/ci.md)).

## Building

Rust: whatever `stable` resolves to (`rust-toolchain.toml`), at least the
`rust-version` pinned in the workspace manifest. The UI is embedded into the
server binary at compile time, so **build it first**:

```sh
cd rustak-ui && trunk build && cd ..
cargo build
```

`trunk` is pinned to a specific version in CI (see `docs/ci.md`); install it
with `cargo binstall trunk` or `cargo install trunk --locked`. `trunk build`
also needs **Node** on the path: the map page's JavaScript libraries
(MapLibre GL, milsymbol, and a 2525C-to-2525D code converter) are pinned in `rustak-ui/package.json` and copied
into `dist/vendor` by a Trunk hook (`rustak-ui/scripts/vendor.mjs`), which runs
`npm ci` the first time and whenever a pin changes. `cargo build`
with no arguments builds only `rustak-server` (the binary you actually run —
it is the workspace's `default-members`); `cargo build --workspace` builds
every crate, including `rustak-plugin-example`.

If you skip the `trunk build` step, `rustak-server` still compiles
(`build.rs` creates an empty embed directory so the build does not fail) but
the running server answers every request with a 500 — see
[`e2e/README.md`](e2e/README.md) for the exact trap and why the order
matters.

Protobuf definitions in `rustak-cot` build with `protox`, a pure-Rust
`protoc` substitute — you do not need `protoc` installed for anything in this
workspace.

## Local checks

Run these before opening a change, in this order; they are exactly what
`.github/workflows/rust.yml` runs (see [`docs/ci.md`](docs/ci.md) for the
full job graph and how to reproduce every job locally):

```sh
# lint
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
scripts/check-file-length.sh
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# rustak-ui lint (separate: excluded from the workspace)
cd rustak-ui
cargo fmt --all --check
cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings
cd ..

# test
cargo test --workspace --no-fail-fast
```

`cargo test -p rustak-server --features testing` additionally exercises
suites gated behind the `testing` feature (see **Testing** below).

## Code conventions

The full standard is [`.claude/plan/conventions.md`](.claude/plan/conventions.md);
the ones that most often surprise a first change:

- **Every Rust source file has under 300 functional lines** — non-blank,
  non-comment lines before the single, column-0 `#[cfg(test)] mod tests`
  block, which must be the last item in the file (or before the whole file,
  for a file under `tests/`, `testing/`, `fixtures/`, or named `*_tests.rs`,
  which the check skips entirely). `scripts/check-file-length.sh` enforces
  this in CI:

  ```sh
  scripts/check-file-length.sh
  ```

  It walks `git ls-files '*.rs'`, so a **new, not-yet-committed** file is not
  covered — check it by hand (`awk` recipe in the script) before committing,
  or just run the script again once the file is staged. Split by
  responsibility, not by trimming — a file at the limit is usually a sign it
  covers two concerns.
- **`unsafe_code = "forbid"`** workspace-wide; edition 2024; current stable
  toolchain; `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo doc -D warnings`.
- **Errors** go through `human-errors` with `ResultExt` (`wrap_user_err`,
  `or_system_err`, advice slices). `Kind::User` messages are shown to
  whoever is running the command; `Kind::System` ones are logged and
  generalised before they reach anyone. Never `unwrap()`/`expect()` outside
  tests, except on a value that is provably infallible, with a comment
  saying why.
- **Tracing** goes through `tracing` spans/events via `tracing-batteries`.
  Never log secrets, tokens, keys, passwords, or CSR/certificate private
  material; `Debug` implementations of key types redact themselves — if you
  add a type that carries one of these, give it a redacting `Debug` impl and
  a test asserting the secret does not appear in `{:?}` output.
- **Config structs** use `#[serde(deny_unknown_fields)]`, and every key is
  documented in `config.example.toml` with its default — see
  `.claude/plan/status/M0-06-server-config.md` for the pattern (the example
  file is itself loaded in a test, so it cannot drift from the schema it
  documents).
- **Wire compatibility**: exact `Content-Type` values, envelope `type`
  strings, date formats and status codes per `.claude/plan/compat/*.md` and
  the verified research reports it distils. Never emit a 3xx from a Marti
  route. Never emit a self-closing `<event/>`.
- **Storage**: SQLite (STRICT tables, RFC 3339 millisecond timestamps bound
  from Rust) for state and metadata, with migrations under
  `rustak-server/migrations/NNNN_*.sql` — **never edit a migration that has
  already been applied/released**; add a new one instead. Content-addressed
  files for blobs. `store::append_log` segment files for time series (CoT
  history now, telemetry later). Nothing high-rate goes into SQLite.

## Testing

- **Unit tests** live in the file they test, in a single `#[cfg(test)] mod
  tests` block that is the last item in the file.
- **Integration tests** live under `<crate>/tests/`. `rustak-server`'s
  exercise the app in-process (no separate server to start) and need the
  `testing` feature for the test-only helpers it exposes (`TestServer`,
  `TestIdentityProvider`, a software WebAuthn authenticator, mock services):

  ```sh
  cargo test -p rustak-server --features testing
  ```

  `tempfile` and `wiremock` are also plain dev-dependencies, so
  `cargo test` (without the feature) still builds and runs everything that
  does not need the feature-gated test doubles.
- **Golden files** live under `tests/golden/`; **fixtures are our own** —
  hand-written sample messages, never a captured packet from a real device
  or a copied file from a GPL reference implementation.
- **Every wire-facing change** adds or updates a contract test asserting
  exact bytes/headers, plus the JSON-schema oracle where node-tak defines the
  shape it must match.
- **Interop suites** under `interop/` run in CI, separately from
  `cargo test --workspace` — see [`docs/interop.md`](docs/interop.md) for
  what each one covers and when it runs (per pull request vs. nightly).
  Anything CI cannot reach (WinTAK, iTAK, a real phone) is a manual checklist
  under `docs/compat/`, not a gate.
- **e2e** (`e2e/`) is Playwright against a real debug build of the UI and the
  server — see [`e2e/README.md`](e2e/README.md) for how to run it and the
  build-order trap above.

## Licensing

rustak is **MIT**. Three reference implementations in this ecosystem —
**atak-civ**, **TAK Server**, and **OpenTAKServer** — are **GPL**. Reading
them is how we know exact endpoint paths, field names, numbers and behaviour;
using them is bounded strictly to that:

- Read GPL sources for **facts only** — never copy code, comments, `.proto`
  text, or fixtures from them.
- `.claude/plan/research/*.md` reports are already paraphrased from that
  reading; cite them by section rather than re-deriving from the source tree.
- Protobuf definitions in `rustak-cot/proto/` are clean-room: written from
  documented field numbers and types, never transcribed from a GPL `.proto`
  file.
- If a piece of work has to build or vendor GPL code to be useful (the
  `interop/eud` harness builds ATAK's own `commoncommo` from `atak-civ`), it
  is kept at arm's length from this repository — see
  `interop/eud/README.md` → Licence posture for the pattern (a separate,
  non-published artefact, driven across a process boundary, never linked
  into anything MIT-licensed here).

## Version control

Implementation work in this repository is orchestrated through
[GitButler](https://gitbutler.com) (`but`); see
[`.claude/plan/conventions.md`](.claude/plan/conventions.md) → Version
control for the branch-per-task and stacking conventions the automated
implementation agents follow. If you are contributing by hand, plain `git`
works the same way any other project's does — the parts of the convention
that matter regardless of tooling are:

- **Branch names**: `<type>/<short-description>` (`feat/…`, `fix/…`,
  `chore/…`, …).
- **Commit messages**: `type: Summary of changes` (`feat`, `fix`, `chore`,
  `docs`, `test`, `ci`, `refactor`), with a body explaining *what* and *why*
  — including any non-obvious decision — not just restating the diff.
- Keep unrelated changes in separate commits; keep a test with the behaviour
  it verifies rather than splitting it into its own commit.

## Opening a change

1. Read [`.claude/plan/plan.md`](.claude/plan/plan.md) for where a change
   fits in the architecture and the milestone plan, and check
   [`.claude/plan/status/`](.claude/plan/status/) for what is already
   implemented and verified in the area you are touching — the status notes
   record deviations and open items that are easy to miss from the design
   documents alone.
2. Make the change, following the conventions above.
3. Run the local checks (**Local checks**, above) for every crate you
   touched, plus `cargo test --workspace` at the end.
4. Open a pull request describing what changed and why. Reference the
   `.claude/plan/briefs/` or `.claude/plan/status/` entry it relates to, if
   any.

## See also

- [`README.md`](README.md) — what rustak is, and where the project stands.
- [`docs/deployment.md`](docs/deployment.md) — running rustak.
- [`docs/ci.md`](docs/ci.md) — the full CI/CD job graph and how to reproduce
  every job locally.
- [`docs/interop.md`](docs/interop.md) — the automated compatibility suites.
- [`docs/plugins.md`](docs/plugins.md) — writing a sidecar.
