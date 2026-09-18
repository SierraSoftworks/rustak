-- The latest position of every contact, and the index over the append-only
-- stream segments that hold the history.
--
-- There is deliberately no `cot_history` table: history is written to
-- `store::append_log` segment files (varint-length-prefixed protobuf), and only
-- the segment metadata lives here. That keeps SQLite's write volume to metadata
-- and leaves the high-rate path on the filesystem.

CREATE TABLE cot_latest (
  uid         TEXT PRIMARY KEY,
  type        TEXT NOT NULL,
  callsign    TEXT,
  user_id     INTEGER REFERENCES users(id)   ON DELETE SET NULL,
  device_id   INTEGER REFERENCES devices(id) ON DELETE SET NULL,
  -- The sender's channel bit-vector at send time, so a replay can answer who
  -- was allowed to see it without re-deriving historical memberships.
  group_bits  BLOB NOT NULL,
  time        TEXT NOT NULL,
  start       TEXT NOT NULL,
  stale       TEXT NOT NULL,
  lat         REAL,
  lon         REAL,
  hae         REAL,
  ce          REAL,
  le          REAL,
  xml         TEXT NOT NULL,
  received_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_cot_latest_stale  ON cot_latest (stale);
CREATE INDEX idx_cot_latest_type   ON cot_latest (type);
CREATE INDEX idx_cot_latest_device ON cot_latest (device_id);

-- One row per segment file. Readers seek by (kind, key, first_time); retention
-- deletes whole segments, which is one unlink and one DELETE.
CREATE TABLE stream_segments (
  id           INTEGER PRIMARY KEY,
  stream_kind  TEXT NOT NULL,            -- 'cot' today; telemetry later
  stream_key   TEXT NOT NULL,            -- CoT uid, or whatever keys that stream
  segment_path TEXT NOT NULL,            -- relative to the stream root
  first_time   TEXT NOT NULL,
  last_time    TEXT NOT NULL,
  record_count INTEGER NOT NULL DEFAULT 0,
  byte_length  INTEGER NOT NULL DEFAULT 0,
  -- 0 while the writer is still appending to the file, 1 once it is closed.
  sealed       INTEGER NOT NULL DEFAULT 0 CHECK (sealed IN (0,1)),
  created_at   TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_stream_segments_path ON stream_segments (segment_path);
CREATE INDEX idx_stream_segments_stream ON stream_segments (stream_kind, stream_key, first_time);
CREATE INDEX idx_stream_segments_open ON stream_segments (stream_kind, stream_key) WHERE sealed = 0;
