-- What one account has chosen about how the console looks to it: today, which
-- edition of MIL-STD-2525 the map draws.
--
-- One row per key rather than a column per preference or a JSON blob, for the
-- reasons `profile_preferences` (0009) gives: the next preference is a row and
-- not a migration, a key can be set without reading the others, and an absent
-- row means "the default" without anybody having to store one. The value is
-- text, read back through the type that owns the key; a value this build does
-- not recognise is treated as absent rather than refused, so a row written by
-- a newer server never stops an older one from answering `/me`.
CREATE TABLE user_preferences (
  user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (user_id, key)
) STRICT, WITHOUT ROWID;
