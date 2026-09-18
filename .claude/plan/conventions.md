# rustak conventions

## Structure
- Flat Cargo workspace, one top-level folder per crate: `rustak-cot`, `rustak-api`, `rustak-core`, `rustak-client`, `rustak-server`, `rustak-ui` (excluded; trunk), `rustak-plugin-<name>`. Versions and lints come from `[workspace.package]`, `[workspace.dependencies]`, `[workspace.lints]`; crate manifests use `workspace = true`.
- Module layout follows `plan.md` → Architecture and the `design/` documents. One aggregate per file; route files stay thin (parsing + service call); logic lives in domain modules.
- **Every Rust source file has < 300 functional lines** (non-blank, non-comment lines before the single column-0 `#[cfg(test)] mod tests` block, which must be the last item). `scripts/check-file-length.sh` enforces this in CI. Split by responsibility, not by line count.
- Dependency direction: `rustak-api ← rustak-core ← rustak-client ← rustak-plugin-*`; `rustak-cot` is a leaf (no I/O); `rustak-server` uses api/core/cot (+ client as dev-dependency). `rustak-api` and `rustak-ui` are wasm-safe (no tokio/rusqlite/tracing).

## Code
- Edition 2024, current stable toolchain; `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo doc -D warnings`; `unsafe_code = "forbid"`.
- Errors: `human-errors` with `ResultExt` (`wrap_user_err`, `or_system_err`, advice slices); `Kind::User` messages are shown, `Kind::System` are logged and generalised. Never `unwrap()`/`expect()` outside tests except on provably infallible values with a comment.
- Tracing: `tracing` spans/events through `tracing-batteries`; never log secrets, tokens, keys, passwords, CSR/certificate private material; `Debug` impls of key types redact.
- Security defaults: no plaintext listeners; client certs required on Marti and stream listeners; Basic auth only on enrollment and `/oauth/token`; one-time enrollment tokens; argon2id for verifiable secrets; `Sealed` (AES-GCM, context-as-AAD) for readable secrets; rate limiting on every credential endpoint; constant-time comparisons.
- Wire compatibility: exact `Content-Type` values, envelope `type` strings, date formats and status codes per `compat/` and `research/06`/`07`/`03`. Never emit 3xx on Marti routes. Never self-closing `<event/>`.
- Storage: SQLite (STRICT tables, RFC 3339 millisecond timestamps bound from Rust, migrations in `rustak-server/migrations/NNNN_*.sql`, never edit an applied migration) for state/metadata; content-addressed files for blobs; `store::append_log` segment files for time series. No high-rate writes into SQLite.
- Config: `#[serde(deny_unknown_fields)]` on every struct; every key documented in `config.example.toml` with its default; `${{ env.X }}` interpolation via `rustak-core`.
- Protobuf: clean-room `.proto` in `rustak-cot/proto/`; built with `protox` + `prost-build` (no `protoc`).

## Tests
- Unit tests in-file (`#[cfg(test)] mod tests` last); integration tests under `<crate>/tests/`; golden files under `tests/golden/`; fixtures are our own (never copied captures).
- Every wire-facing change adds or updates a contract test asserting bytes/headers, plus the JSON-schema oracle where node-tak defines the shape.
- Interop suites live in `interop/` and run in CI (`node-tak` on PRs; `cloudtak` and `eud` nightly). Manual checks are documented as checklists in `docs/compat/`, not used as gates.

## Version control (GitButler)
- Use `but` for all write operations (see the gitbutler skill). One branch per brief: `feat/<slug>`, `fix/<slug>`, `chore/<slug>`; stack with `but move <branch> --above <dep>` when a brief depends on another in-flight branch.
- Commit messages: `type: Summary of changes` (feat, fix, chore, docs, test, ci, refactor), body explains what/why and key decisions; end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Agents do not push or open PRs; the orchestrator integrates and pushes.

## Licensing
- rustak is MIT. atak-civ, TAK Server and OpenTAKServer are GPL: read them for facts (endpoint paths, field names, numbers, behaviour) only; never copy code, comments, `.proto` text or fixtures. `research/` reports are already paraphrased.
