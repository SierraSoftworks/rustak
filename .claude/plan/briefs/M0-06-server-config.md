# M0-06 — `rustak-server` config module and `config.example.toml`

**Goal:** `rustak-server/src/config/{mod,server,storage,web,stream,auth,pki,acme,retention}.rs` per `design/01-foundations-storage-ci.md` §3.3, reconciled with `design/03-identity-pki-acme-auth.md` §8 and the plan deltas: **no `[stream.tcp]`**; `[web.public.tls] mode = "internal" | "files" | "acme"` (default `internal`) plus `allow_insecure_http = false` (when true, `mode = "none"` is permitted for dev/e2e); `[web.marti] client_cert = "required"` (only value; keep the enum with `Required` for future); `[auth]` gains `rate_limit`, `setup_token_file`, `enrollment_token_ttl` (15m), `client_password_ttl` (90d), `client_passwords_enabled` (true — needed for CloudTAK; documented as compatibility), `allow_access_token_retrieval`, `anon_group_default`; `[auth.oidc]` gains the group-mapping fields; `[pki]` gains `name_entries`, `server_names`, `server_ips`, `require_known_cert`, `csr_min_rsa_bits`, `csr_allow_ecdsa`, `p12_password`, `p12_legacy`, `channels_marker_eku`; `[acme]` per design 03 (`directory` accepts `letsencrypt|letsencrypt-staging|<url>`, `contact`, `accept_tos`, `challenge`, `renew_before`); `[storage]` gains `streams_dir` (append-log root, default `<data_dir>/streams`).

`Config::load(path)` uses `rustak_core::config::load` then `validate()`: ACME mode requires `:443` (tls-alpn-01) or `plain_bind`/`:80` (http-01) to be bound and non-empty domains; `mode = "none"` requires `allow_insecure_http`; every `deny_unknown_fields`; defaults written out in `impl Default` (not derived) as automate documents; `config.example.toml` at the repo root documents every key with its default (rewrite the M0-01 placeholder) and is loaded by a test (`include_str!`), plus the misplaced-key-refused and defaults-agree tests lifted from `../automate/agent/src/config.rs`. Add `--check` handling to `main.rs`: load + validate, exit 0/1.

**Read first:** conventions; plan → Architecture (Listeners), "Design artefacts and reconciled decisions"; design 01 §3.3; design 03 §8; `../automate/agent/src/config.rs`.

**Files you own:** `rustak-server/src/config/**`, `rustak-server/src/main.rs` (only the `--check` path and arg parsing), `config.example.toml`. No `git`/`but` writes.

**Exit checks:** `cargo test -p rustak-server config::`, `cargo run -p rustak-server -- --config config.example.toml --check` exits 0, clippy/doc `-D warnings`, file-length script.

**Status file:** `.claude/plan/status/M0-06-server-config.md`.
