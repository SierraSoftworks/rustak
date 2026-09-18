# Missions / Data Sync (`/Marti/api/missions/*`) — wire contract

**Purpose.** The full mission ("Data Sync") API: CRUD, subscriptions, contents, changes, layers,
logs, invitations, roles, and the CoT notifications a mission publishes over the stream. Served on
both HTTPS listeners, client cert or bearer/mission-token auth. Contract for
`rustak-server::missions::*` and `marti::missions::*`.

Baseline: `plan.md` Appendix A.4 "Missions" and "Mission tokens", A.1 "mission dest routing". This
file expands both; do not contradict.

CloudTAK calls a mission a **Data Sync**. See `cloudtak.md` for how CloudTAK's `data` table maps
onto mission name/guid/token/groups — this file is the server-side contract those calls hit.

## 1. Addressing: name vs GUID

Every mission has a stable `name` (the human label, used in `<marti><dest mission="Name">` on the
CoT stream — **always by name there**, never by GUID) and an immutable `guid`. Two families of REST
path exist for most operations:

```
/Marti/api/missions/{name}/...
/Marti/api/missions/guid/{guid}/...
```

**CloudTAK prefers GUID and sniffs the identifier it's holding**: if it matches
`^[{]?[0-9a-fA-F]{8}-([0-9a-fA-F]{4}-){3}[0-9a-fA-F]{12}[}]?$` it uses the `/guid/` family, otherwise
the name family. Implement **both** families for every operation this matters for, but three
operations have **no** GUID path in real TAK Server and CloudTAK falls back to name for them
regardless: `setKeywords`, `deleteKeyword`, `getArchive` (03 §5.4). `DELETE` is the odd one out: the
GUID form is `DELETE /Marti/api/missions?guid={guid}` — a **query parameter on the collection
route**, not `/guid/{guid}` (verified 06 §7.5, corroborated 03 §5.4).

**Hard consequence**: a mission whose *name* happens to be a bare UUID becomes unaddressable by name
once CloudTAK is involved, because it will always route to `/guid/` for anything UUID-shaped. Don't
special-case this — it is expected, documented client behaviour, not a bug to route around.

## 2. Envelope `type` strings — inconsistent by design

Every mission-family response uses the standard envelope `{"version":"3","type":"…","data":…,
"nodeId":"…"}`, but the `type` value is **not one consistent string** — it varies per endpoint
family, verified 06 §1.2. Reproduce these exactly; a client that switches on `type` (as ATAK's
`ServerGroup`-style parsers do elsewhere in this project) will reject an unexpected value:

| Endpoint family | `type` value | Form |
|---|---|---|
| `/missions`, `/missions/{n}`, `/missions/guid/{g}` (Mission payloads) | `Mission` | simple name |
| `/missions/{n}/changes`, `/contents/missionpackage` | `MissionChange` | simple name |
| `/missions/{n}/layers*` | `MissionLayer` | simple name |
| `/missions/{n}/subscription` (singular — `GET`/`PUT`) | `com.bbn.marti.sync.model.MissionSubscription` | FQCN |
| `/missions/{n}/subscriptions`, `/subscriptions/roles`, `/all/subscriptions` (plural) | `MissionSubscription` | **literal, hardcoded** — not the FQCN used by the singular form |
| `/missions/{n}/invitations`, `/missions/invitations`, `/missions/all/invitations` | `MissionInvitation` | literal |
| `/missions/{n}/role` | `com.bbn.marti.sync.model.MissionRole` | FQCN |
| `/missions/{n}/log`, `/missions/logs/entries*`, `/missions/all/logs` | `com.bbn.marti.sync.model.LogEntry` | FQCN |
| `/missions/{n}/token` | `java.lang.String` | FQCN-style (boxed primitive) |
| `/Marti/api/sync/search`, `/resources/{hash}` | `Resource` | simple name |
| `/missions/{n}/maplayers` | `MapLayer` | literal |

`/missions/{n}/cot`, `/missions/{n}/contacts` are **not** enveloped at all — raw XML and a bare JSON
array respectively (§14, and `missions.md` has no separate contacts route beyond this one; see
`contacts.md` for the general contacts surface).

## 3. Create / update

