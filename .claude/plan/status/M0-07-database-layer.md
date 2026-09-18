# M0-07 — SQLite layer: connection, migrations, generic stores, schema, repositories

Brief: `.claude/plan/briefs/M0-07-database-layer.md`
Design: `design/01-foundations-storage-ci.md` §4 (all), `design/03-identity-pki-acme-auth.md` §3–§4,
`design/04-marti-api-missions-files-profiles.md` §4.1/§5.1/§6.1, `plan.md` (Storage; deltas).

## What was built

`rustak-server/src/db/**` (24 files) and `rustak-server/migrations/0001…0007.sql`. The empty
`db/mod.rs` stub was replaced; `lib.rs` was not touched, and nothing under `src/config/` or
`src/crypto/` was either. `rustak-server/Cargo.toml` already carried every dependency this needed
(`rusqlite`, `tokio-rusqlite`, `include_dir`, `async-trait`, `uuid`), so it is unchanged.

| File | Lines | Contents |
|---|---:|---|
| `db/mod.rs` | 24 | module tree, flat re-exports, the two advice constants |
| `db/connection.rs` | 230 | `Database` (one writer + read-only pool), `Checkpoint`, pragmas, `read`/`write`/`checkpoint`/`close`, the `testing`-gated `open_in_memory_at_migration`/`upgrade` |
| `db/migrations.rs` | 168 | `include_dir!` runner, `schema_migrations`, `load`/`migrate`/`migrate_to`/`current_version`, `schema_of`/`tables` for the parity tests |
| `db/row.rs` | 112 | `Timestamp` plus `ts`/`opt_ts`/`bool_col`/`id_col`/`opt_id_col`/`json_col`/`opt_json_col`/`enum_col`/`to_json` |
| `db/kv.rs` | 179 | `KeyValueStore` + `StateKey` + the impl for `Database` |
| `db/queue.rs` | 108 | `Queue`, `QueueMessage`, `PeekedMessage`, `QueuedMessage`, `POLL_INTERVAL` |
| `db/queue_sqlite.rs` | 277 | the `Queue` impl: reserve-and-hide in one transaction |
| `db/cache.rs` | 63 | `Cache`, blanket impl over every `KeyValueStore` |
| `db/partition.rs` | 92 | `Partition<D, T>` over kv, queue and cache |
| `db/audit.rs` | 212 | `AuditEntry`/`AuditQuery`/`AuditStore` + record/read/prune |
| `db/repos/mod.rs` | 95 | `Page`, the thirteen `Database::<aggregate>()` accessors, re-exports |
| `db/repos/{users,groups,members,devices,credentials,passkeys,certificates,services,oauth_keys,refresh_tokens,revoked_jtis,settings,stream_segments}.rs` | 60–293 | one repository per aggregate |

152 tests under `db::`, all passing. Largest file is `repos/certificates.rs` at 293 functional
lines; every file is under the 300 limit with its single trailing column-0 `#[cfg(test)] mod tests`.

## The reconciled schema

Seven migrations, named and numbered as design 01 §4.5 lists them, with design 03's and design 04's
columns folded in and the plan's deltas applied.

| Migration | Tables |
|---|---|
| `0001_kv_queues_audit` | `kv`, `queues`, `audit_log` |
| `0002_identity` | `users`, `groups` (+ `__ANON__` seed), `group_members`, `devices`, `device_group_state`, `credentials`, `passkeys`, `certificates`, `services` |
| `0003_auth_pki` | `oauth_keys`, `oauth_tokens`, `refresh_tokens`, `revoked_jtis`, `settings`, `acme_accounts`, `acme_certificates` |
| `0004_cot` | `cot_latest`, `stream_segments` |
| `0005_files` | `resources`, `resource_keywords` |
| `0006_missions` | `missions`, `mission_subscriptions`, `mission_changes`, `mission_contents`, `mission_uids`, `mission_layers`, `mission_logs`, `mission_invitations` |
| `0007_profiles` | `profiles`, `profile_files` |

Every table is `STRICT` (a test asserts it); pure-key tables are `WITHOUT ROWID`; JSON columns carry
`CHECK (json_valid(…))`; `foreign_keys = ON` on every connection; every timestamp is `TEXT` holding
RFC 3339 with milliseconds, bound from Rust.

### Deltas applied, as the brief lists them

- **No `cot_history`.** History goes to `store::append_log` segment files; `stream_segments`
  (`stream_kind`, `stream_key`, `segment_path` UNIQUE, `first_time`, `last_time`, `record_count`,
  `byte_length`, `sealed`, `created_at`) indexes them, with `(stream_kind, stream_key, first_time)`
  and a partial index on the open segment.
- **`passkeys`** added: `credential_id BLOB UNIQUE`, `public_key BLOB`, `sign_count`, `transports`
  (JSON), `label`, `backup_eligible`/`backup_state`, `created_at`, `last_used_at`.
