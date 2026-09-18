# M4-02 — Data Sync extras: `<dest mission>` ingest, `t-x-m-*` notifications, invitations, logs, layers, archive, admin API, expiry job

**Status: complete.** Every deliverable in the brief is implemented, every `TODO(M4-02)` M4-01
left is filled in, and every exit check is green — including the node-tak scenarios.

## What landed

| Area | Files |
|---|---|
| Stream notices | `rustak-server/src/stream/{mission_notify,mission_payload}.rs`, `stream/notify.rs` (two `Notifier` methods), `stream/mod.rs` (module + re-export lines, `serve` signature) |
| `<dest mission>` | `rustak-server/src/missions/cot.rs`, one line in `runtime.rs` |
| Notices → service | `rustak-server/src/missions/notify.rs` |
| Invitations | `rustak-server/src/missions/{invitations,invitees}.rs`, `marti/missions/invitations.rs` |
| Logs | `rustak-server/src/missions/logs.rs`, `marti/missions/logs.rs`, migration `0011_mission_log_missions.sql` |
| Layers | `rustak-server/src/missions/layers.rs`, `marti/missions/layers.rs` |
| Map layers / external data / feeds | `rustak-server/src/missions/external.rs`, `marti/missions/external.rs` |
| Archive | `rustak-server/src/missions/archive.rs` |
| Admin API | `rustak-server/src/web/api/{missions,missions_view}.rs` (+ 3 lines in `web/api/mod.rs`), `rustak-api/src/mission.rs` (+ 2 blocks in `rustak-api/src/lib.rs`) |
| Expiry job | `rustak-server/src/jobs/mission_expiry.rs` (+ 2 blocks in `jobs/mod.rs`), `config/retention.rs`, `config.example.toml` |
| Tests | `rustak-server/tests/{mission_dest,missions_extras}.rs`, `tests/golden/missions/*.xml` (13 templates), one additive variant in `tests/stream_support/mod.rs` |

## The `t-x-m-*` templates

`stream/mission_notify.rs` holds the notice model (`MissionNotice`, `ChangeKind`, `Recipients`,
`NoticeMission`) and the rendering; `stream/mission_payload.rs` holds what a notice carries
(`MissionChangeXml`, `ResourceXml`, `UidDetailsXml`, `MissionLayerXml`, `MissionRoleXml`). The
split is the file-length rule, not a design statement.

```rust
pub fn events(notice: &MissionNotice, now: CotTime) -> Vec<Event>;          // fresh uids
pub fn events_with(notice, now, uid: impl FnMut() -> String) -> Vec<Event>; // what the goldens pin
pub fn send(notifier: &dyn Notifier, notice: &MissionNotice) -> usize;      // render + dispatch
```

Thirteen templates are pinned byte for byte under `rustak-server/tests/golden/missions/`
(`t-x-m-c` for a content add and a uid add, `-c` remove, `-c-l`, `-c-k`, `-c-k-u`, `-c-k-c`,
`-c-m`, `-c-e`, `-c-h`, `t-x-m-n`, `t-x-m-d`, `t-x-m-i`, `t-x-m-r`).
`RUSTAK_UPDATE_GOLDEN=1 cargo test -p rustak-server --test mission_dest` rewrites them.

Delivery goes through two new `Notifier` methods, both implemented for `Hub` in `stream/notify.rs`:

```rust
fn send_to_uids(&self, client_uids: &[String], event: Event) -> usize;
fn broadcast_to_groups(&self, groups: &[GroupName], except_uid: Option<&str>, event: Event) -> usize;
```

`broadcast_to_groups` selects from `Hub::snapshot()` — the identified connections and the channel
names they receive on — so it needed no change to `hub.rs`. `__ANON__` in the mission's groups
force-includes everybody, which is how a read-only account hears about a public mission.

## Notices, from the service side

`missions/notify.rs` adds these to `MissionService`. Each answers how many connections it reached
and treats "nobody" as ordinary, never as an error:

```rust
pub fn notify(&self, notice: &MissionNotice) -> usize;
pub fn notify_created(&self, m: &Mission, author_uid: Option<&str>) -> usize;     // t-x-m-n
pub fn notify_deleted(&self, m: &Mission, author_uid: Option<&str>) -> usize;     // t-x-m-d
pub fn notify_invited(&self, m, author_uid, token: &str, role: Role, uids: Vec<String>) -> usize;
pub fn notify_role_changed(&self, m, author_uid, role: Role, client_uid: &str) -> usize;
pub fn notify_broadcast(&self, m, kind: ChangeKind, author_uid: Option<&str>) -> usize;
pub async fn notify_subscribers(&self, m, kind: ChangeKind, author_uid) -> Result<usize, MartiError>;
pub async fn notify_layer(&self, m, layer: MissionLayerXml, author_uid) -> Result<usize, MartiError>;
pub async fn notify_content(&self, m, rows: &[MissionChangeRow], author_uid) -> Result<usize, MartiError>;
```