```
PUT|POST /Marti/api/missions/{name}?creatorUid=&group=<repeatable-or-comma-joined, default __ANON__>
    &description=&chatRoom=&baseLayer=&bbox=&boundingPolygon=<repeatable "lat,lon">&path=
    &classification=&tool=public&password=&defaultRole=&expiration=&inviteOnly=false
    &allowGroupChange=false   (PUT only)   &allowDupe=false   (POST only)
[optional JSON body overriding any of the above fields, OR a raw mission-package zip body]
```

- `group` accepts **both** `?group=a&group=b` and a single `?group=a,b` — CloudTAK's `create()`
  comma-joins, its `update()` repeats the param. Accept both forms on every multi-value query param
  in this API (`group`, `boundingPolygon`, `keyword`).
- **This applies to `boundingPolygon` too, and it is not a mistake** (settled 2026-09-18, R-02 M8 /
  contract defect 3). `boundingPolygon` is a `List<String>` bound by Spring, which splits a
  comma-containing value for a collection target exactly as it does for `group` (06 §7.2 line 941),
  so `?boundingPolygon=51.5,-0.12` really does arrive upstream as `["51.5", "-0.12"]` — the `lat,lon`
  pairing survives only on the **JSON body** path, where each array element is taken whole. §3 wins
  over §4 for the query string; §4 describes the model and the body. A client that wants real polygon
  vertices sends them in the body.
- A JSON body (`Content-Type` contains `application/json`) **overrides** matching query params where
  present; a non-JSON body is imported as a mission-package zip instead (06 §7.2).
- **Status**: `201` on create (new mission row) with `token` + `ownerRole` in the response; `200` on
  update, with **no `token`**. This distinction is load-bearing — CloudTAK's `data-mission.ts`
  reads `mission.token` straight off the create response and treats its absence as "creation
  failed" territory (03 §5.1).
- Validate the mission name: printable, `1..1024` chars, **must not contain `/`** — CloudTAK
  validates this client-side too, so a name that would fail there should be rejected server-side
  with a normal `400`, not accepted and then unaddressable (03 §5.1).
- Group-change guard on update: if the requested `group` set differs from the mission's stored
  groups, require the caller to be `MISSION_OWNER` or admin **and** `allowGroupChange=true` (else
  `403`). CloudTAK's client auto-sets `allowGroupChange=true` whenever it sends a `group` field on
  update — accept it as a normal flag, not a privilege escalation to be suspicious of (03 §8.7).
- Fallback: if the caller's own group vector doesn't cover the requested groups but the request
  asked for exactly `__ANON__`, silently substitute the caller's own groups instead of failing.

## 4. `Mission` JSON — exact shape

`@JsonInclude(NON_NULL)` semantics: omit a field when null, but several are non-optional in
CloudTAK's TypeBox schema and **must** be emitted as `[]` rather than omitted even when empty:
`externalData`, `feeds`, `mapLayers`, `uids`, `contents` (03 §8.11 item 11). `passwordProtected` is a
primitive boolean — **always present**.

| Field | Type | Notes |
|---|---|---|
| `name`, `description`, `tool`, `guid` | string | `tool` defaults to `"public"` |
| `chatRoom`, `baseLayer`, `bbox`, `path`, `classification` | string, optional | omit when unset |
| `boundingPolygon` | array of `"lat,lon"` strings | in the **body** and in the response; a query-string value is comma-split first — see §3 |
| `keywords` | array of string | |
| `creatorUid` | string, optional | |
| `createTime`, `lastEdited` | **padded-millis** `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'` | |
| `expiration` | number (epoch millis), optional | `-1` or absent = never |
| `inviteOnly` | boxed bool, omit when null | |
| `passwordProtected` | primitive bool | always present |
| `token` | string, **only on create (201)**, omitted on update | |
| `ownerRole` | `{type, permissions[]}`, **only on create** | |
| `defaultRole` | `{type, permissions[]}` | `type` ∈ `MISSION_OWNER`\|`MISSION_SUBSCRIBER`\|`MISSION_READONLY_SUBSCRIBER` |
| `groups` | array of group **names** (strings) | only populated on `GET` of a single mission |
| `uids` | array of `MissionAdd<string>` (see below) | always `[]` at minimum |
| `contents` | array of `MissionAdd<Resource>` (see below) | always `[]` at minimum |
| `externalData`, `feeds`, `mapLayers` | arrays | always `[]` at minimum (CloudTAK treats these as opaque/unknown-typed) |
| `missionChanges` | array of `MissionChange`, only when `?changes=true` | see §8 |
| `logs` | array of `LogEntry`, only when `?logs=true` | |

