-- Authorization codes issued by our own `/oauth/authorize`.
--
-- Separate from `oauth_tokens` (0003), which was designed as one generic table
-- for codes and pending identity-provider states and has never been written to:
-- every binding a code has to carry — the client it was issued to, the exact
-- redirect URI it was issued for and the proof-key challenge it is bound to —
-- is a column here rather than a key in a JSON blob, so a redemption that
-- forgets to check one of them is a compile error rather than a silent
-- widening. The pending identity-provider state stays in the `auth-state`
-- key/value partition beside the passkey ceremonies, because it is opaque, one
-- component owns it and it is read back whole.
--
-- The code itself is never stored. It is 32 bytes of randomness we generated,
-- so sha256 is enough to find the row and argon2 would only make every
-- redemption slow (design 01 §4.4's rule of three).
CREATE TABLE oauth_codes (
  -- Hex sha256 of the code handed to the client.
  code_hash             TEXT PRIMARY KEY,
  -- The registered client the code was issued to. A redemption naming another
  -- one is refused even when it holds the code.
  client_id             TEXT NOT NULL,
  user_id               INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- The exact URI the code was issued for, compared byte for byte at
  -- redemption: RFC 6749 §4.1.3 requires it, and it is what stops a code
  -- captured from one registered URI being redeemed against another.
  redirect_uri          TEXT NOT NULL,
  -- The scope of the session the code will be exchanged for.
  scope                 TEXT NOT NULL,
  -- The proof-key challenge. `S256` only: `plain` sends the verifier in the
  -- authorization request and defeats the whole mechanism.
  code_challenge        TEXT NOT NULL,
  code_challenge_method TEXT NOT NULL CHECK (code_challenge_method = 'S256'),
  created_at            TEXT NOT NULL,
  expires_at            TEXT NOT NULL,
  -- Set the moment the code is exchanged, inside the same transaction that
  -- reads it, so two simultaneous redemptions cannot both succeed.
  consumed_at           TEXT
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_oauth_codes_expires ON oauth_codes (expires_at);
CREATE INDEX idx_oauth_codes_user ON oauth_codes (user_id);
