# M0-09 — `services/` (AppContext) and `jobs/` (JobHost)

**Goal:** `rustak-server/src/services/{mod,mock}.rs` and `rustak-server/src/jobs/{mod,job,runnable,host,audit_prune,wal_checkpoint}.rs` per `design/01-foundations-storage-ci.md` §3.2 and §8 step 9: single-tenant `AppContext` (config, `Database`, `SecretStore`, `ContentStore` placeholder handle, `Session`, `reqwest::Client` with UA `SierraSoftworks/rustak/<version>`, `Shutdown`, `started_at`, `JwtKeys` slot filled by M0-10) implementing a `Services` trait (as listed in the design, plus `impl<S: Services> Services for &S`); `AppContext::new_mock` for tests (in-memory DB, ephemeral secrets, testing session). `jobs/` lifts `../automate/agent/src/job.rs` (Job/JobRunnable/registry via `inventory`/JobHost) with the tenant loop removed and a shutdown-aware dequeue loop that returns within 1 s of cancellation; `audit_prune` (from automate, using `[retention].audit*`) and `wal_checkpoint` (`[storage].checkpoint_interval`, PASSIVE).

**Read first:** conventions; design 01 §3.2, §3.4, §8; research 01 §2 (JobHost description); `../automate/agent/src/{services/mod.rs,job.rs,jobs/cron.rs (skip domain parts)}`. Depends on M0-04, M0-06, M0-07, M0-08.

**Files you own:** `rustak-server/src/services/**`, `rustak-server/src/jobs/**`, `rustak-server/src/prelude.rs`, `pub mod` lines in `lib.rs`. No `git`/`but` writes.

**Exit checks:** `cargo test -p rustak-server services:: jobs::` (lifted job tests; cancellation test), clippy/doc `-D warnings`, file-length script.

**Status file:** `.claude/plan/status/M0-09-services-jobs.md`.
