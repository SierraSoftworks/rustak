# M3-03 — Admin API: packages, live clients, CoT browser, settings — complete

Brief: `.claude/plan/briefs/M3-03-admin-api-packages-clients-cot.md`
Read first: `conventions.md`; design 04 §8.1 (the Packages, Clients, CoT and Settings rows) and
§8.2 (`package.rs`, `client.rs`, `cot.rs`); status `M3-01-enterprise-sync-files.md` (`files::*`,
the resources repository, the visibility rule), `M2-06-marti-groups-contacts.md` (`LiveState`),
`M1-05-stream-server.md` (`cot_store`), `M2-08-identity-api-gaps.md` (paging conventions and the
authz matrix style); `rustak-server/src/web/api/{profiles,certificates}.rs` as the pattern.

No `git`/`but` command was run.

## What was built

### DTOs (`rustak-api`)

| File | Functional lines (limit 300) | Contents |
|---|---:|---|
| `src/package.rs` | 53 | `PackageSummary`, `PackageUpdate` (+ `is_empty`) |
| `src/client.rs` | 45 | `ConnectedClient`, `IncognitoRequest`, `ClientHistoryEntry` |
| `src/cot.rs` | 27 | `CotSummary`, `CotDetail` |
| `src/settings.rs` | 99 | **appended**: `FileSettings`, `MartiSettings` |
| `src/lib.rs` | 76 | the three `pub mod` lines and five `pub use` lines |

Every new type carries a serde round-trip test plus one asserting the shape a page depends on.
`rustak-api` is unchanged in its dependencies and still compiles for `wasm32-unknown-unknown`
(`cd rustak-ui && trunk build` is green).

### Server

| File | Functional lines | Contents |
|---|---:|---|
| `web/api/packages.rs` | 235 | `routes`, list/get/patch/delete/content, `summarise`, the viewer and the free-text match |
| `web/api/packages_upload.rs` | 180 | `POST /packages`: the streaming multipart walk and the form fields |
| `web/api/clients.rs` | 228 | `routes`, list/disconnect/incognito/history, `describe` |
| `web/api/cot.rs` | 227 | `routes`, list/get/history/delete, `summarise`, the window parser |
| `web/api/settings.rs` | 86 | **added**: `files`, `put_files`, `marti` |
| `cot_store/query.rs` | 156 | `latest`, `history`, `forget` — the browser's reads and the one delete |
| `files/patch.rs` | 59 | `apply`: the admin-only metadata write |
| `files/limits.rs` | 49 | the one resolver for the upload ceiling |

Shared files touched, all additively: `web/api/mod.rs` (four `pub mod` lines, three `/settings/*`
route lines, three `.configure(…)` lines and eighteen rows in the route-table test),
`files/mod.rs` (two `pub mod` lines), `cot_store/mod.rs` (one `pub mod` line),
`cot_store/latest.rs` (two `pub(super)` visibility changes), `store/{mod,frame}.rs` (three
`pub(crate)` visibility changes), `marti/sync.rs` and `marti/version.rs` (the limit resolver — see
the deviations).