- **No `users.password_hash`.** A migration test asserts the column is absent.
- **`credentials.kind ∈ ('enrollment_token','client_password','service_token')`**, matching M0-03's
  `CredentialKind`. A test asserts `device_password`, `local_password` and `password` are refused.
- Design 03's `device_group_state`, `refresh_tokens`, `revoked_jtis`, `settings`, `acme_accounts`,
  `acme_certificates` are all present, and `certificates` carries its `fingerprint`, `serial_hex`,
  `der`, `issued_via`, `credential_id` and revocation columns.
- Design 04's `mission_uids.details`, `mission_layers` tree columns, Title-case-compatible
  `resources` metadata, and `profiles.apply_on_enrollment`/`apply_on_connect` are folded in.

### Decisions where the three designs disagreed

Each of these picks one shape where two documents described the same thing differently. None
changes what the brief asked for; they are recorded because a later brief will read the column.

1. **`certificates` stores `der BLOB`, not `cert_pem`.** Design 01 had PEM, design 03 DER, and the
   brief names `der`. PEM is derivable from DER, so keeping both would be two spellings of one fact.
2. **`certificates.source` uses M0-03's `CertificateSource` values** (`enrollment`,
   `admin_package`, `internal`, `acme`, `imported`) rather than design 01's `internal|acme|file`,
   so the column and the DTO cannot drift. `issued_via` keeps design 03's finer-grained list and is
   nullable, because the CA and the server certificates came out of no endpoint.
3. **`group_members` is one row per single direction** (design 03), not design 01's
   `IN|OUT|BOTH`. M0-03's status note 4 asks for exactly this; `Direction::Both` is expanded before
   any write and never reaches a column.
4. **`devices` has no `active_groups` JSON column.** Design 03's `device_group_state` table
   supersedes it, and §4.4's rule sends a value that is filtered and joined on to a table.
5. **`refresh_tokens` is the rotation store; `oauth_tokens.kind` is `('code','idp_state')`.**
   Design 01 had a `refresh` kind in `oauth_tokens`, which has no room for the `family` column that
   reuse detection needs, and two homes for one record would be worse than either.
6. **`profiles.preferences` stays a JSON column** (design 01) rather than design 04's
   `profile_prefs` table: §4.4 names profile preference maps as the example of an opaque map read
   and written whole.
7. **`settings.value` carries `CHECK (json_valid(value))`**, so a scalar is stored as `"rustak"`
   rather than `rustak`. One decoder, whatever the setting's type.
8. **`__ANON__` is seeded at bitpos 1**, which is design 03's number and
   `rustak_core::identity::ANON_BITPOS`, not design 01's 0.
9. **The `groups` seed uses `strftime('%Y-%m-%dT%H:%M:%fZ','now')`.** A row inserted by a migration
   has no Rust caller to bind from, and that format is byte-for-byte what `Timestamp` writes. The
   `CURRENT_TIMESTAMP` test still refuses the shortcut everywhere else.

## Notes for the orchestrator and later briefs

1. **`Database::open` takes plain parameters**, as the brief allows, because M0-06 was in flight:
   `Database::open(path: &Path, reader_connections: usize, busy_timeout: Duration)`.
   `db::connection::{DEFAULT_READER_CONNECTIONS, DEFAULT_BUSY_TIMEOUT}` are the defaults.
   M0-06 has since landed a `StorageConfig`; wiring it up is a one-line call site in the runtime
   bootstrap (M0-12), and no change here.
2. **Never bind a bare `chrono::DateTime`.** `rusqlite`'s own `chrono` support writes
   `%F %T%.f%:z` — a space instead of the `T`, `+00:00` instead of `Z`, and nanoseconds instead of
   milliseconds — so two rows written through the two paths would not compare. Everything goes
   through `db::row::Timestamp`, which is the only `ToSql`/`FromSql` for a time in this layer.
3. **Mission, file and profile repositories are deliberately absent.** The brief creates those
   tables now and leaves the repositories to their own briefs.
4. **`Queue::try_dequeue_any`** is an addition to automate's trait: the job host has to interleave
   the queue with the shutdown token, and the blocking `dequeue_any` sits inside a sleep it cannot
   cancel. `queues.attempts` is likewise new, and is what a backoff should read.
5. **Three `partition()` methods exist** (`KeyValueStore`, `Queue`, `Cache`), so a bare
   `db.partition(…)` is ambiguous. Call it as `KeyValueStore::partition::<T>(&db, "name")`.
6. **`cargo fmt -p rustak-server` formats the whole crate**, so the run recorded below may have
   reformatted files belonging to the concurrent `config/` brief. Nothing was edited by hand
   outside `src/db/**` and `migrations/**`.
