-- Device-profile preferences, one row per key.
--
-- 0007 put these in a JSON blob on `profiles`. That was fine while a profile's
-- preferences were only ever rendered whole, but the admin UI edits them a row
-- at a time, the editor needs each entry's Java class beside its value, and the
-- rendered `.pref` has to come out in a stable order every build so that a
-- golden test means something. A table gives all three; `profiles.preferences`
-- is left in place because an applied migration is never edited, and is no
-- longer read or written.
--
-- `position` rather than an alphabetical sort: ATAK applies entries in document
-- order, so an operator who puts a switch before the setting it gates wants
-- that order kept.

CREATE TABLE profile_prefs (
  profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
  key        TEXT NOT NULL,
  -- The five classes ATAK's importer dispatches on. A value outside this set
  -- would be dropped by the client, so it is refused here instead.
  class      TEXT NOT NULL DEFAULT 'String'
             CHECK (class IN ('String', 'Boolean', 'Integer', 'Long', 'Float')),
  value      TEXT NOT NULL,
  position   INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (profile_id, key)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_profile_prefs_order ON profile_prefs (profile_id, position);
