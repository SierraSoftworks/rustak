# M2-06 — Marti groups/channels, contacts, clientEndPoints, subscriptions — complete

Brief: `.claude/plan/briefs/M2-06-marti-groups-contacts.md` (including the addendum).
Read first: `conventions.md`; `compat/{groups,contacts,streaming,cloudtak}.md`; design 04 §0 (D8, D9),
§3, §9; status files `M1-05` (Hub snapshot, `LiveState`, `Notifier`), `M2-02` (members/devices),
`M2-03` (principal resolution, listener policies), `M2-04` (envelope, extractors, `kind`).

## What was built

| File | Functional lines (limit 300) | Unit tests | Contents |
|---|---:|---:|---|
| `marti/channels.rs` | 171 | 6 | `GroupJson`, `Selection`, `selection`, `visible`, `visible_to`, `rows`, `apply`, the account-level selection in `kv` |
| `marti/groups.rs` | 184 | 6 | `/groups/{all,active,activebits,groupCacheEnabled,user,{name}/{direction}}`, `/users/all`, the lenient body parser |
| `marti/contacts.rs` | 203 | 5 | `/contacts/all` (bare array), `/clientEndPoints` (envelope + cache headers, live and disconnected rows, `group=` 403) |
| `marti/subscriptions.rs` | 193 | 3 | `SubscriptionInfoJson` (33 keys), `/subscriptions/all`, `/subscription/{uid}`, `incognito`, `delete`, the filter no-ops |
| `marti/mod.rs` | 261 (+89) | +1 case set | `channel_routes`, 9 new `PATHS`, 3 new `PARAMETERISED`, `PARAMETERISED_TAIL`, `PARAMETERISED_PAIR` |
| `identity/members.rs` | 230 (+49) | 0 | `channels_changed` — re-authenticates live connections, then emits `t-x-g-c` |
| `services/mod.rs` | 192 (+13) | 0 | `Late<LiveConnections>`: `install_live`, `live()`, `has_live()` |
| `stream/hub.rs` | 260 (+20) | 0 | `sessions_for_user`, `reauth` |
| `stream/live.rs` | 89 (+13) | 0 | `sessions_for_user`, `reauth`, `resend_latest_sa` |
| `stream/mod.rs` | 170 (+1) | 0 | `serve` installs `LiveState` on the context the moment the listener binds |
| `web/api/devices.rs` | 127 (+1) | 0 | the `TODO(M2-08)` replaced by `members::channels_changed(.., Some(uid))` |
| `web/api/users_groups.rs` | 82 (+1) | 0 | the `TODO(M2-08)` replaced by `members::channels_changed(.., None)` |

Tests (`tests/`, exempt from the line rule): **`tests/marti_channels.rs`** — 9 scenarios over a real
stream listener and the real `App`; **`tests/marti_contract.rs`** — 14 new rows in `CONTRACT`.

**20 new unit tests** and **9 new integration scenarios**. The crate is 1201 lib tests, green.
No manifest change, no new dependency, no migration, no configuration key.

### Routes as mounted

| Method | Path | Auth | Answer |
|---|---|---|---|
| GET | `/Marti/api/groups/all?useCache&sendLatestSA` | any credential | `GROUP` envelope, one row per (channel, direction) |
| PUT | `/Marti/api/groups/active?clientUid&sendLatestSA` | any credential | `200 text/plain`, empty |
| PUT | `/Marti/api/groups/activebits?clientUid` | any credential | as above; body is a bare array of `bitpos` |
| GET | `/Marti/api/groups/groupCacheEnabled` | anonymous | `java.lang.Boolean` envelope, `true` |
| GET | `/Marti/api/groups/{name}/{direction}` | any credential | `GROUP` envelope, or `404` with no `data` |
| GET | `/Marti/api/groups/user?username=` | admin | `GROUP` envelope for that account |
| GET | `/Marti/api/users/all` | admin | `com.bbn.marti.remote.groups.User` envelope |
| GET | `/Marti/api/contacts/all` | any credential | **bare JSON array**, never an envelope |
| GET | `/Marti/api/clientEndPoints?secAgo&showCurrentlyConnectedClients&showMostRecentOnly&group` | any credential | `ClientEndpoint` envelope + `Cache-Control`/`Expires` |
| GET | `/Marti/api/subscriptions/all?sortBy&direction&page&limit` | any credential | `SubscriptionInfo` envelope |
| GET | `/Marti/api/subscription/{uid}` | any credential | as above, or `404` with no `data` |
| POST | `/Marti/api/subscriptions/incognito/{uid}` | own subscription, or admin | `200`, empty |
| DELETE | `/Marti/api/subscriptions/delete/{uid}` | admin | `java.lang.String` envelope |
| PUT/DELETE | `/Marti/api/subscriptions/{clientUid}/filter` | any credential | `200`, empty, no-op |