7. `migrations/.gitkeep` is deleted, replaced by the seven `.sql` files.

## Exit checks

```
$ cargo test -p rustak-server db::
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.41s
     Running unittests src/lib.rs (target/debug/deps/rustak_server-0a4c039909ae8643)
running 152 tests
test result: ok. 152 passed; 0 failed; 0 ignored; 0 measured; 110 filtered out; finished in 1.54s
     Running unittests src/main.rs (target/debug/deps/rustak-8597887d89934ee3)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

$ cargo clippy -p rustak-server --all-targets -- -D warnings
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.87s
exit=0

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.46s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_server/index.html and 1 other file
exit=0

$ cargo fmt -p rustak-server -- --check
exit=0

$ ./scripts/check-file-length.sh
exit=0
```

The 152 `db::` tests break down as: `migrations` 10 (naming and contiguity, no `CURRENT_TIMESTAMP`,
migration-at-version, fresh-vs-upgraded parity across every table at every stopping point,
`foreign_key_check`, `integrity_check`, every table `STRICT`, the plan's deltas present),
`connection` 6 (WAL and `synchronous=NORMAL` on a temp file, foreign keys on every connection,
readers refusing writes, rollback on a failed write, the log truncated at close), `row` 9,
`kv` 6, `queue_sqlite` 10, `cache` 3, `audit` 5, `repos` 103.

### `PRAGMA foreign_key_check` / `integrity_check` / migration version

Run against a file database built by applying the seven migrations in order, as the runner does:

```
$ sqlite3 rustak.sqlite "PRAGMA foreign_key_check;"
(no output)

$ sqlite3 rustak.sqlite "PRAGMA integrity_check;"
ok

$ sqlite3 rustak.sqlite "SELECT id, name FROM schema_migrations ORDER BY id;"
1|0001_kv_queues_audit.sql
2|0002_identity.sql
3|0003_auth_pki.sql
4|0004_cot.sql
5|0005_files.sql
6|0006_missions.sql
7|0007_profiles.sql
```

## `sqlite3 rustak.sqlite .schema`

```sql
CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL) STRICT;
CREATE TABLE kv (
  partition  TEXT NOT NULL,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL CHECK (json_valid(value)),
  updated_at TEXT NOT NULL,
  PRIMARY KEY (partition, key)
) STRICT, WITHOUT ROWID;
CREATE TABLE queues (
  partition       TEXT NOT NULL,
  key             TEXT NOT NULL,
  payload         TEXT NOT NULL CHECK (json_valid(payload)),
  scheduled_at    TEXT NOT NULL,
  hidden_until    TEXT NOT NULL,
  reserved_by     TEXT,
  traceparent     TEXT,
  tracestate      TEXT,
  idempotency_key TEXT,
  attempts        INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (partition, key)
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_queues_partition_hidden ON queues (partition, hidden_until);
CREATE INDEX idx_queues_hidden_scheduled ON queues (hidden_until, scheduled_at);
CREATE TABLE audit_log (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  occurred_at TEXT NOT NULL,
  category    TEXT NOT NULL,
  action      TEXT NOT NULL,
  outcome     TEXT NOT NULL,
  actor       TEXT,
  subject     TEXT,
  message     TEXT,
  detail      TEXT CHECK (detail IS NULL OR json_valid(detail))
) STRICT;
CREATE TABLE sqlite_sequence(name,seq);
CREATE INDEX idx_audit_subject  ON audit_log (subject, id DESC);
CREATE INDEX idx_audit_category ON audit_log (category, id DESC);
CREATE INDEX idx_audit_actor    ON audit_log (actor, id DESC);
CREATE INDEX idx_audit_occurred ON audit_log (occurred_at);
CREATE TABLE users (
  id             INTEGER PRIMARY KEY,
  username       TEXT NOT NULL COLLATE NOCASE,
  kind           TEXT NOT NULL CHECK (kind IN ('person','service')),
  display_name   TEXT,
  email          TEXT,
  is_admin       INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0,1)),
  -- Set by an administrator to pin admin on or off regardless of what the
  -- identity provider's claims say; NULL leaves it to the ACL.
  admin_override INTEGER CHECK (admin_override IS NULL OR admin_override IN (0,1)),
  disabled       INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0,1)),
  source         TEXT NOT NULL CHECK (source IN ('local','oidc','service')),
  oidc_issuer    TEXT,
  oidc_subject   TEXT,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL,
  last_seen_at   TEXT,
  last_login_at  TEXT
) STRICT;
CREATE UNIQUE INDEX idx_users_username ON users (username);
CREATE UNIQUE INDEX idx_users_oidc_subject ON users (oidc_issuer, oidc_subject)
  WHERE oidc_subject IS NOT NULL;
CREATE TABLE groups (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  bitpos      INTEGER NOT NULL,
  description TEXT,
  source      TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual','oidc','system')),
  created_at  TEXT NOT NULL,
  -- Soft delete: a bitpos may only be reused once every live subscription has
  -- been refreshed, so the row outlives the channel.
  deleted_at  TEXT
) STRICT;
CREATE UNIQUE INDEX idx_groups_name ON groups (name);
CREATE UNIQUE INDEX idx_groups_bitpos ON groups (bitpos);
CREATE TABLE group_members (
  user_id   INTEGER NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  group_id  INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
  direction TEXT NOT NULL CHECK (direction IN ('IN','OUT')),
  source    TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual','oidc')),
  PRIMARY KEY (user_id, group_id, direction)
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_group_members_group ON group_members (group_id);
CREATE TABLE devices (
  id                  INTEGER PRIMARY KEY,
  uid                 TEXT NOT NULL,
  user_id             INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  callsign            TEXT,
  platform            TEXT,
  version             TEXT,
  device_model        TEXT,
  os                  TEXT,
  incognito           INTEGER NOT NULL DEFAULT 0 CHECK (incognito IN (0,1)),
  -- Forward reference: `certificates` is created below in this same migration.
  last_certificate_id INTEGER REFERENCES certificates(id) ON DELETE SET NULL,
  first_seen_at       TEXT NOT NULL,
  last_seen_at        TEXT NOT NULL,
  last_ip             TEXT
) STRICT;
CREATE UNIQUE INDEX idx_devices_uid ON devices (uid);
CREATE INDEX idx_devices_user ON devices (user_id);
CREATE TABLE device_group_state (
  device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  group_id  INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
  direction TEXT NOT NULL CHECK (direction IN ('IN','OUT')),
  active    INTEGER NOT NULL CHECK (active IN (0,1)),
  PRIMARY KEY (device_id, group_id, direction)
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_device_group_state_group ON device_group_state (group_id);
CREATE TABLE credentials (
  id          INTEGER PRIMARY KEY,
  user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind        TEXT NOT NULL
                CHECK (kind IN ('enrollment_token','client_password','service_token')),
  label       TEXT NOT NULL,
  -- argon2id PHC string. Never sealed: a hash is not recoverable, so there is
  -- nothing for encryption at rest to protect here that the hash does not.
  secret_hash TEXT NOT NULL,
  -- sha256(secret)[..16] hex, so the candidate row can be found without running
  -- argon2 against every credential the user holds. Not a verifier.
  lookup_hint TEXT NOT NULL,
  max_uses    INTEGER,
  uses        INTEGER NOT NULL DEFAULT 0,
  expires_at  TEXT,
  last_used_at TEXT,
  revoked_at  TEXT,
  created_by  TEXT,
  created_at  TEXT NOT NULL
) STRICT;
CREATE INDEX idx_credentials_user ON credentials (user_id, kind) WHERE revoked_at IS NULL;
CREATE INDEX idx_credentials_hint ON credentials (lookup_hint);
CREATE TABLE passkeys (
  id              INTEGER PRIMARY KEY,
  user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  credential_id   BLOB NOT NULL,
  public_key      BLOB NOT NULL,
  sign_count      INTEGER NOT NULL DEFAULT 0,
  transports      TEXT CHECK (transports IS NULL OR json_valid(transports)),
  label           TEXT NOT NULL,
  backup_eligible INTEGER NOT NULL DEFAULT 0 CHECK (backup_eligible IN (0,1)),
  backup_state    INTEGER NOT NULL DEFAULT 0 CHECK (backup_state IN (0,1)),
  created_at      TEXT NOT NULL,
  last_used_at    TEXT
) STRICT;
CREATE UNIQUE INDEX idx_passkeys_credential_id ON passkeys (credential_id);
CREATE INDEX idx_passkeys_user ON passkeys (user_id);
CREATE TABLE certificates (
  id            INTEGER PRIMARY KEY,
  kind          TEXT NOT NULL CHECK (kind IN ('ca','server','client','service')),
  source        TEXT NOT NULL DEFAULT 'internal'
                  CHECK (source IN ('enrollment','admin_package','internal','acme','imported')),
  -- The exact endpoint the certificate came out of, for audit. NULL for the CA
  -- and for server certificates, which no client asked for.
  issued_via    TEXT CHECK (issued_via IS NULL OR issued_via IN
                  ('enroll_v2_json','enroll_v2_xml','enroll_v1_p12','admin_package','acme_internal')),
  serial_hex    TEXT NOT NULL,
  fingerprint   TEXT NOT NULL,
  subject_cn    TEXT NOT NULL,
  san           TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(san)),
  user_id       INTEGER REFERENCES users(id)   ON DELETE SET NULL,
  device_id     INTEGER REFERENCES devices(id) ON DELETE SET NULL,
  client_uid    TEXT,
  -- The credential that was spent to obtain this certificate; revoking it
  -- revokes the certificates it issued.
  credential_id INTEGER REFERENCES credentials(id) ON DELETE SET NULL,
  issuer_id     INTEGER REFERENCES certificates(id) ON DELETE SET NULL,
  der           BLOB NOT NULL,
  -- Sealed envelope holding the private key, for the CA, server and service
  -- certificates we generated the key for. NULL for anything issued from a CSR.
  key_sealed    TEXT CHECK (key_sealed IS NULL OR json_valid(key_sealed)),
  not_before    TEXT NOT NULL,
  not_after     TEXT NOT NULL,
  last_seen_at  TEXT,
  revoked_at    TEXT,
  revocation_reason TEXT,
  revoked_by    TEXT,
  created_at    TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_certificates_fingerprint ON certificates (fingerprint);
CREATE UNIQUE INDEX idx_certificates_issuer_serial ON certificates (issuer_id, serial_hex);
CREATE INDEX idx_certificates_user ON certificates (user_id);
CREATE INDEX idx_certificates_device ON certificates (device_id);
CREATE INDEX idx_certificates_credential ON certificates (credential_id);
CREATE INDEX idx_certificates_expiry ON certificates (kind, not_after) WHERE revoked_at IS NULL;
CREATE INDEX idx_certificates_revoked ON certificates (revoked_at) WHERE revoked_at IS NOT NULL;
CREATE TABLE services (
  id                INTEGER PRIMARY KEY,
  name              TEXT NOT NULL,
  -- The sidecar's own account, always users.kind = 'service'.
  user_id           INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  display_name      TEXT,
  description       TEXT,
  version           TEXT,
  capabilities      TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(capabilities)),
  endpoints         TEXT CHECK (endpoints IS NULL OR json_valid(endpoints)),
  config            TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config)),
  status            TEXT NOT NULL DEFAULT 'unknown'
                      CHECK (status IN ('unknown','healthy','degraded','unhealthy')),
  status_message    TEXT,
  last_heartbeat_at TEXT,
  enabled           INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
  created_at        TEXT NOT NULL,
  updated_at        TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_services_name ON services (name);
CREATE UNIQUE INDEX idx_services_user ON services (user_id);
CREATE TABLE oauth_keys (
  kid            TEXT PRIMARY KEY,
  alg            TEXT NOT NULL CHECK (alg IN ('RS256','HS256')),
  purpose        TEXT NOT NULL CHECK (purpose IN ('access_token','mission_token')),
  -- NULL for the HS256 mission-token secret, which has no public half.
  public_jwk     TEXT CHECK (public_jwk IS NULL OR json_valid(public_jwk)),
  private_sealed TEXT NOT NULL CHECK (json_valid(private_sealed)),
  created_at     TEXT NOT NULL,
  retired_at     TEXT
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_oauth_keys_active ON oauth_keys (purpose, created_at DESC) WHERE retired_at IS NULL;
CREATE TABLE oauth_tokens (
  id           TEXT PRIMARY KEY,          -- code / state: 32 random bytes, base64url
  kind         TEXT NOT NULL CHECK (kind IN ('code','idp_state')),
  user_id      INTEGER REFERENCES users(id) ON DELETE CASCADE,
  client_id    TEXT,
  scope        TEXT,
  redirect_uri TEXT,
  -- sha256 of the opaque secret handed out. High entropy, so no argon2.
  token_hash   TEXT,
  -- nonce, PKCE challenge, sealed identity-provider refresh token.
  data         TEXT CHECK (data IS NULL OR json_valid(data)),
  created_at   TEXT NOT NULL,
  expires_at   TEXT NOT NULL,
  consumed_at  TEXT
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_oauth_tokens_user ON oauth_tokens (user_id, kind);
CREATE INDEX idx_oauth_tokens_expires ON oauth_tokens (expires_at);
CREATE TABLE refresh_tokens (
  id         INTEGER PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL,
  family     TEXT NOT NULL,
  scope      TEXT NOT NULL,
  client     TEXT,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  used_at    TEXT,
  revoked_at TEXT
) STRICT;
CREATE UNIQUE INDEX idx_refresh_tokens_hash ON refresh_tokens (token_hash);
CREATE INDEX idx_refresh_tokens_family ON refresh_tokens (family);
CREATE INDEX idx_refresh_tokens_user ON refresh_tokens (user_id);
CREATE INDEX idx_refresh_tokens_expires ON refresh_tokens (expires_at);
CREATE TABLE revoked_jtis (
  jti        TEXT PRIMARY KEY,
  expires_at TEXT NOT NULL,
  revoked_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_revoked_jtis_expires ON revoked_jtis (expires_at);
CREATE TABLE settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL CHECK (json_valid(value)),
  updated_at TEXT NOT NULL,
  updated_by TEXT
) STRICT, WITHOUT ROWID;
CREATE TABLE acme_accounts (
  id                 INTEGER PRIMARY KEY,
  directory_url      TEXT NOT NULL,
  contact            TEXT NOT NULL,
  account_url        TEXT NOT NULL,
  -- instant_acme::AccountCredentials, sealed.
  credentials_sealed TEXT NOT NULL CHECK (json_valid(credentials_sealed)),
  created_at         TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_acme_accounts_directory_contact ON acme_accounts (directory_url, contact);
CREATE TABLE acme_certificates (
  id              INTEGER PRIMARY KEY,
  domains         TEXT NOT NULL CHECK (json_valid(domains)),   -- JSON array, sorted
  chain_pem       TEXT NOT NULL,
  key_sealed      TEXT NOT NULL CHECK (json_valid(key_sealed)),
  not_before      TEXT,
  not_after       TEXT,
  challenge_type  TEXT CHECK (challenge_type IS NULL OR challenge_type IN ('tls-alpn-01','http-01')),
  attempts        INTEGER NOT NULL DEFAULT 0,
  last_attempt_at TEXT,
  last_error      TEXT,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_acme_certificates_domains ON acme_certificates (domains);
CREATE TABLE cot_latest (
  uid         TEXT PRIMARY KEY,
  type        TEXT NOT NULL,
  callsign    TEXT,
  user_id     INTEGER REFERENCES users(id)   ON DELETE SET NULL,
  device_id   INTEGER REFERENCES devices(id) ON DELETE SET NULL,
  -- The sender's channel bit-vector at send time, so a replay can answer who
  -- was allowed to see it without re-deriving historical memberships.
  group_bits  BLOB NOT NULL,
  time        TEXT NOT NULL,
  start       TEXT NOT NULL,
  stale       TEXT NOT NULL,
  lat         REAL,
  lon         REAL,
  hae         REAL,
  ce          REAL,
  le          REAL,
  xml         TEXT NOT NULL,
  received_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_cot_latest_stale  ON cot_latest (stale);
CREATE INDEX idx_cot_latest_type   ON cot_latest (type);
CREATE INDEX idx_cot_latest_device ON cot_latest (device_id);
CREATE TABLE stream_segments (
  id           INTEGER PRIMARY KEY,
  stream_kind  TEXT NOT NULL,            -- 'cot' today; telemetry later
  stream_key   TEXT NOT NULL,            -- CoT uid, or whatever keys that stream
  segment_path TEXT NOT NULL,            -- relative to the stream root
  first_time   TEXT NOT NULL,
  last_time    TEXT NOT NULL,
  record_count INTEGER NOT NULL DEFAULT 0,
  byte_length  INTEGER NOT NULL DEFAULT 0,
  -- 0 while the writer is still appending to the file, 1 once it is closed.
  sealed       INTEGER NOT NULL DEFAULT 0 CHECK (sealed IN (0,1)),
  created_at   TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_stream_segments_path ON stream_segments (segment_path);
CREATE INDEX idx_stream_segments_stream ON stream_segments (stream_kind, stream_key, first_time);
CREATE INDEX idx_stream_segments_open ON stream_segments (stream_kind, stream_key) WHERE sealed = 0;
CREATE TABLE resources (
  id                   INTEGER PRIMARY KEY,   -- the legacy "PrimaryKey" field
  hash                 TEXT NOT NULL,
  uid                  TEXT NOT NULL,
  name                 TEXT NOT NULL,
  filename             TEXT,
  mime_type            TEXT NOT NULL,
  size                 INTEGER NOT NULL,
  tool                 TEXT NOT NULL DEFAULT 'public',
  creator_uid          TEXT,
  submitter_id         INTEGER REFERENCES users(id) ON DELETE SET NULL,
  submitter            TEXT,
  submission_time      TEXT NOT NULL,
  -- Epoch milliseconds; NULL means never. TAK sends -1 for "never" and CloudTAK
  -- requires the key to be present in the legacy metadata view.
  expiration           INTEGER,
  is_mission_package   INTEGER NOT NULL DEFAULT 0 CHECK (is_mission_package IN (0,1)),
  manifest             TEXT CHECK (manifest IS NULL OR json_valid(manifest)),
  groups               TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(groups)),
  mission_name         TEXT,
  -- Title-case legacy metadata that ATAK and CloudTAK round-trip verbatim.
  latitude             REAL,
  longitude            REAL,
  altitude             REAL,
  remarks              TEXT,
  permissions          TEXT,
  contacts             TEXT,
  download_path        TEXT,
  plugin_class_name    TEXT,
  -- Admin flag: ship this package in the enrolment profile.
  install_on_enrollment INTEGER NOT NULL DEFAULT 0 CHECK (install_on_enrollment IN (0,1)),
  deleted_at           TEXT,
  created_at           TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_resources_hash ON resources (hash);
CREATE UNIQUE INDEX idx_resources_uid ON resources (uid);
CREATE INDEX idx_resources_name ON resources (name);
CREATE INDEX idx_resources_tool_time ON resources (tool, submission_time DESC);
CREATE INDEX idx_resources_mission_name ON resources (mission_name);
CREATE INDEX idx_resources_enrollment ON resources (install_on_enrollment)
  WHERE install_on_enrollment = 1;
CREATE TABLE resource_keywords (
  resource_id INTEGER NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  keyword     TEXT NOT NULL COLLATE NOCASE,
  PRIMARY KEY (resource_id, keyword)
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_resource_keywords_keyword ON resource_keywords (keyword);
CREATE TABLE missions (
  id               INTEGER PRIMARY KEY,
  guid             TEXT NOT NULL,
  name             TEXT NOT NULL COLLATE NOCASE,
  description      TEXT,
  chat_room        TEXT,
  base_layer       TEXT,
  bbox             TEXT,
  bounding_polygon TEXT CHECK (bounding_polygon IS NULL OR json_valid(bounding_polygon)),
  path             TEXT,
  classification   TEXT,
  tool             TEXT NOT NULL DEFAULT 'public',
  keywords         TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(keywords)),
  creator_uid      TEXT,
  owner_user_id    INTEGER REFERENCES users(id) ON DELETE SET NULL,
  create_time      TEXT NOT NULL,
  last_edited      TEXT,
  default_role     TEXT NOT NULL DEFAULT 'MISSION_SUBSCRIBER'
                     CHECK (default_role IN ('MISSION_OWNER','MISSION_SUBSCRIBER','MISSION_READONLY_SUBSCRIBER')),
  invite_only      INTEGER NOT NULL DEFAULT 0 CHECK (invite_only IN (0,1)),
  -- argon2id; we are the only verifier of a mission password.
  password_hash    TEXT,
  expiration       INTEGER,                    -- epoch seconds; NULL = none
  groups           TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(groups)),
  external_data    TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(external_data)),
  feeds            TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(feeds)),
  parent_id        INTEGER REFERENCES missions(id) ON DELETE SET NULL,
  token_kid        TEXT REFERENCES oauth_keys(kid) ON DELETE SET NULL,
  archived_at      TEXT,
  deleted_at       TEXT,                       -- soft delete, so the API can answer 410
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_missions_guid ON missions (guid);
CREATE UNIQUE INDEX idx_missions_name_live ON missions (name) WHERE deleted_at IS NULL;
CREATE INDEX idx_missions_tool ON missions (tool);
CREATE INDEX idx_missions_parent ON missions (parent_id);
CREATE TABLE mission_subscriptions (
  id         INTEGER PRIMARY KEY,
  mission_id INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  -- The uuid carried as the SUBSCRIPTION claim of the mission token.
  subscription_uid TEXT NOT NULL,
  client_uid TEXT NOT NULL,
  user_id    INTEGER REFERENCES users(id) ON DELETE SET NULL,
  username   TEXT,
  role       TEXT NOT NULL
               CHECK (role IN ('MISSION_OWNER','MISSION_SUBSCRIBER','MISSION_READONLY_SUBSCRIBER')),
  token_jti  TEXT,
  created_at TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_mission_subs_client ON mission_subscriptions (mission_id, client_uid);
CREATE UNIQUE INDEX idx_mission_subs_uid ON mission_subscriptions (subscription_uid);
CREATE INDEX idx_mission_subs_user ON mission_subscriptions (user_id);
CREATE TABLE mission_changes (
  id             INTEGER PRIMARY KEY,
  mission_id     INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  type           TEXT NOT NULL
                   CHECK (type IN ('CREATE_MISSION','DELETE_MISSION','ADD_CONTENT','REMOVE_CONTENT',
                                   'CREATE_DATA_FEED','DELETE_DATA_FEED')),
  timestamp      TEXT NOT NULL,
  server_time    TEXT NOT NULL,
  creator_uid    TEXT,
  content_uid    TEXT,
  content_hash   TEXT,
  log_entry_id   TEXT,
  map_layer_uid  TEXT,
  feed_uid       TEXT,
  is_federated   INTEGER NOT NULL DEFAULT 0 CHECK (is_federated IN (0,1)),
  detail         TEXT CHECK (detail IS NULL OR json_valid(detail))
) STRICT;
CREATE INDEX idx_mission_changes_mission_ts ON mission_changes (mission_id, timestamp);
CREATE INDEX idx_mission_changes_uid ON mission_changes (mission_id, content_uid);
CREATE TABLE mission_contents (
  mission_id  INTEGER NOT NULL REFERENCES missions(id)  ON DELETE CASCADE,
  resource_id INTEGER NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  creator_uid TEXT,
  timestamp   TEXT NOT NULL,
  keywords    TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(keywords)),
  layer_uid   TEXT,
  position    INTEGER,
  PRIMARY KEY (mission_id, resource_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_mission_contents_resource ON mission_contents (resource_id);
CREATE TABLE mission_uids (
  mission_id  INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  uid         TEXT NOT NULL,
  creator_uid TEXT,
  timestamp   TEXT NOT NULL,
  keywords    TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(keywords)),
  -- {type, callsign, title, iconsetPath, color, location, …}: ATAK's detail
  -- dictionary, read and written whole by exactly this row.
  details     TEXT CHECK (details IS NULL OR json_valid(details)),
  layer_uid   TEXT,
  position    INTEGER,
  PRIMARY KEY (mission_id, uid)
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_mission_uids_uid ON mission_uids (uid);
CREATE TABLE mission_layers (
  id          INTEGER PRIMARY KEY,
  mission_id  INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  uid         TEXT NOT NULL,
  name        TEXT,
  type        TEXT NOT NULL CHECK (type IN ('GROUP','UID','CONTENTS','MAPLAYER','ITEM')),
  -- The tree: parent_uid nests, after_uid and position order siblings.
  parent_uid  TEXT,
  after_uid   TEXT,
  position    INTEGER,
  creator_uid TEXT,
  data        TEXT CHECK (data IS NULL OR json_valid(data)),
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_mission_layers_uid ON mission_layers (mission_id, uid);
CREATE INDEX idx_mission_layers_parent ON mission_layers (mission_id, parent_uid);
CREATE TABLE mission_logs (
  id             INTEGER PRIMARY KEY,
  log_id         TEXT NOT NULL,
  mission_id     INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  content        TEXT NOT NULL,
  creator_uid    TEXT,
  entry_uid      TEXT,
  content_hashes TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(content_hashes)),
  keywords       TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(keywords)),
  servertime     TEXT NOT NULL,
  dtg            TEXT,
  created_at     TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_mission_logs_log_id ON mission_logs (log_id);
CREATE INDEX idx_mission_logs_mission_time ON mission_logs (mission_id, servertime);
CREATE TABLE mission_invitations (
  id           INTEGER PRIMARY KEY,
  mission_id   INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  invitee_type TEXT NOT NULL CHECK (invitee_type IN ('clientUid','callsign','userName','group','team')),
  invitee      TEXT NOT NULL,
  creator_uid  TEXT,
  role         TEXT NOT NULL DEFAULT 'MISSION_SUBSCRIBER'
                 CHECK (role IN ('MISSION_OWNER','MISSION_SUBSCRIBER','MISSION_READONLY_SUBSCRIBER')),
  token_jti    TEXT,
  created_at   TEXT NOT NULL,
  accepted_at  TEXT
) STRICT;
CREATE UNIQUE INDEX idx_mission_invitations_target
  ON mission_invitations (mission_id, invitee_type, invitee);
CREATE INDEX idx_mission_invitations_invitee ON mission_invitations (invitee);
CREATE TABLE profiles (
  id                  INTEGER PRIMARY KEY,
  name                TEXT NOT NULL,
  description         TEXT,
  -- Two independent flags rather than one `apply_on` enum: a profile may be
  -- delivered at enrolment, on connection, both, or neither while it is drafted.
  apply_on_enrollment INTEGER NOT NULL DEFAULT 0 CHECK (apply_on_enrollment IN (0,1)),
  apply_on_connect    INTEGER NOT NULL DEFAULT 0 CHECK (apply_on_connect IN (0,1)),
  enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
  priority            INTEGER NOT NULL DEFAULT 0,
  tool                TEXT,
  type                TEXT,
  groups              TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(groups)),   -- [] = everyone
  -- key -> {class, value}; rendered whole into <name>.pref at build time, so it
  -- is never filtered or joined on.
  preferences         TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(preferences)),
  created_at          TEXT NOT NULL,
  updated_at          TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_profiles_name ON profiles (name);
CREATE INDEX idx_profiles_enrollment ON profiles (apply_on_enrollment) WHERE enabled = 1;
CREATE INDEX idx_profiles_connect ON profiles (apply_on_connect) WHERE enabled = 1;
CREATE TABLE profile_files (
  id         INTEGER PRIMARY KEY,
  profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
  path       TEXT NOT NULL,
  hash       TEXT NOT NULL,        -- into the content store
  size       INTEGER NOT NULL,
  mime_type  TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_profile_files_path ON profile_files (profile_id, path);
CREATE INDEX idx_profile_files_hash ON profile_files (hash);
```