Where each `TODO(M4-02)` went:

| Marker | Now |
|---|---|
| `contents.rs` add/remove | `notify_content` after `record_all`, one `t-x-m-c` per change |
| `import.rs` | the same, after a package import |
| `keywords.rs` mission / uid / hash | `t-x-m-c-k` broadcast, `t-x-m-c-k-u`, `t-x-m-c-k-c` |
| `crud.rs` create / update / delete | `t-x-m-n`, `t-x-m-c-m`, archive-to-resource then `t-x-m-d` |
| `subscriptions.rs` set_role / password | `t-x-m-r` per affected device, `t-x-m-c-m` |
| `subscriptions.rs` invite-only subscribe | `apply_invitation` fills `SubscribeReq::invited_role`, and the subscribe spends the invitations naming that device |
| `roles.rs` invitation token | matched by the invitation row the token's `id` claim names |
| `render.rs` | `externalData`, `mapLayers` and `feeds` filled from migration `0010`'s tables |
| `marti/missions/contents.rs` archive | streams the zip with a quoted, percent-encoded filename |
| `marti/missions/misc.rs` send | each contact becomes a `clientUid` invitation plus its `t-x-m-i` |

## `<dest mission>` (`missions/cot.rs`)

`MissionPublisher` implements `MissionIngest`: resolve by name then guid → find the sender's
subscription by device uid then by account → require `MISSION_WRITE` → return the connected
subscriber uids minus the sender → `add_content`, which appends `ADD_CONTENT` **and** emits the
`t-x-m-c`. Both deliveries happen and neither replaces the other. A sender with no subscription or
a read-only role is dropped for that mission and nothing else.

`stream::serve` now takes the ingest rather than building it, so `stream` still knows nothing about
missions; `runtime.rs` passes `MissionPublisher::shared(context.clone())` (one line, as the brief
asked — the `no_missions()` call it named was inside `stream::serve`, not `runtime.rs`).

## Routes added

`marti/missions/mod.rs`'s `reserved_routes` no longer reserves anything:

* `GET /missions/invitations?clientUid=`, `GET /missions/all/invitations`,
  `GET|PUT|DELETE {n}/invitations`, `{n}/invite`, `{n}/invite/{type}/{invitee}`
* `POST|PUT /missions/logs/entries`, `GET|DELETE /missions/logs/entries/{id}`,
  `GET /missions/all/logs`, `GET {n}/log`
* `GET|PUT|DELETE {n}/layers`, `{n}/layers/{uid}/name`, `{n}/layers/{uid}/position`,
  `{n}/layers/parent`
* `POST|PUT {n}/maplayers`, `DELETE {n}/maplayers/{uid}`, `POST {n}/externaldata`,
  `DELETE {n}/externaldata/{id}`, `POST|DELETE {n}/feed`, `DELETE {n}/feed/{uid}`

Admin (`/api/v1`, `Administrative` throughout): `GET /missions`, `GET /missions/{guid}`,
`DELETE /missions/{guid}?deep`, `GET /missions/{guid}/changes?squashed`,
`PUT /missions/{guid}/subscriptions/{uid}/role`, `DELETE /missions/{guid}/subscriptions/{uid}`,
`GET /missions/{guid}/archive`.

## Deviations from the brief

1. **A new migration, `0011_mission_log_missions.sql`.** `0006` made `mission_logs.log_id`
   globally unique, which allows one mission per log entry; `LogEntry.missionNames` is an array
   because ATAK writes one entry naming every mission it had open. The index is dropped and
   replaced with `UNIQUE(log_id, mission_id)` in a new file rather than by editing `0006`.
   Migrations were not in my file list.
2. **Five files more than the brief names, every one for the < 300-line rule.**
   `stream/mission_payload.rs` (split from `mission_notify.rs`), `missions/invitees.rs` (split
   from `invitations.rs`), `missions/external.rs` (map layers, external data and feeds, which
   would not fit in `layers.rs`), `marti/missions/external.rs` (their routes) and
   `web/api/missions_view.rs` (the DTO mapping behind `web/api/missions.rs`).
3. **`missions/subscriptions.rs` is M4-01's file and was at ~302 functional lines.** Rather than
   push it further over, the two helpers my changes needed (`role_change_recipients`,
   `store_and_spend`) live in `missions/notify.rs` and `missions/invitees.rs`, and
   `store_subscription` became `pub(super)`. The file is now **299**.
