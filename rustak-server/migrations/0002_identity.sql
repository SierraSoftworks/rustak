-- Identity: who may connect, what channel they may reach it on, which device
-- they are using, and what they prove themselves with.
--
-- Reconciled from design 01 §4.5 and design 03 §3/§4, with the plan's deltas:
-- there is no `users.password_hash` column (rustak has no local passwords —
-- local sign-in is by passkey), credential kinds are the three the plan keeps,
-- and per-device channel state is a table rather than a JSON column because it
-- is filtered and joined on.

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
-- Subject is unique per issuer: two providers may legitimately mint the same
-- subject string, and only the pair identifies a person.
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
-- The default channel, at the bit position `rustak_core::identity::ANON_BITPOS`
-- names. strftime here rather than a bound parameter because a seed row inside
-- a migration has no Rust caller to bind from; the format is the same RFC 3339
-- with milliseconds every other timestamp uses.
INSERT INTO groups (id, name, bitpos, description, source, created_at)
  VALUES (1, '__ANON__', 1, 'Default channel', 'system', strftime('%Y-%m-%dT%H:%M:%fZ','now'));

-- One row per single direction. IN is permission to write to the channel, OUT
-- permission to read from it; a "both" grant in the UI is stored as the pair.
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

-- Which of a user's channels this particular device currently has switched on,
-- written by `PUT /Marti/api/groups/active?clientUid=`. The effective set for a
-- subscription is the user's memberships intersected with this.
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

-- WebAuthn credentials. Separate from `credentials` because what is stored is a
-- public key rather than the hash of a secret, and nothing here is verifiable
-- by the same argon2 path.
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
