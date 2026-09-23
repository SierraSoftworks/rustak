-- Retention reads the segment index oldest first, and should not have to sort it.
--
-- All three retention reads want sealed segments in age order: the age horizon
-- takes those older than an instant, and the size cap walks from the oldest
-- until enough has been freed. Without an index on `last_time` each of them
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
