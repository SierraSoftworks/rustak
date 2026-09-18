# M3-06 — Admin API polish found by the operations UI — complete

Brief: `.claude/plan/briefs/M3-06-admin-api-polish.md`
Read first: `conventions.md`; `backlog.md` → "Admin API polish found by the operations UI (M3-04)";
status `M3-03-admin-api-packages-clients-cot.md`, `M3-04-admin-ui-operations.md` (the two "Server
endpoint gaps found" lists), `M4-02-missions-sync-notify.md`, `M2-08-identity-api-gaps.md`.

No `git`/`but` command was run.

All eight items are delivered. Seven are exactly what the brief asked for; item 4 answers the same
question from a different endpoint and without one of its three fields, for the reasons under
**Deviations**.

## What each item became

| # | Brief | What it is now |
|---|---|---|
| 1 | List and detail a deleted mission | `GET /missions?include_deleted=true`; `GET /missions/{guid}` answers **`410` with the `MissionDetail` body** |
| 2 | Layer `item_count` | Counted from the rows the detail already reads, per `layer_uid` |
| 3 | Incognito answers the client | `POST /clients/{uid}/incognito` → the updated `ConnectedClient` |
| 4 | Stream listener state | `GET /clients/status` → `StreamStatus { enabled, bound, connections }`; the clients page names the reason its list is empty |
| 5 | CoT time window | `GET /cot?secago&start&end`, decided in SQL, same semantics as the per-uid history |
| 6 | `ApiError::Gone` | Generic; the wizard's sentence moved to `rustak-ui/src/api/setup.rs` |
| 7 | Package `expiration` | RFC 3339 on `/api/v1`, `null` = never; Marti keeps epoch ms / `-1` |
| 8 | `oauth_tokens` | Dropped by migration `0015`; grep first, nothing reads it |

## Files

**New (2):** `rustak-server/migrations/0015_drop_oauth_tokens.sql`; this file.

**Changed (17):**

| File | Functional lines (limit 300) | What changed |
|---|---:|---|
| `rustak-api/src/client.rs` | 63 | `StreamStatus` + `off`/`is_listening`, and its test |
| `rustak-api/src/package.rs` | 63 | `expiration` becomes an instant on both types; `explicit_null` |
| `rustak-api/src/lib.rs` | 76 | one name added to the `client` re-export |
| `rustak-server/src/web/api/clients.rs` | 257 | `status` handler; `incognito` answers the connection |
| `rustak-server/src/web/api/cot.rs` | 252 | `ListQuery`'s window; `bounds` shared with the history |
| `rustak-server/src/web/api/missions.rs` | 205 | `include_deleted`; the `410` detail; real `item_count` |
| `rustak-server/src/web/api/missions_view.rs` | 168 | `resolve_any`, `layer_counts`, `deleted`, `parse`; `resolve` maps `Gone` |
| `rustak-server/src/web/api/packages.rs` | 240 | `expires_at`, the millisecond-to-instant conversion |
| `rustak-server/src/web/api/mod.rs` | 141 | one row in the route-table test |
| `rustak-server/src/cot_store/query.rs` | 170 | `LatestQuery::{since,until}` and two SQL clauses |
| `rustak-server/src/files/patch.rs` | 59 | the expiry write takes an instant (deviation 3) |
| `rustak-ui/src/api/clients.rs` | 30 | `status()`; `set_incognito` answers a `ConnectedClient` |
| `rustak-ui/src/api/mod.rs` | 165 | `ApiError::Gone`'s message |
| `rustak-ui/src/api/setup.rs` | 44 | `completed`, on the five routes that can be gone |
| `rustak-ui/src/pages/clients.rs` | 286 | reads `status`; `empty_reason` |
| `rustak-ui/src/fixtures/packages.rs` | — (exempt) | `stream_status`, the two expiry shapes |
| `.claude/plan/backlog.md` | — | the section closed, two findings recorded |

**Tests changed (4):** `rustak-server/tests/{api_v1_live,api_v1_packages,missions_extras}.rs`,
`e2e/tests/{live,missions}.spec.ts`.

**Not touched:** `rustak-api/src/{mission,cot,health}.rs` and `rustak-server/src/web/api/health.rs`,
all of which the brief made available and none of which needed a change — see deviations 1 and 2.
Nothing under `interop/`, `nightly.yml`, `stream/`, or any other agent's in-flight file.

## Decisions worth recording

### A deleted mission's detail is a `410` **carrying the mission**

The status is the honest answer to "is this mission here?"; the body is the honest answer to "what
happened to it?", and an operator who has followed a link from an audit entry is asking the second
one. So `GET /missions/{guid}` answers `410` with the whole `MissionDetail` — the flattened summary
with `deleted_at` set, the subscriptions, the layer tree and the change count — rather than with the
`{"error": …}` shape every other failure uses.

This is the one place in `/api/v1` where a non-2xx body is not an error object. It is safe because a
`410` is not a failure a generic handler should be parsing: `rustak-ui`'s client already branches on
the status before it looks at the body, and so does anything else that can tell the difference
between "gone" and "broken".

**`resolve` also stopped answering `500` for a deleted mission.** It mapped `MartiError::Gone` into
the catch-all, so `DELETE /missions/{guid}` on an already-deleted mission answered `500` while its
own documentation promised `410`. It now answers `410`, and the integration test asserts it.

### The counts come from the rows the detail already reads

`GET /missions/{guid}` reported `item_count: 0` on every layer while the tree drew the number, so a
folder with four markers in it said it was empty. The brief called for a join. What is here is a
tally over `mission_uids` and `mission_contents` — both read by primary key over one mission, both
already read by the same request, both carrying `layer_uid` — which is the same answer for no extra
query, and which lives in `web/api/missions_view.rs` rather than in a repository file this brief
does not own. A layer with nothing filed under it is absent from the map and reads as zero, which is
what the test asserts alongside the layer that has two.

Direct children only: a `GROUP` containing a `UID` layer counts what is filed on itself, not what is
filed under its children. That matches what the demo fixture draws ("Markers (4 items)" with "North
sector (2 items)" nested under it) and it is the number a client's own tree shows.

### The stream listener's state is administrative, so it is not on `/health`

The brief offered `GET /health` or a small `GET /clients/status`. It is the second one.

`/api/v1/health` is **public** — it is reachable with no credential at all, and it carries a test
named `the_check_says_nothing_that_would_help_somebody_attack_the_server`. How many EUDs are
connected to an installation, and whether its stream listener is up, are exactly the facts that test
exists to keep out: they tell an unauthenticated caller how big the deployment is and when it is
weakest. `/clients/status` is administrative like the rest of `/clients`, which is where the same
information already lives under the same gate.

`enabled` is what the configuration says, `bound` is whether the listener published its registry.
Three states rather than two, because "switched off" and "switched on and did not come up" are a
decision somebody made and a fault nobody has noticed, and the page says which:

* off → "Nothing is connected. This server has no stream listener — switch on `[stream.tls]` …"
* on but not bound → "… switched on but has not come up; its start-up failure is in the server log."
* listening → "Nothing is connected. The stream listener is running and quiet."

Each begins with the sentence the page used to show on its own, so `live.spec.ts`'s existing
assertion still means what it meant; the spec now also asserts the distinguishing half.

### The CoT window is decided in SQL, unlike the channel rule beside it

`web/api/cot.rs` narrows by channel *after* reading a page, and says so at the top of the file. The
time window cannot work that way: a window that closed an hour ago has none of its rows in the
newest hundred, so a post-filter would answer "nothing happened" to every question about the past.
It is therefore two clauses on `LatestQuery` — the only change this brief made to M3-03's
`cot_store/query.rs`, and an additive one.

`bounds` is shared with the per-uid history, so the two endpoints cannot drift about what `secago`
means, which of `start` and `secago` wins, or what a backwards window answers. The one difference is
deliberate and tested: the **listing** has no default window (all of `cot_latest` is what a map's
initial load is) while the **history** keeps its default hour (a device's history is unbounded and
has to start somewhere). An `end` alone therefore reaches back to the epoch on the listing and back
one hour on the history.

### An expiry is an instant on `/api/v1` and epoch milliseconds on Marti

TAK stores it as epoch milliseconds with `-1` for "never", every client reads that spelling, and the
column holds exactly those bits. `/Marti/sync/*` is unchanged, byte for byte — `sync_contract.rs`
still asserts `1_714_564_800_000`.

`/api/v1` is ours, and a browser that has to know "negative means never" before it can render a date
is a convention travelling in a comment rather than in the type. So `PackageSummary::expiration` is
an RFC 3339 instant, **always present**, `null` for never. That is the one field in the type that is
not omitted when absent: an absent `filename` means "we were not told", while an absent expiry would
be indistinguishable from an older server that did not send the field.

`PackageUpdate::expiration` is `Option<Option<DateTime<Utc>>>` behind a `deserialize_with`, because
there are three answers and `Option`'s own `Deserialize` folds two of them together — absent leaves
the expiry alone, `null` clears it, an instant sets it. TAK's `-1` said this with a sign; nothing in
a JSON body can. A form that could not say "clear this" would leave an expired package unreachable
for good.

### `ApiError::Gone` has no message of its own any more

`410` now means two unrelated things on this server — the wizard's routes are gone, and that mission
was deleted — and one sentence cannot be right for both. It was "This server has already been set
up.", which is what a mission detail page was showing.

The variant's `Display` is now generic and the wizard's sentence lives in
`rustak-ui/src/api/setup.rs`, where `completed` maps `Gone` on the five routes that can answer it.
The caller that knows which question it asked is the one that supplies the answer.

### `oauth_tokens` was dead in every sense

Grepped first, as the brief asked: the only occurrences outside migration `0003` are two comments
(`0013`'s explanation of why it made its own table, and design 01) and M0-07/M5-01's status files.
No repository, no query, no row, no test. Codes went to `oauth_codes` (`0013`), pending
identity-provider states to the `auth-state` key/value partition, refresh tokens to `refresh_tokens`.

A dead table with a `user_id` foreign key and a `token_hash` column is worse than no table: the next
person reading the schema has to work out whether it holds live credentials before they can answer
any question about where this server keeps its secrets. `0015` is one `DROP TABLE IF EXISTS`, and it
names both dropped indexes in a comment so a reader knows nothing was left behind.

## Deviations from the brief

1. **`GET /clients/status` rather than a field on `GET /health`.** The brief allowed either. The
   health check is public and carries a test asserting it leaks nothing operationally useful;
   connection counts are exactly that. `rustak-api/src/health.rs` and
   `rustak-server/src/web/api/health.rs` are therefore unchanged.
2. **No `bound_at`.** `StreamStatus` is `{enabled, bound, connections}`. Nothing records when the
   listener bound: `LiveState` has no such field and the timestamp would have to be taken where the
   registry is installed — `stream/live.rs` and `services/mod.rs`, both of which another agent was
   editing throughout this brief (M3-03 recorded the same hazard). `bound` answers the question the
   item exists for ("is `[]` ambiguous?"); `bound_at` is recorded in the backlog for whoever owns
   those files next.
3. **`rustak-server/src/files/patch.rs` was edited** — six lines, not in this brief's ownership list.
   Changing `PackageUpdate::expiration`'s type (item 7, and `rustak-api/src/package.rs` *is* owned)
   makes its one consumer a compile error; the alternative was two representations of one value with
   a conversion in between, which is what item 7 exists to remove. Its two tests moved with it. The
   file belongs to M3-03, which is complete.
4. **`rustak-server/src/cot_store/query.rs` was edited** — two fields and two SQL clauses, also
   M3-03's and also complete. See "The CoT window is decided in SQL" for why a post-filter would not
   have worked. The read is unchanged for a caller that names no window.
5. **`rustak-ui/src/fixtures/packages.rs` was edited** — the brief names `pages/clients.rs` "and its
   API client". Demo mode is served entirely from fixtures, so a new endpoint without a fixture is a
   demo build that fails to compile; `stream_status()` is six lines, and the two expiry values
   changed shape with the DTO. Fixtures are exempt from the file-length check.
6. **`e2e/tests/{live,missions}.spec.ts` were edited.** Both asserted behaviour this brief changed:
   `live.spec.ts`'s empty-state text and `missions.spec.ts`'s comment that `GET /api/v1/missions`
   takes no `include_deleted`. The CI steward owns CI and e2e; these are the two assertions this
   brief invalidated, and leaving them would have handed over a red suite. Both additions are
   assertions rather than restructuring.
7. **The console does not render the `410` body yet.** `rustak-ui/src/api/mod.rs` turns every `410`
   into `ApiError::Gone` before the body is read, so a deleted mission's detail page now says "That
   is no longer available on this server." — correct, and less than the endpoint can give it.
   Reading the body needs a branch in `api/missions.rs::get` and one in `pages/mission_detail.rs`,
   which is more UI than this brief's "minimal `rustak-ui` edits" allows. Recorded in the backlog.
8. **`item_count` is a tally rather than a SQL join.** Deviation in spelling only — see "The counts
   come from the rows the detail already reads". The join would have gone in
   `db/repos/missions/contents.rs`, which this brief does not own.

## What the new tests assert

**Unit (5 new):**

- `rustak-api/src/client.rs` — an empty client list is only ambiguous without `StreamStatus`: quiet,
  off and configured-but-absent are three distinguishable answers, and the wire shape.
- `rustak-api/src/package.rs` — never is stated as `null` rather than left out; an expiry travels as
  an instant; absent leaves it alone and an explicit `null` clears it, both ways through serde.
- `rustak-server/src/web/api/cot.rs` — a listing with no window asked for has none; a listing window
  means what it means on the history (`secago` relative to `end`, an `end` alone reaching back to
  the epoch, a backwards window refused).
- `rustak-server/src/web/api/clients.rs` — hiding a connection answers the connection rather than
  the request, carrying the new flag and both channel directions.
- `rustak-server/src/files/patch.rs` — the existing expiry tests, rewritten around instants and the
  explicit `null`.

**Integration (4 new, 2 extended):**

- `missions_extras.rs` — *a deleted mission is listed only when it is asked for and answers 410 with
  itself*: the default listing stays live-only, `?include_deleted=true` carries the row and its
  `deleted_at`, the detail is a `410` whose body is the mission, deleting twice is `410` rather than
  `404` or `500`, and the listing is administrative either way.
- `missions_extras.rs` — *a layer reports how many items are actually filed under it*: two uids filed
  under one layer through the Marti route a client uses, and a sibling layer with nothing in it that
  still says zero.
- `api_v1_live.rs` — *an empty client list is explained rather than left ambiguous*, and
  `/clients/status` joins the administrative-throughout matrix.
- `api_v1_live.rs` — *the CoT listing takes the same window the history does*: no window is all of
  it, `secago=600` is the recent one, an explicit window that closed six hours ago finds the older
  message (which a post-filter could not), and a backwards window is the same `400`.
- `api_v1_packages.rs` — the patch test now asserts the instant, the explicit `null` that clears it,
  and that a change naming only the name leaves the expiry alone.
- `e2e/tests/live.spec.ts` — the empty state names the missing listener.
  `e2e/tests/missions.spec.ts` — after the delete, `?include_deleted=true` lists the mission with
  its `deleted_at`, and `GET /missions/{guid}` is a `410` whose body carries the mission.

## Exit checks

Run against the final tree. Other briefs were being written throughout — the server's unit-test
count moved from 1572 to 1581 between two of these runs without any change of mine — so each was run
once the tree was consistent, and every one below is green.

```
$ ./scripts/check-file-length.sh
(no output, exit 0)
# The script reads `git ls-files`, so the new migration and this file are not in
# it; neither is Rust. The longest file this brief touched is
# `rustak-ui/src/pages/clients.rs` at 286 functional lines (limit 300), then
# `web/api/clients.rs` at 257 and `web/api/cot.rs` at 252.

$ cargo fmt --all --check
(no output, exit 0)
# `cargo fmt --all` was run first; it reformatted only the ten files this brief
# had edited, which was confirmed by mtime before the check.

$ cargo clippy --workspace --all-targets --features rustak-server/testing -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 12s
# Clean. One finding of my own was fixed rather than allowed: `ListQuery::window`
# returned `Result<Option<(DateTime<Utc>, DateTime<Utc>)>, ApiError>`, which
# `clippy::type_complexity` was right about — it is a `type Window` now.

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
(no findings)

$ cargo test --workspace --features rustak-server/testing
rustak_api              141 passed; 0 failed
rustak_client           122 passed; 0 failed   (+ stream_client 12, stream_reconnect 2, stream_tls 2)
rustak_core             140 passed; 0 failed
rustak_cot              253 passed; 0 failed   (+ codec_framed 9, golden 49, roundtrip_prop 9)
rustak_plugin_example     6 passed; 0 failed
rustak_server (lib)    1581 passed; 0 failed; 2 ignored
  acme_directory  5    api_v1_live     14   api_v1_packages  9   bootstrap      3
  enroll_flows   10    enroll_oauth    10   marti_channels   9   marti_contract 14
  mission_dest   10    mission_squash   4   missions_extras 14   missions_flow 14
  oauth_flows    24    profiles_contract 14 services_flow    2   stream_channel_state 3
  stream_routing 12    stream_session  10   stream_store     8   sync_contract 14
  Doc-tests: rustak_client 10, rustak_core 14, rustak_cot 3, rustak_server 5
# Every suite ok; 0 failed anywhere.

$ cd rustak-ui && trunk build
2026-09-18T20:41:03.606520Z  INFO 🚀 Starting trunk 0.21.14
2026-09-18T20:41:03.610517Z  INFO 📦 starting build
   Compiling rustak-api v0.1.0
   Compiling rustak-ui v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.64s
2026-09-18T20:41:15.368968Z  INFO ✅ success

$ cd rustak-ui && cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings
    Checking rustak-api v0.1.0
    Checking rustak-ui v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.79s

$ cd e2e && npm run typecheck
> tsc --noEmit
(no output)

$ cd e2e && RUSTAK_E2E_CHROMIUM="…/chromium-1234/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing" npx playwright test
Running 29 tests using 1 worker
  ✓   1 [setup] › setup.spec.ts:29 › the first-run wizard turns a token on disk into an administrator who can sign in (1.1s)
  ✓   2 [setup] › setup.spec.ts:124 › the wizard closes itself for good once it has been completed (324ms)
  ✓  12 [chromium] › live.spec.ts:25 › a server with nothing connected says so rather than failing (347ms)
  ✓  15 [chromium] › missions.spec.ts:39 › a mission a client created is listed, opened, and deleted (839ms)
  ✓  23 [chromium] › packages.spec.ts:35 › a package is uploaded, given a channel, downloaded and deleted (698ms)
  … 29 passed (20.1s)
# `RUSTAK_E2E_CHROMIUM` is needed on this machine only, for the reason M0-14
# recorded. CI leaves it unset.
```

## Notes for the briefs that follow

- **Anyone adding a route under `/api/v1`**: add the row to `PROTECTED` in `web/api/mod.rs`'s
  route-table test, or the gate stops being checked for it. `GET /api/v1/clients/status` is in it.
- **Anyone rendering a package expiry**: it is `Option<DateTime<Utc>>` on `/api/v1` and epoch
  milliseconds on `/Marti/sync/*`. `web/api/packages.rs::expires_at` is the one conversion in, and
  `files/patch.rs` the one conversion out. Do not add a third.
- **Anyone writing a `410`**: `ApiError::gone` takes the message, and `ApiError::Gone` in the UI no
  longer supplies one. Say what is gone at the call site.
- **Anyone opening a deleted mission**: the body of the `410` is the whole `MissionDetail`. It is
  there to be rendered — see backlog item, and deviation 7.
- **Anyone who ends up owning `stream/live.rs` or `services/mod.rs`**: a `bound_at` recorded where
  the registry is installed would complete `StreamStatus`. It is a field and one assignment.
- **`cot_store::query::LatestQuery` now has `since`/`until`.** They default to `None`, which is the
  behaviour every existing caller had.
