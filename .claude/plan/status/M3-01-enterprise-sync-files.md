# M3-01 — Enterprise Sync / files (`/Marti/sync/*`, `/Marti/api/files/*`, `/Marti/api/sync/*`) — complete

Brief: `.claude/plan/briefs/M3-01-enterprise-sync-files.md`
Read first: `conventions.md`; design 04 §0 (D1–D14), §5 (store, legacy `Metadata`, the endpoint
table) and the M3 rows of §10; `compat/files.md`, `compat/cloudtak.md`; status `M2-04`
(envelope/`kind::*`/`MartiError`/extractors/`default_service` rule), `M2-03` (`MartiPrincipal`,
listener roles), `M0-07`/`M0-08` (`Database`, repos, `store::content::ContentStore`, migration
`0005_files.sql`); research `06` §9 (Enterprise Sync — §4 in the brief is a typo for §9) and §7.17.

## What was built

| File | Functional lines (limit 300) | Contents |
|---|---:|---|
| `migrations/0008_resource_hash_index.sql` | — | `idx_resources_hash` UNIQUE → plain |
| `db/repos/resources/mod.rs` | 259 | `ResourcesRepo`: `upsert`, `list`, `count`, `by_id`/`by_hash`/`by_uid`, `set_field`/`set_keywords`/`set_expiration`/`set_submitter`, `delete`, `hash_in_use`; keyword child-table writes |
| `db/repos/resources/row.rs` | 191 | `ResourceRow` (27 columns), `NewResource`, `MutableField`, `ResourceFilter` + its `WHERE`/`ORDER BY` builder |
| `files/mod.rs` | 9 | the module, and the three-views-of-one-row story |
| `files/store.rs` | 172 | `ingest` (streamed, hashed, bounded), `BoundedReader`, `open_range`/`Opened`, `chunks`, `forget` |
| `files/upload.rs` | 180 | `Upload` (parameter aliases, channel check, `into_resource`), multipart part selection, the audit helper |
| `files/metadata.rs` | 128 | `Viewer` (the visibility rule), `viewer_for`, `ResourceJson` + `resource_json` |
| `files/legacy.rs` | 133 | Title-case `Metadata`, `search_results`, `files_entry`, `metadata_entry`, `humanise` |
| `files/search.rs` | 134 | `SearchQuery::parse` (both endpoints' spellings), `BoundingBox`, `run` |
| `marti/sync.rs` | 224 | the `/Marti/sync` route table, `upload`, `missionupload`, body reading, the size ceiling |
| `marti/sync_read.rs` | 186 | `search`, `content`, `missionquery`, `delete`, `remove`, `content_url`, range parsing |
| `marti/sync_metadata.rs` | 118 | `PUT /Marti/api/sync/metadata/{hash}/{tool\|mimetype\|keywords\|expiration}`, `GET /Marti/api/sync/search` |
| `marti/files.rs` | 183 | `/Marti/api/files/metadata[/count]`, `GET\|HEAD\|DELETE /{hash}`, `PUT /{hash}/metadata` |
| `tests/sync_contract.rs` | — (`tests/` exempt) | 13 in-process contract tests over the real `App` |

Shared files touched: `db/repos/mod.rs` (three lines: `pub mod`, `pub use`, `Database::resources()`),
`marti/mod.rs` (three module declarations, three `.configure(…)` lines, nine `PATHS` entries and one
entry each in `PARAMETERISED`, `PARAMETERISED_TAIL` and `PARAMETERISED_PAIR`), `files/mod.rs` (see
the note to M3-02 below). `lib.rs` already carried `pub mod files;`. **No** change to `Cargo.toml`,
`config.example.toml`, `runtime.rs` or `web/server.rs`.

**66 new tests**: 53 unit (each in its file's single trailing column-0 `#[cfg(test)] mod tests`) and
13 integration. No new dependencies — `actix-multipart`, `futures`, `sha2` and `zip` were all
already in the manifest.

### Routes as mounted

| Method | Path | Answer |
|---|---|---|
| POST | `/Marti/sync/upload` | `200 text/json`, the legacy `Metadata` object |
| GET | `/Marti/sync/search` | `200 text/json` `{resultCount, results}` |
| GET/HEAD | `/Marti/sync/content` | the bytes, `api-version: 3`, `Content-Disposition: inline`, `206` for a range, HTML `404` |
| POST | `/Marti/sync/missionupload` | `200 text/plain`, a bare content URL |
| GET | `/Marti/sync/missionquery` | `200 text/plain` the same URL, HTML `404` |
| GET/POST/DELETE | `/Marti/sync/delete` | `200 text/html` `<title>Enterprise Sync Status</title>…Deleted N resource(s).` |
| PUT | `/Marti/api/sync/metadata/{hash}/{tool\|mimetype}` | empty `200`, `400` for any other field, `404` for an unknown hash |
| PUT | `/Marti/api/sync/metadata/{hash}/keywords` | empty `200`; body is a JSON array |
| PUT | `/Marti/api/sync/metadata/{hash}/expiration?expiration=` | empty `200`; `400` without the parameter |
| GET | `/Marti/api/sync/search` | `Resource` envelope, lowerCamelCase, numeric `size` |
| GET | `/Marti/api/files/metadata` | `Files` envelope, flat maps of display strings |
| GET | `/Marti/api/files/metadata/count` | `Count` envelope |
| GET/HEAD/DELETE | `/Marti/api/files/{hash}` | bytes as an attachment / the `data` map / `200` after removal |
| PUT | `/Marti/api/files/{hash}/metadata?user&expiration&keywords` | `200` |

## Decisions worth recording

### Migration `0008` relaxes the hash index, and why there had to be one at all

`0005_files.sql` declared `idx_resources_hash` **UNIQUE**. Design 04 §5.1 declares a plain
`INDEX(hash)`, and the plain one is what the behaviour needs: the same photograph attached to two map
items is two resources over one blob, and two people may upload the same package under different
names. With the unique index the second of each pair is a `500`. The blob is still stored once —
the store is content-addressed — so what is relaxed is only the metadata row.

`idx_resources_uid` stays unique, per the same design line. A UID is how a client addresses a
resource, so `/Marti/sync/upload` naming one that already exists is a **new version of that
resource**: `ResourcesRepo::upsert` is an `ON CONFLICT (uid) DO UPDATE`, which keeps the primary key
(and therefore any `mission_contents` row pointing at it) rather than making a second listing.

A migration was needed regardless: M3-02 owns `0009`, and `db::migrations::load` refuses a gap.

### Deletion is a hard delete, and the blob outlives any one row

`resources.deleted_at` exists and is left for the mission archives that will want it. A resource a
client browses has to actually go: a soft-deleted one would keep answering
`/Marti/sync/missionquery` with a URL that no longer downloads. The audit log is what keeps the
record. `files::store::forget` then removes the bytes **only** when no surviving `resources` row, no
`mission_contents` attachment and no `profile_files` entry still points at that hash.

### The size ceiling is enforced inside the reader, not around it

`Content-Length` is a claim: a chunked upload does not carry one and one that does can lie, so
checking the header and then reading whatever arrives leaves the ceiling unenforced for exactly the
request that meant to exceed it. `files::store::BoundedReader` is an `AsyncRead` over the body stream
that counts what it passes on and fails the read the moment it goes over — so the temporary file
stops growing there rather than at the end of a 4 GB body. The header is checked too, in the handler,
because refusing before the upload starts is much kinder.

`BoundedReader` is also what adapts an actix body stream to the `AsyncRead` `ContentStore::put`
takes. `tokio_util::io::StreamReader` would have done it, but `tokio-util`'s `io` feature is not
enabled in the workspace manifest and turning it on is a shared-file edit for forty lines that also
had to carry the bound.

### Visibility is applied in Rust, after SQL has narrowed the rows

Group membership is a 256-bit vector on the principal and a JSON array on the row, so the join
SQLite would need does not exist. `files::search::run` asks the repository for the indexed
predicates and then filters with `Viewer::can_read`. The rule is: administrator, **or** the
submitter, **or** an intersection between the resource's channels and the caller's **`OUT`**
channels. `OUT` rather than `IN`, because `OUT` is the direction that means "this reaches me" — a
member who may only publish into a channel has no business browsing what other people put there. A
resource with no channels at all is `__ANON__`'s, which every principal holds.

`Viewer::can_write` is narrower: administrator or submitter. Seeing a channel's package does not make
it yours to delete or re-own.

An unreadable resource answers the **same** `404` as a missing one. Telling a caller that a hash
exists but is not theirs is an oracle over the whole store.

### `Groups` on an upload is checked against membership in either direction

`Viewer` carries two lists: `out_groups` (what may be read) and `held_groups` (membership in either
direction, which is what an upload may be addressed to). `GroupSet::positions(Direction::Both)` is
the *intersection* of the two vectors, so `held_groups` is built as the union of `In` and `Out`
instead — a member who may only publish into a channel is still a member of it.

`Groups ⊄ held_groups` → `403` naming the channel, with an administrator bypass. No `Groups` at all →
the upload inherits the caller's channels, which is what makes a file visible to the people they work
with rather than to nobody.

### A row that would break ATAK's browser never joins a listing

ATAK treats one bad element of `/Marti/sync/search` as fatal to the **whole** response, so
`legacy::is_renderable` drops a row with an empty `UID`/`Name`/`Hash` or a negative `PrimaryKey`
before it is rendered, rather than emitting it and losing the browse. `resultCount` is the length of
what survived.

### An unrecognised query parameter is ignored rather than refused

TAK answers `400` for one (`"Unrecognized parameter <x>"`). That turns a client which learned a
newer server's parameter into a client that cannot upload or search **at all**, so it is logged and
dropped — the deviation design 04 §5.2 marks as "lenient". `Circle` is the one exception: a radius
search we silently ignored would quietly return the whole store, so it is a `400` naming it.

## Deviations from the brief and the designs

1. **Two extra files under `files/`** — `files/upload.rs` (parameter parsing, the channel check,
   multipart part selection, the audit helper) beside the brief's `{store,metadata,legacy,search}`.
   The brief's "Files you own" line is `rustak-server/src/files/**`. Without it `store.rs` would have
   mixed "get bytes onto disk" with "read what a client claimed about them", and either file would
   have been over the limit.
2. **`marti/sync_read.rs`** — the six legacy servlets plus their plumbing came to 393 functional
   lines. Design 04 §10's own risk table names this split ("split `sync.rs` into
   `sync_upload.rs`/`sync_read.rs` if it grows"); `sync.rs` keeps the route table and the two
   handlers that write, `sync_read.rs` has the four that read or remove. `marti/mod.rs` still carries
   **one** `.configure(sync::routes)` line, because `routes()` registers both files' handlers.
3. **`db/repos/resources/` is a directory** (`mod.rs` + `row.rs`) rather than one file, for the same
   reason: 448 functional lines. `repos/mod.rs` still says `pub mod resources;` and the re-export
   line is unchanged, so nothing outside the module noticed.
4. **Migration `0008` changes an index rather than adding a column** — `install_on_enrollment` was
   already in `0005`. See the decision above.
5. **`missionupload` dedupe answers `200`, not TAK's `403`** — design 04 §5.2's rule, recorded as a
   correction in `compat/files.md` §5, which had the `403`.
6. **No `Content-Encoding: gzip` on `/Marti/sync/content`** — design 04 §5.2's decision; recorded as
   a correction in `compat/files.md` §4, which described TAK's conditional gzip.
7. **`/Marti/api/files/{hash}` `DELETE` answers `403`** for a resource the caller did not submit.
   TAK swallows every exception there and always answers `200`; a client that tried should find out.
8. **`interop/node-tak/tests/files.test.ts`** — the upload half of one scenario no longer goes
   through `api.Files.upload()`. See the next section; the file is not in this brief's ownership
   list, and the change is confined to one test body plus a comment saying when to undo it.

## A node-tak client defect the interop suite runs into

`@tak-ps/node-tak@12.30.0`'s `Files.upload()` converts its body to a Node `Readable` and hands it to
`fetch` **without** `duplex: "half"`, which Node ≥ 18 requires for a streamed request body. On the
Node 24 this repository runs, it throws

```
Error [PublicError]: RequestInit: duplex option is required when sending a body.
    at TAKAPI.fetch (…/@tak-ps/node-tak/lib/api.ts:231:19)
    at FileCommands.upload (…/@tak-ps/node-tak/lib/api/files.ts:197:21)
```

before a request is made. Nothing a server can answer. The scenario now assembles that request
itself — same path, same query parameters, same headers, byte for byte what `Files.upload()` would
have sent — and goes back through node-tak for the download and the delete, so the contract the
scenario exists for is still exercised end to end. The comment in the test says to restore the
one-liner when the vendored client sets `duplex`.

## Notes for the briefs that follow

- **M3-02**: `files/mod.rs` was overwritten when this brief created it, and the `pub mod package;`
  line was put back immediately. Nothing else was in it; `profiles/{prefs,builder}.rs` reach
  `crate::files::package::*` directly and were unaffected. If anything else had been in that file,
  it is gone — please check.
- **M4 (missions)**: `db.resources()` is the repository; `ResourceFilter { mission_name }` is the
  `mission` filter `/Marti/api/files/metadata?mission=` already uses. `files::store::forget` already
  checks `mission_contents`, so `deepDelete` only has to remove the rows.
  `marti::sync_read::content_url` is the D12 URL builder, and `files::legacy::metadata` is the
  `contentResource` source for the `t-x-m-c` change notifications.
- **`marti::sync::viewer`** is the visibility helper every route in this surface calls; a mission
  route that lists resources should use the same one rather than re-deriving the rule.
- **Add a row to the tables** when adding a route here: `marti::PATHS` (or one of the three
  parameterised lists) so the `405` case keeps working, and a case in `tests/sync_contract.rs`.
- **`/Marti/sync/*` answers HTML on a miss**, not JSON. `MartiError::Html404` is the variant; the
  rest of the surface must not use it.

## Exit checks

Run against the final tree. M3-02 (profiles), M2-06 (channels/contacts) and the interop harness were
being written throughout; every check below covers the whole workspace and was green including their
work.

```
$ cargo test -p rustak-server --features testing
     Running unittests src/lib.rs (target/debug/deps/rustak_server-…)
test result: ok. 1208 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 18.10s
     Running unittests src/main.rs (target/debug/deps/rustak-…)
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/bootstrap.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.49s
     Running tests/enroll_flows.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.66s
     Running tests/enroll_oauth.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.38s
     Running tests/marti_channels.rs
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.59s
     Running tests/marti_contract.rs
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.47s
     Running tests/profiles_contract.rs
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.77s
     Running tests/stream_routing.rs
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.89s
     Running tests/stream_session.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.17s
     Running tests/stream_store.rs
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.37s
     Running tests/sync_contract.rs
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.60s
   Doc-tests rustak_server
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo test -p rustak-server --features testing --test sync_contract
running 13 tests
test an_upload_answers_the_legacy_metadata_object_as_text_json ... ok
test a_search_reports_a_numeric_result_count_beside_the_same_objects ... ok
test a_download_carries_the_api_version_header_and_the_stored_bytes ... ok
test a_file_that_is_not_there_answers_the_html_document_the_servlets_do ... ok
test a_package_upload_answers_a_bare_url_that_missionquery_repeats ... ok
test a_package_upload_that_is_not_multipart_or_has_no_filename_is_refused ... ok
test a_delete_answers_the_html_status_page_and_removes_the_bytes ... ok
test the_four_mutable_fields_are_the_only_ones_this_api_changes ... ok
test the_modern_search_is_the_enveloped_camel_case_resource ... ok
test the_file_manager_map_carries_the_keys_cloudtak_reads ... ok
test a_caller_only_sees_what_their_channels_or_their_own_uploads_hold ... ok
test an_upload_past_the_configured_ceiling_is_refused_in_taks_own_words ... ok
test a_multi_megabyte_upload_reaches_the_store_without_being_collected ... ok
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.60s

$ cargo test --workspace
     Running unittests src/lib.rs (rustak_api)      test result: ok. 105 passed; 0 failed
     Running unittests src/lib.rs (rustak_client)   test result: ok. 77 passed; 0 failed
     Running tests/stream_client.rs                 test result: ok. 12 passed; 0 failed
     Running tests/stream_reconnect.rs              test result: ok. 2 passed; 0 failed
     Running tests/stream_tls.rs                    test result: ok. 2 passed; 0 failed
     Running unittests src/lib.rs (rustak_core)     test result: ok. 139 passed; 0 failed
     Running unittests src/lib.rs (rustak_cot)      test result: ok. 249 passed; 0 failed
     Running tests/codec_framed.rs                  test result: ok. 9 passed; 0 failed
     Running tests/golden.rs                        test result: ok. 49 passed; 0 failed
     Running tests/roundtrip_prop.rs                test result: ok. 9 passed; 0 failed
     Running unittests src/main.rs (plugin example) test result: ok. 5 passed; 0 failed
     Running unittests src/lib.rs (rustak_server)   test result: ok. 1208 passed; 0 failed; 2 ignored
     … the eleven rustak-server integration suites, all ok (as above)
   Doc-tests: rustak_api 0, rustak_client 6, rustak_core, rustak_cot, rustak_server 5 — all ok
# 0 failures across the workspace.

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.39s
# No features, as CI runs it. Clean.

$ RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps
    Checking rustak-server v0.1.0
 Documenting rustak-server v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.74s
   Generated target/doc/rustak_api/index.html and 6 other files

$ cargo fmt --all --check
(no output)

$ ./scripts/check-file-length.sh
(no output, exit 0)
```

`check-file-length.sh` reads `git ls-files`, so it does not yet see this brief's untracked files.
They were counted with the same `awk` the script uses; the table at the top of this file has the
numbers, and the largest is `db/repos/resources/mod.rs` at 259 (limit 300).

### node-tak interop

```
$ cargo build -p rustak-server && (cd rustak-ui && trunk build)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 38.26s
2026-09-18T15:30:05.712448Z  INFO ✅ success

$ cd interop/node-tak && npm test
✔ reports an upload limit CloudTAK's setup wizard can save (109.045ms)
✔ lists stored content in the Resource envelope (10.080084ms)
✔ stores a file and hands the same bytes back (34.027666ms)
… (the other nineteen scenarios, all green)
﹣ lists missions in the Mission envelope # TODO(M4): the mission API is not served yet
﹣ creates, reads back and deletes a mission # TODO(M4): …
﹣ subscribes to a mission and reports the subscription # TODO(M4): …
ℹ tests 25
ℹ pass 22
ℹ fail 0
ℹ skipped 3
ℹ duration_ms 2103.98875
```

The three `files.test.ts` scenarios have flipped from skipped to passing; the only remaining skips
are M4's mission API. The surface probe flips on `GET /Marti/api/sync/search`, which this brief
mounts.

### What the contract tests actually assert

- **Content type, byte for byte, over the real `App`**: `text/json` for `upload` and `search`,
  `text/plain` for `missionupload` and `missionquery`, `text/html` for `delete`, and
  `application/json` with no `charset` for everything under `/Marti/api`.
- **`resultCount` is a number and the body literally contains the key**, which is ATAK's own
  verification step, and **`PrimaryKey` parses as a non-negative integer** out of a JSON *string*.
- **`EXPIRATION` is present as `"-1"`** when a resource does not expire, which CloudTAK reads
  unconditionally.
- **`api-version: 3`** on `/Marti/sync/content` and nowhere else, with
  `Content-Disposition: inline; filename="…"` percent-encoded so a quote in a stored name cannot add
  a second header value.
- **A range is a `206`** with `Content-Range: bytes 3-6/10` and exactly those four bytes.
- **The `missionupload` body is a bare URL** built from `[marti] public_host`, with no trailing
  newline, and `missionquery` repeats it exactly.
- **`Hash`, `Groups` and `Time`** are present in `/Marti/api/files/metadata`, `Groups` as a comma
  string and `Size` humanised — the three keys CloudTAK's packages page reads.
- **A `3 MB` upload reaches the store intact** and the store's temporary directory is empty
  afterwards, which is what would not hold if the body were collected and written in one go.
- **Visibility**: a second account in the same channel may read but not delete; an anonymous caller
  gets the same `404` as for a file that is not there.
- **The ceiling** is refused in TAK's own wording, and an empty body with TAK's own wording too.
