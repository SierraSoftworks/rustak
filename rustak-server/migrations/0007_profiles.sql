-- Device profiles: the preferences and files ATAK pulls at enrolment and on
-- every connection.

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
