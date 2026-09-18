I have everything I need; no further reads. Below is the file-level design.

# rustak — Marti HTTP API surface: implementation design

Sources of truth used: report 06 (TAK Server 5.7 source) and 07 (ATAK-CIV source) are authoritative for wire shapes; 03 (node-tak/CloudTAK) supplies hard client requirements, flagged **[CloudTAK]** below; 05 §6.7/§7 for the streaming hooks; 04 only for empirically-accepted package layouts (double-wrapped WinTAK zip, flat iTAK zip). Where 02 conflicts (e.g. `created` datetime), 06/07 win.

## 0. Cross-cutting decisions

| # | Decision | Why |
|---|---|---|
| D1 | The whole `/Marti/**` + `/files/api/config` scope is mounted on **both** listeners (8443 mTLS, 8446 public). Auth-resolution middleware (identity design) sets `Principal`; per-listener policy only differs in *which credential kinds* are accepted. | TAK's 8446 serves the full API to WebTAK; CloudTAK can point `api` and `webtak` at the same host. |
| D2 | `Authorization: Bearer` is consumed by identity resolution **only if** it verifies as our RS256 JWT. Mission-token resolution reads `MissionAuthorization` first, then `Authorization` **only when identity did not come from it**. Verification failure of a mission token → "no token", never an error. | Removes TAK's port-based hack (bearer ignored on 8443) while keeping both headers usable simultaneously. **[CloudTAK]** sends `MissionAuthorization: Bearer <tok>`. |
| D3 | Every JSON response is emitted through one helper that sets `Content-Type: application/json` (no parameters). Legacy servlets that TAK serves as `text/json`/`text/plain`/HTML are reproduced exactly. A contract test walks the route table and asserts header equality. | **[CloudTAK]** strict string compare on `content-type`. |
| D4 | Never emit 3xx from any Marti route. Missing trailing slash, wrong method etc. are 404/405 JSON, not redirects. Disable actix `NormalizePath` redirect mode. | **[CloudTAK]** treats 3xx as success and parses the redirect body. |
| D5 | Dates: emit **3-digit padded** millis `yyyy-MM-ddTHH:mm:ss.SSSZ` everywhere TAK uses either `COT_DATE_FORMAT` or `_PAD_MILLIS`; `Group.created` is `yyyy-MM-dd`; `Files.Time` is Java `Date.toString()` style. Parse inputs leniently (padded/unpadded/offset). | Padded is a valid instance of the single-`S` pattern; ATAK parses `SubmissionDateTime`/`lastEventTime` with `SSS`. |
| D6 | Envelope `type` strings are per-endpoint constants (table in §1.2), faithfully inconsistent. | ATAK string-matches `com.bbn.marti.remote.groups.Group` and `com.bbn.marti.remote.ClientEndpoint`; `ServerConfig`. |
| D7 | Mission names that parse as a UUID are **rejected** on create (400). | **[CloudTAK]** sniffs GUID-shaped ids and routes to `/guid/`; such a mission would be unaddressable. |
| D8 | `GET /groups/all` always returns both IN and OUT memberships with `active` (superset of TAK's `useCache=false` OUT-only quirk). `useCache` is accepted and ignored; `sendLatestSA=true` triggers latest-SA replay. | Simpler, and both ATAK and CloudTAK call with `useCache=true` anyway. |
| D9 | `PUT /groups/active` without `clientUid` emits `t-x-g-c` to **all** of the user's other connections (TAK emits none). | **[CloudTAK]** never sends `clientUid`; the user's ATAK devices should still refresh. |
| D10 | Mission tokens are HS256 with a dedicated 32-byte secret from the `SecretStore` (not the TLS key). Claims per 06 §8.1. | Clean-room; ACME rotates our TLS key. |
| D11 | Squashed change computation is done in Rust over the windowed history (fold), not in SQL. | Small data volumes; testable against a naive model; avoids a 6-way UNION in SQLite. |
| D12 | `b-f-t-r`/`senderUrl` is **never rewritten** by us (TAK parity; only federation rewrites). The URL we return from `/Marti/sync/missionupload` is built from `marti.public_host` (fallback: `Host` header via the trust-proxy-aware `base_url` helper lifted from automate `web/helpers/request.rs`). | ATAK sets `senderUrl` to whatever `missionupload` returned; peer-hosted URLs point at devices we cannot proxy. |
| D13 | `.pref` group name for app prefs is `com.atakmap.app.civ_preferences` (TAK parity; it *is* ATAK-CIV's `<package>_preferences`, 07 §2.5). The `PrefWriter` takes the group name as a parameter so the manual-package builder can emit the legacy alias if WinTAK/iTAK testing demands it. | 06 §10.3 verified; 04 shows the alias is accepted too. |
| D14 | Values in `.pref` and manifests are XML-escaped (TAK does not). | Correct XML; ATAK uses a real parser. |

`Principal` needed from the identity design (define in `auth/principal.rs`):

```rust
pub struct Principal {
    pub username: String,
    pub is_admin: bool,
    pub device_uid: Option<String>,             // from cert (serial→device) or ?clientUid fallback per handler
    pub groups: Vec<GroupMembership>,           // { name, bitpos, direction: In|Out, active: bool }
    pub auth: AuthKind,                         // ClientCert{serial} | Bearer | Basic | Anonymous
    pub identity_from_authorization_header: bool, // for D2
}
impl Principal {
    pub fn out_groups(&self) -> impl Iterator<Item=&str>;   // active OUT
    pub fn in_groups(&self) -> impl Iterator<Item=&str>;
    pub fn can_read(&self, groups: &[String]) -> bool;      // admin || any OUT ∩ groups (empty groups ⇒ __ANON__)
    pub fn holds_any(&self, groups: &[String]) -> bool;     // membership in either direction
}
```

Streaming-side traits the Marti/mission code needs (implemented in `stream/`, injected via `AppContext`):

```rust
#[async_trait]
pub trait LiveState: Send + Sync {
    fn subscriptions(&self) -> Vec<LiveSubscription>;              // all connected
    fn by_client_uid(&self, uid: &str) -> Option<LiveSubscription>;
    fn by_callsign(&self, callsign: &str) -> Vec<LiveSubscription>;
    fn for_user(&self, username: &str) -> Vec<LiveSubscription>;
    fn set_incognito(&self, uid: &str, on: bool) -> bool;
    fn disconnect(&self, uid: &str) -> bool;
    async fn reauth_user(&self, username: &str);                   // re-resolve groups on live connections
}
pub struct LiveSubscription { pub sub_uid: String /* "tls:37" */, pub client_uid: String, pub callsign: String,
    pub username: String, pub team: String, pub role: String, pub takv: String, pub in_groups: Vec<String>,
    pub out_groups: Vec<String>, pub protocol: &'static str, pub ip: String, pub port: u16,
    pub connected_at: DateTime<Utc>, pub last_event_at: Option<DateTime<Utc>>, pub incognito: bool,
    pub latest_sa: Option<Arc<Event>> }

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn send_to_uids(&self, uids: &[String], event: Event);                       // direct, no group check, no flow tag
    async fn send_to_user(&self, username: &str, except_uid: Option<&str>, event: Event);
    async fn broadcast_to_groups(&self, groups: &[String], except_uid: Option<&str>, event: Event); // OUT ∩ groups∪__ANON__
    async fn resend_latest_sa(&self, username: &str);
    async fn send_reachable_disconnect(&self, username: &str);
    async fn group_change(&self, username: &str, client_uid: Option<&str>);           // builds t-x-g-c (uid = uuid[.clientUid])
    async fn mission(&self, notice: MissionNotice);                                    // builds t-x-m-* (§4.8)
}

/// Implemented by the missions module; called by stream/router.rs for each <dest mission|mission-guid>.
#[async_trait]
pub trait MissionIngest: Send + Sync {
    async fn publish(&self, dest: &MissionDest, event: &Event, sender: &SenderCtx) -> Vec<String>; // extra recipient uids
}
pub struct MissionDest { pub name: Option<String>, pub guid: Option<Uuid>, pub path: Option<String>, pub after: Option<String> }
pub struct SenderCtx { pub client_uid: String, pub username: String, pub cert_cn: Option<String>, pub in_groups: Vec<String> }
```

## 1. `rustak-server/src/marti/` layout and shared infrastructure

```
marti/
├── mod.rs              configure(role: ListenerRole) -> Scope; route-table registration order; wraps headers::MartiHeaders,
│                       JsonConfig/QueryConfig/PathConfig error handlers → error::MartiError; mounts /files/api/config at root
├── response.rs         ApiResponse<T>, kind::* consts, json()/text()/xml()/zip() builders, node_id()
├── error.rs            MartiError enum → ErrorResponse {status,code,message} JSON; html_404()/html_unavailable(); From impls
├── extract.rs          ApiVersion, MissionRef, CommaList<T>, LooseBool, CaseInsensitiveQuery, MartiPrincipal (Principal + listener)
├── headers.rs          HSTS / CORS (allow_all_origins) / OPTIONS preflight / Cache-Control helper / api-version header
├── time.rs             cot_date(), group_date(), java_date_string(), TimeWindow::parse(secago,start,end) (24h cap flag)
├── version.rs          /version, /version/config, /version/info, /node/id, /files/api/config, /util/*, /home, /GetTime, /ErrorLog
├── stubs.rs            video, vcm, injectors, repeater, KML 501s, missioncreate 501, subscriptions/add 501
├── groups.rs           /groups/all, /groups/active, /groups/activebits, /groups/groupCacheEnabled, /groups/{name}/{dir}, /groups/user, /users/all
├── contacts.rs         /contacts/all[/lite|/full], /clientEndPoints
├── subscriptions.rs    /subscriptions/all, /subscription/{uid}, /subscriptions/incognito/{uid}, /subscriptions/delete/{uid}, filter no-ops
├── cot.rs              /cot/xml/{uid}, /cot/xml/{uid}/all, /cot GET+POST, /cot/sa, /cot/matchUid
├── sync.rs             /Marti/sync/upload, /search, /content, /missionupload, /missionquery, /delete
├── sync_metadata.rs    /Marti/api/sync/metadata/{hash}/{key|keywords|expiration}, /Marti/api/sync/search (Resource JSON)
├── files.rs            /Marti/api/files/metadata[/count], /files/{hash} GET|HEAD|DELETE, /files/{hash}/metadata
├── profiles.rs         /tls/profile/enrollment, /device/profile/connection, /device/profile/tool/{tool}[/file], /tls/profile/tool/{tool}/file, /{name}/missionpackage
├── profiles_admin.rs   /Marti/api/device/profile* admin CRUD (thin wrappers; optional in M3)
└── missions/
    ├── mod.rs          scope wiring in the correct order (literal segments before {name}); MissionCtx extractor
    ├── dto.rs          exact wire structs: MissionJson, MissionAdd<T>, MissionRoleJson, MissionChangeJson, UidDetailsJson,
    │                   MissionSubscriptionJson, MissionInvitationJson, LogEntryJson, MissionLayerJson, ResourceJson, MapLayerJson
    ├── crud.rs         list, get (name/guid), create PUT/POST, update, delete (name + ?guid=)
    ├── misc.rs         copy, pagedmissions, missioncount, parent/children, send, contacts, kml(501)
    ├── contents.rs     PUT/DELETE contents, contents/missionpackage, keywords (mission/uid/content), archive
    ├── changes.rs      /changes, /cot
    ├── subscription.rs PUT/GET/DELETE/POST subscription, /subscriptions[/roles], /all/subscriptions[/guid], /role GET/PUT, /token, /password, /expiration
    ├── invitations.rs  /invitations?clientUid, /all/invitations, /{n}/invitations, /invite/{type}/{invitee} PUT/DELETE, POST /invite
    ├── logs.rs         /missions/logs/entries*, /missions/all/logs, /{n}/log
    └── layers.rs       /layers*, /maplayers*, /externaldata*, /feed* (stub)
```

Domain modules (route files stay thin; all logic here): `missions/` (model.rs, roles.rs, service.rs, subscriptions.rs, changes.rs, contents.rs, layers.rs, logs.rs, invitations.rs, keywords.rs, archive.rs, import.rs, cot.rs, notify.rs), `files/` (store.rs, metadata.rs, legacy.rs, search.rs, package.rs), `profiles/` (model.rs, prefs.rs, builder.rs, service.rs, config_package.rs), `cot_store/` (latest.rs, history.rs, retention.rs), `auth/mission_token.rs`, `db/repos/{missions,mission_subscriptions,mission_uids,mission_resources,mission_changes,mission_layers,mission_logs,mission_invitations,mission_extras,resources,profiles,cot_latest,cot_history,error_logs}.rs`.

### 1.1 Envelope

```rust
// marti/response.rs
#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub version: &'static str,                                   // always "3"
    #[serde(rename = "type")] pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")] pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")] pub messages: Option<Vec<String>>,
    #[serde(rename = "nodeId")] pub node_id: String,
}
pub fn ok<T: Serialize>(kind: &'static str, data: T) -> HttpResponse;              // 200, exact application/json
pub fn created<T: Serialize>(kind: &'static str, data: T) -> HttpResponse;         // 201
pub fn status<T: Serialize>(s: StatusCode, kind: &'static str, data: Option<T>) -> HttpResponse;
pub fn bare_json<T: Serialize>(data: &T) -> HttpResponse;                          // no envelope (contacts, util, files config)
pub fn text(s: StatusCode, body: impl Into<String>) -> HttpResponse;               // text/plain (no charset)
pub fn text_json(body: &serde_json::Value) -> HttpResponse;                        // Content-Type: text/json
pub fn xml(body: String) -> HttpResponse;                                          // application/xml
pub fn html(s: StatusCode, body: &'static str) -> HttpResponse;
```

The helper inserts `("Content-Type", "application/json")` explicitly rather than relying on `ContentType::json()`; a unit test asserts the header value byte-for-byte for `ok`, `created`, `bare_json`, and for actix's own `web::Json`-error path (which we override via `JsonConfig::error_handler`).

### 1.2 `type` string constants (`response::kind`)

| const | value | used by |
|---|---|---|
| `GROUP` | `com.bbn.marti.remote.groups.Group` | groups/all, groups/user, groups/{n}/{d} |
| `USER` | `com.bbn.marti.remote.groups.User` | users/all |
| `BOOLEAN` | `java.lang.Boolean` | groups/groupCacheEnabled, repeater/remove |
| `STRING` | `java.lang.String` | missions/{n}/token |
| `CLIENT_ENDPOINT` | `com.bbn.marti.remote.ClientEndpoint` | clientEndPoints |
| `MISSION` | `Mission` | every mission payload, missioncount |
| `MISSION_CHANGE` | `MissionChange` | changes, contents/missionpackage |
| `MISSION_LAYER` | `MissionLayer` | layers* |
| `MISSION_SUBSCRIPTION_FQCN` | `com.bbn.marti.sync.model.MissionSubscription` | PUT/GET subscription |
| `MISSION_SUBSCRIPTION` | `MissionSubscription` | subscriptions, subscriptions/roles, all/subscriptions |
| `MISSION_INVITATION` | `MissionInvitation` | all invitation lists |
| `MISSION_ROLE` | `com.bbn.marti.sync.model.MissionRole` | role |
| `LOG_ENTRY` | `com.bbn.marti.sync.model.LogEntry` | logs |
| `RESOURCE` | `Resource` | api/sync/search |
| `MAP_LAYER` | `MapLayer` | maplayers |
| `EXTERNAL_DATA` | `ExternalMissionData` | externaldata |
| `SUBSCRIPTION_INFO` | `SubscriptionInfo` | subscriptions/all, subscription/{uid} |
| `SERVER_CONFIG` | `ServerConfig` | version/config |
| `FILES` / `COUNT` / `DATA` | `Files` / `Count` / `data` | files/metadata, files/metadata/count, HEAD files/{hash} |
| `PROFILE` / `PROFILE_FILE` | `Profile` / `ProfileFile` | device/profile admin |

### 1.3 Errors

```rust
// marti/error.rs
pub enum MartiError {
    NotFound(String), InvalidRequest(String), InvalidBody, Duplicate(String), Validation(String),
    Internal(String), Gone(String), Unauthorized(String), Forbidden(String), NotImplemented(&'static str),
    Html404, HtmlUnavailable,                  // legacy servlet paths only
}
// → {"status":"NOT_FOUND","code":1,"message":"Not Found: …"}  (status = reason-phrase enum name, all three fields always present)
// NotFound 404/1 "Not Found: {m}" · InvalidRequest 400/2 "Invalid Request: {m}" · InvalidBody 400/2 "Invalid Request Body"
// Duplicate 409/4 "Duplicate Exception : {m}" · Validation 400/5 ": {m}" · Internal 500/6 "" · Gone 410/8 ": {m}"
// Unauthorized 401/9 ": {m}" · Forbidden 403/10 ": {m}" · NotImplemented 501/3 "{what} is not implemented"
```
`impl ResponseError for MartiError` (JSON body, exact content type). `From<human_errors::Error>`, `From<rusqlite::Error>` → Internal. HTML variants reproduce the TAK `ErrorController` documents (`<title>404 TAK Server resource not found</title>` …) — used only by `/Marti/sync/*`, `/Marti/GetTime`, `/Marti/ErrorLog`. node-tak's `isHTML` sniffer handles both.

### 1.4 Extractors

```rust
pub struct ApiVersion(pub u32);            // header "API_VERSION" (case-insensitive lookup), parse i32, default 2
pub enum MissionRef { Name(String), Guid(Uuid) }   // from match_info "name" | "guid"; name is trimmed + percent-decoded
pub struct CommaList<T>(pub Vec<T>);        // repeated ?k=a&k=b and/or ?k=a,b (split on `\s*,\s*`), for group/boundingPolygon/Keywords/relativePath/uid
pub struct LooseBool(pub bool);             // "true" (case-insensitive) ⇒ true, anything else false, never 400 (clientEndPoints flags)
pub struct CiQuery(HashMap<String /*lowercased*/, Vec<String>>);  // case-insensitive param names for /Marti/sync/* servlets
pub struct MartiPrincipal { pub principal: Principal, pub listener: ListenerRole, pub api_version: ApiVersion }
pub struct MissionCtx { pub mission: Mission, pub role: Option<MissionRole>, pub who: MartiPrincipal, pub token: Option<MissionClaims> }
```
`MissionCtx: FromRequest` (boxed future): resolves `MissionRef` → mission (404 `NotFound`, 410 `Gone` if `deleted_at` set), computes `role_for_request` (§4.5). Create routes take `MartiPrincipal` + `MissionRef` instead so a missing mission does not 404.

Path handling: actix `{name}` already matches `[^/]+` (dots included), so use plain `{name}`; register literal children (`all`, `logs`, `invitations`, `guid/{guid}/…`) **before** `{name}` routes in `missions/mod.rs`, and reject those literals as mission names on create. Name-route fallback: if `{name}` parses as a UUID and no mission has that name, look up by guid (lenient).

Method handling: create registered for both `PUT` and `POST`; `/Marti/sync/delete` for `DELETE|GET|POST`; `/Marti/api/cot` for `GET|POST` (body on GET: use `web::Bytes` extractor, actix delivers GET bodies); `/repeater/remove/{uid}` is `GET`. `OPTIONS` → 204 with CORS headers when `marti.allow_all_origins`, else 405 JSON.

### 1.5 Headers / CORS / HSTS (`headers.rs`)

`MartiHeaders` middleware: on TLS listeners `Strict-Transport-Security: max-age=63072000; includeSubDomains`; when `marti.allow_all_origins = true` add `Access-Control-Allow-Origin: *`, `-Headers: *`, `-Methods: *` (default off — CloudTAK/ATAK are not browsers; our Yew UI is same-origin). `cache_headers()` helper adds `Cache-Control: must-revalidate, max-age=0, no-cache, no-store` + `Expires: 0` (only `/clientEndPoints`). `api-version: 3` only on `/Marti/sync/content`.

Config additions (`config/marti.rs`, `deny_unknown_fields`): `public_host: Option<String>`, `allow_all_origins: bool = false`, `store_error_logs: bool = true`, `error_log_retention: usize = 200`; `config/files.rs`: `upload_size_limit_mb: u32 = 400`, `content_dir`; `config/missions.rs`: `delete_requires_owner: bool = true`, `create_groups_regex: Option<String>`; `config/profiles.rs`: `enrollment_defaults: bool = true`.

## 2. Version / config / misc (`version.rs`, `stubs.rs`)

| Method | Path | Auth | Response |
|---|---|---|---|
| GET | `/Marti/api/version` | anonymous | `text/plain` body exactly `TAK Server rustak-<CARGO_PKG_VERSION>` (no newline). ATAK matcher needs `TAK Server`; **[CloudTAK]** probes this on every login under mTLS — must be 2xx; cert rejection must happen at the TLS layer. |
| GET | `/Marti/api/version/config` | anonymous | `{"version":"3","type":"ServerConfig","data":{"version":"<semver>","api":"3","hostname":"<Host header sans port>"},"nodeId":…}`. ATAK parses top-level `version` as int (→3 ≥ 2 enables `tool` on sync search). |
| GET | `/Marti/api/version/info` | anonymous | bare `{"major":M,"minor":m,"patch":p,"branch":"rustak","variant":"DIRECT"}` (ints from semver). |
| GET | `/Marti/api/node/id` | anonymous | `text/plain` node id. Node id = `rustak-<8 hex>` generated once into `kv` (must be an XML NCName: it becomes the `TAK-Server-<id>` flow-tag attribute). |
| GET | `/files/api/config` | anonymous | bare `{"uploadSizeLimit": <int MB>}` **[CloudTAK setup gate]**. `POST` → admin only, body `{uploadSizeLimit}` persisted as a `kv` override; non-admin 403. |
| GET | `/Marti/api/util/user/roles` | any | bare array: `["ROLE_ANONYMOUS"]` + `"ROLE_ADMIN"` if admin + `"ROLE_WEBTAK"` + `"ROLE_READONLY"` when the caller has no active IN group. |
| GET | `/Marti/api/util/isAdmin` | any | bare `true`/`false`. |
| GET | `/Marti/api/home` | any | `text/plain` `/webtak/index.html` (admin: `/Marti/metrics/index.html`) — informational. |
| GET | `/Marti/GetTime` | any | `text/plain` current time as `cot_date()` (**unverified** exact TAK format; ATAK only uses it for drift). |
| POST | `/Marti/ErrorLog` | any | 200 empty; body stored in `error_logs(id, received_at, username, client_uid, size, body BLOB)` capped at `error_log_retention`, or discarded when disabled. `GET`/`DELETE` admin-only list/purge. |
| GET | `/Marti/api/video` | any | bare `{"videoConnections":[]}` **[CloudTAK]**; `POST /video`, `PUT|DELETE /video/{uid}` → 501; `GET /video/{uid}` → 404 JSON. |
| GET | `/Marti/vcm` | any | `application/xml` `<videoConnections/>` (legacy ATAK alias sync; **unverified**, harmless). `/vcu`,`/vcs` → 501. |
| GET | `/Marti/api/injectors/cot/uid` | admin | `{"version":"3","type":"UidCotTagInjector","data":[]}`; `/uid/{uid}` 404; `POST|DELETE` 501. |
| GET | `/Marti/api/repeater/list` | admin | `{"version":"1.0.0","type":"Repeatable","data":[]}`; `GET|POST /period` → `{"version":"1.0.0","type":"Integer","data":3000}`; `GET /remove/{uid}` → `ApiResponse<bool>` false. |
| * | `/Marti/ExportMissionKML`, `/Marti/KmlMasterSA`, `/Marti/LatestKML`, `/Marti/TracksKML`, `/Marti/api/missions/{n}/kml` | any | 501 `NotImplemented("KML export")`. |
| POST | `/Marti/sync/missioncreate` | any | 501. |

## 3. Groups / contacts / subscriptions

### 3.1 Groups (`groups.rs`)

Group JSON (`GroupJson`, `skip_serializing_if` on optional fields):
`{"name","direction":"IN"|"OUT","created":"yyyy-MM-dd","type":"SYSTEM","bitpos":<int, never omitted>,"active":<bool>,"description"?}`. ATAK rejects the **whole** response if any element lacks `bitpos`/`created`/`name`/`direction`/`type` or `created` fails to parse.

| Method | Path | Behaviour |
|---|---|---|
| GET | `/Marti/api/groups/all?useCache&sendLatestSA` | Non-admin: one entry per (group, direction) membership of the caller with `active` from the membership row (D8). Admin: every group, both directions, `active` from own membership if any else `true`. `sendLatestSA=true` → `notifier.resend_latest_sa(user)`. Envelope `GROUP`. Always 200. |
| PUT | `/Marti/api/groups/active?clientUid=` | Body: bare JSON array; each element read leniently as `{name, direction, active}` (ignore `created` numeric, `type`, `bitpos`, `distinguishedName`). Non-array/invalid JSON → 400. For each element matching a membership row set `active`; unknown groups ignored. Then: `send_reachable_disconnect(user)` → repo update → `live.reauth_user(user)` → `resend_latest_sa(user)` → `group_change(user, clientUid)` (D9: also when `clientUid` absent). 200 empty body. If zero active groups remain → still 200 (TAK's 400 is config-gated off by default). |
| PUT | `/Marti/api/groups/activebits?clientUid=` | Body `[int]` bitpos values; `active = bitpos ∈ body` for both directions; same side-effects. |
| GET | `/Marti/api/groups/groupCacheEnabled` | `ApiResponse<bool>` `true`, kind `BOOLEAN`. |
| GET | `/Marti/api/groups/{name}/{direction}` | `direction ∈ {IN,OUT}` else 400; membership of caller (admin: any group) → `ApiResponse<Group>` or 404 with `data` omitted. |
| GET | `/Marti/api/groups/user?username=` | admin; `username` required (400). Groups of that user. |
| GET | `/Marti/api/users/all` | admin; `ApiResponse<Vec<UserJson>>` kind `USER`, `UserJson {id: username, name: username, connectionType: "CORE", created: cot_date}` sorted by created desc (**shape unverified beyond FQCN; not consumed by ATAK/CloudTAK**). |
| GET | `/Marti/api/users/{connectionId}` | admin; `{"user": UserJson, "groups": [Group]}` or 404. |
| PUT/POST/GET | `/groups/activeForce`, `/groups/update*` | admin; `activeForce?username=` reuses `set_active`; `update` → 200 no-op. |

Identity-design tables assumed: `groups(id, name UNIQUE, bitpos UNIQUE, type, created)`, `group_members(user_id, group_id, direction, active)`; bitpos allocated from a monotonic `kv` counter (never reused); `__ANON__` created in migration 1.

### 3.2 Contacts / clientEndPoints (`contacts.rs`)

| Method | Path | Behaviour |
|---|---|---|
| GET | `/Marti/api/contacts/all?sortBy&direction&noFederates` | **Bare JSON array** **[CloudTAK]** of `{"filterGroups":null,"notes":"","callsign","team","role","takv","uid"}` — `uid` = `client_uid`, `notes` never null. Source: `live.subscriptions()` filtered to non-incognito subs where caller can read (caller OUT ∩ sub IN, or admin). `sortBy ∈ {CALLSIGN,UID,USERNAME,…}` / `direction ∈ {ASCENDING,DESCENDING}` validated (400) then ignored (parity). |
| GET | `/contacts/all/lite` | same + `"user": UserJson`. `/contacts/all/full` → same fields plus `clientUid`, `username`, sorted per params. |
| GET | `/Marti/api/clientEndPoints?secAgo=0&showCurrentlyConnectedClients&showMostRecentOnly&group=…` | `secAgo<0` → 400. `group` repeatable: 403 Forbidden if caller lacks OUT on any named group or vector empty. Source: join of `cot_store.latest_all()` (one row per client uid seen sending SA, with `groups`, `last_event_at`, callsign/team/role from the stored SA) with `live.by_client_uid()`. Row: `{"callsign","uid","username","team","role","lastStatus":"Connected"|"Disconnected","lastEventTime":cot_date}` — `lastStatus` must be exactly one of those two or ATAK rejects everything. Filters: `secAgo>0` → `last_event_at ≥ now-secAgo`; `showCurrentlyConnectedClients=true` → live only; caller-readable groups; always one row per uid. Envelope `CLIENT_ENDPOINT` (ATAK also reads top-level `"version"`). Adds `cache_headers()`. |

### 3.3 Subscriptions (`subscriptions.rs`)

| Method | Path | Behaviour |
|---|---|---|
| GET | `/Marti/api/subscriptions/all?sortBy&direction&page=-1&limit=-1` | `ApiResponse<Vec<SubscriptionInfoJson>>` kind `SUBSCRIPTION_INFO`. **Every key present** (null when unknown): `dn, callsign, clientUid, lastReportMilliseconds, lastReportDiffMilliseconds, takClient, takVersion, username, groups:[Group], role, ipAddress, port, pendingWrites, team, protocol, xpath, subscriptionUid, numProcessed, appFramerate, battery, batteryStatus, batteryTemp, deviceDataRx, deviceDataTx, heapCurrentSize, heapFreeSize, heapMaxSize, deviceIPAddress, storageAvailable, storageTotal, incognito:false, handlerType, metrics:null`. `takClient/takVersion` = split of `takv` on `:`; `callsign` falls back to uid. Admin: all + paging; non-admin: readable subset, paging ignored. |
| GET | `/Marti/api/subscription/{uid}` | match on `clientUid`; 404 with `data` omitted. |
| POST | `/Marti/api/subscriptions/incognito/{uid}` | toggles via `live.set_incognito`; 200 empty; 404 if not connected. |
| DELETE | `/Marti/api/subscriptions/delete/{uid}` | admin; `live.disconnect(uid)`; `ApiResponse<String>` kind `String` message. |
| POST | `/Marti/api/subscriptions/add` | admin; 501. |
| PUT/DELETE | `/Marti/api/subscriptions/{clientUid}/filter` | 200 no-op (geospatial filters deferred). |

## 4. Missions (Data Sync)

### 4.1 Schema (proposed to foundations; `db/migrations/00xx_missions.sql`)

```sql
missions(id INTEGER PK, guid TEXT UNIQUE NOT NULL, name TEXT UNIQUE NOT NULL, description TEXT DEFAULT '', chat_room TEXT DEFAULT '',
  base_layer TEXT DEFAULT '', bbox TEXT DEFAULT '', bounding_polygon TEXT /*JSON [String]*/, path TEXT DEFAULT '', classification TEXT DEFAULT '',
  tool TEXT NOT NULL DEFAULT 'public', creator_uid TEXT DEFAULT '', create_time INTEGER NOT NULL /*ms*/, last_edited INTEGER,
  expiration INTEGER NOT NULL DEFAULT -1, password_hash TEXT, default_role TEXT NOT NULL DEFAULT 'MISSION_SUBSCRIBER',
  invite_only INTEGER NOT NULL DEFAULT 0, groups TEXT NOT NULL /*JSON [String]*/, parent_id INTEGER REFERENCES missions(id),
  deleted_at INTEGER /* soft delete ⇒ 410 */);
mission_keywords(mission_id, keyword, PRIMARY KEY(mission_id, keyword));
mission_subscriptions(id TEXT PK /*uuid, = SUBSCRIPTION claim*/, mission_id, client_uid TEXT, username TEXT, role TEXT, token_id TEXT,
  create_time INTEGER, UNIQUE(mission_id, client_uid));  INDEX(client_uid)
mission_uids(mission_id, uid TEXT, creator_uid, timestamp INTEGER, keywords TEXT/*JSON*/, layer_uid TEXT, position INTEGER,
  det_type, det_callsign, det_title, det_iconset_path, det_color, det_lat REAL, det_lon REAL, det_name, det_category, det_attachments TEXT,
  PRIMARY KEY(mission_id, uid));  INDEX(uid)
mission_resources(mission_id, resource_hash TEXT, creator_uid, timestamp INTEGER, keywords TEXT, layer_uid TEXT, position INTEGER,
  PRIMARY KEY(mission_id, resource_hash));  INDEX(resource_hash)
mission_changes(id INTEGER PK, mission_id, mission_name TEXT, mission_guid TEXT, type TEXT /*CREATE_MISSION|DELETE_MISSION|ADD_CONTENT|REMOVE_CONTENT|CREATE_DATA_FEED|DELETE_DATA_FEED*/,
  content_uid TEXT, content_hash TEXT, timestamp INTEGER, servertime INTEGER, creator_uid TEXT, is_federated INTEGER DEFAULT 0,
  log_entry_id TEXT, external_data_id TEXT, map_layer_uid TEXT, feed_uid TEXT);  INDEX(mission_id, timestamp)
mission_layers(uid TEXT PK, mission_id, name TEXT, type TEXT /*GROUP|UID|CONTENTS|MAPLAYER|ITEM*/, parent_uid TEXT, position INTEGER, creator_uid, create_time);
mission_logs(id TEXT PK, content TEXT, creator_uid, entry_uid TEXT, servertime INTEGER, dtg INTEGER, created INTEGER, keywords TEXT, content_hashes TEXT);
mission_log_missions(log_id, mission_id, PRIMARY KEY(log_id, mission_id));
mission_invitations(id INTEGER PK, mission_id, mission_name, invitee TEXT, type TEXT /*clientUid|callsign|userName|group|team*/, creator_uid,
  create_time INTEGER, token TEXT /*whole JWT*/, role TEXT, UNIQUE(mission_id, type, invitee));  INDEX(invitee)
mission_external_data(id TEXT PK, mission_id, name, tool, url_data, url_view, notes);
map_layers(uid TEXT PK, mission_id, layer_uid TEXT, body TEXT /*JSON as received*/, creator_uid, create_time);
mission_feeds(uid TEXT PK, mission_id, data_feed_uid, body TEXT);  -- stub storage
```

### 4.2 Roles / permissions (`missions/roles.rs`)

`MISSION_OWNER` → all eight (`MISSION_READ, MISSION_WRITE, MISSION_DELETE, MISSION_SET_ROLE, MISSION_SET_PASSWORD, MISSION_UPDATE_GROUPS, MISSION_MANAGE_FEEDS, MISSION_MANAGE_LAYERS`); `MISSION_SUBSCRIBER` → `READ, WRITE`; `MISSION_READONLY_SUBSCRIBER` → `READ`. `MissionRole { kind: Role, using_default: bool }`, `fn require(role: Option<&MissionRole>, p: Permission) -> Result<(), MartiError /*Forbidden*/>`.

### 4.3 Service API (`missions/service.rs` + siblings)

```rust
pub struct MissionService { repo: MissionRepos, files: FileService, cot: Arc<dyn CotStore>, tokens: MissionTokens,
                            notifier: Arc<dyn Notifier>, live: Arc<dyn LiveState>, cfg: MissionsConfig }
impl MissionService {
  pub async fn list(&self, who: &Principal, f: ListFilter) -> Result<Vec<Mission>>;   // tool (default "public"), password_protected, default_role, paging, name/uid filter
  pub async fn resolve(&self, r: &MissionRef) -> Result<Mission>;                     // NotFound | Gone
  pub fn role_for_request(&self, m: &Mission, who: &Principal, tok: Option<&MissionClaims>) -> Option<MissionRole>;
  pub async fn role_from_token(&self, m: &Mission, allowed: &[TokenType], who: &Principal, tok: Option<&MissionClaims>) -> Option<MissionRole>;
  pub async fn create_or_update(&self, who: &Principal, name: &str, p: MissionParams, body: CreateBody) -> Result<Outcome>; // Outcome::Created{mission, token, owner_role} | Updated(mission)
  pub async fn delete(&self, who, m: Mission, creator_uid: &str, deep: bool, host: &str) -> Result<Mission>;
  pub async fn copy(&self, who, m, p: CopyParams) -> Result<Mission>;
  // contents.rs
  pub async fn add_content(&self, m: &Mission, c: MissionContent, creator_uid: &str, at: DateTime<Utc>) -> Result<Vec<MissionChange>>;
  pub async fn remove_content(&self, m, hash: Option<&str>, uid: Option<&str>, creator_uid) -> Result<Vec<MissionChange>>;
  pub async fn import_package(&self, m, zip: Bytes, creator_uid) -> Result<Vec<MissionChange>>;
  pub async fn set_keywords(&self, m, target: KeywordTarget /*Mission|Uid|Hash*/, kws: Vec<String>, creator_uid) -> Result<()>;
  pub async fn archive(&self, m, host: &str) -> Result<Vec<u8>>;
  // changes.rs / cot.rs
  pub async fn changes(&self, m, w: TimeWindow, squashed: bool) -> Result<Vec<MissionChange>>;
  pub async fn cot_events_xml(&self, m, path: Option<&str>) -> Result<String>;
  // subscriptions.rs
  pub async fn subscribe(&self, m, req: SubscribeReq) -> Result<(MissionSubscription /*with token*/, Vec<MissionChange>, Vec<LogEntry>)>;
  pub async fn unsubscribe(&self, m, client_uid: &str) -> Result<()>;
  pub async fn set_role(&self, m, client_uid: Option<&str>, username: Option<&str>, role: Role) -> Result<()>;
  pub async fn subscriptions(&self, m) -> Result<Vec<MissionSubscription>>;
  pub async fn access_token(&self, m, password: &str) -> Result<String>;          // validates bcrypt → ACCESS token, no exp
  pub async fn set_password(&self, m, pw: Option<&str>) -> Result<()>;
  // invitations.rs, logs.rs, layers.rs, keywords.rs, notify.rs — analogous
}
```

### 4.4 Mission tokens (`auth/mission_token.rs`)

```rust
pub enum TokenType { Subscription, Invitation, Access }   // sub claim = "SUBSCRIPTION"|"INVITATION"|"ACCESS"
pub struct MissionClaims { pub jti: String, pub iat: i64, pub exp: Option<i64>, pub iss: String, pub kind: TokenType,
                           pub id: String /* value of the claim named after the type */, pub mission_name: String, pub mission_guid: Uuid }
pub struct MissionTokens { secret: Zeroizing<[u8; 32]>, issuer: String }   // secret created once, sealed in SecretStore under "mission-token-hmac"
impl MissionTokens {
    pub fn issue(&self, id: &str, kind: TokenType, m: &Mission, ttl: Option<Duration>) -> String;
    //  header {"alg":"HS256","typ":"JWT"}; claims: jti=id, iat, sub=kind.name(), iss, [exp], <KIND>=id, MISSION_NAME, MISSION_GUID
    pub fn verify(&self, token: &str) -> Result<MissionClaims, TokenError>;         // HS256, validates exp if present, no aud/iss check
}
pub fn mission_bearer(req: &HttpRequest, identity_used_authorization: bool) -> Option<&str>;
//  MissionAuthorization first; else Authorization unless identity consumed it; strip "Bearer " (also accept "bearer ")
```

`role_from_token` (06 §8.2): admin → `MISSION_OWNER` unconditionally. Token: kind ∈ allowed; `mission_name == m.name` (case-sensitive) **or** `mission_guid == m.guid` (rename-safe extension) else `None`. `Subscription` → `mission_subscriptions.id == claims.id && mission_id` → its role; `Invitation` → `mission_invitations.token == <whole token>` and `mission_name` equal ignoring case → its role; `Access` → default role. `role_for_request` = `role_from_token([Access, Subscription])` → admin OWNER → `None` if password-protected or invite-only → default role. `Invitation` tokens are only honoured by `PUT …/subscription`.

### 4.5 Wire DTOs (`missions/dto.rs`) — exact

`MissionJson` (all `Option` fields `skip_serializing_if = None`; arrays always present):
```
name, description, chatRoom, baseLayer, bbox, boundingPolygon:[…], path, classification, tool, expiration:<i64, -1 default>,
keywords:[…], creatorUid, createTime, lastEdited?, uids:[MissionAdd{data:"<uid>",timestamp,creatorUid,keywords?,details?}],
contents:[MissionAdd{data:ResourceJson,timestamp,creatorUid,keywords?}], groups:[names], externalData:[…], mapLayers:[…], feeds:[…],
passwordProtected:<bool always>, inviteOnly:<bool always>, defaultRole:{type,permissions}, token?, ownerRole?, missionChanges?, logs?, guid
```
**[CloudTAK]** requires `externalData/feeds/mapLayers/uids/contents` as arrays, `inviteOnly` boolean, `expiration` number, `guid`, and `token` on create. `ownerRole`/`token` present **only** on the 201 create response.

`MissionChangeJson`: `type, timestamp, serverTime, missionName, missionGuid, isFederatedChange:<bool>, contentUid?, creatorUid, details?:{type,callsign,title,iconsetPath,color,attachments,name,category,location:{lat,lon}}, contentResource?:ResourceJson, logEntry?, missionFeed?, mapLayer?, externalData?` (no `contentHash`).

`ResourceJson` (lowerCamel): `filename, keywords:[…], mimeType, contentType?, name, submissionTime, submitter, uid, hash, size:<int>, creatorUid, tool, latitude?, longitude?, altitude?, expiration:<i64,-1>, groups?`.

`MissionSubscriptionJson`: `token?, mission?, clientUid, username, createTime, role:{type,permissions}` (no `uid`).
`MissionInvitationJson`: `missionName, invitee, type, creatorUid, createTime, token, role, missionId:<int>, missionGuid`.
`LogEntryJson`: `id, content, creatorUid, entryUid?, missionNames:[…], servertime, dtg?, created, contentHashes:[…], keywords:[…]` (lower-case `servertime`).
`MissionLayerJson` (`skip_serializing_if` empty **and** None): `uid, name?, type, parentUid?, mission_layers?, uids?, contents?, maplayers?`.
`MissionRoleJson`: `{"type":"MISSION_…","permissions":[…]}`.
`MissionContentBody` (input): `{hashes?:[…], uids?:[…], paths?:{"<layerUid>":[MissionContentBody]}, after?}`; at least one non-empty → else 400.

### 4.6 Endpoint table (all under `/Marti/api`; `{n}` = `/missions/{name}` or `/missions/guid/{guid}` unless noted)

**crud.rs**

| Verb | Path | Permission | Behaviour / status |
|---|---|---|---|
| GET | `/missions?passwordProtected=false&defaultRole=false&tool` | readable groups | `tool` absent ⇒ `"public"`; `passwordProtected=false` excludes pw-protected; `defaultRole=false` excludes missions whose default role ≠ `MISSION_SUBSCRIBER`; invite-only never listed; group filter `mission.groups ∩ caller groups` (admin: all). `groups` populated. 200 `ApiResponse<[Mission]>`. |
| GET | `{n}?password&changes=false&logs=false&secago&start&end` | see text | `password` non-empty → bcrypt check (403 on mismatch) → `token` = ACCESS (no exp). Else `require(role, READ)`; on failure: `API_VERSION ≤ 2` → 403; `≥ 3` → 200 with `uids/contents/externalData/mapLayers/feeds` emptied. `changes=true` → **full history** in window; `logs=true`. 200 `ApiResponse<[Mission]>` (single-element array). |
| PUT/POST | `/missions/{name}` (+ all params of 06 §7.2 incl. `allowGroupChange`, `allowDupe`) | create: any authenticated; update: see text | Name: trim; regex `^[\p{L}\p{N}\w\s.()!=@#$&^*_\-+\[\]{}:,./|\\]*$` minus `/`; 1..=1024; not reserved (`all,logs,invitations,guid,hierarchy`); not UUID-shaped (D7) → 400 Validation. Body: `Content-Type` contains `application/json` → `MissionJson`-shaped partial overrides query params (non-null fields); any other non-empty body → treated as a data package and imported after create. `group` via `CommaList` default `["__ANON__"]` **[CloudTAK]** (comma-joined on create, repeated on update). Group rule: requested ⊄ caller's groups → if requested == `[__ANON__]` substitute caller's group names, else 403 (admin bypass). Optional `create_groups_regex`. `password` → bcrypt(cost 10). `defaultRole` enum (400 invalid). New → 201 with `token` (SUBSCRIPTION JWT for owner subscription `client_uid = creatorUid` or `device_uid` or username) + `ownerRole`; `t-x-m-n` broadcast. Existing → update; description/keywords-type fields need `WRITE`; `group`/`password`/`defaultRole`/`inviteOnly`/`expiration` changes need `OWNER` or admin; `allowGroupChange` accepted and ignored **[CloudTAK]**; 200 without token; `t-x-m-c-m` broadcast. |
| DELETE | `/missions/{name}?creatorUid&deepDelete=false` | owner/admin (`delete_requires_owner`), `deepDelete` needs `MISSION_DELETE` | Archive zip → stored as Resource (`keywords:["ARCHIVED_MISSION"]`, name `<name>_<guid>.zip`, tool `archive`); soft-delete (`deleted_at`) → later GETs 410; `deepDelete` also drops `cot_history` for mission uids and unreferenced resources; `DELETE_MISSION` change row; `t-x-m-d` broadcast. 200 with deleted Mission. |
| DELETE | `/missions?guid=&creatorUid&deepDelete` | same | `guid` required; invalid UUID → 400 `Invalid Request: Invalid mission guid in request` **[CloudTAK]**. |

**misc.rs**

| Verb | Path | Behaviour |
|---|---|---|
| PUT | `/missions/{name}/copy?creatorUid&copyName&copyPath&defaultRole&password` | READ on source; clones metadata, keywords, uids, resources, layers; 201 new Mission. |
| GET | `/pagedmissions?passwordProtected=true&defaultRole=true&page=0&pagesize=10&tool&sort&nameFilter&uidFilter&ascending=true` | paging over the same filter; `sort ∈ {name, createTime}`; `nameFilter` substring; `uidFilter` = missions containing that uid. |
| GET | `/missioncount?passwordProtected=true&defaultRole=true&tool` | `ApiResponse<int>` kind `MISSION`. |
| PUT/DELETE/GET | `/missions/{child}/parent/{parent}`, `/missions/guid/{child}/parent/guid/{parent}`, `/{n}/parent`, `/{n}/children` | WRITE on child; `GET /parent` → `ApiResponse<Mission>` (single object) or 404; `children` → array. |
| POST | `{n}/send?contacts=…` | non-empty `contacts` (400) → treated as clientUid invites (§invitations) → 200 `ApiResponse<[Mission]>`. |
| GET | `{n}/contacts` | bare array `{callsign, clientUid, uid, username, team, role, takv}` of connected subscribers. |
| GET | `{n}/kml` | 501. |

**contents.rs**

| Verb | Path | Behaviour |
|---|---|---|
| PUT | `{n}/contents?creatorUid` | WRITE; body `MissionContentBody`; hashes must exist in `resources` (404 otherwise); uids: upsert `mission_uids` with details cached from `cot_store.latest(uid)` (type, callsign from `<contact>`, iconsetPath from `<usericon iconsetpath>`, color from `<color argb>`, lat/lon, title/name/category from `<title>`/`<contact>`/`<archive>` if present); `paths` → file under layer uid with `after` ordering; one `ADD_CONTENT` change per item; `t-x-m-c` per item to connected subscribers minus author. 200 `ApiResponse<[Mission]>`. |
| DELETE | `{n}/contents?hash&uid&creatorUid` | WRITE; `REMOVE_CONTENT` change; `t-x-m-c`. 200 `[Mission]`. |
| PUT | `/missions/{name}/contents/missionpackage?creatorUid` | WRITE; body zip; `files/package.rs` reader (manifest may sit one level down); `.cot` entries (`isCoT`/`.cot`) → parse → `cot_store` + uid add; others → `files.store` + Resource (name from `Content/Parameter[name]` else basename, mime guessed, keywords from manifest) + hash add. 200 `ApiResponse<[MissionChange]>` kind `MISSION_CHANGE`; 409 only on malformed manifest. |
| PUT/DELETE | `/missions/{name}/keywords` (body `[String]`), DELETE `/keywords/{kw}` | WRITE; replace/clear/remove; `t-x-m-c-k` broadcast. |
| PUT/DELETE | `/missions/{name}/uid/{uid}/keywords`, `/content/{hash}/keywords` | WRITE; `t-x-m-c-k-u` / `t-x-m-c-k-c` to subscribers. |
| GET | `/missions/{name}/archive` | READ; zip per 06 §7.10: dir entries `cot/`, `contents/`, `MANIFEST/`; `cot/<uid>.cot`; `contents/<n>_<name>` (fallback `contents/<hash>`); `MANIFEST/manifest.xml` v2 with Parameters in order `uid,name,mission_guid,password_hash,creatorUid,create_time,expiration,chatroom,description,tool,onReceiveImport=true,onReceiveDelete=false,mission_name,mission_label,mission_uid=<host>-8443-ssl-<name>,mission_server=<host>:8443:ssl`, `<Groups><Group name/>`, `<Role name><Permission name/></Role>`; zip times 0. Header `Content-Disposition: attachment; filename="<urlencoded name>.zip"` (quote fixed), `Content-Type: application/zip`. |

**changes.rs**

| Verb | Path | Behaviour |
|---|---|---|
| GET | `{n}/changes?secago&start&end&squashed=true` | READ; `TimeWindow` (`secago` wins); `squashed=true` → fold (D11): per `(ADD|REMOVE, uid|hash, creatorUid)` keep newest; keep ADD only if item still present, REMOVE only if absent; always include `CREATE_MISSION`/feed rows; `squashed=false` → every row. Sorted newest first. 200 `ApiResponse<[MissionChange]>`. |
| GET | `{n}/cot?path` | READ; `<?xml version='1.0' encoding='UTF-8' standalone='yes'?><events>` + for each mission uid (optionally only `layer_uid == path`) `cot_store.latest(uid)` XML with `<marti>` stripped + `\n` … `</events>`; `application/xml`. |

**subscription.rs**

| Verb | Path | Behaviour |
|---|---|---|
| PUT | `{n}/subscription?uid&topic&password&secago&start&end` | `subRole = role_from_token([Invitation,Subscription,Access])`. Password-protected: `password` given → bcrypt else 403 `Password did not match.`; none and `subRole==None` → 403 `No token role provided.`. Not protected and `password` given → 403 `No password provided.` (parity). Invite-only and no token role → look up invitation by (`clientUid==uid`, `userName==username`, `callsign` of live uid, `group`/`team` of caller) else 403. `role = subRole ?? default`. `uid` or `topic` required (400). Upsert subscription (keep existing role if row exists; new token id); token = SUBSCRIPTION JWT (no exp); auto-delete matching `clientUid`/`callsign` invitations. **201** `ApiResponse<MissionSubscription>` kind FQCN **with `token`** **[CloudTAK]**; `mission` nested only when `API_VERSION ≥ 3` (with full-history `missionChanges` in window + `logs`). |
| GET | `{n}/subscription?uid` | 404 `NotFound` when none; kind FQCN, token included. |
| DELETE | `{n}/subscription?uid&topic&disconnectOnly=true` | delete row regardless of `disconnectOnly` (documented deviation); 200 empty. |
| POST | `{n}/subscription?creatorUid` | body `[MissionSubscriptionJson]`; SET_ROLE; bulk upsert roles; 200 empty. |
| GET | `{n}/subscriptions` | `ApiResponse<[String]>` client uids, kind `MissionSubscription`. |
| GET | `{n}/subscriptions/roles` | `ApiResponse<[MissionSubscription]>` **tokens omitted**. |
| GET | `/missions/all/subscriptions[/guid]` | admin; `[{"<missionName>":"<clientUid>"}]` (`/guid` keys by guid). |
| GET | `{n}/role` | `ApiResponse<MissionRole>` kind `MISSION_ROLE` = role for request (data omitted if none). |
| PUT | `{n}/role?clientUid&username&role` | SET_ROLE; update by clientUid (or all subs of username); `t-x-m-r` to that uid; 200 empty. |
| GET | `/missions/{name}/token?password` | pw check (403) → **201** `ApiResponse<String>` kind `STRING` = ACCESS token. |
| PUT/DELETE | `{n}/password?password&creatorUid` | SET_PASSWORD; `t-x-m-c-m` broadcast. |
| PUT | `{n}/expiration?expiration` | OWNER; store i64 (epoch seconds, `-1` clears). |

**invitations.rs**

| Verb | Path | Behaviour |
|---|---|---|
| PUT | `{n}/invite/{type}/{invitee}?creatorUid&role` | WRITE; `type ∈ {clientUid,callsign,userName,group,team}` exactly else 400; role default `MISSION_SUBSCRIBER`; upsert; INVITATION token (`id` = invitation row id, stored whole); resolve recipient uids: clientUid→that uid; callsign→`live.by_callsign`; userName→`live.for_user`; group→live subs with that OUT group; team→live subs with that team; `t-x-m-i` with `token` and `<role>`. 200 empty. |
| DELETE | `{n}/invite/{type}/{invitee}` | WRITE; 200. |
| POST | `{n}/invite?creatorUid` | body `[{type, invitee, role?}]` → bulk of the above. |
| GET | `/missions/invitations?clientUid=` | **[CloudTAK]** called in parallel with the list page. Invitations where `(type,invitee)` matches: `clientUid`=param; `callsign`= live callsign of that uid; `userName`= caller; `group` ∈ caller groups; `team` = live team. Full objects incl `token`. 200 (empty array when none). |
| GET | `/missions/all/invitations?clientUid=` | same query → `ApiResponse<[String]>` mission names. |
| GET | `{n}/invitations` | READ; list for mission. |

**logs.rs**

| Verb | Path | Behaviour |
|---|---|---|
| POST | `/missions/logs/entries` | body `LogEntryJson`; `id` present → 400; each `missionNames` entry must resolve and grant WRITE; `id` uuid, `servertime=created=now`, `dtg` default now; **201** `ApiResponse<LogEntry>`; `t-x-m-c-l` to subscribers of each mission. |
| PUT | `/missions/logs/entries` | `id` required, `servertime` must be absent → else 400; **201**. |
| GET/DELETE | `/missions/logs/entries/{id}` | `ApiResponse<[LogEntry]>` (1 element) / 404; DELETE 200. |
| GET | `/missions/all/logs` | admin. |
| GET | `{n}/log?secago&start&end` | READ; entries by `created` window. |

**layers.rs**

| Verb | Path | Behaviour |
|---|---|---|
| GET | `{n}/layers`, `{n}/layers/{layerUid}` | READ; tree: root layers with nested `mission_layers`, and `uids`/`contents`/`maplayers` (as `MissionAdd`) filed under each; positions ordered. |
| PUT | `{n}/layers?name&type&uid&parentUid&afterUid&creatorUid` | WRITE (MANAGE_LAYERS for admins-only semantics not enforced — CloudTAK creates layers with a SUBSCRIBER token); `uid` default uuid; `afterUid` empty/absent ⇒ append; `t-x-m-c-h` with `<missionLayer name parentUid type uid/>`. `ApiResponse<MissionLayer>`. |
| PUT | `{n}/layers/{uid}/name?name&creatorUid`, `{n}/layers/{uid}/position?afterUid`, `{n}/layers/parent?layerUid&parentUid&afterUid` | WRITE; 200 empty. |
| DELETE | `{n}/layers?uid=…&creatorUid` | `uid` repeatable; deletes layer + descendants; items become unfiled (`layer_uid = NULL`), not removed. |
| POST/PUT | `{n}/maplayers?creatorUid` body MapLayer JSON | WRITE; stored verbatim (`uid` generated if missing); `ApiResponse<MapLayer>` kind `MAP_LAYER`; `t-x-m-c`. DELETE `/maplayers/{uid}` (both `/missions/{name}/…` and `/missions/{guid}/…` spellings). |
| POST | `{n}/externaldata` body `{name,tool,urlData,urlView,notes}` | WRITE; 201 `ApiResponse<ExternalMissionData>`; DELETE `/{id}`; POST `/{id}/change` → `t-x-m-c-e`. |
| POST/DELETE | `/missions/{missionName}/feed`, `/missions/{missionGuid}/feed`, `/feed/{uid}` | 200 no-op (row kept in `mission_feeds` for listing only). |

**sync_metadata.rs** also hosts `GET /Marti/api/sync/search?box&circle&startTime&endTime&minAltitude&maxAltitude&filename&keyword*&mimetype&name&uid&hash&mission&tool` → `ApiResponse<[Resource]>` kind `RESOURCE` (`box`+`circle` → 400).

### 4.7 `<dest mission>` publish path (`missions/cot.rs` implements `MissionIngest`)

1. Resolve mission by `name` (then `guid`); not found → log, return `[]`.
2. Subscription lookup by `(mission, sender.client_uid)`, fallback `(mission, username)`, fallback `(mission, cert_cn)`; none or role lacks `MISSION_WRITE` → skip (log at info), return `[]`.
3. Recipients = connected subscriber uids (`mission_subscriptions ⋈ live`) minus sender → returned to the router, which delivers the raw CoT as explicit-uid hits (router still applies IN/OUT).
4. Persist: `add_content(mission, MissionContent{uids:[event.uid], paths: path.map(|p| {p: [..]}), after}, sender.client_uid, event.time)` — upserts `mission_uids` with cached details from the event itself, writes `ADD_CONTENT`, and emits `t-x-m-c` (one MissionChange) to recipients (not the sender).
5. `b-t-f` with `<dest mission>`: relayed as normal; mission-chat id fixup deferred (documented).

### 4.8 Notifications (`missions/notify.rs` → `Notifier::mission`)

```rust
pub enum MissionNotice {
    Change   { kind: ChangeKind /*Content|Log|Keyword|UidKeyword|ResourceKeyword|Metadata|ExternalData|Layer|Default*/,
               mission: MissionRef2 /*name, guid, tool*/, author_uid: Option<String>, changes: Vec<MissionChangeXml>,
               layer: Option<MissionLayerXml>, recipients: Recipients },
    Created  { mission, author_uid },                 // t-x-m-n, type=CREATE, broadcast
    Deleted  { mission, author_uid },                 // t-x-m-d, type=DELETE, broadcast
    Invite   { mission, author_uid, token: String, role: MissionRoleXml, uids: Vec<String> },   // t-x-m-i, type=INVITE
    RoleChange { mission, author_uid, role: MissionRoleXml, uid: String },                        // t-x-m-r, type=INVITE (sic)
}
pub enum Recipients { Subscribers(Vec<String>), BroadcastGroups(Vec<String>) }
```
`stream/notify.rs` renders (05 §7.1): `<event version='2.0' uid='<uuid>' type='t-x-m-c[-l|-k|-k-u|-k-c|-m|-e|-h]' how='h-g-i-g-o' time start stale=+20s><point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/><detail><mission type='CHANGE|CREATE|DELETE|INVITE' name='…' guid='…' tool='…' [authorUid] [token]>[<MissionChanges><MissionChange><contentResource><creatorUid/><expiration/><filename/><hash/><keywords/>…<mimeType/><name/><size/><submissionTime/><submitter/><tool/><uid/></contentResource><contentUid/><creatorUid/><details type callsign color iconsetPath title><location lat lon/></details><isFederatedChange>false</isFederatedChange><missionGuid/><missionName/><timestamp/><type/></MissionChange></MissionChanges>][<missionLayer name parentUid type uid/>][<role type='…'><permissions><permission type='MISSION_READ'/>…</permissions></role>]</mission></detail></event>`. `contentResource` children are elements, `details` uses attributes **[CloudTAK/node-cot]**. One `MissionChange` per `t-x-m-c` so CloudTAK's `missionChanges.length === 1 && contentResource.name` branch fires for file adds/removes. Delivery: `Subscribers` → `send_to_uids`; `BroadcastGroups` → `broadcast_to_groups(mission.groups, except author)`.

Who gets what: content add/remove, log, uid/resource keywords, external data, layers → connected subscribers minus author; create/delete/mission keywords/metadata/password → broadcast to readers of the mission's groups minus creator; invite → resolved invitee uids; role change → the uid.

## 5. Enterprise Sync / files

### 5.1 Store and metadata

```rust
// files/store.rs
pub struct ContentStore { root: PathBuf }              // <data_dir>/content/<sha256>, tmp under <root>/tmp
impl ContentStore {
  pub async fn ingest<S: Stream<Item=Result<Bytes>>>(&self, body: S, limit: u64) -> Result<Ingested { hash: String, size: u64 }>; // streams to tmp file hashing as it goes; rename to <hash> (dedupe: keep existing); limit → Err(TooLarge)
  pub async fn open(&self, hash: &str) -> Result<(tokio::fs::File, u64)>;
  pub async fn remove_if_unreferenced(&self, hash: &str) -> Result<bool>;    // checks resources + mission_resources + profile_files
}
// files/metadata.rs — table `resources`
resources(id INTEGER PK /*PrimaryKey*/, uid TEXT UNIQUE, hash TEXT NOT NULL, filename, name, mime_type, size INTEGER, submission_time INTEGER,
  submitter, creator_uid, tool DEFAULT 'public', keywords TEXT/*JSON*/, groups TEXT/*JSON*/, mission_name, latitude REAL, longitude REAL,
  altitude REAL, expiration INTEGER DEFAULT -1, remarks, permissions TEXT, contacts TEXT, download_path, plugin_class_name,
  install_on_enrollment INTEGER DEFAULT 0 /*admin flag, §6*/);  INDEX(hash), INDEX(mission_name), INDEX(submission_time)
pub struct Resource { … }  impl Resource { fn to_json(&self) -> ResourceJson; fn to_legacy(&self) -> serde_json::Value /*Title-case*/; fn to_files_entry(&self) -> BTreeMap<String,String> }
```
Legacy `Metadata` view (`files/legacy.rs`): keys `UID, Name, Hash, PrimaryKey (string), SubmissionDateTime (padded), SubmissionUser, CreatorUid, Keywords:[…], MIMEType, Size (string), EXPIRATION (string, "-1" when unset — **[CloudTAK]** requires the key), Tool, Groups:[…], MissionName?, Altitude?, Latitude?, Longitude?, DownloadPath?, Remarks?, Permissions?, Contacts?, PluginClassName?` — absent values omitted. ATAK requires `UID,Name,Hash,SubmissionDateTime` non-empty and `PrimaryKey` parseable as int ≥ 0.

Visibility rule: a resource is readable if admin, or `resource.groups` (empty ⇒ `["__ANON__"]`) intersects caller OUT groups, or caller is its `submitter`. Uploads default `groups` to the caller's group names when no `Groups` param is given.

### 5.2 Endpoints (`sync.rs`, `sync_metadata.rs`, `files.rs`)

| Verb | Path | Behaviour |
|---|---|---|
| POST | `/Marti/sync/upload` | Params via `CiQuery`: `UID, Name (alias name), Hash (ignored), MIMEType (aliases MIME, mimetype), Keywords, Permissions, Contacts, Groups (comma/repeated), Latitude, Longitude, Altitude, Remarks, Tool, EXPIRATION, CreatorUid (alias creatorUid), MissionName, DownloadPath, PluginClassName`; unknown params logged and ignored (lenient deviation). `Groups` ⊄ caller groups → 403 (admin bypass). Body: `multipart/form-data` → part `assetfile` or `resource` (filename → `DownloadPath`, and `Name` if empty); otherwise raw body (`MIMEType` defaults to request `Content-Type`). `Content-Length` > limit → 400 `Uploaded file exceeds server's size limit of N MB!`; empty → 400 `HTTP request body has no content.`. `UID` default uuid; `SubmissionUser` forced to caller. 200 **`Content-Type: text/json`**, body = legacy Metadata **[CloudTAK]** (handles string or JSON). `GET/PUT` → 405. |
| GET | `/Marti/sync/search` | `CiQuery`: `Keywords, Tool, UID, Name, MIMEType, Filename, PrimaryKey, StartTime|SubmissionDateTime, StopTime, BBox, MinAltitude, MaxAltitude, Remarks, Permissions`; `Circle` → 400 unsupported; unknown ignored. Visibility filter. 200 `text/json` `{"resultCount":<int>,"results":[Metadata…]}` **[ATAK needs literal `resultCount`; CloudTAK parses string body]**. |
| GET/HEAD | `/Marti/sync/content?hash|uid&offset&length&receiver` | `hash` wins; param names case-insensitive; none → 404 HTML `File not found`. Headers: `api-version: 3`, `Content-Type: <MIMEType>`, `Content-Disposition: inline; filename="<urlencoded DownloadPath|Name>"`, `Content-Length`. `offset`/`length` (and standard `Range`) → 206 when partial. No gzip (decision; zips are incompressible and both clients accept identity). Streams from disk (`actix_files::NamedFile`-style body). HEAD → headers only. |
| POST | `/Marti/sync/missionupload?hash&filename&creatorUid&mimetype&keyword&tool&Groups|groups` | Must be multipart else 400 with TAK's exact message; part `assetfile`/`resource`; `filename` required (400); defaults `keyword=missionpackage`, `mimetype=application/x-zip-compressed`, `tool=public`; client `hash` ignored; **`Groups` and `groups` both accepted [CloudTAK]**. Dedupe: existing row with same content hash and same `filename` → reuse (200); else new row with `uid = hash`. Response 200 `text/plain` `https://<public_host>:<port>/Marti/sync/content?hash=<hash>` (D12). |
| GET | `/Marti/sync/missionquery?hash=` | row with `uid == hash` or `hash == hash` → 200 `text/plain` same URL; else 404 HTML. |
| DELETE/GET/POST | `/Marti/sync/delete?hash|PrimaryKey` | `Hash` wins (first value); `PrimaryKey` repeatable ints (400 if non-numeric); caller must be admin or submitter (403). 200 `text/html` `<html><head><title>Enterprise Sync Status</title></head><h1>Success</h1><p>Deleted N resource(s).</p></html>`. |
| PUT | `/Marti/api/sync/metadata/{hash}/{key}` | text body; `key` ∈ `tool|mimetype` (case-insensitive) else 400; 404 no row; ATAK sends `text/plain` `private|public`. 200 empty. |
| PUT | `/Marti/api/sync/metadata/{hash}/keywords` | body JSON `[String]`; 200/404. |
| PUT | `/Marti/api/sync/metadata/{hash}/expiration?expiration=` | required i64 (400); 200/404. |
| GET | `/Marti/api/sync/search` | Resource JSON variant (§4.6). |
| GET | `/Marti/api/files/metadata?page=-1&limit=-1&mission&missionPackage=false&name&sort&ascending=true` | `ApiResponse<[Map<String,String>]>` kind `FILES`: `{Name, User (submitter), Creator (creator_uid), Size ("12kB" humanised), Time (Java `Date.toString()` style `EEE MMM dd HH:mm:ss UTC yyyy`), MimeType, Keywords ("a,b"), Expiration (ISO without Z or "none"), Hash, Groups ("a,b")}` **[CloudTAK reads Hash, Groups, Time]**. Dispatch: `mission` → that mission's resources; `missionPackage=true` → keyword `missionpackage`; `name` → exact name. |
| GET | `/Marti/api/files/metadata/count?mission&missionPackage` | `ApiResponse<int>` kind `COUNT`. |
| GET/HEAD/DELETE | `/Marti/api/files/{hash}` | GET: bytes, `Content-Disposition: attachment; filename=<urlencoded Name>`; HEAD: `ApiResponse<Map>` kind `DATA` (same keys minus Groups, `Time` = padded date); DELETE: admin or submitter → 200 (else 403; TAK swallows). |
| PUT | `/Marti/api/files/{hash}/metadata?user&expiration&keywords*` | admin/submitter; updates submitter, expiration, keywords; 200. |

### 5.3 Manifest reader/writer (`files/package.rs`)

```rust
pub struct Manifest { pub version: u8 /*2*/, pub configuration: Vec<(String,String)>, pub contents: Vec<ContentEntry>, pub groups: Vec<String>, pub role: Option<RoleXml> }
pub struct ContentEntry { pub zip_entry: String, pub ignore: bool, pub params: Vec<(String,String)>,
                          /* attributes when writing mission archives: keywords,mimeType,name,submitter,uid,creatorUid,size,tool,latitude,longitude,altitude,submissionTime,filename */ }
pub fn read_manifest(zip: &mut ZipArchive<impl Read+Seek>) -> Result<(Manifest, String /*prefix dir*/)>;   // finds */MANIFEST/manifest.xml, entries relative to prefix
pub fn write_manifest(m: &Manifest) -> String;   // <MissionPackageManifest version="2"><Configuration><Parameter name value/>…</Configuration><Contents><Content ignore zipEntry …><Parameter/>…</Content></Contents>[<Groups><Group name/></Groups>][<Role name><Permission name/></Role>]</MissionPackageManifest>, escaped
```
Reader tolerance per 07 §6.6: `Configuration` requires `name`+`uid` (we still accept missing `uid` and mint one); `Content` valid iff `zipEntry` non-empty.

## 6. Device profiles

### 6.1 Model (`profiles/model.rs`)

```sql
profiles(id INTEGER PK, name TEXT UNIQUE, active INTEGER DEFAULT 1, apply_on_enrollment INTEGER DEFAULT 0, apply_on_connect INTEGER DEFAULT 0,
         tool TEXT, groups TEXT /*JSON [] = everyone*/, type TEXT, updated INTEGER NOT NULL);
profile_files(id INTEGER PK, profile_id, name TEXT, content_hash TEXT /*→ ContentStore*/ , size INTEGER, updated INTEGER, UNIQUE(profile_id,name));
profile_prefs(profile_id, key TEXT, class TEXT /*String|Boolean|Integer|Long|Float*/, value TEXT, PRIMARY KEY(profile_id,key));  -- rendered as <profile.name>.pref at build time
```
Files delivered = `profile_files` + generated `<name>.pref` (if any prefs) + `install_on_enrollment` data packages (resources flag, enrollment only) + the server default `rustak-enrollment.pref`.

### 6.2 `.pref` writer (`profiles/prefs.rs`)

```rust
pub enum PrefValue { Str(String), Bool(bool), Int(i32), Long(i64), Float(f32) }   // class="class java.lang.String|Boolean|Integer|Long|Float"
pub struct PrefGroup { pub name: String, pub version: u8 /*1*/, pub entries: Vec<(String, PrefValue)> }  // Vec ⇒ deterministic order
pub fn render(groups: &[PrefGroup]) -> String;
// "<?xml version='1.0' standalone='yes'?><preferences><preference version=\"1\" name=\"…\"><entry key=\"…\" class=\"class java.lang.String\">…</entry>…</preference></preferences>"
pub const APP_PREFS: &str = "com.atakmap.app.civ_preferences";  pub const LEGACY_APP_PREFS: &str = "com.atakmap.app_preferences";  pub const COT_STREAMS: &str = "cot_streams";
pub fn enrollment_defaults(host: &str, user: Option<&UserSettings /*callsign, team, role*/>) -> PrefGroup;
//  deviceProfileEnableOnConnect=true, displayServerConnectionWidget=true, prefs_enable_channels=true, prefs_enable_channels_host-<host>=true (String "true"),
//  optional locationCallsign / locationTeam / atakRoleType
```

### 6.3 Zip builder (`profiles/builder.rs`) and manual config package (`profiles/config_package.rs`)

`build_profile_package(name: &str /*Enrollment|Connection|<tool>|<profile>*/, files: &[ProfileFileData]) -> Vec<u8>`: entries `file0/`, `file0/<name>`, `file1/…`, `MANIFEST/`, `MANIFEST/manifest.xml` with Parameters `uid=<uuid>`, `name`, `onReceiveImport=true`, `onReceiveDelete=true` and one `<Content ignore="false" zipEntry="fileN/<name>"/>` per file. `build_multifile_package(files_with_relpaths)` for the `/file` endpoint (`name=multiFile`).

```rust
pub struct ConfigPackageInput { pub host: String, pub stream_port: u16, pub description: String, pub username: String,
    pub truststore_p12: Vec<u8>, pub truststore_password: String, pub client_p12: Option<(Vec<u8>, String)> /* None ⇒ enrol variant */, pub app_prefs_group: &'static str }
pub fn build_wintak_atak(i: &ConfigPackageInput) -> (String /*<user>_CONFIG.zip*/, Vec<u8>);   // outer zip: MANIFEST(uid,name "<user>_CONFIG") + <folder>/<user>.zip
                                                                                              // inner zip: MANIFEST(uid,name,onReceiveDelete=true) + <folder>/<user>.pref + <folder>/truststore.p12 [+ <folder>/<user>.p12]
pub fn build_itak(i: &ConfigPackageInput) -> (String /*<user>_CONFIG_iTAK.zip*/, Vec<u8>);      // flat: config.pref + truststore.p12 [+ <user>.p12], no manifest
```
`cot_streams` entries: `count=1 (Integer)`, `description0`, `enabled0=true`, `connectString0=<host>:<port>:ssl`, `caLocation0=cert/truststore.p12`, `caPassword0`, `certificateLocation0=cert/<user>.p12`, `clientPassword0`, `enrollForCertificateWithTrust0=false`, `useAuth0=false`; enrol variant: no `certificateLocation0/clientPassword0`, `enrollForCertificateWithTrust0=true`, `useAuth0=true` (user enters username/device-password in ATAK; creds are not read from `.pref`, 07 §2.5). iTAK variant uses unsuffixed `caLocation/caPassword/certificateLocation/clientPassword` under the app group. Plus the app group with the enrollment defaults. `<folder>` = a fixed 32-hex constant (any folder works; `.p12` is re-homed to `cert/` by ATAK's sorter).

### 6.4 Endpoints (`profiles.rs`)

| Verb | Path | Listener/auth | Behaviour |
|---|---|---|---|
| GET | `/Marti/api/tls/profile/enrollment?clientUid=` | public (Basic/Bearer) | `clientUid` required (400). Files = default enrollment pref (unless disabled) + active `apply_on_enrollment` profiles whose `groups` are empty or intersect the user's groups + `install_on_enrollment` packages. Empty → **204**; else 200 `application/zip`, `Content-Disposition: attachment; filename=profile.zip`. |
| GET | `/Marti/api/device/profile/connection?syncSecago&clientUid` | mTLS | both required (400). Active `apply_on_connect` profiles, group-matched, `updated > now - syncSecago` (`-1` ⇒ all). 204 or zip; `Last-Modified` = newest `updated` (RFC 1123). |
| GET | `/Marti/api/device/profile/tool/{tool}?syncSecago=-1&clientUid` | mTLS | same for profiles with `tool == {tool}`. |
| GET | `/Marti/api/tls/profile/tool/{tool}/file`, `/Marti/api/device/profile/tool/{tool}/file?relativePath*&syncSecago&clientUid` | public / mTLS | `relativePath` repeatable (leading `/` stripped, path traversal rejected 400); match against `profile_files.name` (exact or directory prefix). No tool profiles → 404; no files → 404; `If-Modified-Since` drops files not newer → all dropped → **304**; 1 file → raw bytes, `Content-Type` guessed from extension (`application/octet-stream` fallback), `Content-Disposition: attachment; filename=<name>`; >1 → `application/zip` multiFile package. Always `Last-Modified`. |
| HEAD/GET | `/Marti/api/device/profile/{name}/missionpackage` | mTLS | HEAD 200 empty; GET zip of that profile. |
| admin | `/Marti/api/device/profile*` (§10.5 of 06) | admin | thin wrappers over `ProfileService` (list/get/create/update/delete/files/file upload/download/delete; `directories*` → 501). `Profile` JSON `{id,name,active,applyOnEnrollment,applyOnConnect,type,updated,tool,groups}`. Optional in M3. |

## 7. CoT query (`cot.rs`)

`cot_store` API needed:
```rust
#[async_trait] pub trait CotStore: Send + Sync {
  async fn latest(&self, uid: &str) -> Option<StoredEvent>;
  async fn latest_many(&self, uids: &[String]) -> Vec<StoredEvent>;
  async fn latest_all(&self, filter: LatestFilter /*readable groups, since, type prefix*/) -> Vec<StoredEvent>;
  async fn history(&self, uid: &str, w: &TimeWindow) -> Vec<StoredEvent>;
  async fn sa_query(&self, w: &TimeWindow, bbox: Option<BBox>, sa_only: bool, readable: &GroupFilter) -> Vec<StoredEvent>;
  async fn match_uid(&self, needle: &str) -> Vec<String>;
  async fn store(&self, ev: &Event, groups: &[String], archive: bool);   // used by mission import
}
pub struct StoredEvent { pub uid: String, pub kind: String, pub xml: String, pub time: DateTime<Utc>, pub stale: DateTime<Utc>, pub lat: f64, pub lon: f64,
                         pub callsign: Option<String>, pub team: Option<String>, pub role: Option<String>, pub username: Option<String>, pub groups: Vec<String> }
```
Tables: `cot_latest(uid PK, type, xml, time, stale, lat, lon, callsign, team, role, username, groups, updated)`, `cot_history(id PK, uid, type, xml, time, lat, lon, groups)` with `INDEX(uid,time)`, `INDEX(time)`; retention job prunes history (`retention.cot_history_days`).

| Verb | Path | Behaviour |
|---|---|---|
| GET | `/Marti/api/cot/xml/{uid}` | latest, readable; `<marti>` element removed; body = `XML_HEADER + <event…>`; `application/xml`; 404 **empty body** if none. |
| GET | `/Marti/api/cot/xml/{uid}/all?secago&start&end` | history in window → `<events>` doc; 404 empty when zero. ATAK sends `Accept: text/xml` + gzip (actix `Compress` middleware may gzip; fine). |
| GET/POST | `/Marti/api/cot` | body JSON `[uid]` (GET body accepted); empty → 400; `<events>` of latest. |
| GET | `/Marti/api/cot/sa?start&end&left&bottom&right&top&isFiltered=true` | `start`,`end` required; `start>end` or window > 24 h → 400; all four bbox or none; SA types (`a-*`) only when `isFiltered`; 404 empty when none. |
| GET | `/Marti/api/cot/matchUid?search=` | bare JSON array of uids (substring, readable). |

## 8. Admin `/api/v1`, DTOs, Yew

### 8.1 Endpoints (`web/api/{missions,packages,profiles,groups,clients,cot,config_packages}.rs`, all behind the `/api/v1` bearer/session middleware; `Administrative` extractor lifted from automate `api/scope.rs`)

| Area | Endpoints |
|---|---|
| Missions | `GET /missions` (all incl. invite-only/password, with subscriber count, change count, groups) · `GET /missions/{guid}` (detail: mission + subscriptions with roles + layers) · `DELETE /missions/{guid}?deep` · `GET /missions/{guid}/changes?squashed` · `PUT /missions/{guid}/subscriptions/{clientUid}/role {role}` · `DELETE /missions/{guid}/subscriptions/{clientUid}` · `GET /missions/{guid}/archive` |
| Packages | `GET /packages?missionPackage&q` (Resource + `install_on_enrollment`) · `POST /packages` multipart (`file`, `name`, `keywords[]`, `groups[]`, `tool`) · `GET /packages/{hash}` · `PATCH /packages/{hash} {groups?, tool?, keywords?, install_on_enrollment?, expiration?, name?}` · `DELETE /packages/{hash}` · `GET /packages/{hash}/content` |
| Profiles | `GET/POST /profiles` · `GET/PATCH/DELETE /profiles/{id}` · `GET /profiles/{id}/files` · `POST /profiles/{id}/files` (multipart, `filename`) · `GET/DELETE /profiles/{id}/files/{fileId}` · `GET/PUT /profiles/{id}/prefs` (`[PrefEntry]`) · `GET /profiles/{id}/preview` (zip a device would receive) · `GET /profiles/map-sources` (built-in catalogue of map source XML templates) · `POST /profiles/{id}/map-sources {id}` (adds `<id>.xml` file) · `GET /profiles/pref-catalog` (known ATAK pref keys with classes/descriptions for the editor) |
| Groups | `GET/POST /groups` · `PATCH/DELETE /groups/{id}` · `GET /groups/{id}/members` · `PUT /groups/{id}/members [{username, direction, active}]` · `DELETE /groups/{id}/members?username&direction` |
| Clients | `GET /clients` (LiveState → `ConnectedClient`) · `DELETE /clients/{uid}` (disconnect) · `POST /clients/{uid}/incognito {on}` · `GET /clients/history?secago` (clientEndPoints-style, incl. disconnected) |
| CoT | `GET /cot?type&callsign&group&page` (latest per uid summaries) · `GET /cot/{uid}` (latest XML + parsed summary) · `GET /cot/{uid}/history?secago` · `DELETE /cot/{uid}` |
| Config packages | `POST /config-packages {username, credential_id?, variant: "wintak_atak" \| "itak", include_client_cert: bool}` → zip download (uses `pki/p12.rs` + §6.3). (Identity design owns credential minting; this just consumes it.) |
| Settings | `GET/PUT /settings/files {upload_size_limit_mb}` · `GET /settings/marti {public_host, allow_all_origins}` |

### 8.2 DTOs (`rustak-api/src/`)

`mission.rs`: `MissionSummary {guid, name, description, tool, creator_uid, create_time, groups, subscriber_count, uid_count, content_count, password_protected, invite_only, default_role: MissionRoleKind, expiration}`, `MissionDetail {summary, subscriptions: Vec<MissionSubscriptionDto {client_uid, username, role, create_time}>, layers: Vec<MissionLayer

Dto>, keywords}`, `MissionChangeDto {kind, content_uid, content_hash, timestamp, creator_uid, details: Option<UidDetailsDto>}`, `MissionRoleKind` enum (`MISSION_OWNER|MISSION_SUBSCRIBER|MISSION_READONLY_SUBSCRIBER`, serde as-is).
`package.rs`: `PackageSummary {hash, uid, name, filename, mime_type, size, submitter, creator_uid, submission_time, keywords, groups, tool, expiration, install_on_enrollment, mission_name}`, `PackageUpdate {…Option fields…}`.
`profile.rs`: `Profile {id, name, active, apply_on_enrollment, apply_on_connect, tool, groups, updated, file_count, pref_count}`, `ProfileCreate/ProfileUpdate`, `ProfileFile {id, name, size, updated}`, `PrefEntry {key, class: PrefClass, value}`, `PrefClass` enum (`String|Boolean|Integer|Long|Float`), `PrefCatalogEntry {key, class, description, default}`, `MapSource {id, name, description}`.
`group.rs`: `Group {id, name, bitpos, created, member_count}`, `GroupMember {username, direction: Direction, active}`, `Direction` (`IN|OUT`).
`client.rs`: `ConnectedClient {client_uid, callsign, username, team, role, takv, protocol, ip, connected_at, last_event_at, incognito, in_groups, out_groups}`.
`cot.rs`: `CotSummary {uid, kind, callsign, team, role, time, stale, lat, lon, groups}`, `CotDetail {summary, xml}`.
`config_package.rs`: `ConfigPackageRequest {username, credential_id, variant: ConfigPackageVariant, include_client_cert}`.

All wasm-safe (serde/chrono/uuid only), each file < 300 lines with round-trip tests as in automate `api/src/connection.rs`.

### 8.3 Yew (`rustak-ui/src/`)

Pages: `pages/missions.rs` (table: name, tool, groups, subscribers, changes; row → `pages/mission_detail.rs`: tabs Overview / Subscribers (role dropdown, remove) / Changes (squashed toggle) / Layers tree; Delete with confirm; Download archive), `pages/packages.rs` (upload drop zone, list with groups chips, tool, `install on enrollment` toggle, delete, download; inline groups editor), `pages/profiles.rs` (list + create; row → `pages/profile_editor.rs`: flags, groups picker, tool; `PrefsEditor` (typed rows with class select, catalog autocomplete), `ProfileFiles` (upload/delete), `MapSourcePicker`, `Preview` download), `pages/groups.rs` (create group, bitpos shown read-only, members table with IN/OUT/active toggles, add member), `pages/clients.rs` (live list, auto-refresh 5 s via `RefreshButton`/interval, disconnect, incognito toggle), `pages/cot_browser.rs` (latest per uid with filters, detail drawer with `XmlView` + history list).

Shared components: `components/groups_picker.rs` (multi-select chips), `components/role_badge.rs`, `components/file_drop.rs` (multipart via `gloo_net` `FormData`), `components/xml_view.rs` (escaped `<pre>` with light highlighting), `components/prefs_editor.rs`, `components/confirm.rs`. Reused from automate: `admin_shell`, `alert`, `status_pill`, `page_title`, `refresh_button`, `filter_input`, `json_highlight`, `form`, `entity`, `api.rs` client pattern (bearer + demo fixtures macro), `fixtures/` demo data for each new page.

## 9. Test plan

Location: `rustak-server/tests/marti/` (in-process app via `rustak_server::testing::TestServer` with test CA, `TestIdentityProvider`, fake EUD from `rustak-client`); golden files under `rustak-server/tests/golden/marti/`; node-tak TypeBox types transcribed to JSON Schema under `rustak-server/tests/schemas/node-tak/*.schema.json` (Mission, MissionChange, MissionSubscriber, MissionInvite, MissionLayer, MissionLog, Group, Contact, ClientEndpoint, Package, Content, Config, TAKItem/TAKList) validated with the `jsonschema` crate.

| Suite | File | Asserts |
|---|---|---|
| Content-Type exactness | `content_type.rs` | Iterates a static route table (method, path, fixture setup); every JSON route's `content-type` == `application/json`; legacy routes == `text/json`/`text/plain`; no 3xx anywhere (incl. trailing-slash and missing-method probes). |
| Envelope + type strings | `envelope.rs` | Each endpoint's `type` matches §1.2; `nodeId` present; `version` == "3". |
| Golden shapes | `golden_*.rs` | Byte-exact (after canonical key ordering) JSON for Mission (create 201 with token/ownerRole; get 200 without), MissionChange, Resource, legacy Metadata, Group, ClientEndpoint, MissionSubscription (API_VERSION 2 vs 3), MissionInvitation, LogEntry, MissionLayer (`mission_layers`), ServerConfig, `/files/metadata` entry; XML goldens for `<events>`, `/cot/xml/{uid}` (marti stripped), `t-x-m-c`/`-n`/`-d`/`-i`/`-r`/`-h`/`-l`, `t-x-g-c`, profile `manifest.xml`, `.pref`, mission archive manifest. |
| Schema oracle | `node_tak_schema.rs` | Every CloudTAK-critical response validates against the transcribed TypeBox schema. |
| Errors | `errors.rs` | `{status,code,message}` mapping table; 404 vs 410 for deleted missions; API_VERSION ≤2 → 403 vs ≥3 → 200-cleared. |
| Groups | `groups.rs` | IN+OUT emitted with bitpos/created/type; PUT active with and without clientUid (assert `t-x-g-c` seen by other fake EUD, not by the initiator when clientUid given); latest-SA replay on `sendLatestSA=true`; ATAK-style body with numeric `created` accepted. |
| Contacts/endpoints | `contacts.rs` | bare array; `notes: ""`; `lastStatus` values; `group` 403; secAgo filter; cache headers. |
| Mission flows | `missions_flow.rs` | create(PUT and POST; comma and repeated `group`; JSON body override; package body import) → list filters (tool default, passwordProtected, defaultRole, invite-only hidden) → get by name and guid → subscribe (201 + token; password-protected paths incl. the "No password provided" 403) → contents attach/detach → changes squashed vs full (property test vs naive model) → `/cot` → keywords → archive (unzip, manifest params order) → delete by name and `?guid=` → 410 → archived resource exists. |
| Tokens | `mission_tokens.rs` | issue/verify round-trip for the 3 kinds; wrong mission → None; tampered → None; `MissionAuthorization` precedence over `Authorization`; identity JWT in `Authorization` not mistaken for a mission token; admin bypass; invitation token accepted only on subscribe. |
| Invitations/roles/logs/layers | `missions_extras.rs` | invite by each type, `/missions/invitations?clientUid=` matching rules, auto-clear on subscribe, `t-x-m-i` delivered with token; set role → `t-x-m-r`; log 201/400 validation; layer tree + `mission_layers` key; layer delete unfiles items. |
| dest publish | `mission_dest.rs` | fake EUD subscribed via REST sends `<dest mission="X" path="<layer>" after=""/>`; other subscriber receives raw CoT and a `t-x-m-c`; sender receives neither; non-subscriber's dest is dropped; READONLY role dropped. |
| Enterprise sync | `sync.rs` | raw and multipart (`assetfile`, `resource`) upload; `MIME`/`name` aliases; oversize 400 message; `Groups` 403; `text/json` Metadata; search `resultCount` int + Title-case; `keywords=missionpackage&tool`; content GET/HEAD headers + 206 via `offset/length` and `Range`; missionupload (multipart required message, `filename` required, dedupe, URL body, `Groups`/`groups`); missionquery 200/404; delete via DELETE/GET/POST + HTML body; metadata PUT keys; `/files/metadata` map shape incl `Time` format. |
| Profiles | `profiles.rs` | enrollment 200 zip / 204 when disabled + no profiles; unzip → `file0/<name>`, `MANIFEST/manifest.xml` params; `.pref` byte-exact; connection `syncSecago` filtering; `/file`: 404, 304, single-file vs multiFile; config package variants (outer/inner zip, iTAK flat). |
| CoT query | `cot_query.rs` | 404 empty bodies; `<events>` framing; sa 24 h cap; matchUid bare array. |
| Admin API | `api_v1_*.rs` | CRUD per area; DTO round-trips; non-admin 403. |

CloudTAK smoke (`e2e/cloudtak/README.md` + `scripts/cloudtak-smoke.sh`, run manually / nightly): 1) start rustak with ACME-less BYO publicly-trusted cert on 8446 (or `NODE_EXTRA_CA_CERTS` in the CloudTAK container); 2) `PATCH /api/server` with admin cert → `/files/api/config` succeeds; 3) user login (`/oauth/token`, `tls/config`, `signClient/v2`) → profile cert stored; 4) `/Marti/api/version` probe on session check; 5) Channels page lists groups; toggle → `PUT /groups/active`; 6) Contacts page; 7) create Data Sync (`groups/all` force-activate → `POST /missions/{name}` → `token`+`guid` stored → `PUT /keywords`) → subscribe from the map → `/cot`, `/changes`, `/layers` (ETL layer creates `layer-<id>`) → `t-x-m-c` reaches the browser worker; 8) Data Packages page (`/Marti/sync/search`), upload (`missionupload` with `Groups`), channel column (`/files/metadata?missionPackage=true&name=`), delete; 9) mission attachment (`sync/upload` → `PUT /contents`); 10) delete Data Sync (`DELETE /missions?guid=`).

