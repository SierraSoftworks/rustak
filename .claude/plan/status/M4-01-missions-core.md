# M4-01 — Data Sync core: mission model, roles, tokens, CRUD, subscriptions, contents, changes

**Status: complete.** The mission API is served, the three node-tak mission scenarios pass, and
every exit check that does not depend on M4-02's in-flight files is green (see *Exit checks*).

## What landed

| Area | Files |
|---|---|
| Schema | `rustak-server/migrations/0010_mission_extras.sql` |
| Repositories | `rustak-server/src/db/repos/missions/{mod,row,changes,contents,subscriptions}.rs` (+ 6 lines in `db/repos/mod.rs`) |
| Tokens | `rustak-server/src/auth/mission_token.rs` (+ 2 lines in `auth/mod.rs`) |
| Domain | `rustak-server/src/missions/{mod,model,roles,service,crud,render,contents,import,changes,subscriptions,keywords,dto}.rs` |
| Routes | `rustak-server/src/marti/missions/{mod,crud,misc,contents,changes,subscription}.rs` (+ 2 lines in `marti/mod.rs`, + 3 `PATHS` entries) |
| Tests | `rustak-server/tests/{missions_flow.rs,mission_squash.rs,schema/mod.rs,fixtures/*}` (+ 1 dev-dependency) |
| Docs | `.claude/plan/compat/missions.md` §16 |

## The `MissionService` surface M4-02 codes against

`crate::missions::MissionService` — `MissionService::new(context: AppContext)`, cheap to clone,
built per request by `marti::missions::MissionCtx`. Everything is `async` and returns
`Result<_, MartiError>` unless noted. `service.context()` hands back the `AppContext` for the
stream-side handles.

### Resolution and roles (`service.rs`, `roles.rs`)

```rust
pub async fn resolve(&self, r: &MissionRef) -> Result<Mission, MartiError>;   // NotFound | Gone(410)
pub async fn by_name(&self, name: &str) -> Result<Option<Mission>, MartiError>;  // deleted rows included
pub async fn by_id(&self, id: i64) -> Result<Option<Mission>, MartiError>;
pub async fn viewer(&self, who: &MartiPrincipal) -> Result<Viewer, MartiError>;
pub async fn role_for_request(&self, m: &Mission, who: &MartiPrincipal, claims: Option<&MissionClaims>)
    -> Result<Option<MissionRole>, MartiError>;
pub async fn role_from_token(&self, m: &Mission, allowed: &[TokenType], who: &MartiPrincipal,
                             claims: Option<&MissionClaims>) -> Result<Option<MissionRole>, MartiError>;
pub async fn role_of(&self, m: &Mission, client_uid: &str) -> Result<Option<Role>, MartiError>;
```

`crate::missions::roles`: `Role { Owner, Subscriber, ReadonlySubscriber }` (`as_str`, `parse`,
`permissions`, `allows`), `Permission` (the eight `MISSION_*`),
`MissionRole { kind, using_default }` with `granted`/`default_role` constructors, and
`require(Option<MissionRole>, Permission) -> Result<MissionRole, MartiError>` (403 either way).

Resolution order: mission token → administrator → **the caller's own subscription** (by device uid
or account name, most permissive when there are several) → the mission's default role, which a
password-protected or invite-only mission does not have.

### CRUD (`crud.rs`, `service.rs`)

```rust
pub async fn list(&self, who: &MartiPrincipal, f: ListFilter) -> Result<Vec<Mission>, MartiError>;
pub async fn create_or_update(&self, who: &MartiPrincipal, claims: Option<&MissionClaims>,
                              name: &str, p: MissionParams) -> Result<Outcome, MartiError>;
pub async fn delete(&self, m: &Mission, creator_uid: Option<&str>, deep: bool)
    -> Result<Mission, MartiError>;
pub async fn copy(&self, who: &MartiPrincipal, m: &Mission, p: CopyParams) -> Result<Mission, MartiError>;
pub async fn children(&self, m: &Mission) -> Result<Vec<Mission>, MartiError>;
pub async fn set_parent(&self, child: &Mission, parent: Option<&Mission>) -> Result<Mission, MartiError>;
```

