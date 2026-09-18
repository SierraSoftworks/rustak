# M0-08 — `crypto/`: sealed secrets (lift from automate)

**Goal:** `rustak-server/src/crypto/{mod,key,store,context,keyfile}.rs` per `design/01-foundations-storage-ci.md` §5: lift `../automate/agent/src/crypto.rs` verbatim (with its tests), split into the four files to satisfy the length rule; `SecretStore::load(auth: &AuthConfig, database: &Path)` reads `auth.secret_key` / `auth.previous_secret_keys`; key-id domain `"rustak/secret-key-id/v1"`; `SecretContext` variants exactly as design 01 §5 (`CaKey`, `ServerCertKey`, `ServiceCertKey`, `AcmeAccount`, `JwtSigningKey`, `MissionTokenKey`, `IdpRefreshToken`, `ServiceSecret`) rendered as `rustak/v1/<kind>/<id>`; add a relocation-detection test (`CaKey{1}` vs `CaKey{2}`) and a test that a sealed value's JSON never contains the plaintext.

**Read first:** conventions; design 01 §5; research 01 §3 (crypto module description); `../automate/agent/src/crypto.rs`. Depends on M0-06 (`AuthConfig`).

**Files you own:** `rustak-server/src/crypto/**` (+ `pub mod crypto;` in `lib.rs`). No `git`/`but` writes.

**Exit checks:** `cargo test -p rustak-server crypto::`, clippy/doc `-D warnings`, file-length script.

**Status file:** `.claude/plan/status/M0-08-crypto.md`.
