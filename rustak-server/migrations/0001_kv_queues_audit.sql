-- Generic stores: the key/value store, the job queue and the audit log.
--
-- Lifted from automate's schema with the `tenant` column removed (rustak is a
-- single installation, not a multi-tenant one) and every table made STRICT.
-- Timestamps are TEXT holding RFC 3339 with milliseconds, always bound from
-- Rust: SQLite's own CURRENT_TIMESTAMP resolves only to the second, and
-- DATETIME is not a STRICT storage class.

CREATE TABLE kv (
  partition  TEXT NOT NULL,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL CHECK (json_valid(value)),
  updated_at TEXT NOT NULL,
  PRIMARY KEY (partition, key)
) STRICT, WITHOUT ROWID;

CREATE TABLE queues (
  partition       TEXT NOT NULL,
  key             TEXT NOT NULL,
  payload         TEXT NOT NULL CHECK (json_valid(payload)),
  scheduled_at    TEXT NOT NULL,
  hidden_until    TEXT NOT NULL,
  reserved_by     TEXT,
  traceparent     TEXT,
  tracestate      TEXT,
  idempotency_key TEXT,
  attempts        INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (partition, key)
) STRICT, WITHOUT ROWID;
CREATE INDEX idx_queues_partition_hidden ON queues (partition, hidden_until);
CREATE INDEX idx_queues_hidden_scheduled ON queues (hidden_until, scheduled_at);

-- Entries are ordered by id rather than by occurred_at: several commonly share
-- a timestamp, and only the id gives a total order stable enough to page with.
CREATE TABLE audit_log (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  occurred_at TEXT NOT NULL,
  category    TEXT NOT NULL,
  action      TEXT NOT NULL,
  outcome     TEXT NOT NULL,
  actor       TEXT,
  subject     TEXT,
  message     TEXT,
  detail      TEXT CHECK (detail IS NULL OR json_valid(detail))
) STRICT;
CREATE INDEX idx_audit_subject  ON audit_log (subject, id DESC);
CREATE INDEX idx_audit_category ON audit_log (category, id DESC);
CREATE INDEX idx_audit_actor    ON audit_log (actor, id DESC);
CREATE INDEX idx_audit_occurred ON audit_log (occurred_at);