`Outcome::Created { mission, token, owner_role } | Outcome::Updated(mission)`, with
`outcome.mission()` and `outcome.is_created()`. `ListFilter::from_query(&CiQuery, paged: bool)` and
`MissionParams::from_query(&CiQuery)` / `apply_body(MissionBody)` are the parameter parsers.

### Contents, import, keywords, changes

```rust
pub async fn render(&self, m: &Mission, extras: Render) -> Result<MissionJson, MartiError>;
pub async fn add_content(&self, m: &Mission, body: &MissionContentBody, creator_uid: Option<&str>,
                         at: DateTime<Utc>) -> Result<Vec<MissionChangeRow>, MartiError>;
pub async fn remove_content(&self, m: &Mission, hash: Option<&str>, uid: Option<&str>,
                            creator_uid: Option<&str>) -> Result<Vec<MissionChangeRow>, MartiError>;
pub async fn import_package(&self, m: &Mission, zip: Vec<u8>, creator_uid: Option<&str>)
    -> Result<Vec<MissionChangeRow>, MartiError>;
pub async fn set_keywords(&self, m: &Mission, target: KeywordTarget, kws: Vec<String>,
                          creator_uid: Option<&str>) -> Result<Mission, MartiError>;
pub async fn remove_keyword(&self, m: &Mission, keyword: &str, creator_uid: Option<&str>)
    -> Result<Mission, MartiError>;
pub async fn changes(&self, m: &Mission, w: TimeWindow, squashed: bool)
    -> Result<Vec<MissionChangeJson>, MartiError>;
pub async fn changes_since(&self, m: &Mission, since: DateTime<Utc>)
    -> Result<Vec<MissionChangeJson>, MartiError>;
pub async fn render_changes(&self, m: &Mission, rows: Vec<MissionChangeRow>)
    -> Result<Vec<MissionChangeJson>, MartiError>;
pub async fn presence(&self, m: &Mission) -> Result<changes::Presence, MartiError>;
pub async fn cot_events_xml(&self, m: &Mission, path: Option<&str>) -> Result<String, MartiError>;
```

`Render { token, owner_role, changes, logs, stripped }` — everything a mission payload carries
beyond the mission. `KeywordTarget::{Mission, Uid(String), Hash(String)}`.
`changes::squash(Vec<MissionChangeRow>, &Presence) -> Vec<MissionChangeRow>` and
`changes::without_marti(&str) -> String` are free functions M4-02 may reuse.
`contents::details_of(&Event) -> UidDetailsJson` caches an item's rendering fields.

### Subscriptions and tokens

```rust
pub async fn subscribe(&self, m: &Mission, req: SubscribeReq) -> Result<MissionSubscription, MartiError>;
pub async fn unsubscribe(&self, m: &Mission, client_uid: &str) -> Result<bool, MartiError>;
pub async fn subscription(&self, m: &Mission, client_uid: &str)
    -> Result<Option<MissionSubscription>, MartiError>;
pub async fn subscriptions(&self, m: &Mission) -> Result<Vec<MissionSubscription>, MartiError>;
pub async fn subscribers_for(&self, m: &Mission, except: Option<&str>)
    -> Result<Vec<String>, MartiError>;          // connected client uids — M4-02's notice recipients
pub async fn all_subscriptions(&self, by_guid: bool)
    -> Result<Vec<BTreeMap<String, String>>, MartiError>;
pub async fn set_role(&self, m: &Mission, client_uid: Option<&str>, username: Option<&str>,
                      role: Role) -> Result<usize, MartiError>;
pub async fn access_token(&self, m: &Mission, password: &str) -> Result<String, MartiError>;
pub async fn set_password(&self, m: &Mission, pw: Option<&str>) -> Result<(), MartiError>;
pub async fn set_expiration(&self, m: &Mission, expiration: Option<i64>) -> Result<(), MartiError>;
pub async fn tokens(&self) -> Result<MissionTokens, MartiError>;
```

