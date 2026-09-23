-- Retention reads the segment index oldest first, and should not have to sort it.
--
-- All three retention reads want sealed segments in age order: the age horizon
-- takes those older than an instant, the size cap walks from the oldest of every
-- stream until enough has been freed, and the per-stream cap does the same
-- within one stream. Without an index on `last_time` each of them
-- sorted the table, and the size cap did it inside a window function, which
-- SQLite materialises whole: a server runs with `temp_store = MEMORY`, so that
-- was the entire index held in memory once per page of a sweep (about 200 MiB
-- per million segments), however few rows the page returned.
--
-- Partial on purpose. `last_time` moves on every flush of an open segment, and
-- an index over it would put a second b-tree write on the path that records CoT
-- history. A sealed segment is never appended to again, so its entry is written
-- once, when it is sealed, and retention only ever reads sealed segments.

CREATE INDEX idx_stream_segments_sealed_age ON stream_segments (last_time, id) WHERE sealed = 1;

-- The same order within one stream, for the per-stream record cap.
--
-- Not `idx_stream_segments_stream`, which holds a stream's segments by when each
-- *began*, and nothing promises that is the order they ended in. The log files a
-- record under whatever time its caller hands it and tolerates those arriving
-- out of order, which is why `last_time` only ever moves forward. CoT history is
-- filed under this server's clock, which can be stepped backwards, and the
-- streams that come after it need not be filed under a clock of ours at all.
-- Age is when a segment *ended* — the newest thing it holds — which is what the
-- other two reads already mean by it.
CREATE INDEX idx_stream_segments_sealed_stream_age
  ON stream_segments (stream_kind, stream_key, last_time, id) WHERE sealed = 1;
