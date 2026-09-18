# M0-15 — `rustak-client::sidecar` harness and `rustak-plugin-example`

**Goal:** `rustak-client/src/sidecar/{mod,run,config}.rs`: `Sidecar` trait (`async fn start(&mut self, ctx: SidecarContext)`, `async fn tick(&mut self)`, `async fn on_event(&mut self, ev: Event)` — stream wiring itself arrives in M1; for now the harness loads config via `rustak-core`, bootstraps telemetry, logs the `ServiceDescriptor`, runs `tick` on an interval and waits for `Shutdown`), `run::<S: Sidecar>(args)` entrypoint with clap; `rustak-plugin-example/src/main.rs` (~100 lines) implementing `Sidecar` with a heartbeat log; `rustak-plugin-example/config.example.toml`; `docs/plugins.md` first draft (identity model, control API to come, how to copy the crate).

**Read first:** conventions; plan → Plugin (sidecar) contract; design 01 §1.2 (crate deps), §8 step 15. Depends on M0-04.

**Files you own:** `rustak-client/src/sidecar/**`, `rustak-client/src/lib.rs` module list, `rustak-plugin-example/**` (not its Dockerfile), `docs/plugins.md`. No `git`/`but` writes.

**Exit checks:** `cargo run -p rustak-plugin-example -- --config rustak-plugin-example/config.example.toml` logs and exits cleanly on SIGINT; `cargo test -p rustak-client`; clippy/doc; file-length.

**Status file:** `.claude/plan/status/M0-15-client-sidecar-example.md`.