`SubscribeReq { client_uid, username, password, token_role: Option<MissionRole>, invited_role:
Option<Role> }` — **M4-02 fills `invited_role`** from a standing invitation; the service already
threads it through and refuses an invite-only mission when both are `None`.

`MissionSubscription { client_uid, username, role, create_time, token }`.

### Mission tokens (`crate::auth::mission_token`)

```rust
pub enum TokenType { Subscription, Invitation, Access }   // as_str/parse give the wire spelling
pub struct MissionClaims { jti, iat, exp: Option<i64>, iss, kind, id, mission_name, mission_guid }
impl MissionTokens {
    pub async fn load(services: &impl Services) -> Result<Self, Error>;
    pub fn issue(&self, id: &str, kind: TokenType, mission_name: &str, mission_guid: Uuid,
                 ttl: Option<Duration>) -> Result<String, Error>;
    pub fn verify(&self, token: &str) -> Result<MissionClaims, TokenError>;
}
pub fn mission_bearer(request: &HttpRequest, identity_used_authorization: bool) -> Option<String>;
pub fn identity_used_authorization(who: &MartiPrincipal) -> bool;
```

HS256 over a dedicated 32-byte secret, generated once and sealed with
`SecretContext::MissionTokenKey { kid: "mission-token-hmac" }` in the key/value store under
partition `missions`. `MissionAuthorization` is read first, then `Authorization` only when identity
resolution did not consume it (design 04 D2). A token that does not verify is *absent*, never a
refusal.

### Wire DTOs (`crate::missions::dto`)

`MissionJson`, `MissionAddJson<T>`, `MissionRoleJson`, `MissionChangeJson`, `UidDetailsJson`,
`LocationJson`, `MissionSubscriptionJson`, `MissionContentBody` (with `flatten()` →
`Vec<(Option<layer>, Filed)>`), `MissionBody`. `crate::missions::role_json(Role)` and
`subscription_json(&MissionSubscription, Option<MissionJson>, include_token)` render them.

## What M4-02 needs to know about the routes

`marti/missions/mod.rs` registers, in this order:

1. `reserved_routes` — `/missions/all/invitations`, `/missions/all/logs`,
   `/missions/logs/entries`, `/missions/logs/entries/{id}`, `/missions/invitations` on all four
   methods, answering `501` through `marti::missions::reserved`.
2. `/missions/all/subscriptions/guid`, `/missions/all/subscriptions`.
3. `/pagedmissions`, `/missioncount`, `GET /missions`, `DELETE /missions?guid=`.
4. `family(config, "/missions/guid/{guid}", false)` then `family(config, "/missions/{name}", true)`
   — the GUID family **first**, and the by-name family carries the five routes TAK has no GUID
   spelling for (keywords, archive, package import, copy, token) plus `parent/{parent}`.
5. The bare `/missions/guid/{guid}` and `/missions/{name}` verbs last.

Inside `family`, `{n}/invitations`, `{n}/invite[/{kind}/{invitee}]`, `{n}/log`, `{n}/layers`,
`{n}/maplayers`, `{n}/externaldata` and `{n}/feed` are already mounted in the right order pointing
at `reserved`. **M4-02 replaces the bodies of `reserved_routes` and those `family` lines** with its
own modules' handlers; no reordering is needed and the ordering test in that file covers it.

`MissionCtx` is the extractor every handler takes: it carries `service`, `who: MartiPrincipal` and
`claims: Option<MissionClaims>` (resolved once per request, because reading the sealed secret is
not free).

Two seams M4-02 should take over in files M4-01 owns:

* `marti::missions::contents::archive` — currently `501` with a `TODO(M4-02)`; replace its body
  with a call into `missions/archive.rs`.
* `marti::missions::misc::send` — validates `contacts` and answers the mission; the invitation
  fan-out is the `TODO(M4-02)` inside it.