4. **`marti/missions/mod.rs`'s ordering test asserted `!= 404`.** That was right while the paths
   answered `501`; `GET /missions/logs/entries/abc` now answers a perfectly ordinary `404` for a
   log entry that is not there. The assertion is now "not a `404` that names a *mission*", which
   is what falling through to `{name}` actually looks like.
5. ~~**`<role>` renders `<permissions>` as a wrapper** with `<permission type=…/>` children, per
   design 04 §4.8.~~ **Withdrawn 2026-09-18 by M4-04 (R-02 M11).** The deviation cited design 04
   §4.8 over research 05 §7.5, and `compat/README.md` makes the research authoritative — so it did
   not hold on its own terms. `stream/mission_payload.rs` now renders repeated `<permissions>` text
   elements, which is what JAXB produces from `@XmlElement(name="permissions")` on a `Set<String>`
   (`MissionRole.java:104-105`), and the `t-x-m-i` / `t-x-m-r` goldens are re-pinned to that shape.
   A client reading `role/permissions` text got nothing at all from the nested form.
6. **`MissionChange` child order** is `rustak-cot`'s (type, isFederatedChange, missionName,
   missionGuid, timestamp, creatorUid, contentUid, details, contentResource) — research 05 §7.5's
   getter order, already pinned by `rustak-cot`'s own tests. Design 04 §4.8 illustrates an
   alphabetical order; element order inside `MissionChange` is not significant to any client that
   parses it.
7. **An archived item with no event in the CoT store is rebuilt from its cached details** rather
   than dropped, so a mission whose events have aged out still archives its markers.
8. **An archive's `password_hash` parameter is the string `true`/empty**, never the hash: an
   archive travels, and a hash that travelled would be a password offered up for cracking.
9. **`missions/notify.rs` holds an `impl From<Role> for rustak_api::MissionRoleKind`.** The stream
   renderer takes the API's role spelling so that it never reaches into the mission service for a
   permission list; this is the one place the two vocabularies meet.
10. **`tests/stream_support/mod.rs` gained `Harness::start_with_missions()`** (additive): the
    default harness keeps the M1 stub, and the mission variant also publishes the live registry on
    the context, which is what `subscribers_for` reads.

## Exit checks

```
$ cargo fmt --all --check
FMT OK

$ ./scripts/check-file-length.sh
LENGTH OK        (no output; every file created or changed here is under 300 functional lines)

$ cargo clippy --workspace --all-targets -- -D warnings
CLIPPY done      (clean)

$ RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps
DOC done         (clean)

$ cargo test -p rustak-server --features testing
test result: ok. 1357 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 16.07s
test result: ok. 3 passed; …       (bootstrap)
test result: ok. 10 passed; …      (enroll_flows)
test result: ok. 10 passed; …      (enroll_oauth)
test result: ok. 9 passed; …       (marti_channels)
test result: ok. 14 passed; …      (marti_contract)
test result: ok. 10 passed; …      (mission_dest)
test result: ok. 4 passed; …       (mission_squash)
test result: ok. 12 passed; …      (missions_extras)
test result: ok. 14 passed; …      (missions_flow)
test result: ok. 14 passed; …      (profiles_contract)
test result: ok. 11 passed; …      (stream_routing)
test result: ok. 10 passed; …      (stream_session)
test result: ok. 8 passed; …       (stream_store)
test result: ok. 13 passed; …      (sync_contract)
test result: ok. 5 passed; …       (doc-tests)
0 failures.

$ cargo test --workspace
32 suites, every one `test result: ok`, 0 failures.

$ cd interop/node-tak && npm test
✔ lists missions in the Mission envelope (106.262291ms)
✔ creates, reads back and deletes a mission (32.574209ms)
✔ subscribes to a mission and reports the subscription (24.581667ms)
ℹ tests 25
ℹ pass 24
ℹ fail 0
ℹ skipped 1
```

Migration `0011_mission_log_missions.sql` applies cleanly on a fresh database in the node-tak run
(`Applied a database migration. migration="0011_mission_log_missions.sql"`).

## The `mission_dest` suite is deterministic

M4-01 saw `a_subscriber_gets_the_message_and_the_notice_and_the_sender_gets_neither` fail once and
then pass. The first version drained the connect-time situational awareness with a fixed 400 ms
window and asserted the negative cases with `expect_none`, so a late SA message could either be
still in flight when the assertions began or arrive during one of them.

Both are now event-driven. `announce` sends each client's SA and then a **barrier** — an ordinary
broadcast with a distinctive uid — per sender, and waits for every other client to see it:
messages reach one connection in the order the server wrote them, so seeing a sender's barrier
means having already seen everything that sender caused. The negative assertions use `first_after`,
which sends a barrier and asserts the watching client sees *it* before it sees `UID-MARKER` or any
`t-x-m-c`. Nothing waits on a clock. Run five times in a row: `10 passed` each time.
