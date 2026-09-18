# M0-12 — `main.rs` / `lib.rs::run` / `runtime.rs` and the bootstrap integration test

**Goal:** wire everything: `rustak-server/src/main.rs` (clap `--config`, `--env`, `--check`; env-file load; telemetry bootstrap; `Shutdown::listen_for_signals`; run; telemetry shutdown; exit code), `lib.rs::run(config, session, shutdown)` and `build_context`, `runtime.rs::run_all` (public HttpServer bound per `[web.public].listen` with the TLS config from M0-11, `JobHost`, checkpoint task, `try_join`, cancel-on-error, `db.close()` with WAL TRUNCATE) per design 01 §3.4; install the rustls aws-lc-rs provider defensively; first-start setup token generation. `tests/bootstrap.rs` (feature `testing`): temp dir → `run()` → `/robots.txt` 200 over TLS with the internal CA (`reqwest` trusting `<data_dir>/pki/ca.crt`) → `/api/v1/health` → setup wizard API path (admin + passkey registration via the test authenticator, or at least `setup/status` + `setup/admin` with the token) → cancel → returns Ok within 10 s and the `-wal` file is truncated.

**Read first:** conventions; design 01 §3.1, §3.4, §8 step 12, §9 (shutdown risks). Depends on M0-09…M0-11.

**Files you own:** `rustak-server/src/{main.rs,lib.rs,runtime.rs}`, `rustak-server/tests/bootstrap.rs`. No `git`/`but` writes.

**Exit checks:** `cargo test -p rustak-server --features testing --test bootstrap`; manual: `cargo run -- --config config.example.toml` (with a temp data dir) then SIGTERM → exit 0, no telemetry-flush warning; clippy/doc; file-length.

**Status file:** `.claude/plan/status/M0-12-runtime-bootstrap.md`.
