-- The mission tables design 04 §4.1 names that 0006 did not carry.
--
-- 0006 already has the cached uid details (`mission_uids.details`), the layer
-- ordering (`mission_layers.after_uid`/`position`), the changes, contents,
-- subscriptions, logs and invitations. What is missing is the three side
-- aggregates a mission reports as `externalData`, `mapLayers` and `feeds` —
-- all three of which CloudTAK requires as arrays on every Mission payload — and
-- the column an invitation token is matched against.

-- `role_from_token` matches an INVITATION token against the whole JWT rather
-- than its `jti`, because that is what TAK Server stores and what an invited
-- client replays verbatim. The `jti` column stays: it is what a revocation
-- would list.
ALTER TABLE mission_invitations ADD COLUMN token TEXT;

-- An external tool's data, reported verbatim under `Mission.externalData`.
CREATE TABLE mission_external_data (
  id          INTEGER PRIMARY KEY,
  mission_id  INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  uid         TEXT NOT NULL,
  name        TEXT NOT NULL,
  tool        TEXT,
  url_data    TEXT,
  url_view    TEXT,
  notes       TEXT,
  creator_uid TEXT,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_mission_external_data_uid ON mission_external_data (mission_id, uid);

-- A map layer, stored as the client sent it: the body is opaque to us and is
-- handed back unchanged, so a client that adds a field keeps it.
CREATE TABLE map_layers (
  id          INTEGER PRIMARY KEY,
  mission_id  INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  uid         TEXT NOT NULL,
  name        TEXT,
  body        TEXT NOT NULL CHECK (json_valid(body)),
  creator_uid TEXT,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_map_layers_uid ON map_layers (mission_id, uid);

-- Data feeds are listed and nothing more; the row exists so that a mission
-- reports the feed a client registered rather than an empty array it did not
-- expect.
CREATE TABLE mission_feeds (
  id          INTEGER PRIMARY KEY,
  mission_id  INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  uid         TEXT NOT NULL,
  body        TEXT NOT NULL CHECK (json_valid(body)),
  creator_uid TEXT,
  created_at  TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_mission_feeds_uid ON mission_feeds (mission_id, uid);