Invitation tokens are resolved by `mission_invitations.token_jti`, so store the token's `jti` there
as well as the whole JWT in the `token` column migration `0010` adds. `role_from_token`'s
`TokenType::Invitation` arm is the one-line change that turns it on.

`TODO(M4-02)` markers naming the notice each place needs: `missions/crud.rs` (`t-x-m-n`,
`t-x-m-c-m`, `t-x-m-d`, and the archive-to-resource step of a delete), `missions/contents.rs`
(`t-x-m-c` on add and remove), `missions/import.rs` (`t-x-m-c` per imported entry),
`missions/keywords.rs` (`t-x-m-c-k`, `-k-u`, `-k-c`), `missions/subscriptions.rs` (`t-x-m-r`,
`t-x-m-c-m` on a password change, and the invitation lookup and auto-clear on subscribe), and
`missions/render.rs` (fill `externalData`, `mapLayers` and `feeds` from the three tables
migration `0010` adds — M4-02 did this while this brief was finishing; they are emitted even when
empty because CloudTAK's schema makes all three non-optional).

## Deviations from the brief

1. **`GET {n}/archive` was handed over rather than implemented.** The route, its ordering and its
   `MISSION_READ` check are M4-01's; the zip builder is `missions/archive.rs`, which the brief
   assigns to M4-02 — and which M4-02 filled in while this brief was finishing, so the route now
   answers a real `application/zip`. `missions_flow` accepts either answer so that the two agents'
   landing order cannot make it flap. The delete path's archive-to-resource step is likewise
   M4-02's; soft delete, the `DELETE_MISSION` change row and the `410` afterwards all work today.
2. **`POST {n}/send` does not invite.** It validates `contacts` and answers the mission; the
   invitation fan-out is M4-02's.
3. **Mission passwords are argon2id**, not TAK's bcrypt — documented in `missions/mod.rs` and in
   `compat/missions.md` §16. The hash never leaves the server, so there is no parity to keep.
4. **Extra files in `missions/`.** The brief named
   `{model,roles,service,contents,changes,subscriptions,keywords,dto}`; `crud.rs`, `render.rs` and
   `import.rs` were split out of `service.rs` and `contents.rs` to stay under 300 functional lines.
   All three are in design 04 §1's own module list (`import.rs` verbatim). Role *resolution* lives
   in `roles.rs` rather than `subscriptions.rs` for the same reason.
5. **`db/repos/missions/` is a directory**, matching `db/repos/resources/`, rather than a flat
   `missions*.rs`. M4-02's log, layer and invitation repositories can sit beside it.
6. **A caller's own subscription grants its role without a token** (see `compat/missions.md` §16).
   Without it, a client that creates a mission over an ordinary session cannot delete it again,
   which is exactly what node-tak's `missions.test.ts` does.
7. **One extra dev-dependency**: `proptest` on `rustak-server`, for the squash property test.
8. **No new nested scope.** The brief asks that every nested scope carry a `.default_service()`.
   The mission routes are registered directly into the existing `/Marti/api` scope, which already
   has `.default_service(web::to(unmatched))`, so there is no new scope that could inherit the
   single-page-application catch-all.
9. **`marti/mod.rs` took two lines, not one** — the `pub mod missions;` declaration and the
   `.configure(missions::routes)` call — plus three entries in the `PATHS` table so that a wrong
   method on `/Marti/api/missions` answers `405` rather than `404`.

## Exit checks

All run at the end of the brief, with M4-02 working concurrently in the same crate.

```
$ cargo fmt --all --check
(clean)

$ ./scripts/check-file-length.sh
(clean — the longest file M4-01 owns is missions/model.rs at 242 functional lines)

$ RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps
(clean)

$ cargo test -p rustak-server --features testing --lib
test result: ok. 1353 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 16.50s

$ cargo test -p rustak-server --features testing --test missions_flow
running 14 tests
test deleting_without_a_guid_carries_taks_own_wording ... ok
test a_uuid_shaped_name_is_refused ... ok
test a_create_answers_201_with_a_token_and_matches_the_schema ... ok
test a_password_protected_mission_hands_out_an_access_token ... ok
test a_subscription_token_alone_grants_the_subscriptions_role ... ok
test an_empty_mission_still_answers_a_cot_document ... ok
test a_second_create_of_the_same_name_is_an_update_with_no_token ... ok
test a_mission_is_reachable_by_guid_and_by_name ... ok
test attaching_and_detaching_a_file_shows_up_in_the_changes ... ok
test deleting_by_guid_makes_every_later_read_a_410 ... ok
test the_created_mission_matches_its_golden ... ok
test subscribing_answers_201_with_a_token_and_the_fully_qualified_type ... ok
test keywords_are_replaced_and_removed_one_at_a_time ... ok
test the_rest_of_the_family_is_wired_and_extracts_its_parameters ... ok
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.58s

$ cargo test -p rustak-server --features testing --test mission_squash
running 4 tests
test add_remove_add_squashes_to_one_add ... ok
test the_fold_is_a_subsequence_of_its_input ... ok
test the_delta_describes_the_current_state ... ok
test the_fold_agrees_with_a_naive_replay ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s

$ # the other rustak-server integration targets, one at a time
marti_contract       ok. 14 passed; 0 failed
sync_contract        ok. 13 passed; 0 failed
profiles_contract    ok. 14 passed; 0 failed
marti_channels       ok.  9 passed; 0 failed
bootstrap            ok.  3 passed; 0 failed
enroll_flows         ok. 10 passed; 0 failed
enroll_oauth         ok. 10 passed; 0 failed
stream_routing       ok. 11 passed; 0 failed
stream_session       ok. 10 passed; 0 failed
stream_store         ok.  8 passed; 0 failed

$ cargo test -p rustak-cot -p rustak-api -p rustak-core -p rustak-client
(14 targets) all ok; 0 failed

$ cd interop/node-tak && npm test
✔ lists missions in the Mission envelope (111.7265ms)
✔ creates, reads back and deletes a mission (32.654791ms)
✔ subscribes to a mission and reports the subscription (24.419541ms)
ℹ tests 25
ℹ suites 0
ℹ pass 24
ℹ fail 0
ℹ cancelled 0
ℹ skipped 1
ℹ todo 0
ℹ duration_ms 1276.63625
```

The three mission scenarios were skipped before this brief (`TODO(M4): the mission API is not
served yet`) and all three now run and pass. An earlier run of the same suite was **25 pass, 0
skipped**; the one skip above is `stream.test.ts`, whose surface probe now races the stream
listener's bind (`[interop] surfaces missing: stream` is logged in the same millisecond as
`The CoT stream listener is bound`). That is a harness start-up race on the stream side, not a
mission regression — nothing fails either way.