## Decisions worth recording

### `active` has an account-level layer as well as a per-device one

This is the substantive design decision of the brief and it is worth reading in full.

M2-02 scoped the active-channel state to a **device** (`device_group_state`), on the good argument
that switching a channel off on a phone must not switch it off on a laptop. TAK Server's own cache is
per **user**. The two disagree for exactly one caller: the one with no device — which is CloudTAK's
browser half, the admin UI, a script, and the node-tak interop suite's administrator. Such a caller
`PUT`s a selection and there is no device row to write it to, so the next `GET` would report
everything on again, and `compat/groups.md` §5's CloudTAK force-activate flow would loop.

So `marti::channels` adds an **account-level selection** in the key/value store (partition
`marti-channels`, key `active-<user id>`), and reads layer most-specific-first: the device's row,
then the account's, then on.

- `PUT …/active?clientUid=` writes that device's rows and nothing else — the phone-and-laptop case,
  unchanged from M2-02.
- `PUT …/active` with no `clientUid` writes the account default **and** every one of that account's
  device rows. Writing the device rows as well is what keeps routing honest:
  `members::effective_for_device` — which the stream resolver and the certificate auth path both
  use — reads only `device_group_state`, and it is not this brief's to change.

The one place the two layers can disagree is a device enrolled *after* an account-level change: it
reads the account default (off) but routes from its absent device rows (on) until it sets its own
state. That is the permissive direction and it corrects itself the first time the device calls the
endpoint, which ATAK does on connect. A `user_group_state` table would close it properly; migrations
are outside this brief's file list.

### `channels_changed` is one function, called from three places

`identity::members::channels_changed(context, user_id, username, originating_uid)` does the two
things a channel change owes a connected client, in this order:

1. **Re-authenticate every live connection** the account has, against the set it would get if it
   connected now (`effective_for_device` per device, `group_set` for a connection with no device).
   A connection holds the rights it authenticated with, so without this the server would keep routing
   by the old selection until the device reconnected — while telling the client something else. This
   is asserted end to end by `switching_a_channel_off_stops_that_device_receiving_on_it`.
2. **Send `t-x-g-c`** to the account's *other* devices.

It never fails: a notice that could not be sent is logged, because a client that missed one re-reads
its channels on its next connect and a request that *was* applied must not be reported as having
failed. `PUT /Marti/api/groups/active`, `PUT /api/v1/devices/{uid}/active-groups` and
`PUT /api/v1/users/{username}/groups` all call it, which is what closed M2-02's two `TODO(M2-08)`s.

### `t-x-g-c` is sent when no `clientUid` was given, following design 04 D9

`compat/groups.md` §2 records that TAK Server sends **nothing** in that case. Design 04 D9 overrides
it deliberately, and this implementation follows the design: CloudTAK never sends a `clientUid`, so a
channel toggled from a browser would otherwise never reach the phone that is looking at the map.
`a_change_that_names_no_device_reaches_every_one_of_them` pins it.

### The replay happens after the re-authentication, not before

`sendLatestSA=true` is served by pushing every reachable peer's latest SA down the caller's *stream*
connections — a side effect on `:8089`, never on the HTTP response. On `PUT …/active` it runs after
`channels_changed`, because a replay computed from the old channel selection would refill the map
with exactly the peers the client had just switched off.

### An administrator is shown every channel; everybody else is shown their grants