`MissionAdd<T>`: `{"data": <string-or-Resource>, "timestamp": "<padded-millis>", "creatorUid": "…",
"keywords": […]}`.

Verified 06 §7.11 (field-by-field, including which getters are `@JsonIgnore`); CloudTAK's own
TypeBox `Mission` schema matches this shape field-for-field except it treats `externalData`/
`feeds`/`mapLayers` as `Unknown[]` rather than typing their contents (03 §3.8) — safe to leave those
opaque in rustak too for M4.

## 5. `GET /missions/{name|guid}` — read semantics

```
GET /Marti/api/missions/{name}?password=&changes=false&logs=false&secago=&start=&end=
```
- No `password` given: require the caller to already hold `MISSION_READ` via cert/bearer role,
  else `403`. With `API_VERSION` header `>= 3` (see `cloudtak.md` §"API_VERSION"), a `403` here is
  instead a **`200` with a stripped/emptied mission body** (all content arrays cleared) — this is
  the forward-compat behaviour newer TAK ecosystem clients expect; implement it once the
  `API_VERSION` gate exists, plain `403` is fine before that.
- Correct `password` on a password-protected mission: mint an **ACCESS** mission token (no
  expiry) and return it as `Mission.token` on this same response (not just on `/token`).
- `changes=true` uses the **full-history** form (not squashed — see §8); `logs=true` attaches
  `LogEntry[]`.

## 6. `GET /missions` — list

```
GET /Marti/api/missions?tool=public&passwordProtected=false&defaultRole=false
```
`tool` has **no "all tools" mode** — if omitted it defaults to `"public"`, it does not mean
"every tool". `passwordProtected=false` excludes password-protected missions from the list;
`defaultRole=false` excludes missions whose default role isn't `MISSION_SUBSCRIBER`. Invite-only
missions are **never** returned by this endpoint regardless of these flags.

## 7. Contents

```
PUT    /missions/{n}/contents          body {"hashes":[…], "uids":[…], "paths":{…}, "after":"…"}  (>= 1 of hashes/uids/paths, else 400)
DELETE /missions/{n}/contents?hash=|uid=
PUT    /missions/{n}/contents/missionpackage?creatorUid=   body = raw zip   -> 200, or 409 with the conflict list as ApiResponse<MissionChange[]>
```
Attach flow used by CloudTAK for per-marker attachments: `POST /Marti/sync/upload` (see `files.md`)
to get a `hash`, then `PUT …/contents {"hashes":["<hash>"]}`. Removal is `DELETE …/contents?hash=`.
An uploaded file whose `uid` matches an existing map item's uid is treated as an **attachment of
that item** by TAK-ecosystem clients — pass `uid` through on upload when the caller supplies one.

## 8. Changes: squashed vs full history

```
GET /missions/{n}/changes?secago=&start=&end=&squashed=true
```
`squashed` **defaults to `true`** on the dedicated `/changes` endpoint. Squashed = current-state
delta: at most one row per (content item, creator), only items still present for ADDs / only items
actually gone for REMOVEs. Full history (`squashed=false`, and this is what `GET /missions/{n}` uses
internally when `?changes=true`) = one row per historical change record. On error, prefer returning
an **empty array** over a `500` — TAK Server itself does this (returns `null` → empty body).

### `MissionChange` JSON

| Field | Notes |
|---|---|
| `type` | `CREATE_MISSION`\|`DELETE_MISSION`\|`ADD_CONTENT`\|`REMOVE_CONTENT`\|`CREATE_DATA_FEED`\|`DELETE_DATA_FEED` |
| `isFederatedChange` | bool, always present |
| `missionName`, `missionGuid` | |
| `timestamp` | padded-millis |
| `serverTime` | padded-millis; **JSON key is `serverTime`** even though it maps from a field literally called `servertime` |
| `creatorUid`, `contentUid` | optional |
| `details` | `{type, callsign?, title?, iconsetPath?, color?, attachments?[], name?, category?, location?{lat,lon}}` |
| `contentResource` | a `Resource` (see `files.md`) |
| `logEntry` | a `LogEntry`, when the change is a log addition |

