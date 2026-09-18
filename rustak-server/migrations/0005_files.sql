-- Enterprise Sync metadata. The bytes live at <content_dir>/<hash[0..2]>/<hash>;
-- only the metadata is here.

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

-- A child table rather than a delimited string, because keyword search is one
-- of the two ways the sync API is queried.
CREATE TABLE resource_keywords (
  resource_id INTEGER NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  keyword     TEXT NOT NULL COLLATE NOCASE,
  PRIMARY KEY (resource_id, keyword)
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_resource_keywords_keyword ON resource_keywords (keyword);