### Two checks could not be completed, both for M4-02's files

`cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` still stop on
files M4-02 was editing while this brief finished:

* `rustak-server/tests/mission_dest.rs` imports `ResourceXml`, `UidDetailsXml`,
  `MissionChangeXml`, `MissionLayerXml` and `MissionRoleXml` from
  `stream::mission_notify`, which no longer exports them — so the `mission_dest` **test target**
  does not compile. Every other target in the workspace does, and all of them pass (above).
* Clippy's remaining findings are `rustak-server/src/marti/missions/layers.rs:180`
  (`needless_borrow`), `src/stream/mission_payload.rs:220` (`wrong_self_convention`),
  `src/missions/{external,invitations,layers,logs}.rs` and `src/stream/mission_notify.rs`
  (`unused_imports`) — all M4-02's — plus one pre-existing
  `rustak-core/src/identity/password.rs:333` (`assertions_on_constants`).

`cargo clippy -p rustak-server --all-targets -- -D warnings` reports **nothing in any file M4-01
owns**. The four findings it had were fixed:

* `auth/mission_token.rs` — `then(|| …)` → `then_some(…)`
* `db/repos/missions/changes.rs` — needless `Ok(…?)`
* `missions/service.rs` — `sort_by` → `sort_by_key`
* `missions/roles.rs` — hand-written `Default` → `#[derive(Default)]` with `#[default]`