Verified 06 §7.12. CloudTAK's UI reacts to a **live** `t-x-m-c` stream event carrying this same
shape (§12), not by polling `/changes` — implement the push path with the same field names.

## 9. Subscriptions, roles, tokens

```
GET    /missions/{n}/subscription?uid=          -> 200 MissionSubscription, 404 if none
PUT    /missions/{n}/subscription?uid=|topic=&password=&secago=&start=&end=   -> 201 MissionSubscription (includes token)
DELETE /missions/{n}/subscription?uid=|topic=&disconnectOnly=true
GET    /missions/{n}/subscriptions              -> bare client-uid list
GET    /missions/{n}/subscriptions/roles        -> MissionSubscription[] with token nulled out
GET    /missions/{n}/role                       -> caller's own MissionRole
PUT    /missions/{n}/role?clientUid=|username=&role=
GET    /missions/{n}/token?password=            -> 201 {data: "<ACCESS jwt>"}
```

**Subscribe auth resolution** (checked in this order):
1. Password-protected + `password` param → BCrypt-check it.
2. Otherwise, an `INVITATION`/`SUBSCRIPTION`/`ACCESS` mission token in `MissionAuthorization` (or
   `Authorization`) — see §10.
3. Invite-only mission with neither → look up a standing invitation for this clientUid/username; none
   found → `403`.
4. Supplying `password` on a **non**-password-protected mission is itself a `403` (mirror this exact
   edge case; it surprises implementers but both real servers and CloudTAK's client code treat a
   stray `password` as an error, not a no-op).

Response always includes `token` (a fresh **SUBSCRIPTION**-type mission token) — this is what
CloudTAK stores as `profile_overlays.token` / `data.mission_token` and replays on every subsequent
call. `createTime` on this object uses the **unpadded**-millis format (`yyyy-MM-dd'T'HH:mm:ss.S'Z'`)
— different from `Mission.createTime`'s padded form; don't reuse the same formatter for both.

Subscribing auto-clears any matching clientUid/callsign invitation for that mission.

## 10. Mission tokens

HS256 JWT, claims: `jti`, `iat`, `sub` = the **token type name** (`SUBSCRIPTION`\|`INVITATION`\|
`ACCESS`), `iss`, optional `exp` (absent = never expires), `<TYPE>` = the id (e.g.
`"SUBSCRIPTION": "<subscription-id>"`), `MISSION_NAME`, `MISSION_GUID`. Sign with a **dedicated
sealed secret** (`conventions.md`), not the server's TLS/RS256 key — TAK Server reuses its RSA key
bytes as an HMAC secret for this, which rustak should not copy (key-reuse across algorithms is an
anti-pattern worth breaking from here).

**Header precedence**: read `MissionAuthorization` first, then fall back to `Authorization`. The
`Bearer ` prefix check is case-sensitive (`"Bearer "` exactly) on this path, unlike the OAuth bearer
resolver in `oauth.md`. An admin caller always resolves to `MISSION_OWNER` regardless of any token.
Token → role: `SUBSCRIPTION` → that subscription's stored role; `INVITATION` → the invitation's role
(only accepted on the subscribe call, never for general access); `ACCESS` → the mission's default
role. The `MISSION_NAME` claim must match the mission being accessed (case-sensitive) or the token is
rejected — this is what stops a token minted for one mission being replayed against another.
CloudTAK sends this token **verbatim** as `MissionAuthorization: Bearer <token>` — treat it as
opaque, no CloudTAK-side validation happens on it (03 §5.2). Verified 06 §8.

## 11. CoT `<dest mission="Name">` routing (stream → mission)

Cross-reference `streaming.md` §8 for the general `<marti><dest>` resolution order. Mission-specific
rules:
1. Require the sender to hold a subscription with `MISSION_WRITE` on that mission; if not, drop the
   message for that mission (don't error the connection — other `<dest>` targets on the same
   message, if any, still apply).
2. Recipients = every **connected** client currently subscribed to that mission, **minus the
   sender**. Still subject to the normal `IN`/`OUT` reachability check.
