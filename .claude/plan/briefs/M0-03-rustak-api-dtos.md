# M0-03 — `rustak-api`: identity newtypes and admin DTOs

**Goal:** implement `rustak-api` per `design/01-foundations-storage-ci.md` §2.1 (module table) with the plan deltas: no `LocalPassword`; credential kinds are `EnrollmentToken` (one-time), `ClientPassword` (opt-in, expiring, compatibility only), `ServiceToken`; add `passkey.rs` DTOs (`PasskeyRegistrationStart/Finish`, `PasskeyLoginStart/Finish`, `PasskeySummary {id, label, created_at, last_used_at}`) and `AuthMode::{Oidc{…}, Passkey}` (no `Local`).

**Read first:** brief conventions (`.claude/plan/conventions.md`), plan → "Identity & auth model", design 01 §2.1, design 03 §4 (`Username` normalisation rules: lowercase, 1..=64, `[a-z0-9._@+-]`, must start alphanumeric, forbidden `}{"\\,=/;<>`, reserved names) — design 03's rules win over design 01's for `Username`. Reference DTO style: `/Users/bpannell/dev/gh/SierraSoftworks/automate/api/src/{ids.rs,tenant.rs,audit.rs,connection.rs,lib.rs}` (round-trip test pattern; `#[serde(default, skip_serializing_if)]` conventions; `as_str/label/parse` on enums).

**Files you own:** everything under `rustak-api/` (keep the crate wasm-safe: serde, serde_json, chrono, uuid only; `cargo check -p rustak-api --target wasm32-unknown-unknown` must pass). One file per module from the design table, each < 300 functional lines, each with round-trip tests. Do not touch other crates or `.claude/**` except your status file. No `git`/`but` writes.

**Exit checks:** `cargo test -p rustak-api`, `cargo clippy -p rustak-api --all-targets -- -D warnings`, `cargo doc -p rustak-api --no-deps -D warnings`, wasm32 `cargo check`, `scripts/check-file-length.sh`.

**Status file:** `.claude/plan/status/M0-03-rustak-api-dtos.md`.