## 10. Ordered implementation steps and risks

**M2 — version / groups / contacts / misc** (after identity design lands `Principal` + auth middleware)
1. `marti/{mod,response,error,extract,headers,time}.rs` + `config/marti.rs`; unit tests for envelope, error mapping, `ApiVersion`, `CommaList`, `TimeWindow`, date formatters. Verify: `cargo test -p rustak-server marti::` green; `check-file-length.sh`.
2. `version.rs` + `stubs.rs`; mount on both listeners. Verify: contract test for `/version` text, `/version/config`, `/files/api/config`; `curl --cert` on 8443 and Basic on 8446.
3. `stream` traits (`LiveState`, `Notifier`, `MissionIngest`, `CotStore`) as crate-level traits with in-memory test doubles in `testing/`. Verify: compile + doubles used by later suites.
4. `groups.rs` + `db/repos/group_members` active flag. Verify: `groups.rs` suite; ATAK Channels UI shows and toggles channels; CloudTAK Channels page.
5. `contacts.rs`, `subscriptions.rs`. Verify: suite; ATAK `clientEndPoints` fetch on connect does not error (matcher + lastStatus).
6. `cot_store` (latest/history) + `cot.rs`. Verify: `cot_query.rs`; CloudTAK marker history.
7. M2 gate: CloudTAK smoke steps 1–6; ATAK QR enrolment + channels.

