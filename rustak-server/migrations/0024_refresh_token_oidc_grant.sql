-- The OpenID scopes a session was granted, remembered for as long as the
-- session lives (M10-06).
--
-- `/oauth/userinfo` has to release only the claims the scopes granted at
-- `/oauth/authorize` cover (OpenID Connect Core §5.3.2, §5.4). Those scopes were
-- recorded on the authorization code (`0019`), which is spent within seconds of
-- being issued, and an access token has nowhere to carry them: its claims are
-- flat and pinned (`compat/oauth.md` §2). So they are recorded here, on the
-- refresh-token row that is minted beside every session access token:
--
--  * `jti` — the identifier of the access token minted together with this
--    refresh token. It is how userinfo finds the row from the token in front of
--    it. NULL for a row written before this migration.
--  * `oidc_scope` — the OpenID scopes granted, space-separated, exactly as
--    `oauth_codes.oidc_scope` held them. NULL when the session was not started
--    by a relying party that asked for any — the admin UI, a passkey, a session
--    from before this migration — and userinfo then answers as if `openid`
--    alone had been granted: `sub` and nothing else.
--
-- # Why here and not in a table of its own
--
-- The refresh-token row already *is* the session: it is written once per
-- issuance, copied forward by every rotation (which is where the scopes have to
-- survive a refresh), revoked with the session and deleted when it expires or
-- its account does. A separate table would need every one of those lifecycles
-- again and could drift from them. The rotation copies the value unchanged; a
-- refresh can neither widen nor narrow what the relying party was granted.
--
-- Existing rows get NULL for both, which is the narrowest answer.
ALTER TABLE refresh_tokens ADD COLUMN jti TEXT;
ALTER TABLE refresh_tokens ADD COLUMN oidc_scope TEXT;

-- Userinfo's lookup. Unique because a `jti` names exactly one access token, and
-- partial because every row written before this migration has none.
CREATE UNIQUE INDEX idx_refresh_tokens_jti ON refresh_tokens (jti) WHERE jti IS NOT NULL;
