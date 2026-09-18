-- A log entry may belong to several missions at once.
--
-- ATAK writes one entry naming every mission the operator had open, and
-- `LogEntry.missionNames` is an array on the wire for exactly that reason. 0006
-- made `log_id` globally unique, which allows one row per entry and therefore
-- one mission per entry; the uniqueness that was meant is one row per
-- (entry, mission).
--
-- Not an edit of 0006: the index is dropped and replaced here, so an
-- installation that has already run 0006 is brought to the same shape as a new
-- one.

DROP INDEX idx_mission_logs_log_id;

-- One row per entry per mission. Reading an entry back unions the mission names.
CREATE UNIQUE INDEX idx_mission_logs_entry ON mission_logs (log_id, mission_id);

-- The lookup `GET /missions/logs/entries/{id}` and the delete both use.
CREATE INDEX idx_mission_logs_lookup ON mission_logs (log_id);