**M3 — files / profiles**
8. `files/store.rs` streaming ingest with `sha2`, `actix-multipart` streaming to tmp file, size limit from config wired into `PayloadConfig`/`MultipartFormConfig`. Verify: unit tests incl. oversize and dedupe.
9. `files/{metadata,legacy,search,package}.rs` + `resources` migration. Verify: golden Metadata/Resource, manifest reader with nested prefix.
10. `sync.rs`, `sync_metadata.rs`, `files.rs`. Verify: `sync.rs` suite; ATAK "Send to server" (missionquery→missionupload→metadata/tool) and ATAK download from `/Marti/sync/search`; CloudTAK packages page.
11. `profiles/{model,prefs,builder,service}.rs` + `profiles.rs`. Verify: `profiles.rs` suite; ATAK enrolment applies prefs (channels enabled, connection widget); connection profile 204/200/304.
12. `profiles/config_package.rs` + `/api/v1/config-packages`; manual import into ATAK, WinTAK, iTAK (manual gate).
13. Admin `/api/v1/{packages,profiles,groups,clients,cot}` + DTOs + Yew pages. Verify: `api_v1_*` suites, Playwright smoke per page with demo fixtures.

**M4 — missions**
14. Migrations + `db/repos/mission_*` + `missions/{model,roles}.rs` + `auth/mission_token.rs`. Verify: repo tests, token round-trips.
15. `missions/{service,subscriptions,changes,contents,keywords,notify}.rs` + `marti/missions/{mod,dto,crud,changes,subscription,contents}.rs`. Verify: golden Mission/MissionChange/MissionSubscription; `missions_flow.rs`; CloudTAK create/subscribe/attach.
16. `missions/cot.rs` (`MissionIngest`) wired into `stream/router.rs`; `stream/notify.rs` `t-x-m-*` builders. Verify: `mission_dest.rs`; CloudTAK ETL layer → Data Sync path; ATAK Data Sync plugin round-trip (manual).
17. `missions/{invitations,logs,layers,archive,import}.rs` + remaining route files (`invitations`, `logs`, `layers`, `misc`). Verify: `missions_extras.rs`; archive/import round-trip (archive → import into new mission → equal uids/hashes).
18. Admin `/api/v1/missions` + Yew pages; `jobs/mission_expiry.rs` (expiration + purge of soft-deleted after `missions.purge_after_days`).
19. M4 gate: CloudTAK smoke steps 7–10; ATAK Data Sync create/subscribe/add marker/file/log/delete.