Design 04 §3.1. An administrator may grant themselves any channel, and the Channels UI they
administer from would otherwise be able to display only what they had already joined. Every row still
carries a real `bitpos` and a real `created`, so nothing ATAK parses changes shape by caller.

### `/clientEndPoints`' visibility for a disconnected device is judged from the account

A device that is not connected is not routing anything, and whether its owner switched a channel off
*on that device* says nothing about whether the caller was ever allowed to see it. So the filter is
`can_reach(owner.groups, caller.groups)`, read once per distinct account rather than once per device
— a fleet is a handful of accounts and a great many phones.

`group=` is a hard `403` on any channel the caller cannot read (`compat/contacts.md` §2), not a
silent drop: a filtered listing that quietly ignored half the filter looks like an empty network. An
administrator passes the check, consistently with the listing rule above.

### An element of `PUT …/active` we cannot read is dropped, not refused

`compat/groups.md` §2 requires `name`/`direction`/`type`; design 04 §3.1 reads `{name, direction,
active}` and ignores the rest. This follows the **design**, which is the more forgiving of the two:
requiring `type` would drop a well-formed-enough element from a third-party client for no gain, and
dropping an element is worse than accepting one. `created` is ignored entirely, which is how the
"date out, epoch millis in" asymmetry costs nothing — it is never read on the way in.

