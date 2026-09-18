# M0-10 — content store, root CA, JWT issuer

**Goal (three small modules, all needed by the M0 web layer):**
1. `rustak-server/src/store/{mod,content.rs,append_log.rs}`: `ContentStore` per design 01 §4.6 (put/open/exists/remove/iter; atomic rename; sha256 hex), and `AppendLog` per plan → Storage: rolling segment files under `<streams_dir>/<kind>/<key>/`, records = varint length + protobuf payload bytes (the caller supplies encoded bytes; the log is payload-agnostic), `open` truncates a trailing partial frame, `append(time, bytes)`, `read_range(from, to)` via `stream_segments` index rows (design 01 §4/M0-07 table), `seal`/`roll` at a configurable size (default 8 MiB), `prune_before(time)`. Tests: crash-truncation, roll, range reads, 100k appends throughput sanity (`#[ignore]`).
2. `rustak-server/src/pki/{mod,keys,ca,pem}.rs` — only what the setup wizard needs now: `KeyType {Rsa2048, Rsa3072, EcdsaP256}`, `generate_key` (pure-Rust `rsa` for RSA → PKCS#8 → rcgen `KeyPair`; rcgen for ECDSA), `load_or_create_root_ca` (rcgen CA params per design 03 §3 `ca.rs`; cert DER in kv partition `pki`, key `Sealed` with `SecretContext::CaKey`), `ca.crt` written to `<data_dir>/pki/`, `pem.rs` helpers (`bare_base64_64col`, `pem_certificate`, `parse_pem_chain`, `sha256_fingerprint`). Full issuance/verification is M2.
3. `rustak-server/src/auth/{mod,jwt}.rs`: `JwtKeys`/`JwtIssuer` per design 03 §5 `oauth_server/jwt.rs`: RSA-2048 signing key sealed in the DB (`SecretContext::JwtSigningKey{kid}`), header exactly `{"alg":"RS256","typ":"JWT"}`, flat `AccessClaims {sub, aud(string), iss, iat, nbf, exp, jti, scope, dev?}`, `issue`/`verify` (RS256 only, aud/iss/exp/nbf required, 60 s leeway, active + previous keys), `token_key_json`, `jwks`; tests incl. the CloudTAK-style lenient parse simulation (decode whole token as base64 ignoring `.`, split on `}`, parse `split[1] + "}"` → `sub`).

**Read first:** conventions; plan → Storage, Identity & auth model; design 01 §4.6, §6.2 (`auth/jwt.rs`), §8 step 10; design 03 §3 (`keys.rs`, `ca.rs`, `pem.rs`), §5 (`jwt.rs`, `keys.rs`); research 03 §2.1 (node-tak JWT parser). Depends on M0-07, M0-08, M0-09.

**Files you own:** `rustak-server/src/store/**`, `rustak-server/src/pki/**`, `rustak-server/src/auth/{mod,jwt}.rs`. No `git`/`but` writes.

**Exit checks:** `cargo test -p rustak-server store:: pki:: auth::jwt`, clippy/doc `-D warnings`, file-length script.

**Status file:** `.claude/plan/status/M0-10-store-pki-jwt.md`.