**Risks and mitigations**

| Risk | Mitigation |
|---|---|
| actix emits `application/json; charset=utf-8` somewhere (extractor error paths, `web::Json` responder, third-party middleware) | Single `response::ok/bare_json` helper with explicit header; `JsonConfig/QueryConfig/PathConfig::error_handler` → `MartiError`; the route-table content-type test runs against the real app including middleware stack; forbid `web::Json` as a responder via clippy `disallowed_types`. |
| Multipart streaming to disk (400 MB packages) blocking or buffering in memory | `actix-multipart` field stream → `tokio::fs::File` in the content tmp dir with incremental sha256; `PayloadConfig` limit only for non-multipart bodies; integration test uploads a 50 MB synthetic file and asserts peak RSS is flat via `/proc/self/statm`-style check behind `#[ignore]`. |
| Squashed change semantics wrong (CloudTAK diff view, ATAK sync) | Rust fold with a documented rule set; proptest comparing fold output to a naive "replay full history and diff current state" model; golden for the canonical add→remove→add sequence. |
| `type` string inconsistencies across endpoints | Constants in one module; envelope test table; schema oracle for CloudTAK; ATAK matchers (`Group`, `ClientEndpoint`, `ServerConfig`) asserted by substring exactly as ATAK does. |
| ATAK date formats (`created` date-only, `lastEventTime`/`SubmissionDateTime` millis with literal `Z`) | `time.rs` with unit tests for each formatter; parse tests using the exact Java patterns transcribed as regexes; manual gate on a real ATAK for `groups/all` and `clientEndPoints`. |
| Route ordering (`/missions/all/...`, `/missions/logs/...`, `/missions/guid/...` vs `{name}`) | Explicit ordered registration in `missions/mod.rs` with a test that requests each literal path and asserts it is not treated as a mission name; reserved-name validation on create. |
| `Authorization: Bearer` ambiguity between identity JWT and mission token | D2 rule implemented in one function with tests for the four header combinations. |
| File-length constraint (< 300 functional lines) on `crud.rs`/`subscription.rs`/`sync.rs` | Route files contain only parameter parsing + service calls; parameter structs live in `dto.rs`; `scripts/check-file-length.sh` in CI; split `sync.rs` into `sync_upload.rs`/`sync_read.rs` if it grows. |
| Profile package accepted by ATAK but not WinTAK/iTAK | Empirically-accepted OTS layouts (double-wrapped `_CONFIG.zip`, flat iTAK zip) implemented as separate variants; `app_prefs_group` parameter allows switching to the legacy alias without code changes; manual gate documented in `docs/compat/profiles.md`. |
| `missionupload` URL unreachable by peers (Docker/NAT) | `marti.public_host` config; startup warning when unset and bound to a non-public address; smoke test checks ATAK→ATAK package share via server. |