An element naming a channel that is not here is dropped by `members::set_active` (M2-02's rule) and
never appears in the answer, because the answer is rebuilt from the `groups` table.

### `incognito` is a toggle on the connection, and only its owner may flip it

There is no "set to this value" spelling upstream and there is none here: the client asking is the
one that knows what it is now. A `uid` nothing is connected under is a `404` — there is nothing to
hide — and a subscription somebody else owns is a `403` however visible it happens to be.

### `/subscriptions/all` emits every key as `null` rather than omitting it

Thirty-three fields, most of them device telemetry rustak has no source for. A client reading
`row.battery` wants a value it can test; `undefined` is the one answer that throws. `deviceIPAddress`
is renamed explicitly because upstream capitalises the acronym there and nowhere else in the model —
a test caught `rename_all = "camelCase"` producing `deviceIpAddress`.

### Every nested Marti scope's `serves_path` table grew three shapes

M2-04's `404`-versus-`405` table only understood "a prefix plus one segment" and one hard-coded
two-segment case. It now has `PARAMETERISED_TAIL` (prefix, one segment, literal tail — for
`/subscriptions/{uid}/filter`) and `PARAMETERISED_PAIR` (prefix plus exactly two segments — for
`/groups/{name}/{direction}`), with the `missions/{name}/kml` special case folded into the first.
The contract test enumerates the new shapes and four near-misses.

## Deviations from the brief, and why

1. **`marti/channels.rs` is a fourth file the brief did not name.** `groups.rs` with the reading and
   writing folded in was over the 300-line limit, and the same logic is needed by
   `/clientEndPoints`' `group=` filter and by `/subscriptions/all`. Split by responsibility:
   `channels.rs` is what a channel *is* and which are on, `groups.rs` is the routes.
2. **`stream/hub.rs` and `stream/live.rs` gained three small methods.** The addendum says not to
   modify `stream/**` beyond "a tiny accessor … and say so", and the brief names
   `LiveState::reauth_user` and `Notifier::resend_latest_sa`, neither of which existed. What was
   added is the minimum:
   - `Hub::sessions_for_user` / `LiveState::sessions_for_user` — a read-only accessor.
   - `Hub::reauth` / `LiveState::reauth` — replaces one connection's `Arc<Principal>` groups and its
     `OUT` channel names. It has to be on the `Hub` because the registry is private and the lock
     admits no `.await`; the per-device set is computed outside it, in `members::channels_changed`.
   - `LiveState::resend_latest_sa` — a two-line fold over the already-public
     `Hub::handles_for_user` and `replay::replay_latest_sa`.

   No existing type or signature changed, and no existing test needed editing.
3. **`LiveState` is installed from `stream::serve` rather than from `runtime::listen`.** The addendum
   asks for the latter. `StreamRuntime::bind` happens *inside* `stream::serve`, so doing it in
   `runtime.rs` would mean copying `serve`'s body — the listener-off branch included — into
   `runtime.rs` and leaving `stream::serve` as dead code with a doc comment calling itself the
   runtime's entry point. One line inside `serve`, immediately after the bind, is the same moment and
   the smaller change; it also left `runtime.rs` untouched, which mattered because M0-19 was editing
   that file throughout this brief. The consequence is identical: nothing is installed when the
   listener is off, and both listings answer an empty array rather than a `500`
   (`AppContext::has_live`).
4. **`identity/members.rs` gained `channels_changed` rather than only "calling the `Notifier` where
   the TODOs are".** The TODOs are in `web/api/{devices,users_groups}.rs`, and the Marti endpoint
   needs the same work; three copies of "re-authenticate then notify" would have been three places to
   get the exclusion rule wrong. It is one function in the module that already owns channel state.
5. **`/Marti/api/groups/activeForce` and `/groups/update*` were not built.** Design 04 §3.1 lists
   them as admin conveniences; the brief's endpoint list does not, and neither ATAK nor CloudTAK
   calls them. They fall into the `/Marti/api` scope's default service and answer a JSON `404`.
6. **`/contacts/all/lite` and `/contacts/all/full` were not built**, for the same reason.
7. **`sortBy`/`direction` are accepted and ignored rather than validated.** Design 04 §3.2 suggests
   validating then ignoring; `compat/contacts.md` §1 records that TAK Server accepts and never
   applies them. A `400` for a value a real server accepts breaks a client for being no worse than
   upstream.
8. **`filterGroups` is `[]`, not `null`.** `compat/contacts.md` §1 and the brief both say the empty
   array; design 04 §3.2 writes `null`. The compat file is the wire contract.

## Behaviour the wire contract fixes, asserted end to end

| Contract | Where |
|---|---|
| `groups.md` §1 `bitpos ≥ 0`, `created` `yyyy-MM-dd`, `type`, `direction` all present | `marti_channels::the_listing_carries_everything_ataks_parser_refuses_to_do_without` |
| §1 envelope `type` is `com.bbn.marti.remote.groups.Group` | same, and `marti_contract::CONTRACT` |
| §2 bare array in, numeric `created` accepted, bad elements dropped | `groups::tests::{a_body_ataks_writer_produced_is_read_whole, an_entry_we_cannot_read_is_dropped_and_the_rest_applied, an_envelope_where_a_bare_array_belongs_is_refused}` |
| §2 the new selection actually re-authenticates the live session | `marti_channels::switching_a_channel_off_stops_that_device_receiving_on_it` |
| §3 `t-x-g-c` to the other devices, `uid` suffixed with the originating `clientUid` | `marti_channels::the_device_that_made_the_change_is_the_one_not_told_about_it` |
| design D9 `t-x-g-c` with no `clientUid` reaches every device | `marti_channels::a_change_that_names_no_device_reaches_every_one_of_them` |
| §1 `sendLatestSA=true` replays over the stream, not in the response | `marti_channels::asking_for_the_latest_sa_fills_the_map_again` |
| `contacts.md` §1 bare array, `notes` never null, reachability | `marti_channels::contacts_are_a_bare_array_of_the_people_this_caller_can_reach` |
| §1 every key present | `contacts::tests::a_contact_never_omits_a_key` |
| §2 `lastStatus` is one of exactly two literals; `groups` never serialised | `contacts::tests::the_only_two_statuses_are_the_ones_ataks_enum_has` |
| §2 cache headers, `group=` 403, `secAgo < 0` 400 | `marti_channels::client_endpoints_says_connected_and_refuses_a_channel_it_cannot_read` |
| §2 disconnected devices come from `devices.last_seen_at` | `marti_channels::a_device_that_has_gone_is_listed_as_disconnected` |
| §3 `SubscriptionInfo` with every key present | `subscriptions::tests::every_key_is_present_even_when_we_have_no_value_for_it`, `marti_channels::a_subscription_listing_describes_what_is_connected` |
| `cloudtak.md` exact `application/json`, no `3xx`, JSON `404`/`405` | the 14 new `marti_contract::CONTRACT` rows, through all four of that suite's probes |

## Exit checks

Run against the working tree. Three other agents were writing in `rustak-server/src/{profiles,files}`,
`marti/{files,profiles,sync,sync_metadata}.rs`, `web/api/{profiles,profile_files,config_packages}.rs`,
`runtime.rs`, `web/server.rs` and `db/repos/resources.rs` throughout; every failure below is in one of
their files and none of them is in this brief's list. See the concurrency note.

```
$ cargo test -p rustak-server --features testing --lib -- marti::subscriptions marti::channels \
      marti::groups marti::contacts identity::members
test result: ok. 30 passed; 0 failed; 0 ignored; 0 measured; 1173 filtered out; finished in 0.37s

$ cargo test -p rustak-server --features testing
     Running unittests src/lib.rs
running 1203 tests
test result: ok. 1201 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 20.02s
     Running tests/bootstrap.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.44s
     Running tests/enroll_flows.rs
test result: FAILED. 9 passed; 1 failed
# `the_profile_endpoints_answer_no_content_until_they_have_something_to_send`:
# M2-03 left `/Marti/api/tls/profile/enrollment` and `/Marti/api/device/profile/connection`
# as 204 stubs and the M3 profiles agent has just given them content. Not this
# brief's route, file or test.

$ for t in enroll_oauth marti_contract marti_channels stream_routing stream_session stream_store
$ do cargo test -p rustak-server --features testing --test $t; done
enroll_oauth    test result: ok. 10 passed; 0 failed
marti_contract  test result: ok. 14 passed; 0 failed
marti_channels  test result: ok.  9 passed; 0 failed
stream_routing  test result: ok. 11 passed; 0 failed
stream_session  test result: ok. 10 passed; 0 failed
stream_store    test result: ok.  8 passed; 0 failed

$ cargo test -p rustak-server --features testing --doc
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo clippy --workspace --all-targets -- -D warnings
error: unnecessary closure used with `bool::then`
   --> rustak-server/src/marti/profiles.rs:398:5
# The only finding in the workspace, and it is the profiles agent's. Two of this
# brief's own were reported and fixed: a redundant `.into_iter()` in
# `marti/contacts.rs` and an `unnecessary_sort_by` in `marti/groups.rs`.
# Run with no `--features`, which is how `.github/workflows/rust.yml` runs it.

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server -p rustak-api --no-deps
error: unresolved link to `read_manifest`            (files/package.rs:13)
error: unresolved link to `files::store::ingest`     (marti/sync.rs:25)
# Both the files agent's. One of this brief's own — a redundant explicit link in
# `marti/groups.rs` — was reported and fixed.

$ cargo fmt -p rustak-server --check
# Diffs remain only in the profiles and files agents' files (18 of them). This
# brief's files were formatted individually with `rustfmt --edition 2024` rather
# than by running `cargo fmt -p rustak-server`, which would have reformatted
# somebody else's half-landed work mid-edit.

$ ./scripts/check-file-length.sh
(no output; exit 0)
# It reads `git ls-files`, so it does not yet see this brief's four new files.
# They were measured with the script's own `awk`; the table at the top of this
# file is that measurement, and the largest is `marti/contacts.rs` at 203.

$ cargo run -p rustak-server -- --config config.example.toml --check
config.example.toml is valid: rustak would listen on 0.0.0.0:8446, with data in ./data.
exit=0
```

### The node-tak interop suite

```
$ cargo build -p rustak-server && (cd rustak-ui && trunk build)
$ cd interop/node-tak && npm test

✔ Node verifies the server's chain against the internal authority
✔ the bootstrap left a working administrator session
✔ the credential CloudTAK will use is a client password
✔ every surface was probed, and every missing one names what will flip it
✔ lists contacts as a bare array                                          ← M2-06
✔ lists client endpoints in the envelope, with a status node-tak understands ← M2-06
✔ the certificate config is the ns2 document CloudTAK's parser requires
✔ signs a CSR into a client certificate for the authenticated account
✔ accepts a second enrollment for the same identity
✔ reports an upload limit CloudTAK's setup wizard can save
✔ lists stored content in the Resource envelope
✖ stores a file and hands the same bytes back
✔ lists channels in the envelope CloudTAK types against                   ← M2-06
✔ takes a channel out of a device's active set and puts it back           ← M2-06
✔ exchanges a client password for a token node-tak can parse
✔ issues exactly the body CloudTAK's client expects
✔ issues a JWT whose header and claims survive CloudTAK's parser
✔ refuses a password that is not the one minted
﹣ lists missions in the Mission envelope                    # TODO(M4)
﹣ creates, reads back and deletes a mission                 # TODO(M4)
﹣ subscribes to a mission and reports the subscription      # TODO(M4)
✔ answers a ping with t-x-c-t-r over a mutually authenticated connection
✔ answers a bearer token with a plain-text version string
✔ answers an enrolled client certificate on the mutually authenticated listener
✔ never answers a Marti route with a redirect

ℹ tests 25   ℹ pass 21   ℹ fail 1   ℹ skipped 3
```

All four M2-06 scenarios flipped from skipped to passing with **no change to `interop/node-tak`** —
the probe noticed the endpoints, exactly as `src/probe.ts` was written to. M2-03 recorded 16 passing
and 9 skipped; the four here plus the two the files agent landed make 21.

The one failure is `files.test.ts::stores a file and hands the same bytes back`, which the M3 files
agent's surface has just made reachable; it fails inside node-tak's own `fetch`
(`RequestInit: duplex option is required when sending a body`) before the request leaves the client,
and has nothing to do with channels or contacts.

`interop/node-tak/src/surfaces.ts` still carries the `TODO(M2-06)` strings for the three probes. They
are now unreachable — `unless()` only returns them when the probe fails — and the file's own comment
says nothing there needs editing when a milestone lands, so they were left alone.

## Not done here

- **`[marti]`'s `groups` surface has no admin write path.** `PUT /Marti/api/groups/activeForce` and
  `/groups/update*` are unbuilt (deviation 5). `/api/v1/users/{username}/groups` already does the
  same job for the admin UI.
- **The account-level selection lives in `kv`, not in a table.** A `user_group_state` table would let
  `members::effective_for_device` consult it directly and close the one-device-enrolled-later gap
  described above. That is a migration, which is outside this brief's file list.
- **`/Marti/api/users/{connectionId}`** (design 04 §3.1's second users route) is not built; the
  brief's list has only `/users/all`, and nothing reads either.
- **`.github/workflows/rust.yml`'s comment on the `interop-node-tak` job** still names M2-06 among
  the milestones whose scenarios skip. It is one sentence in a file another agent was editing at the
  time; whoever next touches that job should drop the `M2-06,`.
- **`Hub::set_mode` is still never called** (M1-05's own note), so `/subscriptions/all` reports every
  connection's `protocol` from the listener rather than from the negotiated encoding. The field it
  would feed is `mode`, which this brief does not serialise.

## Concurrency note

Three other agents were writing in the same tree throughout: M0-19 (`runtime.rs`, `web/server.rs`,
`stream/mod.rs`'s shutdown budget, `config/server.rs`, the Dockerfile), and two on M3
(`files/**`, `profiles/**`, `marti/{files,profiles,sync,sync_metadata}.rs`,
`web/api/{profiles,profile_files,config_packages}.rs`, `db/repos/resources.rs`).

- The crate was uncompilable for stretches of it — `pub mod profiles;` with no `profiles/mod.rs`,
  then `ProfilesRepo` not yet exported — always only in their files, confirmed each time with
  `cargo check -p rustak-server --lib`. Every number above was taken after the tree settled.
- `stream/mod.rs` was edited by M0-19 (a `drain` field on `StreamRuntime`) while this brief's one
  line was in it; both survived and the file compiles. `runtime.rs` was **not** touched by this
  brief — see deviation 3.
- `marti/mod.rs` is shared with the M3 agents, who added four modules to it. This brief's edits there
  are the module lines, `channel_routes`, and the three `serves_path` tables; nothing existing was
  reorganised.
