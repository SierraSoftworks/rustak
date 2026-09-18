-- Token signing keys, the OAuth2 server's short-lived state, wizard-managed
-- settings, and the ACME account and certificates.

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

-- Single-use authorization codes and pending identity-provider states. Refresh
-- tokens are not here: they rotate in families and live in `refresh_tokens`.
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

-- Rotating refresh tokens. `family` ties a chain of rotations together so that
-- replaying a spent token can revoke every descendant of it at once.
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

-- Access tokens we have disowned before their expiry. Rows are pruned once
-- `expires_at` passes, because after that the signature check refuses them.
CREATE TABLE revoked_jtis (
  jti        TEXT PRIMARY KEY,
  expires_at TEXT NOT NULL,
  revoked_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_revoked_jtis_expires ON revoked_jtis (expires_at);

-- Settings the first-run wizard and the admin UI own. The TOML file wins where
-- it sets the same key, so this is the fallback layer rather than the truth.
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
