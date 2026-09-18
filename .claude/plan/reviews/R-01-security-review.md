# R-01 — Independent security review: identity, PKI, enrolment, OAuth, services

**Reviewer.** Read-only pass over `rustak-server/src/{auth,pki,identity,marti,missions,stream,plugins,crypto,web}/**`,
`rustak-core/src/identity/**`, `rustak-api/src/identity/**`, the migrations, `config.example.toml`, and the
parts of `webauthn_rp` 0.3 the passkey ceremonies delegate to — against `plan.md` → "Identity & auth model
(secure by default)" and Appendix A, `design/03` §5, the M0-20 Level 3 RP checklist, and
`compat/{enrollment,oauth,cloudtak,groups}.md`. No source file was changed and no `git`/`but` write was run.
Baseline test runs were clean (`cargo test -p rustak-server --features testing --lib` across the auth,
passkey, services and events modules).

Every finding carries a `file:line` and a way to reproduce or a failing-test sketch. Nothing here is generic
advice; where I looked hard and found the control correct it is recorded in **What is done well** rather
than softened into a finding.

Severity is the effect on this system as `config.example.toml` configures it, not a CVSS score. There is no
**Critical**: I found no path from an unauthenticated caller to administrative control or to key material.

---

## High

### H1 — The `scope` claim is issued on every token and enforced nowhere

`rustak-server/src/auth/resolve.rs:174-178` copies the verified `claims.scope` into
`AuthMethod::Bearer { jti, scope }`. `rustak-server/src/identity/users.rs:167-170` then computes
`principal.is_admin` from `row.admin_override` / `row.is_admin` / `acl_admin` **only** — the `via` argument
carrying the scope is stored as audit metadata and never read.
`rustak-server/src/web/api/extract.rs:64` (`Administrative`) gates on `identity.principal.is_admin` alone. A
repository-wide search for readers of `SCOPE_ADMIN` or `claims.scope` outside tests returns only the *issue*
sites (`rustak-server/src/auth/tokens.rs:50-56`, `rustak-server/src/marti/oauth.rs:372`).

Two controls written as if scope were authoritative are therefore no-ops:

- `rustak-server/src/marti/oauth.rs:365-372` calls the recorded code scope "the *ceiling*, so a code minted
  for an ordinary session cannot become an administrative one either". It computes `is_admin` correctly and
  the value reaches only the `scope` **string**.
- `rustak-server/src/auth/oauth_server/authorize.rs:167` records `scope_for(...)` on the code for the same
  reason, with the same effect.

**Why it matters.** A token deliberately minted narrow still carries whatever its account can do. Combined
with H2 this is the difference between "a client password reaches `/oauth/token` and `signClient` and nothing
else" (`conventions.md`, `compat/oauth.md` §1) and "a client password is an administrator session".

**Reproduction (failing test).**

```rust
let server = TestServer::start().await;
let (user, _) = server.signed_in("ada", /* admin */ true).await;
let (token, _) = server.jwt().unwrap().issue(&user.username, "api", None, None).unwrap();
// GET /api/v1/users with `Authorization: Bearer {token}`
// expected 403 ("Only an administrator may do that."); actual 200
```

**Fix.** Decide whether `scope` is authoritative. If it is, thread the granted scope into `users::principal`
and AND it in for `AuthMethod::Bearer` (`is_admin = stored_admin && scope_contains("admin")`), and fix M6 in
the same change. If it is not, delete the "ceiling" comments so the code stops describing a control it does
not have.

---

### H2 — `/oauth/authorize` accepts a raw bearer, which launders a password-grant token into a 30-day administrative refresh family

`rustak-server/src/auth/oauth_server/authorize.rs:115-126` resolves the caller with
`resolve_principal(..., ListenerAuthPolicy::public())`. That policy has `bearer: true`
(`rustak-server/src/auth/resolve.rs:229-235`) and the header arm runs *first*
(`rustak-server/src/auth/resolve.rs:304-322`). So an access token — not a browser session — mints an
authorization code, with no user interaction and no consent step.

`compat/oauth.md` §1 pins the password grant's "**no `refresh_token`**" property deliberately, and
`rustak-server/src/marti/oauth.rs:256-278` honours it. Two extra requests undo it.

**Reproduction.**

```
1) POST /oauth/token   grant_type=password&username=ada&password=<client password>
   -> 200 {"access_token":"T","token_type":"Bearer","expires_in":...}      # no refresh token, as designed

2) GET /oauth/authorize?response_type=code&client_id=<registered>&redirect_uri=<registered>
        &code_challenge=<S256(V)>&code_challenge_method=S256
   Authorization: Bearer T
   -> 302 Location: <registered>?code=C

3) POST /oauth/token   grant_type=authorization_code&code=C&client_id=<registered>
        &redirect_uri=<registered>&code_verifier=V
   -> 200 {"access_token":..., "refresh_token":R, "scope":"api admin"}
      # R lives for [auth] refresh_token_ttl, default 30d (rustak-server/src/config/auth.rs:44)

4) GET /api/v1/users   Authorization: Bearer <the new access token>   -> 200   (see H1)
```

The cookie-driven form of step 2 is already covered by `rustak-server/tests/oauth_flows.rs:147`; only the
header form is untested.

**Fix.** Require a browser session (the `access_token_N` cookie) at `/oauth/authorize` rather than any
bearer, or refuse to mint a code for a principal whose token came from the password grant.

---

### H3 — `GET /api/v1/events` is an unfiltered cross-channel firehose

`rustak-server/src/web/api/events.rs:83-87` is the entire authorization on the feed:

```rust
if !caller.is_admin() && !caller.is_service() {
    return Err(ApiError::forbidden(...));
}
```

Nothing downstream filters by channel. `rustak-server/src/plugins/events.rs:166-182` (client
connect/disconnect), `:192-201` (missions), `:203-208` (channels) and `:216-228` (packages) publish to one
global bus, and `rustak-server/src/web/api/events.rs:133-178` writes every frame to every subscriber.

