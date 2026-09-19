-- Room in `certificates.issued_via` for the CloudTAK hand-over.
--
-- `POST /api/v1/users/{username}/cloudtak-onboarding` (M5-03) is a new endpoint
-- that issues a client certificate, and `issued_via` is the column that records
-- which endpoint a certificate came out of. It matters more here than anywhere
-- else: this is the one flow in which rustak generates the client's private key
-- rather than signing a request the client made, so "how many of these exist"
-- is a question an operator should be able to ask of the register itself rather
-- than of the audit log.
--
-- `source` already says `admin_package`, which means "the key was generated on
-- this server" and is true of both this and the manual package. It is the
-- endpoint the two differ by.
--
-- # Why the whole table is rebuilt for one value
--
-- The allowed set is a CHECK constraint, and SQLite has no way to alter one in
-- place: a table whose constraints change is rebuilt (`conventions.md`,
-- "Storage"). Nothing else about the shape changes — same columns, same order,
-- same indexes — so this migration is a copy, and the interesting part is the
-- two foreign keys that point at this table.
--
-- `PRAGMA foreign_keys` is ON for every connection, and `DROP TABLE` on a
-- parent fires the `ON DELETE SET NULL` actions of its children. There are two:
--
--  * the table's own `issuer_id`. Handled by declaring the new table's
--    self-reference against `certificates_migrated`, so the drop below is not a
--    drop of *its* parent; the rename then rewrites it back to `certificates`,
--    which is what `ALTER TABLE ... RENAME` does to every reference to the name
--    it is changing, including a table's references to itself.
--  * `devices.last_certificate_id`, which belongs to a table that is not being
--    rebuilt and would simply be emptied. It is copied out first and put back
--    afterwards, so "which certificate did this device last present" survives.

-- Which certificate each device last presented, saved from the drop below.
CREATE TABLE certificates_device_link AS
  SELECT id, last_certificate_id FROM devices WHERE last_certificate_id IS NOT NULL;

-- Identical to the table 0002 created, with 'cloudtak_onboarding' added to the
-- `issued_via` set and the self-reference pointed at this table's own name.
CREATE TABLE certificates_migrated (
  id            INTEGER PRIMARY KEY,
  kind          TEXT NOT NULL CHECK (kind IN ('ca','server','client','service')),
  source        TEXT NOT NULL DEFAULT 'internal'
                  CHECK (source IN ('enrollment','admin_package','internal','acme','imported')),
  issued_via    TEXT CHECK (issued_via IS NULL OR issued_via IN
                  ('enroll_v2_json','enroll_v2_xml','enroll_v1_p12','admin_package',
                   'acme_internal','cloudtak_onboarding')),
  serial_hex    TEXT NOT NULL,
  fingerprint   TEXT NOT NULL,
  subject_cn    TEXT NOT NULL,
  san           TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(san)),
  user_id       INTEGER REFERENCES users(id)   ON DELETE SET NULL,
  device_id     INTEGER REFERENCES devices(id) ON DELETE SET NULL,
  client_uid    TEXT,
  credential_id INTEGER REFERENCES credentials(id) ON DELETE SET NULL,
  issuer_id     INTEGER REFERENCES certificates_migrated(id) ON DELETE SET NULL,
  der           BLOB NOT NULL,
  key_sealed    TEXT CHECK (key_sealed IS NULL OR json_valid(key_sealed)),
  not_before    TEXT NOT NULL,
  not_after     TEXT NOT NULL,
  last_seen_at  TEXT,
  revoked_at    TEXT,
  revocation_reason TEXT,
  revoked_by    TEXT,
  created_at    TEXT NOT NULL
) STRICT;

INSERT INTO certificates_migrated (
  id, kind, source, issued_via, serial_hex, fingerprint, subject_cn, san,
  user_id, device_id, client_uid, credential_id, issuer_id, der, key_sealed,
  not_before, not_after, last_seen_at, revoked_at, revocation_reason,
  revoked_by, created_at
)
SELECT
  id, kind, source, issued_via, serial_hex, fingerprint, subject_cn, san,
  user_id, device_id, client_uid, credential_id, issuer_id, der, key_sealed,
  not_before, not_after, last_seen_at, revoked_at, revocation_reason,
  revoked_by, created_at
FROM certificates;

DROP TABLE certificates;

ALTER TABLE certificates_migrated RENAME TO certificates;

UPDATE devices
   SET last_certificate_id = (
         SELECT last_certificate_id
           FROM certificates_device_link
          WHERE certificates_device_link.id = devices.id
       )
 WHERE id IN (SELECT id FROM certificates_device_link);

DROP TABLE certificates_device_link;

-- The indexes went with the old table; they are the same seven 0002 created.
CREATE UNIQUE INDEX idx_certificates_fingerprint ON certificates (fingerprint);
CREATE UNIQUE INDEX idx_certificates_issuer_serial ON certificates (issuer_id, serial_hex);
CREATE INDEX idx_certificates_user ON certificates (user_id);
CREATE INDEX idx_certificates_device ON certificates (device_id);
CREATE INDEX idx_certificates_credential ON certificates (credential_id);
CREATE INDEX idx_certificates_expiry ON certificates (kind, not_after) WHERE revoked_at IS NULL;
CREATE INDEX idx_certificates_revoked ON certificates (revoked_at) WHERE revoked_at IS NOT NULL;