**67 new tests**: 46 unit (each in its file's single trailing column-0 `#[cfg(test)] mod tests`)
and 21 integration across two new suites. No new dependencies.

### Routes as mounted

| Method | Path | Who | Answer |
|---|---|---|---|
| GET | `/api/v1/packages?missionPackage&q&page&limit` | signed in | `[PackageSummary]`, newest first, narrowed by the visibility rule |
| POST | `/api/v1/packages` | administrator | `201` + `PackageSummary`; `multipart/form-data`, part `file`, fields `name`, `tool`, `keywords`, `groups` |
| GET | `/api/v1/packages/{hash}` | signed in | one `PackageSummary`, `404` when it is not theirs |
| PATCH | `/api/v1/packages/{hash}` | administrator | the updated `PackageSummary` |
| DELETE | `/api/v1/packages/{hash}` | administrator | `204`; every row with the hash, then the bytes nothing else wants |
| GET | `/api/v1/packages/{hash}/content` | signed in | the bytes, streamed, as an attachment |
| GET | `/api/v1/clients` | administrator | `[ConnectedClient]`; `[]` when there is no stream listener |
| DELETE | `/api/v1/clients/{uid}` | administrator | `204`; every connection claiming the uid is closed |
| POST | `/api/v1/clients/{uid}/incognito` | administrator | the `IncognitoRequest` that was applied |
| GET | `/api/v1/clients/history?secago&limit` | administrator | `[ClientHistoryEntry]`, connected or not |
| GET | `/api/v1/cot?type&callsign&group&page&limit` | signed in | `[CotSummary]`, newest relayed first |
| GET | `/api/v1/cot/{uid}` | signed in | `CotDetail` — the summary with the relayed XML |
| GET | `/api/v1/cot/{uid}/history?secago&start&end&limit` | signed in | `[CotSummary]`, newest first |
| DELETE | `/api/v1/cot/{uid}` | administrator | `204`; the latest row and every history segment |
| GET/PUT | `/api/v1/settings/files` | administrator | `FileSettings`; `409` when the file pins it |
| GET | `/api/v1/settings/marti` | administrator | `MartiSettings` |

## Decisions worth recording

### Reading is not administrative; writing is

The brief's default is "admin unless noted". Two areas are noted here, for the same reason.

A **package** listing follows the Marti visibility rule rather than the administrative default,
because it is the same row `/Marti/sync/search` serves and a caller who can browse their channels'
packages through a TAK client should see the same set through the admin API. Uploading, patching and
deleting stay administrative: a package is *delivered* to devices, and `install_on_enrollment` puts
one on every device that enrols.

**CoT** reading is likewise narrowed rather than refused: somebody diagnosing why a marker is not on
their map needs to see what the server holds for that uid, and `LatestRow::visible_to` already
answers exactly "were you entitled to receive this?" from the sender's channels *at send time*.
Deleting is administrative — it drops the latest row and every history segment, and there is no undo.

**Clients** is administrative throughout. The list names every device on the installation and the
address each is connecting from; the participant's view of the same question already exists as
`/Marti/api/contacts/all`, narrowed to what the caller can reach.

### A message that is not yours answers the same `404` as one that is not there

Both `/packages/{hash}` and `/cot/{uid}`. Telling a caller that a hash or a uid exists but is out of
their channels is an oracle over everything this server holds — M3-01 made the same call for
`/Marti/sync/content`, and the two surfaces must not disagree.

### The upload ceiling got one resolver, and the readers were moved onto it

`GET/PUT /settings/files` is in the brief, and a stored number that nothing enforces would be worse
than no setting at all: CloudTAK's setup wizard reads `/files/api/config` before it will save a
connection, so what a client is *told* and what it is *allowed* have to be the same number.

`files/limits.rs` is that number. The configuration file wins where it says anything — the same rule
`identity::settings` applies to the server's identity, and "says anything" is the same test, namely
"differs from the built-in default". A `PUT` while the file pins the value answers `409` rather than
storing something the next start would override.

Three existing readers were moved onto the resolver: `marti/version.rs::files_config` (one
expression) and `marti/sync.rs::read_body`/`files_ingest` (the limit is resolved once per upload and
the megabyte figure is threaded through instead of being re-read from the configuration). See the
deviations — these are not this brief's files.

### The history read does not open an `AppendLog`

`AppendLog::open` *recovers*: it scans the newest segment, truncates it back to the last complete
record and reconciles the index against the file. That is right for the writer, which owns the
stream, and destructive for a reader — the writer is holding the same segment open, and a reader
that truncated it would destroy a record that had already been acknowledged to a client.

So `cot_store::query::history` resolves segments through `stream_segments` and walks the files
itself. It asks for the open segment **by name** as well as by window, because the index lags the
file by up to one flush: a uid that reported a second ago has its bytes on disk and its `last_time`
still a batch behind. `store::frame::Frames` was made `pub(crate)` so that the framing is read in
one place rather than reimplemented.

A record that cannot be decoded is counted and dropped rather than failing the request: one corrupt
frame must not hide the rest of a device's history. The count is logged.

### A history entry's channels describe the sender, not the message

The segments hold only the payload, so a history entry's `groups` is the channel list from the
**latest** row for that uid. A message sent before a membership changed may therefore be listed under
a channel it did not actually reach. The alternative — storing the bit vector per record — is a
schema change to the segment format for a display field, and the *gate* is still correct: the latest
row is what decides whether the caller may see this uid's history at all.

### The team and the role are parsed out of the stored XML

`cot_latest` has `callsign` as a column and no `team`/`role`; design 04 §7 sketched them, migration
`0004` did not add them. Rather than add a migration for two display fields, `summarise` parses the
stored XML — which is the bytes the recipients were sent, so `<__group>` inside it is the same answer
without a column that could disagree with the message it describes. The parse is bounded by the page
size (at most 200 small documents) and a row whose XML will not parse still lists, because the uid,
the type and the times are columns.

### A filtered page may be short, and that is the same trade M3-01 made

The type, callsign and `missionPackage` predicates are decided in SQL. The channel rule and the
free-text search are not — a caller's channels are a bit vector the row cannot be joined against, and
a substring over two columns and a child table is not what the index is for at these volumes. So a
page is narrowed after it is read and the next page starts where this one did. `files::search::run`
already worked this way; doing it differently in one of the two would be the surprise.

### `PATCH /packages/{hash}` writes every row with the hash

A hash may back several rows — the same photograph attached to two map items, the same package
uploaded by two people. The Marti metadata writes (`set_keywords`, `set_expiration`, `set_field`)
already work that way, and keeping one row's name in step with another's is the operator's business.
`files/patch.rs` writes the keywords through the repository and the columns through one dynamic
`UPDATE`, and keeps `is_mission_package` in step with the `missionpackage` keyword — a client's
data-package browser reads the indexed column, so leaving it set after the keyword went would keep a
file listed that no longer says it belongs there.

### An expiry travels as epoch milliseconds, and a negative clears it

TAK stores it that way with `-1` meaning "never", every client that reads it reads that spelling, and
the column holds exactly those bits. `Upload::into_resource` already filtered negatives; `PackageUpdate`
does the same, so one representation exists rather than two with a conversion at each end.

### Incognito is *set* here and *toggled* there

`POST /Marti/api/subscriptions/incognito/{uid}` toggles, because the client asking is the one that
knows what it is now. An operator's page does not, so `POST /api/v1/clients/{uid}/incognito` takes
`{on}`. The flag belongs to the connection rather than to the device row, which is why both it and
`DELETE /clients/{uid}` answer `404` when nothing is connected under that uid.

### Disconnecting is not revoking

`DELETE /clients/{uid}` closes the sockets; the device may reconnect with the same certificate a
moment later. Taking access away is `POST /api/v1/certificates/{id}/revoke`, which closes the
connection as a *consequence* through the hook the stream listener registers. Two verbs, two
outcomes, and the audit entry names which one happened.

### `/clients` answers `[]` rather than `503` when there is no listener

An installation with `[stream.tls] enabled = false` has nothing connected, which is the true answer
and the one a page can render. The two endpoints that *act* on a connection answer `503` instead,
because "there is no registry" and "that uid is not connected" are different things to be told when
you have just clicked Disconnect.

### `stream/live.rs` was not edited

The brief allows an additive change there. None was needed: `LiveState::hub()` is public and `Hub`
already answers every question this surface asks — `snapshot`, `handles_for_uid`, `principal`,
`set_incognito`, `is_incognito`. Another agent is actively adding `ConnectionWatcher` to that file,
so not touching it was also the cheaper merge.

## Deviations from the brief

1. **`web/api/packages_upload.rs` is a fifth file** beside the brief's
   `{packages,clients,cot,settings}`. `packages.rs` reached 235 functional lines and the streaming
   multipart walk is another 180; one file would have been over the limit, and it would have put the
   reading of a form and the reading of four hundred megabytes under one set of imports. M3-01 split
   `marti/sync.rs` the same way for the same reason.
2. **`files/limits.rs` and `files/patch.rs`** — the brief grants "read helpers you need in `files/`".
   `limits.rs` is a read helper (plus the one `save` the `PUT` needs); `patch.rs` is a *write*
   helper, which the brief does not name. It is here rather than in `db/repos/resources/mod.rs`
   because that file is at 259 of 300 functional lines and the admin-only columns (`name`, `groups`,
   `install_on_enrollment`) are not part of the Marti metadata contract the repository documents.
3. **`marti/sync.rs` and `marti/version.rs` were edited** — three call sites, about ten lines, to
   read the resolved upload ceiling instead of the configuration value directly. These belong to
   M3-01/M2-04, both complete. Without it the `PUT` in item 4 of the brief would store a number
   nothing reads. Whoever owns those files next should know the resolver exists.
4. **`store/frame.rs` and `store/mod.rs` visibility** — `mod frame` → `pub(crate) mod frame`, and
   `Frames`/`Frames::new` from `pub(super)` to `pub(crate)`. Three tokens, so that the read-only
   history walk uses the same framing the writer does rather than reimplementing varint parsing.
   `cot_store/latest.rs` likewise: `LatestRow::COLUMNS` and `LatestRow::from_row` are now
   `pub(super)` so the sibling `query` module can select the same columns.
5. **`FileSettings` and `MartiSettings` live in `rustak-api/src/settings.rs`**, which is not in this
   brief's ownership list (`{package,client,cot}.rs`). They are settings, that is where settings
   live, and putting them anywhere else would have left the UI with two places to look. They were
   **appended** below M2-10's `TlsStatus`, which was being added at the same time; both are present
   and both round-trip. If M2-10's next write loses them, the two structs and their two tests are the
   whole change.
6. **`GET /settings/marti` is read-only**, as the brief specifies (`GET` only). Both values change
   how URLs handed to *peers* resolve and whether every Marti response is readable by any page an
   operator's users visit; those are decisions for the file an operator deploys.
7. **`ConnectedClient` carries `port` beside `ip`**, which design 04 §8.2 does not list. Two
   connections from one host are otherwise indistinguishable in the list, which is exactly the case
   an operator is looking at when they open it.
8. **`CotSummary` carries `received_at`** beside `time`, which §8.2 does not list. The listing is
   ordered by it — a client's own `time` is whatever its clock said — so a page that cannot show it
   cannot explain its own ordering.
9. **`PackageSummary` carries `mission_package`**, the indexed copy of the `missionpackage` keyword.
   §8.2 lists `mission_name`; both are present. A page filtering on `missionPackage=true` needs to
   render why a row is in the result.
10. **No live-connection integration suite.** `/clients` with a real socket needs the stream harness
    (`tests/stream_support`) *and* the web application over the same context, and the harness
    installs neither the signing keys nor the rate limiter a session needs. The behaviour is covered
    instead by two unit tests in `web/api/clients.rs` that build a real `Hub`, register a real
    `Subscription`, feed it a real situational-awareness event and assert what `describe` makes of
    it — which is the piece the endpoint is, with only the socket faked.

## Notes for the briefs that follow

- **M3-04 / the UI agent**: the DTOs are `rustak_api::{PackageSummary, PackageUpdate,
  ConnectedClient, IncognitoRequest, ClientHistoryEntry, CotSummary, CotDetail, FileSettings,
  MartiSettings}`. Field names are snake_case on the wire, as everywhere else in that crate; the one
  rename is `CotSummary::kind` → `"type"`. A package upload is `multipart/form-data` with the file in
  a part named `file`; `keywords` and `groups` may be repeated or comma-separated. `GET /clients`
  answers `[]` rather than failing on an installation with no stream listener, so the page needs no
  special case for it.
- **Anyone adding a route under `/api/v1`**: add the row to `PROTECTED` in `web/api/mod.rs`'s
  route-table test, or the gate stops being checked for it.
- **Anyone reading the upload limit**: call `files::limits::limit_mb` (or `limit_bytes`), never
  `config.marti.upload_size_limit_mb`. The configuration value is one of two inputs.
- **`cot_store::query::forget`** is what a mission or device deletion should call if it ever needs to
  forget a uid's CoT: it takes the latest row and unlinks the segments, file before index row.
- **The `cot_latest` table has no `team`/`role` columns.** If a later brief adds them, `web/api/cot.rs`
  and `web/api/clients.rs` each have one helper (`summarise`, `team_and_role`) to switch over, and
  the XML parse can go.

## Exit checks

Run against the final tree. **Six other briefs were being written throughout**, and three of them
(M2-10's `pki/acme/**`, M6-01's `plugins/**` + `web/api/services.rs` + `jobs/service_health.rs`, and
whoever is refactoring `db/queue.rs`) have untracked modules that did not compile for most of the
window. The whole-workspace runs were retried on a loop for half an hour and never found a moment
when the tree was simultaneously consistent, so what is recorded below is **per crate and per
target**, with each remaining finding attributed. Everything this brief owns or touched is green;
nothing below is a finding in one of its files.

The one full `cargo test -p rustak-server --features testing --lib` that did catch a consistent
moment is the first entry, and it is green with all 46 of this brief's new unit tests in it.

```
$ ./scripts/check-file-length.sh
(no output, exit 0)
# The script reads `git ls-files`, so it does not yet see this brief's untracked
# files. They were counted with the same `awk` it uses; the tables above have the
# numbers, and the largest is `web/api/packages.rs` at 235 (limit 300).

$ cargo fmt --all --check
# One diff, in `rustak-server/tests/services_flow.rs` — M6-01's untracked suite.
# Every file this brief owns or touched was checked individually with
# `rustfmt --edition 2024 --check` and is clean.

$ cargo test -p rustak-api
test result: ok. 138 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
   Doc-tests rustak_api
test result: ok. 0 passed; 0 failed

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-api --no-deps
 Documenting rustak-api v0.1.0
   Generated target/doc/rustak_api/index.html

$ cd rustak-ui && trunk build
2026-09-18T18:44:03.130691Z  INFO ✅ success

$ cargo test -p rustak-server --features testing --lib
     Running unittests src/lib.rs (target/debug/deps/rustak_server-…)
test result: ok. 1563 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 23.11s

$ cargo test -p rustak-server --features testing --test api_v1_packages
running 9 tests
test an_upload_is_stored_with_what_the_form_said_about_it ... ok
test an_upload_with_no_file_part_is_refused_rather_than_stored_empty ... ok
test only_an_administrator_may_upload_change_or_remove_a_package ... ok
test a_package_in_a_channel_somebody_is_not_in_is_not_there_for_them ... ok
test a_listing_pages_narrows_and_caps_what_it_is_asked_for ... ok
test a_patch_writes_every_field_and_a_change_that_does_nothing_is_refused ... ok
test the_content_is_handed_back_byte_for_byte_and_then_deleted ... ok
test a_multi_megabyte_upload_reaches_the_store_without_being_collected ... ok
test an_upload_past_the_configured_ceiling_is_refused_before_it_is_read ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.49s

$ cargo test -p rustak-server --features testing --test api_v1_live
running 12 tests
test a_server_with_no_stream_listener_answers_an_empty_client_list ... ok
test disconnecting_or_hiding_a_uid_nothing_is_connected_under_is_not_found ... ok
test the_client_surface_is_administrative_throughout ... ok
test the_client_history_lists_devices_that_are_not_connected ... ok
test the_cot_browser_lists_the_latest_message_per_uid_and_pages_it ... ok
test one_message_carries_its_xml_and_a_uid_nobody_stored_is_not_found ... ok
test a_message_published_into_a_channel_somebody_is_not_in_is_not_there_for_them ... ok
test history_is_newest_first_and_a_backwards_window_is_refused ... ok
test deleting_a_stored_message_is_administrative_and_takes_the_row ... ok
test the_upload_limit_can_be_read_changed_and_is_then_enforced ... ok
test a_limit_the_configuration_file_pins_cannot_be_changed_here ... ok
test the_marti_settings_are_administrative_and_read_only ... ok
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.76s

$ cargo test -p rustak-server --features testing \
      --test sync_contract --test profiles_contract --test marti_contract
# The three suites this brief's shared-file edits could have broken: the
# `/Marti/sync` uploads now resolve their ceiling through `files::limits`, and
# `/files/api/config` reports the same number.
tests/sync_contract.rs      test result: ok. 14 passed; 0 failed
tests/profiles_contract.rs  test result: ok. 14 passed; 0 failed
tests/marti_contract.rs     test result: ok. 14 passed; 0 failed

$ cargo clippy -p rustak-api --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 40s
# Clean.

$ cargo clippy -p rustak-server --features testing --lib -- -D warnings
# Clean for every file this brief owns or touched; the only findings were two
# `nonminimal_bool` lints in `plugins/auth.rs`, which is M6-01's untracked
# module and was mid-edit throughout.

# NOT RUN: `cargo test --workspace` and `cargo clippy --workspace --all-targets`.
# Both were retried on a loop for half an hour and never found a moment when
# every crate compiled at once — `rustak-client` (a dev-dependency of
# `rustak-server`) was mid-reqwest-upgrade, and `plugins/**`, `pki/acme/**`,
# `db/queue.rs` and `jobs/service_health.rs` each broke and healed several
# times. The orchestrator should run both once the in-flight briefs land.

$ env RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server --no-deps
# Clean for every file this brief owns or touched. Three broken intra-doc links
# of mine were found by this check and fixed (`cot_store/query.rs`,
# `web/api/packages.rs`, `files/limits.rs`). The remaining findings are in
# `pki/acme/**`, `plugins/**`, `web/api/services.rs`, `jobs/acme_renew.rs` and
# `config/validate.rs` — all untracked or in-flight work belonging to M2-10 and
# M6-01.
```

### What the contract tests actually assert

**`tests/api_v1_packages.rs` (9 scenarios)**

- **An upload is stored with what the form said** — the name, the filename it arrived under, the
  tool, the size, the submitter, and `keywords` read from one comma-separated field.
- **A body with no `file` part is a `400`**, rather than a row over an empty blob.
- **The write verbs are administrative** — `POST`, `PATCH` and `DELETE` all `403` for an ordinary
  session, over the same stored package.
- **A package in a channel somebody is not in is a `404`** for both `GET /{hash}` and
  `GET /{hash}/content`, and is absent from their listing.
- **Paging** — `limit=2` gives two, `page=2` gives the remainder, `limit=1000000` gives a page
  rather than a refusal, `missionPackage=true` gives the three that carry the keyword, and
  `q=package%203` matches one name without regard to case.
- **A patch writes every field** and is refused for an empty body and for a blank name.
- **The content is byte-for-byte** with the stored `Content-Type`, and the package then deletes,
  `404`s on the next read and `404`s on a second delete.
- **A 3 MB upload reaches the store intact** and the store's temporary directory is empty
  afterwards — which is what would not hold if the body were collected and written in one go.
- **An upload past the ceiling** is refused with the limit named.

**`tests/api_v1_live.rs` (12 scenarios)**

- **An installation with no listener** answers an empty client list, and the two acting endpoints
  say so rather than pretending.
- **The whole client surface is `403` for an ordinary session.**
- **The history lists a device that is not connected**, with the team out of its last stored
  message, `platform version` out of the device row and `connected: false`.
- **The CoT listing** is newest first, narrows by `type` and `callsign` together, and pages without
  overlap.
- **One message carries its XML**, and a uid nobody stored is a `404`.
- **A message published into a channel the caller does not hold** is absent from their listing and a
  `404` by uid, while an administrator sees it.
- **A history window that ends before it starts is a `400`.**
- **Deleting a stored message** is `403` for an ordinary session, `204` for an administrator and
  `404` the second time.
- **The upload limit** reads `400`, writes `25`, is then what `/files/api/config` advertises, and
  refuses `0`.
- **A limit the configuration file pins** reports `from_config_file` and refuses a `PUT` with `409`.
- **The Marti settings** are administrative and report the configured host.
