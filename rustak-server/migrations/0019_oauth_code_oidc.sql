-- What an authorization code has to remember when the client is an OpenID
-- Connect relying party (M8-01).
--
-- Two new facts, and one relaxation:
--
--  * `oidc_scope` — the OpenID scopes granted at `/oauth/authorize`, as the
--    intersection of what was asked for with `openid profile email groups`.
--    It is **not** the rustak scope: that one stays in `scope` and is still the
--    ceiling `tokens::scope_for` is clamped by. Conflating the two would let a
--    client widen what its token may do by asking for a profile claim.
--  * `nonce` — the value the relying party sent, which its ID token has to echo
--    byte for byte. It binds the token to the flow that asked for it, and a
--    standard library refuses a token whose nonce it did not choose.
--  * `code_challenge` becomes nullable. A confidential client authenticates
--    with a secret at `/oauth/token`, so its proof key is optional — and
--    CloudTAK's relying party sends no `code_challenge` at all. A public client
--    is unchanged: `authorize.rs` still refuses to issue it a code without one,
--    so a NULL here can only ever belong to a client that had to present a
--    secret to redeem it.
--
-- # Why the table is rebuilt rather than altered
--
-- `code_challenge TEXT NOT NULL` cannot be relaxed in place — SQLite has no
-- `ALTER COLUMN` — and the alternative, storing an empty string to mean "no
-- proof key", is a sentinel a redemption could read as a challenge that
-- trivially matches. The rows are ten-minute authorization codes, so the copy
-- below is measured in tens of rows on the busiest installation; `oauth_codes`
-- is nobody's parent (only `users` is its), so the drop fires no cascade.

-- Identical to the table 0013 created, with the two new columns and a
-- `code_challenge` that may be absent — with its method, which is still only
-- ever `S256` when there is one at all.
CREATE TABLE oauth_codes_migrated (
  code_hash             TEXT PRIMARY KEY,
  client_id             TEXT NOT NULL,
  user_id               INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  redirect_uri          TEXT NOT NULL,
  -- The rustak scope of the session the code will be exchanged for.
  scope                 TEXT NOT NULL,
  -- The OpenID scopes granted, space-separated. NULL when the request asked
  -- for none, which is every client that is not a relying party.
  oidc_scope            TEXT,
  -- The relying party's nonce, echoed into the ID token. NULL when none was
  -- sent, and then the ID token carries no `nonce` claim at all.
  nonce                 TEXT,
  -- NULL only for a confidential client that sent no `code_challenge`.
  code_challenge        TEXT,
  code_challenge_method TEXT CHECK (code_challenge_method IS NULL OR
                                    code_challenge_method = 'S256'),
  created_at            TEXT NOT NULL,
  expires_at            TEXT NOT NULL,
  consumed_at           TEXT,
  -- A code with a method and no challenge, or the other way round, is a row
  -- redemption would have to guess about.
  CHECK ((code_challenge IS NULL) = (code_challenge_method IS NULL))
) STRICT, WITHOUT ROWID;

INSERT INTO oauth_codes_migrated (
  code_hash, client_id, user_id, redirect_uri, scope, oidc_scope, nonce,
  code_challenge, code_challenge_method, created_at, expires_at, consumed_at
)
SELECT
  code_hash, client_id, user_id, redirect_uri, scope, NULL, NULL,
  code_challenge, code_challenge_method, created_at, expires_at, consumed_at
FROM oauth_codes;

DROP TABLE oauth_codes;

ALTER TABLE oauth_codes_migrated RENAME TO oauth_codes;

CREATE INDEX idx_oauth_codes_expires ON oauth_codes (expires_at);
CREATE INDEX idx_oauth_codes_user ON oauth_codes (user_id);