3. Persist an `ADD_CONTENT` `MissionChange` row for the CoT uid, and push a `t-x-m-c` notification
   (§12) to subscribers — this is a **separate** delivery from the raw CoT relay in step 2, both
   happen.
4. CloudTAK addresses missions **by name** in `<dest mission=…>`, even though it prefers GUID for
   every REST call — don't assume the stream and REST identifiers will match casing/encoding
   quirks; resolve name→mission the same way `GET /missions/{name}` does.
5. **Known client-side bug to defend against, not fix**: an ETL layer with mission-sync enabled can
   still emit `<dest mission=…>` alongside a broadcast, so rustak may receive mission-destined CoT
   that looks like ordinary broadcast traffic too — don't assume `<dest mission>` is exhaustive of
   "this should go into the mission" from CloudTAK's behaviour (03 §8.10, CloudTAK issue #1160).

## 12. `t-x-m-*` stream notifications

Seed (own-words rendering, verified 05 §7.1 — `stale` is always **20s**, point is always the null
island 0/0 with 9999999 ce/le):
```xml
<event version="2.0" uid="{fresh-uuid}" type="{see table}" how="h-g-i-g-o" time="{t}" start="{t}" stale="{t+20s}">
  <point lat="0" lon="0" hae="0" ce="9999999" le="9999999"/>
  <detail><mission type="{mission-type}" name="{mission-name}" [guid="…"] [authorUid="…"] [tool="…"] [token="…"]>
    <!-- MissionChanges/MissionChange content appended here when relevant, same field names as §8 -->
  </mission></detail>
</event>
```

| Trigger | `event/@type` | `mission/@type` | Recipients |
|---|---|---|---|
| Content add/remove | `t-x-m-c` | `CHANGE` | every **connected** subscriber uid (topic-prefixed uids diverted separately) |
| Log change | `t-x-m-c-l` | `CHANGE` | connected subscribers |
| Keyword / uid-keyword / resource-keyword / metadata / external-data / layer change | `t-x-m-c-k`, `-k-u`, `-k-c`, `-c-m`, `-c-e`, `-c-h` | `CHANGE` | connected subscribers |
| Mission created | `t-x-m-n` | `CREATE` | **every** subscription except the creator, gated by group-vector overlap (`__ANON__` is force-included so read-only users still see public mission announcements) |
| Mission deleted | `t-x-m-d` | `DELETE` | same broadcast rule as create |
| Invitation sent | `t-x-m-i` | `INVITE` | the invited `uids[]` only, plus a `<role>` child and `token=` |
| Role changed | `t-x-m-r` | **`INVITE`** (yes — not `ROLE`; reproduce this exactly) | the single affected `clientUid`, plus a `<role>` child |

The `<role>` child is `<role type="…"><permissions>MISSION_READ</permissions><permissions>…</permissions></role>`
— **repeated text elements**, one per permission, not a `<permissions>` wrapper holding
`<permission type=…/>` children. `MissionRole` is `@XmlElement(name="permissions")` on a
`Set<String>` (05 §7.5), which is what JAXB renders from that. Design 04 §4.8 illustrates the nested
form and M4-02 followed it; per `README.md` the research wins, and a client reading
`role/permissions` text got nothing from the nested shape (R-02 M11). Corrected 2026-09-18.

These are delivered directly to the target subscription(s), **bypassing the normal group-
reachability broker** (except the broadcast-announcement path, which still applies the group-vector
check described above) — no flow tag is added to these. CloudTAK's live-update path listens for
`type` starting with `t-x-m-c` and, for the single-change case, reads
`mission.missionChanges[0].contentResource.name` / `.type` to drive its add/remove UI (03 §4.5) — so
the `<mission>` child must carry a real `<MissionChanges><MissionChange>…</MissionChange>
</MissionChanges>` block using the same element names as the REST `MissionChange` JSON (attributes
for `<mission>` itself, **child elements** for everything under `MissionChanges` — mixed convention,
reproduce exactly). Verified 05 §7.1–§7.4, cross-checked against CloudTAK's independent wire-type
definitions in 03 §4.5, which match element-for-element.

## 13. Archive, layers, logs, invitations — condensed