A **service account is not an administrator** — it is an ordinary account, normally with no channel
memberships at all; `rustak-server/src/plugins/mod.rs:28-35` says so ("Nothing here can do more than a client
could"). Over the feed it nevertheless receives, for the whole installation: every device's `username`, `uid`
and `callsign` as it connects and disconnects; every mission `name`, `guid` and `author_uid`; every account
whose channel memberships changed; and every package's `uid`, `name`, `hash`, `size` and `submitter`.

The codebase has the correct model for each of these and the feed bypasses all three:

- `rustak-server/src/stream/hub.rs:403-412` — `snapshot_for(viewer)` exists precisely to filter connections
  by `can_reach(peer.groups, viewer.groups)`.
- `rustak-server/src/web/api/clients.rs:86` — `GET /api/v1/clients` is `Administrative`, so a service token
  gets `403` there and the same data over SSE.
- `rustak-server/src/web/api/packages.rs:330-341` — `readable()` answers an out-of-channel package with the
  same `404` as a missing one, because "telling a caller that a package exists but is out of their channels
  is an oracle over the whole store". The feed hands out that oracle *including the `hash`*, which is the
  download handle for `GET /Marti/sync/content?hash=`.

`PackageEvent` (`rustak-api/src/event.rs:163-184`) also drops `ResourceRow.groups`, so a well-behaved
consumer could not self-filter even if it wanted to — the server has the information and discards it.

**Reproduction.**

```
# admin
POST /api/v1/users        {"username":"svc.weather","kind":"service"}         # no memberships
POST /api/v1/credentials  {"kind":"service_token","username":"svc.weather"}   -> rsk_...
# sidecar
POST /api/v1/services/register  Authorization: Bearer rsk_...  {"name":"weather"}
GET  /api/v1/clients            Authorization: Bearer rsk_...  -> 403         # correctly refused
GET  /api/v1/events             Authorization: Bearer rsk_...  -> 200 text/event-stream
# connect an EUD in channel "Blue" (svc.weather is not in Blue) and upload a Blue-only package
# the feed emits client.connected with that EUD's username/uid/callsign,
# and package.uploaded with the package name, size and hash.
```

**Failing-test sketch** (`web::api::events::tests`): create `svc.weather` with no groups; record a
`Blue`-only resource; open the feed with the service token; assert the body contains neither the package name
nor its hash.

**Fix.** Resolve the subscriber's `GroupSet` and filter each event the way `hub::snapshot_for` and
`packages::readable` already do; carry `groups` on `PackageEvent` so the filter has something to match.

---

### H4 — The event feed never re-checks authorization for the life of the connection

`rustak-server/src/web/api/events.rs:78-116` resolves the caller once; `:133-178` then streams until process
shutdown or client disconnect. Nothing revisits the credential, the account or admin status.

A revoked service token, a disabled account, a demoted administrator and a removed channel membership all
keep receiving the full feed indefinitely. This is the opposite of what the rest of the codebase does and
says: `rustak-server/src/auth/cert.rs:11-17` ("A certificate revoked, an account switched off or a channel
taken away has to take effect on the next request"), `rustak-server/src/auth/resolve.rs:139-140`, and
`rustak-server/src/identity/members.rs:176-190`, which re-authenticates live stream connections on a channel
change.

**Reproduction.** Open `GET /api/v1/events` with a service token; as an administrator
`DELETE /api/v1/credentials/{id}` and `PATCH /api/v1/users/svc.weather {"disabled":true}`; the open stream
keeps delivering.

**Fix.** Re-run `plugins::auth::caller` on the keepalive tick inside `frames()` and end the stream on
failure — which is also where H3's filter has to be re-derived, since membership can change mid-connection.

---

### H5 — Disabling an account does not end its live `:8089` session

`rustak-server/src/web/api/users.rs:210-225` sets `disabled` and revokes the account's refresh tokens. HTTP
is then safe: `bearer()` re-reads `disabled` per request (`auth/resolve.rs:141-145`) and so does
`client_cert()` (`auth/cert.rs:111-115`).

The CoT stream is not. `rustak-server/src/stream/resolver.rs:103-215` runs **once**, at the TLS handshake,
and checks `user.disabled` there (`:207`); `rustak-server/src/stream/router.rs` and
`rustak-server/src/stream/connection.rs` make no database call at all thereafter. The only thing that drops a
live session is certificate revocation, through the hook at `rustak-server/src/stream/mod.rs:175-178` →
`rustak-server/src/stream/live.rs:222-224`. Nothing revokes an account's certificates when it is disabled:
`RevokeReason::UserDisabled` (`rustak-server/src/pki/revoke.rs:79`) is reachable only as an operator-typed
reason on `rustak-server/src/web/api/certificates.rs:280`.

`pki::revoke::revoke_for_credential` *does* drop the sessions, so revoking an enrolment credential is fine;
the gap is the disable path, and `DELETE /api/v1/credentials/{id}` for a client password, which has no
certificate to cascade to.

**Why it matters.** Disabling an account is the control an operator reaches for when a device is lost or an
account is compromised. Until that device's TCP connection drops on its own (idle timeout, or a server
restart) it continues to receive every reachable peer's position and to inject CoT.

**Reproduction.** Connect an EUD to `:8089` with a valid certificate; `PATCH /api/v1/users/{u}`
`{"disabled": true}`; observe the connection stays in `GET /api/v1/clients` and that position reports still
flow in both directions.

**Failing-test sketch** (`rustak-server/tests/stream_session.rs`): connect, disable the account, assert the
connection is closed within one keepalive interval.

**Fix.** Give `LiveState` a `disconnect_by_user` and call it from the disable path (and from credential
revocation), next to the existing `revoke_all_for_user`.

---

## Medium

### M1 — A mission `ACCESS` token matches on name **or** guid, so it survives a delete-and-recreate

`rustak-server/src/missions/roles.rs:219`:

```rust
if claims.mission_name != mission.name && claims.mission_guid != mission.guid {
    return Ok(None);
}
```

Either spelling is enough — deliberately, so that renaming a mission does not invalidate outstanding tokens.
But mission names are reusable: `rustak-server/migrations/0006_missions.sql:40` is
`CREATE UNIQUE INDEX idx_missions_name_live ON missions (name) WHERE deleted_at IS NULL`, so deletion is a
soft delete and a new mission may take the old name with a fresh guid.

For `TokenType::Subscription` and `TokenType::Invitation` the row lookups then filter on
`row.mission_id == mission.id` (`roles.rs:225-247`), so a stale token fails. `TokenType::Access` does not:
`roles.rs:224` returns `MissionRole::default_role(mission.default_role)` computed from *this* mission, without
ever comparing `claims.id` (the old guid) to anything. ACCESS tokens are minted with `ttl = None`
(`rustak-server/src/missions/subscriptions.rs:370-378`), so they never expire.

**Reproduction.**

```
1) PUT  /Marti/api/missions/alpha?password=p1                        (mission A, guid Ga)
2) PUT  /Marti/api/missions/alpha/token?password=p1  -> 201 { token: T }     # ACCESS, no exp
3) DELETE /Marti/api/missions/alpha
4) somebody else: PUT /Marti/api/missions/alpha?password=p2          (mission B, guid Gb)
5) GET  /Marti/api/missions/alpha    MissionAuthorization: Bearer T
   -> 200 with mission B's default role, without ever knowing p2
```

**Fix.** For `Access`, require `claims.mission_guid == mission.guid` (the guid is what the token's own `id`
claim carries), or record `mission_id` beside the token and compare that. Renaming is already covered by the
guid arm, so the name arm is not load-bearing for ACCESS.

---

### M2 — `access_token_N` cookies reach `/Marti/**`, and `GET /Marti/sync/delete` is destructive

`rustak-server/src/auth/oauth_server/cookies.rs:53` allows cookie authentication on the `/Marti` prefix, and
`rustak-server/src/auth/resolve.rs:327-343` reads it. The reasoning in the module docstring is sound as far
as it goes — `SameSite=Lax` blocks cross-site `POST` and `fetch` — but Lax **does** attach the cookie to a
cross-site *top-level navigation*, and the Marti surface has a state-changing `GET`:

- `rustak-server/src/marti/sync.rs:63` — `.route("/sync/delete", web::get().to(sync_read::delete))`
- `rustak-server/src/marti/sync_read.rs:149-181` — deletes by `hash` **or** by `PrimaryKey`, and
  `CiQuery::list` (`rustak-server/src/marti/extract.rs:295-309`) accepts a comma-separated list, so one URL
  can name hundreds of integer ids with nothing to guess.
- `rustak-server/src/marti/sync_read.rs:176` — `viewer.can_write(resource)` is the submitter check, and
  `rustak-server/src/files/metadata.rs:83-85` short-circuits it to `true` for an administrator.

So a single link, opened by a signed-in operator, deletes every enterprise-sync resource on the server.

**Reproduction.** Sign in through `/login/*` in a browser (so the `access_token_0` cookie is held), then visit
an attacker page containing `<a href="https://tak.example/Marti/sync/delete?PrimaryKey=1,2,3,...,500">…</a>`
or a `window.open` of the same URL. The response is the `Deleted N resource(s)` HTML page and the rows are
gone.

**Fix.** Do not authenticate a cookie for a request whose method is `GET` on a path that mutates; the
cheapest version is to make `/Marti/sync/delete` cookie-ineligible, or to require an `Authorization` header
(not a cookie) for any Marti route that writes. ATAK and CloudTAK both send a header, so nothing in `compat/`
depends on the cookie reaching a write.

Related, lower impact: `rustak-server/src/marti/login.rs:57` mounts `GET /logout`, and
`rustak-server/src/auth/oauth_server/session.rs:99-136` revokes the `jti` **and** every refresh family for the
account (`auth/tokens.rs:167`). Any site can therefore sign a rustak user out everywhere with a link.

---

### M3 — The one-time enrolment token is a check-then-act race, and spending it is best-effort

`rustak-server/src/marti/enroll.rs:51-111` runs: parse CSR → check CN → issue certificate → `spend(...)` at
`:108`. The usability check happens much earlier and in a separate read:
`rustak-server/src/identity/verify.rs:147-201` reads the row and `usable()` at `:235` compares
`candidate.uses >= max_uses`. Consumption is a second, later write:
`rustak-server/src/db/repos/credentials.rs:268-288` does an unconditional `uses = uses + 1` followed by a
conditional `revoked_at` update. Nothing serialises the two.

Two concurrent `POST /Marti/api/tls/signClient/v2` with the same token therefore both pass `verify()`
(`uses = 0`, `revoked_at IS NULL`) and both receive a certificate. The window spans one argon2 verification
plus a signature — comfortably milliseconds.

Separately, `rustak-server/src/marti/enroll.rs:162-182` treats a failed spend as a `warn!`, so a token whose
consuming write fails stays live.

**Impact, stated honestly.** Both certificates name the same account, so this is not a path to somebody
else's identity. It breaks the *property* `plan.md` rests the EUD credential model on ("one-time … consumed
on successful `signClient`") and gives a single token holder N independently-revocable certificates.
`rustak-server/tests/enroll_flows.rs:344` covers the sequential case only.

**Failing-test sketch** (`rustak-server/tests/enroll_flows.rs`, which already has a live-server harness): mint
one enrolment token, fire two `signClient/v2` posts with `tokio::join!`, assert exactly one succeeds and that
`credentials.uses == 1`.

**Fix.** Make consumption the gate: one `UPDATE credentials SET uses = uses + 1, revoked_at = ?
WHERE id = ? AND revoked_at IS NULL AND (max_uses IS NULL OR uses < max_uses)` that reports rows changed,
claimed *before* the certificate is signed and released (or the certificate revoked) if signing then fails.

---

### M4 — Any account can re-bind another account's device record by enrolling with its `clientUid`

`rustak-server/src/identity/devices.rs:32-59` (`upsert_seen`) writes `db.devices().seen(uid, user_id, seen)`
unconditionally and only `warn!`s when the row changes hands (`:43-53`). It is reached from enrolment with
the caller-supplied `clientUid` (`rustak-server/src/marti/enroll.rs:86-89, 118-143`), which is never validated
against the account.

A device uid is not a secret: it is the `uid` attribute of every CoT event that device sends on a shared
channel, and it is listed by `GET /Marti/api/clientEndPoints`.

**Consequences.** After user B enrols with `clientUid = <A's device uid>`, the `devices` row belongs to B. A's
certificate row still references that `device_id`, and `rustak-server/src/auth/cert.rs:152-180` computes A's
effective channels as `effective_for_device(db, A, device_id, ...)` — an intersection with the
`device_group_state` rows for a device B now owns. `channels::apply` correctly refuses a `clientUid` that is
not the caller's (`rustak-server/src/marti/channels.rs:222-246`), but it now *is* B's, so B can switch A's
channels off through `PUT /Marti/api/groups/active?clientUid=<A's uid>`. A's device also disappears from
`GET /api/v1/devices?username=A`.

**Reproduction.** As B (any account can mint itself an enrolment token), read A's device uid off any CoT event
or from `clientEndPoints`, then `POST /Marti/api/tls/signClient/v2?clientUid=<A's uid>` with a CSR for `CN=B`.
Then `PUT /Marti/api/groups/active?clientUid=<A's uid>` with every channel `active: false` and watch A's
device go dark.

**Fix.** Refuse an enrolment whose `clientUid` already belongs to a different account (a `403`, with an
administrator-only "release this device" action), or key `device_group_state` on `(user_id, device_id)`.

---

### M5 — Service tokens bypass the configured `user_acl` expression

`rustak-server/src/plugins/auth.rs:124-126` returns as soon as the presented secret resolves as a service
token, **before** the `bearer()` call at `:141`. The `user_acl` evaluation lives inside `bearer()`
(`rustak-server/src/auth/resolve.rs:159-171`).

An operator who writes `[auth] user_acl = 'client_ip in 10.0.0.0/8'` (or a path or header restriction) gets it
enforced on every `/api/v1` route *except* `/api/v1/services/*` and `/api/v1/events` when a service token is
presented — which, given H3, is exactly backwards.

**Reproduction.** Set `user_acl` to refuse the test client's address; `GET /api/v1/me` with a user JWT →
`403`; `GET /api/v1/events` with a service token → `200` and streaming.

**Fix.** Build the `AuthRequestFilter` and call `evaluate` in the service-token arm too, before returning
`Resolved`.

Related, same file: `rustak-server/src/plugins/auth.rs:114-118` reads `conn_data::<PeerCertificate>()` without
consulting any `ListenerAuthPolicy`. That arm is unreachable today (`PeerCertificate` is attached only by
`on_connect_capture` at `rustak-server/src/web/server.rs:141`, on the Marti listener, and `/api/v1` is mounted
only on the public one at `:64`), so the documented "certificate, then service token, then access token" order
is not what runs — and if `/api/v1` is ever mounted on the mTLS listener, *any* EUD certificate will silently
outrank an explicit bearer header on the control API.

---

### M6 — Scope widening on refresh: `rotate` ignores the scope stored with the family

`rustak-server/src/auth/tokens.rs:134-137` recomputes `is_admin` from the account and mints
`scope_for(is_admin)`. `spent.scope` is read out of the row at `:110-122` (it is a real column —
`rustak-server/src/db/repos/refresh_tokens.rs:36,113`) and never compared. The ceiling deliberately applied at
`rustak-server/src/marti/oauth.rs:371-372` therefore survives exactly one token: the first refresh widens
`api` back to `api admin`.

Latent while H1 stands, which is what makes it dangerous — fixing H1 alone leaves a working bypass.

**Failing-test sketch** (`auth::tokens::tests`): insert a refresh row with `scope: "api"` for an admin account,
`rotate`, assert the new access token's `scope` is still `"api"`.

---

### M7 — The `state` cookie has no `__Host-` prefix, so a sibling origin can fixate a sign-in

`rustak-server/src/auth/oauth_server/cookies.rs:103-115` sets `state` with
`HttpOnly; Secure; SameSite=Lax; Path=/login` and no `__Host-` prefix, so any HTTPS origin under the same
registrable domain (`anything.example.com`) can write a `Domain=example.com; Path=/login; Secure` cookie of
that name which the browser will send to the rustak host.

The whole browser binding is `sha256(cookie) == state`
(`rustak-server/src/auth/oauth_server/login.rs:182-192`, `state.rs:141-145`), and the server-side one-shot
record is keyed by **that same digest** (`state.rs:152-155,168-185`) — so it is satisfied by the attacker's
own live flow rather than being an independent second binding.
`rustak-server/src/auth/oauth_server/mod.rs:24-31` claims the cookie "stops a callback replayed into somebody
else's browser"; that holds only while the cookie cannot be written by somebody else.

**Reproduction** (requires an HTTPS sibling subdomain, or a script-injection foothold on one).

```
1) attacker's browser: GET /login/auth      -> state S; server stores PendingAuth under idp:H(S)
2) attacker completes the IdP leg, capturing /login/redirect?code=C&state=H(S) without visiting it
3) from https://evil.example.com:
     document.cookie = "state=S; Domain=example.com; Path=/login; Secure; SameSite=Lax"
4) lure the victim to https://tak.example.com/login/redirect?code=C&state=H(S)   (top-level GET; Lax sends it)
-> complete() (login.rs:297-339) sets access_token_N for the ATTACKER's account in the victim's browser
```

The victim then works under the attacker's identity — every upload and every mission edit lands in the
attacker's account.

**Fix.** Keep the `state` name TAK clients expect and add a second, independent binding the attacker cannot
transplant: a high-entropy value in a `__Host-`-prefixed cookie at `Path=/`, stored in `PendingAuth` and
required on the callback. Record the decision in `compat/oauth.md` §4 either way.

---

### M8 — Every sweep over the `auth-state` partition aborts on the other record shapes

Four incompatible record types share `AUTH_STATE_PARTITION` (`rustak-server/src/auth/setup.rs:30`):

| writer | key | type |
|---|---|---|
| `rustak-server/src/auth/passkey_store.rs:118-133` | `ceremony:<handle>` | `Ceremony` |
| `rustak-server/src/auth/setup.rs:129-137` | `setup-token` | `SetupRecord` |
| `rustak-server/src/auth/setup.rs:203-211` | `registration:<digest>` | `RegistrationRecord` |
| `rustak-server/src/auth/oauth_server/state.rs:153` | `idp:<digest>` | `PendingAuth` |

`KeyValueStore::list` (`rustak-server/src/db/kv.rs:126-142`) ends in `rows.collect()` over a `Result`
iterator, and `json_col` (`rustak-server/src/db/row.rs:127-134`) turns a serde failure into a
`rusqlite::Error` — so **one** row of the wrong shape aborts the whole listing, before the
`key.starts_with(PREFIX)` filter in either loop body is ever reached
(`passkey_store.rs:183,187`; `state.rs:199,203`).

`setup::ensure` writes `SetupRecord` on the **first start of every installation** (`setup.rs:112-140`) and
only removes it when the wizard completes, so both sweeps fail from first boot until setup finishes;
afterwards they fail whenever a passkey ceremony and a pending sign-in are outstanding together, which the
5-minute and 10-minute TTLs against a 600 s sweep interval (`runtime.rs:74`) make the common case. Both
failures are swallowed as `warn!` at `rustak-server/src/runtime.rs:413-424`.

Both row-producing endpoints are unauthenticated and effectively unlimited:
`GET /login/auth` writes one row per call (`rustak-server/src/auth/oauth_server/login.rs:117-128`), and
`POST /api/v1/auth/passkey/login/start` writes one per call and records nothing with the rate limiter (M9).
So an anonymous caller can grow the `kv` table without bound. Authentication itself is unaffected — every
record is expiry-checked at claim time (`state.rs:180`, `passkey_store.rs:158`) — so this is disk exhaustion
plus a silently dead housekeeping job.

**Failing-test sketch** — extend `sweeping_takes_away_the_ceremonies_nobody_finished`
(`passkey_store.rs:391`):

```rust
db.set(AUTH_STATE_PARTITION, "setup-token".to_string(),
       serde_json::json!({"hash": "x", "created_at": Utc::now()})).await.unwrap();
assert_eq!(sweep(&db).await.unwrap(), 1);   // currently Err, not Ok(1)
```

**Fix.** Filter by key prefix in SQL, list into `serde_json::Value` and deserialise per row, or give each
family its own partition.

---

### M9 — `passkey/login/start` records no rate-limiter outcome: unthrottled username probing

`rustak-server/src/web/api/passkey.rs:150-183` calls `limiter.check(address, SUBJECT)` at `:157` but never
`record_failure` or `record_success` — unlike the other three ceremony handlers (`:59, :62, :111, :117,
:207, :216`). A bucket only locks once `record_failure` is called
(`rustak-server/src/auth/ratelimit.rs:94-122`), so this endpoint is effectively unlimited.

Two consequences:

**(a) Username enumeration.** With `username` supplied, a `200` carrying challenge options means "this account
exists, is enabled and has at least one passkey"; everything else is the same `400 "That account has no
passkey registered."` (`passkey.rs:403-405` and `auth/passkeys/login.rs:77-82`). Unknown-versus-disabled-
versus-no-passkey is correctly indistinguishable — the oracle is the `200`. The module comment at
`auth/passkeys/login.rs:44-48` claims the endpoint "must not be a way to ask which accounts exist", which is
true of the discoverable branch and false of the username-assisted one.

**(b)** Each call writes an unbounded KV row — the input side of M8.

**Reproduction.**
`for u in wordlist; do curl -s -o /dev/null -w '%{http_code}\n' -XPOST .../auth/passkey/login/start -H 'content-type: application/json' -d "{\"username\":\"$u\"}"; done`
— `200` marks real accounts, indefinitely.

**Fix.** `limiter.record_failure(address, SUBJECT)` on the `no_passkey` path, and count `login/start` itself.

---

### M10 — WebAuthn relying-party identity is not pinned, and falls back to the client-supplied `Host`

`rustak-server/src/web/api/passkey.rs:357-371` derives the RP id and the single allowed origin from
`settings::base_url(...)` **or**, when that is unset, `request_base_url(trust_proxy, request)` — the `Host`
header, or `X-Forwarded-Host` under `trust_proxy` (`rustak-server/src/web/helpers/request.rs:92-137`). That
value becomes both the `RpId` and the only entry of `allowed_origins`
(`rustak-server/src/auth/passkeys/mod.rs:101-148`), i.e. the caller supplies the value the origin check
compares against.

Two details make it worse than the fallback alone:

- `Passkeys` is rebuilt per request in all four handlers and the stored `Ceremony`
  (`rustak-server/src/auth/passkey_store.rs:103-111`) does **not** record the RP ID, so a ceremony started
  under one `Host` can be finished under another.
- `settings::base_url` is `None` only before the wizard has run
  (`rustak-server/src/identity/settings.rs:77-85`) — which is exactly when `register_finish` hands back an
  **admin session** (`rustak-server/src/web/api/passkey.rs:128-141`).

Phishing resistance itself survives: the browser binds the credential to the RP ID it saw, and
`validate_rp_id_hash` checks `SHA-256(rpId)` against `authData`. What is lost is pinning — a server reachable
under a second name (an extra A record, split-horizon DNS, a proxy that forwards `X-Forwarded-Host` verbatim)
mints credentials under that name and accepts them.

**Fix.** Record the RP ID in `Ceremony` and require the finish to match it; refuse a ceremony outright when no
base URL is configured.

---

### M11 — `services.enabled` is a kill switch that nothing reads

The column exists (`rustak-server/migrations/0002_identity.sql:194`), is loaded onto `ServiceRow`
(`rustak-server/src/db/repos/services.rs:41,65`) and has a setter (`:297-306`). No code outside tests reads it
— not `plugins/auth.rs`, not `plugins/registry.rs`, not `plugins/health.rs`, not `web/api/services.rs` — and
no route exposes it. A service marked `enabled = 0` still registers, heartbeats, reads its configuration and
opens the event feed. A fail-open control is worse than no control: wire it into `plugins::auth::caller` /
`registry::require`, or delete the column and the setter.

---

### M12 — An ordinary credential can pin ~128 MiB of memory and stall the stream hub

`Heartbeat.message` (`rustak-api/src/service.rs:258-259`) has no length bound.
`rustak-server/src/plugins/health.rs:46-68` passes it into `ServerEvents::service_status`, and
`rustak-server/src/plugins/events.rs:136-157` clones it into both the 256-entry ring and the 256-slot
broadcast buffer. The only cap is actix's default 256 KiB `web::Bytes` limit — no `PayloadConfig` is installed
anywhere in the crate — and the control API is deliberately unrate-limited
(`rustak-server/src/plugins/auth.rs:20-27`). Worst case from one service token: ~512 × 256 KiB ≈ 128 MiB
resident.

Worse, `rustak-server/src/plugins/events.rs:115-123` clones the matching slice of the ring **while holding**
`recent.lock()`, and `rustak-server/src/web/api/events.rs:93-96` calls it synchronously in the request handler.
Every publisher blocks on that lock — including `Hub::announce`, which runs on the connecting client's own
task (`rustak-server/src/stream/hub.rs:70-78`). Concurrent SSE reconnects with `?lastEventId=0` therefore
stall CoT connection registration.

**Fix.** Bound `message` (and the serialised event) at registration and heartbeat time; clone outside the lock
or publish `Arc<ServerEvent>`; cap concurrent subscribers (`events.rs:92` has no limit).

---

### M13 — Every one-shot claim over the KV store is a read-then-delete across two transactions

`rustak-server/src/auth/passkey_store.rs:146-163` (`claim`),
`rustak-server/src/auth/setup.rs:225-242` (`claim_registration`) and
`rustak-server/src/auth/oauth_server/state.rs:168-185` (`claim`) all `get` and then `remove` in **separate**
transactions. `Database::read` draws from a read pool while `Database::write` uses the single writer
(`rustak-server/src/db/connection.rs:209-249`), so two concurrent requests carrying the same handle both
observe the row before either delete lands, and both proceed. Each docstring asserts one-shot behaviour, and
each test covers only the sequential case (`passkey_store.rs:352`, `state.rs`).

The sharpest case is the wizard: `claim_registration` guards the registration token that
`register_finish` turns into an **administrator session** (`web/api/passkey.rs:128-141`), so two parallel
posts register two passkeys against the first admin account off one token. On the login side, N parallel
posts of a captured `{challenge_id, credential}` pair yield N sessions; on the registration side the second
`create` is saved only by `UNIQUE INDEX idx_passkeys_credential_id`
(`rustak-server/migrations/0002_identity.sql:136`) turning it into a `500`. The authorization-code claim is
*not* affected — `codes::redeem` does the read and the spend in one write transaction
(`rustak-server/src/auth/oauth_server/codes.rs:198-209`), which is the pattern the others should copy.

**Reproduction.**

```rust
let handle = begin(&db, CeremonyKind::Discover, b"state").await.unwrap();
let (a, b) = tokio::join!(claim(&db, &handle), claim(&db, &handle));
assert!(a.is_ok() != b.is_ok(), "a challenge must be claimable once");   // both are Ok today
```

**Fix.** One write transaction doing `DELETE FROM kv WHERE partition = ?1 AND key = ?2 RETURNING value`.

---

### M14 — `client.disconnected` leaks clients that went incognito

`rustak-server/src/stream/hub.rs:109-111` announces unregistration unconditionally, and
`ConnectionSummary::of` (`rustak-server/src/stream/live.rs:72-79`) does not carry `subscription.incognito`.
`rustak-server/src/plugins/events.rs:169-181` therefore publishes `client.disconnected` with `username`, `uid`
and `callsign` for a client that has deliberately gone incognito — a first-class privacy feature the client
toggles over CoT (`rustak-server/src/stream/control.rs:60-65`) and which every other reader honours
(`hub.rs:295`, `hub.rs:408`).

**Reproduction.** Connect, send incognito-on, confirm the client is absent from `GET /api/v1/clients`, then
disconnect — the feed still names it.

---

### M15 — The username-claim fallback chain can be an account-selection primitive

`rustak-server/src/web/helpers/oidc/claims.rs:68-72`:

```rust
let raw = string_claim(claims, &oidc.username_claim)
    .or_else(|| string_claim(claims, FALLBACK_USERNAME_CLAIM))   // preferred_username
    .or_else(|| string_claim(claims, "sub"))
```

An operator who deliberately points `username_claim` at a verified, immutable claim (`email`, `upn`, `oid`)
gets a **silent** downgrade to `preferred_username` for any principal whose chosen claim is absent. At Entra
ID, Keycloak and others that claim is self-service editable.

The takeover case is closed: `refuse_takeover` (`rustak-server/src/identity/users.rs:94-138`) blocks the
first-provision collision, and `users.username` is `UNIQUE COLLATE NOCASE`
(`rustak-server/migrations/0002_identity.sql:12,29`) while `Username::parse`
(`rustak-api/src/identity/username.rs:51-89`) lower-cases and reserves `anonymous`, `rustak`, `takserver` and
the `__` prefix. What remains: with `link_by_username = true` the fallback is a direct takeover by name
(`users.rs:105-107` returns early); without it, a user can still steer themselves onto an arbitrary *unused*
name, which matters wherever `user_acl` / `admin_acl` are written over `username` rather than `claims.*`
(`rustak-server/src/auth/acl.rs:59`).

**Fix.** Fail closed when an explicitly configured `username_claim` is absent; fall back only when the key was
left at its default.

---

### M16 — Service-registration existence oracle for any authenticated caller

`rustak-server/src/web/api/services.rs:200-214` (`owned`) resolves the caller, then calls `registry::require`
— which answers `404 "No service named '{name}' is registered."` (`:271-273`) — **before**
`caller.require_owns(&row)` produces the `403`. So any holder of any valid credential enumerates the sidecar
fleet by probing `GET /api/v1/services/{name}/config`: `403` means registered, `404` means not.
`GET /api/v1/services` is admin-only precisely to prevent that, and the same 404-before-403 policy is written
out twice elsewhere (`rustak-server/src/web/api/subject.rs:9-15`,
`rustak-server/src/web/api/packages.rs:325-327`).

The existing test `one_service_may_not_read_or_change_another` (`services.rs:406`) registers both services, so
it never exercises the missing-name branch.

**Fix.** Check ownership before existence, or map `Unknown` to the same `403` for a non-admin caller.

---

## Low

### L1 — `rate_limit` is keyed on `(client_ip, subject)`, so a distributed or proxy-spoofed caller has no limit

`rustak-server/src/auth/ratelimit.rs:51,73,94` buckets on `(Option<IpAddr>, String)`. There is no per-account
counter independent of the address, so guessing from many addresses is unbounded; and where
`[server] trust_proxy = true` **without** a proxy that overwrites the header, a fresh `X-Forwarded-For` per
request yields a fresh bucket (`rustak-server/src/web/helpers/request.rs:52-65`). Every secret behind the
limiter is 100–256 bits of server-generated entropy, so this is not currently exploitable; it becomes so the
day a human-chosen secret is added. A per-account counter alongside the per-pair one would close it.

### L2 — The passkey limiter's subject is the constant `"passkey"`, so one failing client locks out a NAT

`rustak-server/src/web/api/passkey.rs:38` (`const SUBJECT: &str = "passkey"`), against
`rustak-server/src/auth/ratelimit.rs:70-72`, which documents the subject as "whatever the endpoint is guessing
at — a username, a challenge id … so that failures against one account do not lock out another from the same
office". For passkeys that property does not hold: the bucket is `(client_ip, "passkey")`, shared by all four
handlers. Ten failures in a minute (`config/auth.rs:355-365`) — ten `register/start` calls with an expired
session, say — lock every passkey sign-in from that address for fifteen minutes. With `trust_proxy = false`
behind a reverse proxy that address is the proxy's, i.e. everyone.

### L3 — `register_finish` issues a bootstrap session without re-checking `disabled`

`rustak-server/src/web/api/passkey.rs:119-141` loads the user at `:119` and goes straight to
`tokens::issue_session(..., user.is_effective_admin(), ...)` at `:132`. `login_finish` refuses a disabled
account (`:220-224`); this path does not, so an account disabled between `register/start` and
`register/finish` still receives a session. Reaching it needs the challenge handle, which is only handed to
the authorised starter, so this is a hardening gap rather than a live bypass — and a three-line fix.

### L4 — Adding an authenticator needs no recent interactive authentication

`rustak-server/src/web/api/passkey.rs:318-354` accepts any non-revoked RS256 token whose `sub` maps to an
enabled account (`auth/resolve.rs:114-175`). Registering a passkey is a permanent credential addition that
grants full interactive sign-in, so a token minted for a non-interactive client is enough to escalate to one.
Given H1 (scope is not enforced) there is no narrower token to hold, which is why this is worth stating
alongside it.

### L5 — No minimum curve strength for an ECDSA CSR

`rustak-server/src/pki/csr.rs:381-392` enforces `min_rsa_bits` (default 2048) for RSA and accepts **any**
elliptic-curve key when `csr_allow_ecdsa` is on (the default, `config.example.toml:397`). A P-192 or
secp112r1 request passes policy. In practice rcgen or rustls refuses the result later, so the effect is an
unhelpful `500` rather than a weak certificate — but the asymmetry with the RSA floor is not intentional. Add
a curve allow-list (P-256/P-384) beside `min_rsa_bits`.

### L6 — The revocation cache fails **open** on a poisoned lock

`rustak-server/src/pki/revoke.rs:239-245` reads a poisoned `RwLock` as "not in the set". That is right for
`known` (it refuses) and wrong for `revoked` (it accepts). The comment says "the reload that follows any such
panic restores it" — it cannot: `replace` at `:247-251` is also `if let Ok(...)`, and a poisoned lock stays
poisoned for the life of the process. Practically unreachable (nothing panics under the lock), but the stated
reasoning does not hold; use `unwrap_or_else(PoisonError::into_inner)` as `RateLimiter::lock`
(`auth/ratelimit.rs:147-149`) already does, or treat a poisoned `revoked` read as a refusal.

### L7 — Service-token uses are never recorded, so `max_uses` is dead and `last_used_at` is always null

`rustak-server/src/plugins/auth.rs:202-207` checks `candidate.uses < max`, but nothing in that module calls
`identity::credentials::record_use` — the only thing that increments `uses`
(`rustak-server/src/db/repos/credentials.rs:268-288`). Every other credential path does
(`auth/basic.rs:142`, `marti/oauth.rs:246`, `marti/enroll.rs:173`). A service token minted with
`max_uses = 1` works forever, and an operator cannot tell a live sidecar from a leaked dormant token.
**Failing test:** mint with `max_uses = 1`, call `plugins::auth::caller` twice, expect the second to be
`Rejected`.

### L8 — `azp` is never checked, so a multi-audience ID token is accepted

`rustak-server/src/web/helpers/oidc/validate.rs:121,125` uses `set_audience(&[client_id])`, which
`jsonwebtoken` satisfies when the token's `aud` array merely *contains* our id. OIDC Core §3.1.3.7 steps 4–5
require rejecting a multi-audience token unless `azp == client_id`; `azp` appears only at
`rustak-server/src/web/helpers/oidc/claims.rs:29`, as a claim hidden from the ACL expressions. Exploitable
only where another client at the same IdP may request our audience.

### L9 — The ID-token algorithm check is a deny-list

`rustak-server/src/web/helpers/oidc/validate.rs:85-99` rejects HS256/384/512 and then builds
`Validation::new(header.alg)` at `:120` — the token still chooses from every other algorithm `jsonwebtoken`
supports. Not exploitable today (`DecodingKey::from_jwk` carries a key family and a mismatch is refused), but
`rustak-server/src/auth/jwt.rs:233,369-372` gets this right twice for our own tokens and this should match,
ideally against the provider's `id_token_signing_alg_values_supported`.

### L10 — Rate limiting covers only the password grant on `/oauth/token`

`rustak-server/src/marti/oauth.rs:199-207` installs the limiter inside `password_grant` alone.
`refresh_grant` (`:286-308`) and `code_grant` (`:319-397`) each open a SQLite **write** transaction
(`db/repos/refresh_tokens.rs:160-200`, `auth/oauth_server/codes.rs:158-212`), and `GET /login/auth` writes a
KV row per call — all unauthenticated and unlimited. Not a guessing risk (every secret is 256 bits); it is
write amplification against a single-writer database, and with M8 an unbounded disk fill.
(`/api/v1/auth/token` and `/api/v1/auth/refresh` *are* limited — `web/api/auth.rs:92,203`.)

### L11 — Refresh tokens are not bound to the client they were issued to

`rustak-server/src/db/repos/refresh_tokens.rs:36,76` stores `client`;
`rustak-server/src/auth/tokens.rs:102-144` never reads it and overwrites it with the caller's own label at
`:137`; `marti/oauth.rs:286-308` takes no `client_id` at all. A token minted for OAuth client A is redeemable
by B or at `/api/v1/auth/refresh`. All clients here are public bearer holders so the practical impact is
small, but it removes the audit value of the column (RFC 6749 §6).

### L12 — `[auth.oidc] endpoint` and the discovered `jwks_uri` are fetched with no scheme or origin check

`rustak-server/src/web/helpers/oidc/discovery.rs:62-63` builds the discovery URL from `oidc.endpoint` with no
`https` requirement, and `:87-112` fetches `discovery.jwks_uri` verbatim — an arbitrary host chosen by the
discovery document. `rustak-server/src/config/validate.rs` validates `oauth_clients` (`:246-306`, including
https-or-loopback) but nothing under `[auth.oidc]`. Operator-controlled, so low; a compromised provider
currently gets a same-process SSRF primitive.

### L13 — Unauthenticated `POST /Marti/ErrorLog` writes to SQLite on every request

`rustak-server/src/marti/util.rs:180-235` accepts an anonymous body, writes a KV row and prunes. Retention
bounds the rows at 200 (`rustak-server/src/config/marti.rs:29`) and the stored body at 64 KiB
(`util.rs:71`), so this is not storage growth — but it is an unauthenticated write plus a sort and truncate
per request against a single-writer database, which `conventions.md` ("no high-rate writes into SQLite")
would rather avoid. Either rate-limit it or make `store_error_logs` default off.

### L14 — A lagged SSE consumer receives history it did not ask for

`rustak-server/src/web/api/events.rs:168-171` refills from `events.since(feed.seen)`, and a consumer that sent
no `Last-Event-ID` has `seen == 0` (`:109`), so lagging before its first read replays the whole ring —
contradicting the contract its own test asserts (`:335-352`). Seed `seen` from `events.latest_id()`
(`plugins/events.rs:127-129`, currently unused) when no resume id was supplied.

### L15 — Check-then-act in `registry::register`, and a rename answers `500`

`rustak-server/src/plugins/registry.rs:67-76` reads the existing row and then upserts
(`db/repos/services.rs:133-141`), so two concurrent registrations of a momentarily-absent name can both pass
and the later write takes the registration. Separately, `idx_services_user` is UNIQUE on `user_id`
(`migrations/0002_identity.sql:199`), so a sidecar re-registering under a different name hits a constraint
violation that surfaces as a `500` (`web/api/services.rs:274-278`) rather than a message an operator can act
on.

### L16 — No request-body limit on the ceremony endpoints

No `JsonConfig` or `PayloadConfig` is installed anywhere in `rustak-server/src`, so actix's 2 MiB default
applies to the attacker-supplied `credential` blob that `finish_registration` base64-decodes and CBOR-parses.
`webauthn_rp` bounds the RSA modulus to 2048–16384 bits and its CBOR parser never pre-allocates from a
wire-supplied length, so this is CPU cost rather than a crash or an allocation bomb — but a few KiB is a
generous ceiling for a WebAuthn response.

---

## Info

- **`anon_group_default = true` overrides an administrator's channel removal.**
  `rustak-server/src/identity/active.rs:263-301` adds `__ANON__` in both directions to every effective set
  regardless of membership. This is honest rather than hidden — `members::replace_manual`
  (`rustak-server/src/identity/members.rs:95-107`) re-adds `__ANON__` to the grant list so the API echoes it
  back — but it means the only way to isolate an account is `anon_group_default = false` or disabling it.
  Worth one line in the UI where memberships are edited.
- **`allow_access_token_retrieval = true` (default).** `GET /token/access` hands a cookie-authenticated caller
  its own bearer token, which converts any same-origin script execution into token exfiltration despite
  `HttpOnly`. It is not reachable cross-site (a top-level navigation cannot read the body, and Lax blocks
  subresources), and CloudTAK needs it, so the default is defensible — but it belongs in the deployment notes
  as a stated trade.
- **The WebAuthn user handle encodes the row id.** `rustak-server/src/auth/passkey_store.rs:204-206` derives a
  16-byte handle from `user_id`, so it is almost entirely zeroes with the row id in the low bytes. WebAuthn L3
  §5.4.3 asks for 64 random bytes carrying no identifying information; the concern is unlinkability across
  relying parties and what a platform passkey manager displays, not guessability (which the code's own comment
  addresses). It does **not** enable cross-user authentication — `finish_login` selects the row by credential
  id and that row decides who signs in.
- **`dynamic_state_of` hardcodes `user_verified: true`.** `rustak-server/src/auth/passkey_store.rs:226-237`.
  The library's `verify_static_and_dynamic_state` returns early when that flag is true, disarming its
  `credProtect: userVerificationRequired` and `hmac-secret` guards. Harmless today (`cred_protect` is `None`
  at `auth/passkeys/register.rs:83` and PRF is never requested) and wrong the moment either extension is
  enabled. Actual UV enforcement is elsewhere and is correct.
- **`Purpose::ServiceApi` is declared and never used.** `rustak-server/src/identity/verify.rs:58-59,77`
  defines it and `plugins/mod.rs:32` cites it as the enforcement mechanism; `plugins/auth.rs:164` hand-rolls
  the equivalent check. Equivalent today, able to drift, and the hand-rolled path also skips `verify()`'s
  `VerifiedSecretCache` and `record_use` (see L7).
- **Passkey ceremonies run on the canonical domain only.** `Passkeys::for_base_url` produces exactly one
  allowed origin and `settings::base_url` resolves `[server] domains` to a single canonical entry
  (`rustak-server/src/identity/settings.rs:77-85`). That is consistent with how WebAuthn works but narrower
  than M0-20's "configured base URL(s)" wording; worth stating in `docs/deployment.md`.
- **Doc/code drift worth one commit each.** `rustak-server/src/auth/setup.rs:1-17` says the setup token is
  "logged once"; `runtime.rs:442-467` deliberately does not log it (the code is right, the docstring is
  stale). `docs/plugins.md:346` says a service token "authenticates `/api/v1/services/*` and nothing else";
  it also authenticates `/api/v1/events`. `rustak-server/src/web/api/auth.rs:138-142` still points at "M2" for
  a nonce the shipped `/login/*` flow now supplies elsewhere.
- **`/Marti/api/cot/xml/{uid}` is advertised and not served.** `rustak-server/src/stream/writer.rs:188` builds
  a `b-f-t-r` pointing there for an oversized protobuf frame, and no route is registered for it
  (`marti/mod.rs` `PATHS`). A robustness/compat item rather than a security one — flagged for R-02/R-03.

---

## What is done well

This is not a courtesy section; these are the places I tried to break and could not.

**Credential handling.** argon2id at RFC 9106's second recommended setting with **no configuration key**, and
the testing parameters compiled out of a non-`testing` build (`rustak-core/src/identity/password.rs:84-131`).
Every failing verification costs a real hash, including "no such user" and "no candidate row"
(`rustak-server/src/identity/verify.rs:154-200`). `lookup_hint` is 64 bits of unsalted SHA-256 used only to
*select* candidate rows, and every secret it indexes is server-generated at 100–256 bits
(`rustak-core/src/identity/secret.rs:142-174`), so it is not an offline shortcut. `VerifiedSecretCache` keys
on `sha256(credential_id ‖ secret)`, never caches failures, and is cleared by every revocation
(`rustak-server/src/identity/secret_cache.rs:80-120`). `Secret`, `PasswordHash`, `Sealed`, `BasicCredential`,
`MissionTokens`, `SigningKey`, `IssuedCert` and `SetupToken` all have hand-written redacting `Debug` impls,
and a grep of every `info!/warn!/error!/debug!` and every `AuditEntry::detail` found no secret, token,
password, CSR or key material.

**Where Basic is accepted.** `BasicPolicy` is a three-valued type rather than a bool, and `basic_purpose`
(`rustak-server/src/auth/resolve.rs:253-265`) confines Basic to `/Marti/api/tls/` and `/oauth/token` on both
listeners, with a table-driven test that asserts `/Marti/api/groups/all`, `/api/v1/users`, `/oauth/jwks` and
the near-miss `/Marti/api/tls` are all `None` "must not be a password-guessing oracle" (`:490-521`). A Basic
path with no rate limiter installed **fails closed** (`:353-360`, tested at `:566-581`). A stray Basic header
on an ordinary route is left unread rather than checked and refused.

**Cookies and `/api/v1`.** `cookies_allowed` (`rustak-server/src/auth/oauth_server/cookies.rs:66-77`) is an
allow-list with a real segment-boundary check — `/loginary`, `/Martian`, `/files/apis/config` and
`/oauth/authorized` are all refused, with tests that say why. `/api/v1` and `/oauth/token` are excluded, and
`api_auth` (`web/api/middleware.rs:56-97`) reads only the `Authorization` header, so the admin API has no CSRF
surface at all. The cookie arm of `resolve_principal` runs only when the header carried nothing. Chunk
reassembly stops at the first gap and `MAX_CHUNKS` bounds the loop over attacker-named cookies. `Secure` is
unconditional, with the "a proxy forwarding as HTTP must not silently drop it" reasoning written down.

**PKI.** The CSR contributes its public key and nothing else: `rustak-server/src/pki/issue.rs:170-182` clears
`subject_alt_names`, `custom_extensions`, `crl_distribution_points` and `name_constraints`, sets
`is_ca = ExplicitNoCa` and writes our own DN, key usages, EKU and 127-bit serial. The CSR **signature** is
verified (`rustak-server/src/pki/csr.rs:148-156`) so we never certify a key the sender does not hold, and the
CN must equal the authenticated user, case-insensitively, as a `403` distinct from the `400` a malformed body
gets (`marti/enroll.rs:72-84`). Validity is clamped a day inside the CA's own (`issue.rs:156-165`). Revocation
is enforced *at the handshake* through an in-memory cache authoritative for refusals only
(`pki/tls/client_verifier.rs:114-143`), and revoking disconnects live sessions through a registered hook
(`stream/mod.rs:175-178`). `require_known_cert` defaults on. The Marti request path re-reads the row, the
account and the memberships on **every** request (`auth/cert.rs:49-69`). The server never holds a device's
private key, and `config_packages` says so in words rather than minting one
(`web/api/config_packages.rs:179-192`), so the legacy `atakatak` PKCS#12 protects public certificates only.

**OAuth.** `redirect_uri` is byte equality against the registered list with no runtime parsing at all
(`config/auth.rs:322-326`); the client and URI are resolved *before* any redirect can happen, so an
unregistered one gets a JSON `400` rather than a bounce (`authorize.rs:84-98`); and the URI is re-compared at
redemption against the value stored with the code (`codes.rs:192`). PKCE is mandatory for confidential clients
too, `plain` is refused, omitting `code_challenge` refuses rather than downgrades, and the DB pins
`code_challenge_method = 'S256'` with a `CHECK`. Codes are 32 CSPRNG bytes, stored as SHA-256, single-use
decided by `UPDATE … WHERE code_hash = ? AND consumed_at IS NULL` *inside* the reading transaction, and a
wrong client/URI/verifier deliberately does **not** consume the code. `state` and `nonce` are 32 CSPRNG bytes
compared in constant time, with the degenerate empty-cookie case explicitly refused, and the state cookie is
cleared on every callback outcome including failure. Our JWT verifier pins RS256 twice, requires
`aud`/`iss`/`exp`/`nbf`/`sub`, and its multi-key retry loop only retries on `InvalidSignature` — an expired or
misaddressed token fails immediately rather than being silently retried against every key
(`auth/jwt.rs:376-393`). Refresh rotation, reuse detection and family revocation all happen in one write
transaction (`db/repos/refresh_tokens.rs:160-200`). `returnTo` refuses `//evil`, `https://evil`, `/\evil`,
bare hosts and empty (`login.rs:363-365`), and a `Location` that could not be a header value becomes a refusal
rather than header injection.

**Token confusion (design 04 D2).** Mission tokens are HS256-only at issue and verify with a dedicated sealed
secret (`auth/mission_token.rs:241,256-260`); our access tokens are RS256-only; and a bearer that is not one
of ours yields "no identity" rather than a refusal, so the two resolution paths never cross
(`auth/resolve.rs:314-321`). `identity_used_authorization` decides which header a mission token may be read
from (`mission_token.rs:322-331`).

**Group routing.** `can_reach` is exactly "the sender may publish into a group the receiver may receive from"
(`rustak-core/src/identity/groups.rs:233-239`), a bit position outside the set width is ignored rather than
panicking, and `PUT /Marti/api/groups/active` is a *preference* intersected with membership — it can never
grant (`identity/active.rs:56-92`), and a `clientUid` naming somebody else's device falls back to the account
selection (`marti/channels.rs:222-246`). Enterprise-sync reads fail closed for an anonymous caller
(`files/metadata.rs:107-141` → empty `out_groups`), and an unreadable resource answers the **same** `404` as a
missing one. `/api/v1/cot` filters by `row.visible_to(&caller.principal.groups)` (`web/api/cot.rs:330-331`).
`relativePath` on the profile endpoints refuses `..` and `\` and matches against database rows rather than the
filesystem (`marti/profiles.rs:436-459`).

**Self-service boundaries.** `subject::resolve` and `subject::owns` (`web/api/subject.rs:52-113`) are the
single rule for "yours, or anybody's if you administer", used by credentials, devices, certificates and
packages; `owns` returns `Err(403)` rather than `Ok(false)` for a stranger's row. I walked every route in
`web/api/mod.rs:80-145`: none is missing an extractor, and a handler mounted outside the gate fails closed with
a `500` and a log line (`extract.rs:79-95`). Service identity comes from the credential, never from a path or
body parameter (`plugins/auth.rs:72-74`), a service token belonging to a person-kind account is refused
(`plugins/auth.rs:176-180`), and one service cannot read, heartbeat, delete or reconfigure another. Control-API
bodies are parsed *after* authentication, so a malformed body with no credential gets `401` rather than `400`.

**Passkeys (M0-20 Level 3 checklist).** I checked each §7.1/§7.2 item against `webauthn_rp`'s source rather
than assuming. The clientData `type` is enforced by a const generic fixed by the server-side `CeremonyKind`,
so a client cannot choose which check runs. Origin comparison is exact on scheme, host and port, with the
`Port::Any` relaxation confined to `localhost`/`*.localhost` and IP-address base URLs refused up front
(`auth/passkeys/mod.rs:101-148`). `crossOrigin` must be false and `topOrigin` absent. `rpIdHash` is compared
over the full SHA-256. UP is mandatory; UV is `Required` on **both** login paths — the username-assisted one
starts from a `second_factor()` builder and explicitly overrides it before `start_ceremony`, with the reason
written down (`auth/passkeys/login.rs:98-107`). The signature base is `authData ‖ SHA-256(clientDataJSON)` with
the hash computed server-side. There is no algorithm confusion: the verifying algorithm comes from the
**stored** key variant, never from client input, and registration checks the COSE `alg` against an allow-list
narrowed to ES256/RS256/EdDSA. Sign-count enforcement is `Fail` on regression *and* the new value is persisted
on every sign-in, without which the check would be inert. Credential→user binding is airtight in both
directions (`login.rs:134-190`, plus the library's user-handle and `allowCredentials` checks), and a ceremony
started for one account or one kind cannot be spent on another. Attestation is never trusted — only `none` and
self-attested `packed` parse, and AAGUID, format and client extension results are discarded rather than acted
on. Challenges are 128-bit CSPRNG compared after signature verification, ceremony handles are 192 bits, and
the library's own five-minute expiry travels inside the encoded state independently of the outer TTL. Three of
the four endpoints record rate-limiter outcomes correctly (M9 is the fourth), and every failure collapses to
one message.

**Setup.** Both first-run tokens are 256 bits, stored as SHA-256, compared in constant time, written 0600 and
never logged (`auth/setup.rs:146-273`, `runtime.rs:442-467`); the registration token's record is removed
*before* its expiry is checked. `/api/v1/setup/*` is rate limited and answers `410` for good once the wizard
completes.

**Configuration.** `deny_unknown_fields` throughout; `user_acl` and `admin_acl` both default to denying;
`config/validate.rs` refuses a plaintext public listener unless the operator says so twice, an ACME mode that
disagrees with `[acme] enabled`, a `files` mode missing its files, a challenge with no listener to answer it,
and a registered redirect URI that is relative, fragmented or plaintext-non-loopback. Sealed secrets are
AES-256-GCM with a fresh nonce and a context string as AAD that differs per row (`crypto/context.rs`, with the
"a ciphertext relocated between two rows must not open" property asserted). The Marti CORS trio is `*` with
**no** `Access-Control-Allow-Credentials`, so cross-origin reads cannot carry a cookie
(`marti/headers.rs:120-124`), and `assert_no_redirect` runs in every build. No SQL is built from caller input
— every `format!` around a query interpolates a `const COLUMNS`, with values always bound.

---

## Tests I would add

1. `web::api::extract` — a token whose `scope` is `"api"` is refused by `Administrative` even when the account
   is an administrator (H1); and the mirror, `auth::tokens::rotate` preserves the stored scope (M6).
2. `rustak-server/tests/oauth_flows.rs` — `GET /oauth/authorize` with only an `Authorization: Bearer` header
   and no cookie does **not** issue a code (H2).
3. `web::api::events` — a service account with no channel memberships sees no `package.uploaded`,
   `client.connected` or `mission.changed` for a channel it does not hold (H3); and a feed opened with a token
   that is then revoked closes within one keepalive (H4).
4. `rustak-server/tests/stream_session.rs` — disabling an account closes its live stream connection (H5), and
   the same for revoking the credential its certificate was enrolled with.
5. `missions::roles` — an `ACCESS` token minted for a deleted mission does not open a new mission that took
   its name (M1).
6. `marti::sync_read` — a request authenticated **only** by cookie is refused on `GET /Marti/sync/delete` (M2).
7. `rustak-server/tests/enroll_flows.rs` — two concurrent `signClient/v2` calls with one enrolment token yield
   exactly one certificate and `uses == 1` (M3).
8. `marti::enroll` — enrolling with a `clientUid` that belongs to another account is refused (M4).
9. `plugins::auth` — a service token is refused when `user_acl` refuses the request (M5); and a token minted
   with `max_uses = 1` is refused on its second use (L7).
10. `auth::passkey_store` / `auth::oauth_server::state` — `sweep` succeeds with a setup record and a pending
    sign-in present in the same partition (M8); and `tokio::join!` on two `claim` calls for one handle leaves
    exactly one `Ok` (M13, plus the same for `setup::claim_registration`).
11. `web::api::passkey` — `login/start` for an account that exists with a passkey and for one that does not
    become distinguishable only until the limiter locks (M9); a ceremony started under one `Host` cannot be
    finished under another (M10); `register_finish` refuses a disabled account (L3).
12. `auth::oauth_server::login` — a callback whose `state` cookie was not the one this server set is refused
    (M7), once a second binding exists to make that expressible.
13. `pki::csr` — a P-192 signing request is refused by policy rather than by rcgen (L5).
14. `web::helpers::oidc::validate` — an ID token with `aud = ["rustak","other"]` and `azp = "other"` is
    refused (L8); an `EdDSA`-signed token is refused by the allow-list (L9).
