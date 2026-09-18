//! rustak server library: listeners, SQLite, PKI/ACME, Marti API, OAuth2,
//! admin API, embedded UI.
//!
//! `src/main.rs` is a thin CLI entry point; this crate exposes the app
//! itself so integration tests under `tests/` can build and drive it
//! in-process. M0 ships an empty crate root — `run()`, `AppContext` and
//! every module in `.claude/plan/design/01-foundations-storage-ci.md` §3.1
//! arrive across the rest of the M0 implementation briefs.