| Feature | Key endpoints | Notes |
|---|---|---|
| Archive | `GET /missions/{n}/archive` → zip | Mission-package layout: `cot/<uid>.cot`, `contents/<n>_<name>`, `MANIFEST/manifest.xml` (params incl. `mission_uid`, `mission_server`, plus `<Groups>` and `<Role>` — see `files.md` for the general manifest shape) |
| Layers | `GET/PUT/DELETE /missions/{n}/layers[/…]` | `MissionLayer{uid, name, type: GROUP\|UID\|CONTENTS\|MAPLAYER\|ITEM, parentUid, mission_layers[] (snake_case — the one deliberately non-camelCase key in this whole API), uids[], contents[], maplayers[]}`; `@JsonInclude(NON_EMPTY)` — omit empty arrays/strings too, not just nulls |
| Logs | `POST/PUT /missions/logs/entries`, `GET/DELETE …/{id}` | `POST` requires **no** `id` on the body (server assigns it); `PUT` requires `id` and **no** `servertime`; both return `201`. `LogEntry{id, content, creatorUid, entryUid, missionNames[], servertime (lowercase t, unlike MissionChange.serverTime), dtg, created, contentHashes[], keywords[]}` |
| Invitations | `GET /missions/{n}/invitations`, `GET /missions/invitations?clientUid=`, `PUT/DELETE /missions/{n}/invite/{type}/{invitee}` | `type` ∈ `clientUid`\|`callsign`\|`userName`\|`group`\|`team` (exact casing, `userName` capital N). CloudTAK calls `list(clientUid)`, **not** `/missions/all/invitations` — implement the `?clientUid=` form as priority |
| Roles | `MissionRole{type, permissions[]}` | permissions ⊂ `MISSION_READ, MISSION_WRITE, MISSION_DELETE, MISSION_SET_ROLE, MISSION_SET_PASSWORD, MISSION_UPDATE_GROUPS, MISSION_MANAGE_FEEDS, MISSION_MANAGE_LAYERS` |

## 14. `/cot` — mission-scoped CoT read

```
GET /missions/{n}/cot?path=          ->  200 application/xml, <?xml …?><events>…</events>
```
The body is the XML declaration followed by a bare `<events>` wrapper containing each cached CoT
`<event>` in sequence (same `<events>` root convention TAK Server uses for its other CoT-history
endpoints, `/Marti/api/cot/xml/{uid}/all` and `/Marti/api/cot/sa`, outside the scope of this file),
`path` filters on `/detail/marti/dest[@path=…]`.
CloudTAK's mission-feature rendering depends on this returning valid XML even for an empty mission
(`<events></events>`, not `404`).

## 15. CloudTAK-specific requirements

- **The create response must include `data[0].token` and `data[0].guid`** — CloudTAK immediately
  persists both (`data.mission_token`, `data.mission_guid`) and has no other way to manage the
  mission afterward (03 §5.1).
- Every group CloudTAK's caller holds is force-activated (see `groups.md` §5) before it attempts
  mission creation — expect a `PUT /groups/active` immediately before `PUT /missions/{name}`.
- CloudTAK re-subscribes every one of a Connection's Data Syncs on every stream reconnect, and its
  retry logic **only retries on connection-refused** — a transient `5xx` from
  `PUT …/subscription` silently drops that Data Sync until the next reconnect (03 §5.3). Keep this
  endpoint reliable; don't treat a slow dependency as an excuse to 500 here.
- Mission layers: CloudTAK maintains at most 5 `UID`-type layers per Data Sync and will
  delete-and-recreate any layer whose `type` isn't `UID` — expect occasional churn on
  `PUT/DELETE …/layers`, not a sign of a client bug.

## 16. Where rustak deliberately differs (M4-01)

Each of these is a decision, not a gap; the wire shape a client sees is
unchanged unless the row says otherwise.

