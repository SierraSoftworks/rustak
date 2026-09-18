-- Data Sync: missions and their sub-aggregates.
--
-- Design 01 §4.5's tables with design 04 §4.1's columns folded in where that
-- document is more specific about what the Marti API has to report.

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
