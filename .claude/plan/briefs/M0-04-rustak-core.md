# M0-04 — `rustak-core`: config, telemetry, runtime, identity primitives

**Goal:** implement `rustak-core` per `design/01-foundations-storage-ci.md` §2.2 (file table with line budgets), lifting automate code where the design says "lift": `config/interpolation.rs` (verbatim from `../automate/agent/src/parsers/interpolation.rs` incl. tests), `config/duration.rs` (patterned on `../automate/agent/src/serde_duration.rs`), `telemetry.rs` (bootstrap/shutdown patterns from `../automate/agent/src/main.rs`). Deltas: `identity/password.rs` hashes with argon2id and exposes `hash_blocking`/`verify_blocking`; `identity/principal.rs` `AuthMethod` has no `StreamAuth`/`Anonymous` password variants — variants are `ClientCert{fingerprint, serial}`, `Bearer{jti, scope}`, `Basic{credential_id, kind}`, `Passkey{credential_id}`, `SetupToken`; there is no anonymous principal constructor.

**Read first:** conventions; plan → Architecture, "Design artefacts and reconciled decisions"; design 01 §2.2 and §3.3 (config shape it must be able to load); design 03 §4 (`Username`/`GroupSet` semantics: IN = may publish, OUT = may receive; `can_reach(sender, receiver) = sender.in ∩ receiver.out ≠ ∅`); research 01 §2 (automate config/interpolation behaviour). `rustak-api` (M0-03) provides the identity newtypes — depend on it and re-export from `identity/`.

**Files you own:** everything under `rustak-core/`. Each file < 300 functional lines; tests in-file; a doctest that loads a TOML snippet containing `${{ env.X }}`. `config::load_env_file` must skip anything that is not a regular file (FIFO guard). No `git`/`but` writes; no edits outside the crate except your status file.

**Exit checks:** `cargo test -p rustak-core`, clippy `-D warnings`, doc `-D warnings`, `scripts/check-file-length.sh`.

**Status file:** `.claude/plan/status/M0-04-rustak-core.md`.