| Area | TAK Server | rustak | Why |
|---|---|---|---|
| Mission password hash | BCrypt, cost 10 (§9) | **argon2id**, the same parameters as every other stored secret | The hash never leaves the server: a client sends the password, we answer with a token we minted, and nothing federates or compares it. There is no parity to keep, and a second hashing algorithm is a second thing to review. |
| Mission-token secret | the bytes of the server's RSA key, used as an HMAC secret | a dedicated 32-byte secret, generated once and sealed in the `SecretStore` under `mission-token-hmac` | Key reuse across two algorithms means a weakness in either reaches both, and our RSA key rotates on a schedule that has nothing to do with missions (§10 already says this; it is now implemented). |
| `Bearer` prefix | case-sensitive `"Bearer "` exactly (§10) | `Bearer `, `bearer ` **and** a bare token with no prefix | ETL clients send all three, and refusing two of them buys nothing: the token is verified either way. |
| Mission identity in a token | `MISSION_NAME` must match, case-sensitively (§10) | `MISSION_NAME` **or** `MISSION_GUID` matches | A rename would otherwise invalidate every token already issued for the mission. Still narrower than "any mission": one of the two has to name *this* mission. |
| Role without a token | the token is the only way to hold a role beyond the default | the caller's **own subscription** also grants its role, matched by device uid or account name | A client that creates a mission over an ordinary session can then manage it without replaying the token it was handed. It grants nothing a token would not — the subscription was created for that account in the first place — and the most permissive of several subscriptions wins. |
| `DELETE …/subscription?disconnectOnly=true` | keeps the row and stops delivery | deletes the row either way | There is no "subscribed but receiving nothing" state here, and a row that lies about itself is worse than one that is gone. Documented in §9's terms: the client re-subscribes on its next reconnect regardless. |
| `mission_contents` key | `(mission_id, resource_hash)` | `(mission_id, resource_id)`, joined to `resources` | A hash is not unique — the same photograph attached to two items is two resource rows over one blob — so detaching by hash detaches every row holding it, which is what "remove that file" means. |
| Invitation tokens | matched against the whole stored JWT | matched against the stored `token_jti` | The `jti` is unique per token and is already indexed; the whole JWT is stored alongside it (`mission_invitations.token`, migration `0010`) for clients that replay it verbatim. |

**Withdrawn 2026-09-18 (R-01).** "Mission identity in a token: `MISSION_NAME` **or** `MISSION_GUID`
matches." A token now has to match the **guid**. The rename-safety that deviation was for is what
the guid check already gives — a rename changes the name and not the guid — while accepting the name
meant a never-expiring `ACCESS` token minted for a deleted mission opened a brand new
password-protected mission that reused the name. A guid is not reused; a name is.

## Gotchas

- `201` (create, with `token`) vs `200` (update, no `token`) is load-bearing — get this wrong and
  CloudTAK's Data Sync creation silently has no token to manage the mission with (§3).
- `t-x-m-r`'s `<mission type>` is the string `"INVITE"`, not `"ROLE"` — verified as intentional, not
  a typo in the source (§12).
- `mission_layers` is the **only** snake_case key in the whole mission JSON surface (§13).
- `MissionChange.serverTime` (camelCase JSON key) vs `LogEntry.servertime` (lowercase JSON key) —
  both exist, spelled differently, don't unify them.
- `MissionSubscription.createTime` uses unpadded millis; `Mission.createTime`/`lastEdited` use
  padded millis. Pick the formatter per field from the tables above, not globally.
- A mission whose name is a bare UUID cannot be reached by name once a UUID-sniffing client (like
  CloudTAK) is involved — document this as a naming constraint, don't try to special-case routing.
- `squashed` defaults to `true` on `/changes` but the *implicit* changes fetched via
  `GET /missions/{n}?changes=true` use full history (`squashed=false`) — these are genuinely
  different defaults for the same underlying data (§8).

## Verified in

- `research/06-takserver-http-api-verified.md` §7 (full `MissionApi` endpoint table, JSON shapes,
  status codes, addressing rules) and §8 (mission tokens) — authoritative, largest single source.
- `research/05-takserver-streaming-auth-verified.md` §7 (mission CoT dest routing, `t-x-m-*`
  templates, recipient rules) — authoritative for the stream side.
- `research/03-cloudtak-node-tak-contract.md` §3.8–§3.11, §5 (node-tak's `mission.ts` client types,
  CloudTAK's Data Sync usage, GUID-vs-name sniffing, force-activate-groups, token storage,
  known gotchas §8.7/§8.10/§8.11) — authoritative for CloudTAK.
- `plan.md` Appendix A.4 "Missions", "Mission tokens", A.1 mission-dest bullet — baseline digest,
  expanded here.
