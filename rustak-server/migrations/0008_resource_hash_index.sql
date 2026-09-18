-- Enterprise Sync: one content hash may back more than one metadata row.
--
-- 0005 declared `idx_resources_hash` UNIQUE. The files design declares a plain
-- `INDEX(hash)`, and TAK's own behaviour needs the plain one: two map items may
-- carry the same photograph, two users may upload the same data package under
-- different names, and ATAK's "send to server" uploads a package a peer has
-- already sent. None of those is a conflict — the *blob* is stored once because
-- the store is content-addressed, and what differs between the rows is their
-- name, submitter, keywords and channels.
--
-- `idx_resources_uid` stays unique: a resource's UID is how a client addresses
-- it, and `/Marti/sync/upload` re-using one is a new version of that resource
-- rather than a second one.

DROP INDEX idx_resources_hash;
CREATE INDEX idx_resources_hash ON resources (hash);
