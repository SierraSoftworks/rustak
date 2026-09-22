-- What a service's configuration document may hold.
--
-- 0002 gave `services` a `config` column and nothing that said what belonged in
-- it, so the admin UI could only offer a text box and an administrator could
-- only guess at the keys. A sidecar now registers a JSON Schema for its own
-- configuration (`rustak_api::service::ServiceDescriptor::config_schema`), and
-- this is where it is kept: the UI draws a form from it, and a `PUT` of the
-- configuration is held to it.
--
-- NULL rather than `'{}'` for a service that registered none. An empty schema
-- accepts everything, which is a statement; "this plugin never said" is a
-- different one, and it is what keeps the free-form editor for plugins that
-- predate this.
--
-- Replaced on every registration, like the rest of what a sidecar says about
-- itself: the schema belongs to the build that is running, not to the row.

ALTER TABLE services
  ADD COLUMN config_schema TEXT CHECK (config_schema IS NULL OR json_valid(config_schema));
