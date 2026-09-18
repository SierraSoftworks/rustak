# M0-01 — Workspace skeleton

**Goal:** create the compilable Cargo workspace exactly as `design/01-foundations-storage-ci.md` §1 (root manifest, per-crate manifests, build scripts, supporting root files) with placeholder crates, so later briefs have a green baseline. No feature code.

**Read first:** `.claude/plan/plan.md` (Architecture → Workspace; Design artefacts → deltas), `.claude/plan/conventions.md`, `design/01-foundations-storage-ci.md` §0–§1.5, §2.1–2.2 (only the module *lists*; do not implement bodies), and `../automate/{Cargo.toml,agent/build.rs,ui/Trunk.toml,ui/index.html,.cargo/config.toml,.gitignore}` for the patterns being mirrored (never read `../automate/.env` — it is a named pipe).

**Files you own (create only these):**
- `Cargo.toml` (workspace; copy the design's `[workspace]`, `[workspace.package]`, `[workspace.dependencies]`, `[workspace.lints]`, dev-profile overrides verbatim, then verify every version resolves with `cargo update --dry-run`; if a version in the design no longer resolves, use the latest stable of that crate and note it in your status file), `.cargo/config.toml`, `.gitignore`, `LICENSE` (MIT, "Sierra Softworks"), `README.md` (short: what rustak is, build order `trunk build` → `cargo build`, link to `.claude/plan/plan.md`), `rust-toolchain.toml` (channel stable), `config.example.toml` (copy the design §3.3 example verbatim; delete the `[stream.tcp]` block per the plan deltas).
- `rustak-cot/` (`Cargo.toml`, `build.rs` with protox + prost-build per design §1.3, `proto/tak_protocol_v1.proto` containing ONLY the message skeleton from design §1.3 — `TakMessage`, `TakControl`, `CotEvent` with the documented field numbers and an empty `Detail` — clean-room, our own text, no copied comments), `src/lib.rs` with `pub mod proto;`, `src/proto/mod.rs` including the generated file plus one round-trip unit test.
- `rustak-api/`, `rustak-core/`, `rustak-client/` (`Cargo.toml` + `src/lib.rs` with crate docs and the `pub mod` list from design §2.1/§2.2 as empty modules or `//! TODO(M0-xx)` stubs — no logic).
- `rustak-server/` (`Cargo.toml` with `[lib]` + `[[bin]] name = "rustak"`, `build.rs` per §1.4, `src/lib.rs`, `src/main.rs` printing the version and exiting 0, `migrations/.gitkeep`).
- `rustak-plugin-example/` (`Cargo.toml`, `src/main.rs` printing its name, `config.example.toml` placeholder).
- `rustak-ui/` (`Cargo.toml` with the pinned versions from design §1.2 — cannot use `workspace = true`, `Trunk.toml`, `index.html`, `src/main.rs` rendering "rustak" with yew, `styles.scss` with the 8-section skeleton headings from automate's stylesheet, `.gitignore` for `dist/`). Run `trunk build` once so `rustak-ui/Cargo.lock` exists and commit it.
- `scripts/check-file-length.sh` exactly as design §7.2 (executable).

**Do not touch:** `.github/**`, `Cross.toml`, `*/Dockerfile`, `e2e/**`, `.claude/**` except your status file. Do not commit, push or run `but`/`git` write commands — the orchestrator commits.

**Exit checks (run them all, paste results in your status file):**
- `cargo metadata --format-version 1 | jq '.workspace_members | length'` == 6
- `cargo build` (builds only `rustak-server` via `default-members`) and `cargo build --workspace` succeed **with no `protoc` on PATH** (`env -i PATH=/usr/bin:/bin HOME=$HOME cargo build -p rustak-cot`)
- `cargo test --workspace`, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo doc --workspace --no-deps -D warnings`
- `cd rustak-ui && trunk build` succeeds; `cargo clippy --target wasm32-unknown-unknown -- -D warnings` inside `rustak-ui`
- `scripts/check-file-length.sh` passes
- `cargo run -p rustak-server -- --version` prints the version; `cargo run -- --config config.example.toml --check` is NOT required yet (config loading is M0-06)

**Status file:** `.claude/plan/status/M0-01-workspace-skeleton.md` — what you created, exact versions used where they differ from the design, exit-check output, anything left open.