### Critical Files for Implementation
- /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/marti/response.rs — envelope, `type` constants, exact Content-Type helpers (everything else depends on it)
- /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/marti/missions/mod.rs — route ordering, `MissionCtx` extractor, name/guid resolution
- /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/missions/service.rs — mission create/update/delete/permission logic and notification fan-out
- /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/auth/mission_token.rs — HS256 mission tokens, header precedence, role-from-token
- /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/marti/sync.rs — legacy Enterprise Sync servlets (upload/search/content/missionupload) that ATAK and CloudTAK both hit

Reference code to lift patterns from: /Users/bpannell/dev/gh/SierraSoftworks/automate/agent/src/web/api/mod.rs (scope + middleware), /Users/bpannell/dev/gh/SierraSoftworks/automate/agent/src/web/api/scope.rs (`Administrative` extractor), /Users/bpannell/dev/gh/SierraSoftworks/automate/agent/src/web/helpers/request.rs (`base_url`/trust-proxy), /Users/bpannell/dev/gh/SierraSoftworks/automate/api/src/connection.rs (DTO + round-trip tests), /Users/bpannell/dev/gh/SierraSoftworks/automate/ui/src/pages/connections.rs and /Users/bpannell/dev/gh/SierraSoftworks/automate/ui/src/api.rs (Yew page + API client with demo fixtures).
