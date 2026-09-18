-- What a service reports alongside its health.
--
-- `Heartbeat.metrics` has been part of the control-API contract since the DTOs
-- landed (`rustak_api::service::Heartbeat`): a sidecar knows what is worth
-- counting — events published, queue depth, seconds since its upstream last
-- answered — and this server does not. 0002 gave `services` a status, a message
-- and a heartbeat time but nowhere to keep the numbers, so the admin UI could
-- show that a feed was degraded and never by how much.
--
-- One JSON column rather than a table of name/value pairs, and rather than a
-- time series: it is the *latest* reading, replaced on every heartbeat, read
-- back whole by `GET /api/v1/services`. A sidecar that wants history publishes
-- CoT like any other client.
--
-- `'{}'` rather than NULL for the default, so that a service which has
-- registered but never reported reads back as an empty object rather than as a
-- missing key every consumer has to special-case.

ALTER TABLE services
  ADD COLUMN metrics TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metrics));
