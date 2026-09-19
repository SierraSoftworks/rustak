-- What a one-time enrolment token bought, and for whom.
--
-- ATAK enrols in three calls with the same token: `tls/config`,
-- `signClient/v2`, and then `GET /Marti/api/tls/profile/enrollment?clientUid=`
-- 0.4 s later, unconditionally (research 07 §1.5, §2.1). The second call spends
-- the token, so the third one used to be answered `401` and the device reported
-- "TAK server registration failed" (M2-15 field report).
--
-- The fix is a grace window, and a window needs two facts the row did not
-- carry. `revoked_at` alone cannot answer either: the claim writes it as it
-- spends the token, so it says *when* the row died but not *whether* it died by
-- being spent or by an administrator taking it back, and it says nothing about
-- which device spent it.
--
-- `spent_at` is therefore written only by `claim_single_use`, and cleared by
-- `release_single_use` (an issuance that then failed) and by every revocation
-- (an administrator ending the grace). `spent_uid` is the `clientUid` the
-- signing request carried; NULL when it carried none, which means no grace,
-- because a grace that cannot name the device it is for is a grace for
-- anybody.
--
-- Existing rows get NULL for both. A token spent before this migration ran is
-- simply spent, which is what it was.
ALTER TABLE credentials ADD COLUMN spent_at TEXT;
ALTER TABLE credentials ADD COLUMN spent_uid TEXT;

-- The grace lookup is by `lookup_hint` (the index from 0002 already serves it)
-- and then by `spent_at`; this partial index keeps the spent rows the window
-- can still reach separable from the far larger set of ordinary revoked ones.
CREATE INDEX idx_credentials_spent ON credentials (spent_at) WHERE spent_at IS NOT NULL;
