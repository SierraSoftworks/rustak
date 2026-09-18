# M0-07 — SQLite layer: connection, migrations, generic stores, schema, repositories

**Goal:** `rustak-server/src/db/**` and `rustak-server/migrations/*.sql` per `design/01-foundations-storage-ci.md` §4 (connection = one writer + read pool with the listed pragmas incl. `foreign_keys = ON`; migration runner over `include_dir!`; `row.rs` helpers; `kv`/`queue`/`queue_sqlite`/`cache`/`partition`/`audit` lifted from `../automate/agent/src/db/*` with the tenant column removed; repos one per aggregate), with the schema reconciled across designs:

- Base: design 01 §4.5 files `0001_kv_queues_audit` … `0007_profiles`.
- Apply design 03 §3/§4 tables and columns where they extend design 01: `certificates` (fingerprint, serial_hex, der, issued_via, credential_id, revocation columns), `device_group_state`, `refresh_tokens`, `revoked_jtis`, `settings`, `acme_accounts`, `acme_certificates`; `credentials.kind ∈ ('enrollment_token','client_password','service_token')`; `users` has **no** `password_hash` column; add `passkeys` (id, user_id, credential_id BLOB UNIQUE, public_key BLOB, sign_count INTEGER, transports TEXT json, label, created_at, last_used_at, backup_eligible/backup_state INTEGER).
- Plan delta: **no `cot_history` table**; add `stream_segments` (stream_kind, stream_key, segment_path UNIQUE, first_time, last_time, record_count, byte_length, created_at, sealed INTEGER) with indexes on (stream_kind, stream_key, first_time).
- Mission/file/profile tables from design 04 §4.1/§5.1/§6.1 where they add columns to design 01's versions (`mission_uids.details`, `mission_layers` tree columns, `resources` Title-case-compatible metadata, `profiles.apply_on_*` flags, `profile_files`).

All tables STRICT; timestamps TEXT RFC 3339 millis bound from Rust (a migration test rejects `CURRENT_TIMESTAMP` defaults); pure-key tables WITHOUT ROWID; JSON columns `CHECK (json_valid(...))`. Repos for M0: users, groups, members (+device_group_state), devices, credentials, passkeys, certificates, services, oauth_keys/refresh_tokens/revoked_jtis, settings, stream_segments (mission/file/profile repos are later briefs — create the tables now, not the repos).

**Read first:** conventions; design 01 §4 (all); design 03 §3 (`certificates` DDL), §4 (identity DDL); design 04 §4.1, §5.1, §6.1; `../automate/agent/src/db/{mod,sqlite,cache,partition,audit}.rs`. Depends on M0-03/M0-04 types (`Username`, ids, `PasswordHash`).

**Files you own:** `rustak-server/src/db/**`, `rustak-server/migrations/**`. Each file < 300 functional lines (split `queue.rs`/`queue_sqlite.rs`, one repo per file). No `git`/`but` writes.

**Exit checks:** `cargo test -p rustak-server db::` (migration-at-version, fresh-vs-upgraded parity, `foreign_key_check`, `integrity_check`, WAL applied on a temp file, repo CRUD/constraint tests), clippy/doc `-D warnings`, file-length script; `sqlite3 <tmp>.sqlite .schema` pasted into the status file.

**Status file:** `.claude/plan/status/M0-07-database-layer.md`.
