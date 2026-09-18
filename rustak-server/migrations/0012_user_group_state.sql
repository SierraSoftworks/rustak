-- The account-level half of "which channels are switched on".
--
-- `device_group_state` (0002) scopes the preference to a device, so that
-- switching a channel off on a phone does not switch it off on a laptop. But
-- `PUT /Marti/api/groups/active` arrives with no `clientUid` from every caller
-- that is not an end-user device — CloudTAK's browser half, the admin UI, a
-- script — and those callers have no device row to write to.
--
-- M2-06 kept that selection in `kv` under `marti-channels/active-<user id>`,
-- which the Marti reads could consult but the routing path could not: a device
-- enrolled *after* an account-level change has no `device_group_state` rows, so
-- `members.effective` saw nothing switched off and routed the channel anyway
-- until the device called the endpoint itself. A table closes that, because the
-- same SQL that intersects grants with the device's preference can fall back to
-- the account's.
--
-- Deliberately the same shape as `device_group_state`, one row per single
-- direction: `Direction::Both` is a UI convenience and never reaches a column.
-- No rows are copied out of `kv`; an account that had a selection there simply
-- re-reads as everything-on until it sets one again, which is the permissive
-- direction and corrects itself the first time a client calls the endpoint.
CREATE TABLE user_group_state (
  user_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  group_id  INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
  direction TEXT NOT NULL CHECK (direction IN ('IN','OUT')),
  active    INTEGER NOT NULL CHECK (active IN (0,1)),
  PRIMARY KEY (user_id, group_id, direction)
) STRICT, WITHOUT ROWID;

-- Deleting a channel has to find every account that had an opinion about it,
-- the same way it does for every device.
CREATE INDEX idx_user_group_state_group ON user_group_state (group_id);
