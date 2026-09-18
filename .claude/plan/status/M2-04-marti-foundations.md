# M2-04 — Marti API foundations: envelope, errors, extractors, version/config, stubs — complete

Brief: `.claude/plan/briefs/M2-04-marti-foundations.md`
Read first: `conventions.md`; `compat/cloudtak.md` (first), `compat/README.md`, `compat/contacts.md`,
`compat/files.md` §1, `compat/missions.md` §2; `design/04-marti-api-missions-files-profiles.md`
§0 (D1–D14), §1, §2, §9; status files `M0-06`, `M0-11`, `M0-12`, `M2-02`.

## What was built

`rustak-server/src/marti/` — ten files, plus one config section and one integration suite.
`lib.rs` was touched once (`pub mod marti;`) and `web/server.rs` once (the mount).

| File | Functional lines (limit 300) | Contents |
|---|---:|---|
| `marti/mod.rs` | 141 | `services(role)`, the two scopes, registration order, `unmatched`/`serves_path`, the route-table contract tests |
| `marti/response.rs` | 208 | `ApiResponse<T>`, `kind::*` (26 constants), `ok`/`created`/`status`/`versioned`/`bare_json`/`bare_json_with`/`text`/`text_json`/`xml`/`html`/`no_store`, node id |
| `marti/error.rs` | 139 | `MartiError` (12 variants), `ErrorResponse`, the status/code/message table, the two HTML documents, `From<Error>`/`From<serde_json::Error>` |
| `marti/extract.rs` | 196 | `ListenerRole`, `ApiVersion`, `MissionRef`, `CommaList<T>`, `LooseBool`, `CiQuery` |
| `marti/principal.rs` | 105 | `MartiPrincipal`, `AuthPolicy`, `auth_policy(ListenerRole)` — the M2-03 seam |
| `marti/headers.rs` | 75 | `marti_headers` middleware: HSTS, CORS, preflight, the redirect guard; `api_version_header()` |
| `marti/time.rs` | 95 | `cot_date`, `cot_date_unpadded`, `group_date`, `java_date_string`, `parse_date`, `TimeWindow` |
| `marti/version.rs` | 101 | `/Marti/api/version`, `/version/config`, `/version/info`, `/node/id`, `/util/isAdmin`, `/files/api/config` |
| `marti/util.rs` | 139 | `/util/user/roles`, `/Marti/api/home`, `/Marti/GetTime`, `POST /Marti/ErrorLog` |
| `marti/stubs.rs` | 72 | video, `vcm`/`vcu`/`vcs`, injectors, repeater, KML, `sync/missioncreate` |
| `config/marti.rs` | 35 | `[marti]`: `public_host`, `upload_size_limit_mb`, `allow_all_origins`, `store_error_logs`, `error_log_retention` |
| `tests/marti_contract.rs` | — (`tests/` exempt) | 14 in-process contract tests over the real `App` |

**114 tests**: 100 unit (in each file's single trailing column-0 `#[cfg(test)] mod tests`) and 14
integration. No manifest changes; no new dependencies.

### Routes as mounted

| Method | Path | Auth | Answer |
|---|---|---|---|
| GET | `/Marti/api/version` | anonymous | `text/plain` `TAK Server rustak-<version>`, no newline |
| GET | `/Marti/api/version/config` | anonymous | `ServerConfig` envelope, `data.{version,api,hostname}` |
| GET | `/Marti/api/version/info` | anonymous | bare `{major,minor,patch,branch:"rustak",variant:"DIRECT"}` |
| GET | `/Marti/api/node/id` | anonymous | `text/plain` `rustak-<8 hex>` |
| GET | `/files/api/config` | anonymous | bare `{"uploadSizeLimit": <int MB>}` |
| GET | `/Marti/api/util/user/roles` | any | bare array; `ROLE_READONLY` derived from `IN` membership |
| GET | `/Marti/api/util/isAdmin` | any | bare `true`/`false` |
| GET | `/Marti/api/home` | any | `text/plain` `/webtak/index.html`, admin `/Marti/metrics/index.html` |
| GET | `/Marti/GetTime` | any | `text/plain` `cot_date(now)` |
| POST | `/Marti/ErrorLog` | any | `200`, body kept in `kv` (truncated + capped) or discarded |
| GET | `/Marti/api/video` | any | bare `{"videoConnections":[]}` |
| POST/PUT/DELETE | `/Marti/api/video[/{uid}]` | any | `501`; `GET /video/{uid}` `404` |
| GET | `/Marti/vcm` | any | `application/xml` `<videoConnections/>`; `/vcu`, `/vcs` `501` |
| GET | `/Marti/api/injectors/cot/uid` | admin | `UidCotTagInjector` envelope, empty `data` |
| GET | `/Marti/api/repeater/{list,period,remove/{uid}}` | admin | `1.0.0` envelopes: `Repeatable` `[]`, `Integer` `3000`, `java.lang.Boolean` `false` |
| GET | `/Marti/{ExportMissionKML,KmlMasterSA,LatestKML,TracksKML}`, `/Marti/api/missions/{name}/kml` | any | `501` `"KML export is not implemented"` |
| POST | `/Marti/sync/missioncreate` | any | `501` |

## Decisions worth recording

### `MartiPrincipal.identity` is an `Option`, not the design's bare `Principal`

Design 04 §0 sketches `MartiPrincipal { principal: Principal, … }`. `rustak_core::identity::Principal`
deliberately has **no way to construct an unauthenticated one** (M0-04 states that as a design
constraint with a test naming it), and most of this surface is anonymous by contract — a device
probes `/Marti/api/version` before it has a certificate, and CloudTAK calls `/files/api/config` to
validate a connection it has not saved. So the extractor carries `Option<Resolved>` and a handler
says what it needs: `require()` (`401`) or `require_admin()` (`401` with no credential, `403` with
the wrong one). Nothing had to be weakened in `rustak-core` to make it work.

A bearer token that does not verify resolves to **anonymous rather than a refusal**, per design D2:
the same header will carry mission tokens in M4, so "not one of our identity tokens" has to mean
"no identity", not "go away".

### `auth_policy(ListenerRole)` is a struct, and both listeners say the same thing today

`AuthPolicy { bearer, client_cert, basic }`. Both arms are written out rather than collapsed, with a
`TODO(M2-03)` at the one place in `principal::resolve` where the other two branches go. A test
asserts the current answer for both roles, so widening the seam is a deliberate edit to that test
rather than a silent change. `ListenerRole` is `app_data` on the scope, which is how the extractor
reads it without a second `services()` signature.

### `marti/principal.rs` and `marti/util.rs` are two files the brief did not name

`extract.rs` with `MartiPrincipal` folded in came to just over 280 functional lines and mixed two
responsibilities — parsing what a request *says* and establishing who it is *from*. Splitting them
keeps `extract.rs` free of the database, which is what lets it be tested with a bare `TestRequest`.

`version.rs` with the whole of §2 in it was similar: the version/identity probes are what a client
uses to decide whether to keep talking, and roles/home/clock/crash-reports are four unrelated
endpoints that happen to sit nearby. `version.rs` is 101 lines and `util.rs` is 139; together they
would have been over the limit and would not have shared a single helper.

### `MethodNotAllowed` is a thirteenth `MartiError` variant

Design 04 §1.3's table has no `405`, because TAK Server leaves those to its servlet container, which
answers HTML. D4 nevertheless requires "wrong method etc. are 404/405 JSON, not redirects", so the
variant exists: `405`, code `2` (the invalid-request family, rather than inventing a number in the
one gap upstream's sequence leaves), message `Invalid Request: <method>`.

### The `404`-versus-`405` decision comes from a literal path table

A scope's default service cannot ask the router whether the path would have matched under another
method, so `marti::serves_path` lists every mounted path (plus the three one-parameter shapes and
`/missions/{name}/kml`). A route added without a line there answers `404` where it meant `405`; the
contract test in `mod.rs` iterates both route tables through `serves_path` so that shows up as a
test failure rather than as a confused client.

### Every nested scope carries its own default service

This is the one thing that was actually broken on the first run and is worth knowing: in actix a
**nested** scope inherits the *application's* default service, not its parent scope's. Without
`.default_service(…)` on the inner `/api` scope, `/Marti/api/nothing-here` and
`/Marti/api/version/` fell through to `web/server.rs`'s single-page-application catch-all and
answered **HTML with a `200`** — which a TAK client would have parsed as a payload. Three of the
route-table tests caught it. Any future Marti sub-scope needs the same line.

### The redirect guard is belt and braces

`NormalizePath` is never installed and the default services answer `404`/`405`, but
`headers::marti_headers` additionally replaces any `3xx` that reaches it with a `500` and an
`error!` line. A redirect is the one failure mode where the client reports success and breaks
somewhere else entirely, so it is worth turning into a loud failure at the cost of one comparison
per response. The integration suite probes trailing slash, doubled slash, a dot segment and two
unsupported methods against every route.

### The node id is a process-wide `OnceLock`, filled by the middleware

`response::ok(…)` is synchronous and every envelope carries `nodeId`, so the id cannot be a database
read per response. `marti_headers` calls `response::ensure_node_id` — an atomic load after the first
request — and `load_node_id` generates `rustak-<8 hex>` into the `marti`/`node-id` key with
`insert` (not `set`), so two workers racing on a first request agree on one value. The format is an
XML NCName on purpose: it becomes the `TAK-Server-<id>` flow-tag attribute in M1's stream code, and
a raw UUID would not be a legal XML name. A unit test asserts the character set.

### `ROLE_READONLY` is derived, not stored

Sending to a channel is an `IN` grant, so a caller holding `IN` on nothing has nowhere to send —
which is what read-only means. `util::can_send` asks `identity::members::grants_for_user` (M2-02)
rather than reading a flag, so an administrator who removes the last `IN` grant does not also have
to remember to set one. An anonymous caller is read-only by the same rule.

### Crash reports: stored in `kv`, and the discard is audited **once per process**

There is no `error_logs` table because migrations are outside this brief's file list, so the reports
go in the `marti-error-log` key/value partition: body truncated to 64 KiB, `[marti]
error_log_retention` (200) entries kept, oldest pruned. Keys are `<timestamp>-<sequence>` with a
process-wide `AtomicU64` rather than a random suffix — the first version used randomness and a test
caught that several reports arriving in the same millisecond then sort arbitrarily, so the pruner
could drop the newest and keep the oldest.

The brief says "discard with audit". Audited **once per process** rather than per request: an audit
entry for every discarded report would let any client fill the audit log by posting in a loop, and
the operator only needs to be told that `store_error_logs = false` is taking effect. Subsequent
discards are a `debug!`. The test asserts *at most* one entry, because the flag is process-wide and
the suite runs many servers in one binary.

### `[marti]` has five keys, not the brief's two

`public_host` and `upload_size_limit_mb` are the two the brief names. `allow_all_origins` is needed
by "CORS allow-all when configured" and `store_error_logs`/`error_log_retention` by "store
size-capped in kv or discard with audit" — all three are in design 04 §1.5's list for this section.
`[storage]` was left alone: it has no upload-limit key, and adding one there would have put a
wire-visible number in the section about where files live.

Note that `[marti]` and `[web.marti]` are different sections: the first is the TAK *surface* (served
on both listeners), the second is the mutually authenticated *listener*. Both were already named
that way by designs 01 and 04.

### Three date formatters, and a fourth spelling

`cot_date` (padded millis, literal `Z`) is the default; `group_date` is `yyyy-MM-dd` for a channel's
`created`; `java_date_string` is `Date.toString()` for the Enterprise Sync `Time` key.
`cot_date_unpadded` was added because `compat/contacts.md` §2 says `clientEndPoints`'s
`lastEventTime` is the one-to-three-digit form and calls the padded form there a mistake — M2-08 will
need it, and putting it beside the others with its own test is cheaper than re-deriving it.
`parse_date` is lenient by design (RFC 3339 with any offset, the literal-`Z` forms, a bare date, and
epoch millis — the last because ATAK sends a channel's `created` as a number).

`TimeWindow::parse` takes `parse_at(now, …)` underneath so that every bound is a literal in the
tests rather than an approximation. Rules: an explicit `start`/`end` beats `secago`; a negative
`secago` and an inverted window are `400`; nothing at all is the most recent 24 h; anything longer
than 24 h is narrowed to its **most recent** day with `capped = true` so a handler can say so.

## Deviations from the brief and the designs

1. **Two extra files** (`principal.rs`, `util.rs`) — see above.
2. **`MethodNotAllowed`** added to `MartiError` — see above.
3. **`POST /files/api/config`** (design §2, admin-only, persists a `kv` override) was **not** built.
   The brief's endpoint list has only the `GET`. A `POST` falls into the `/files/api` scope's default
   service and answers `405` JSON, which is a clean refusal rather than a silent success. Whoever
   owns the admin surface in M3 can add it in a handful of lines.
4. **`cot_date_unpadded`** added beyond the brief's three formatters, for `compat/contacts.md` §2.
5. **`MartiPrincipal.identity` is optional** and `auth_policy` returns a struct — see above.
6. **`config/mod.rs`'s `fully_populated` fixture** gained one line (`marti.public_host`). Without it
   the existing `the_example_file_documents_every_key` test would not cover the new optional key,
   which is the whole point of that fixture.

## Notes for the briefs that follow

- **M2-03 (cert/Basic auth)**: the whole seam is `marti::auth_policy` and the two `TODO(M2-03)`
  branches in `principal::resolve`. Both return the same `crate::auth::resolve::Resolved`. No route
  file needs to change, and `ListenerRole::Marti` already exists for the `:8443` mount.
- **The Marti listener itself is not bound yet.** `runtime::run_all` binds only the public listener
  (M0-12), so `marti::services(ListenerRole::Marti)` currently has no caller. Mounting it is one
  `.configure(…)` line in whatever builds that listener, exactly as in `web/server.rs`.
- **Every new sub-scope needs its own `default_service`** — see the actix note above. This is the
  single most likely way for a future Marti route to start answering HTML with a `200`.
- **Add a row to both route tables** when adding a route: `CONTRACT` in
  `tests/marti_contract.rs` and `ROUTES`/`REFUSALS` in `marti/mod.rs`'s tests, plus a line in
  `marti::PATHS` or `PARAMETERISED` so the `405` case keeps working.
- **`response::kind`** already carries every `type` string from design §1.2, including the ones no
  endpoint uses yet (`MISSION_*`, `RESOURCE`, `PROFILE*`, `FILES`/`COUNT`/`DATA`). M3/M4 should use
  the constant rather than a literal; the envelope test is a table over them.
- **`headers::api_version_header()`** is the `api-version: 3` response header that
  `/Marti/sync/content` alone carries (M3).
- **`response::no_store()`** is the cache-header helper `/Marti/api/clientEndPoints` alone needs
  (M2-08).
- **`extract::CiQuery`** is the case-insensitive query parser the legacy `/Marti/sync/*` servlets
  need (M3); `strings()`/`list::<T>()` accept both the repeated and the comma-joined spellings of a
  multi-valued parameter, which CloudTAK uses interchangeably.
- **`MissionRef`** already sniffs a UUID-shaped `{name}` into `Guid`, per D7. M4 still has to
  **reject** a UUID-shaped name on create.
- **The pki hang M2-02 reported is gone.** `cargo test -p rustak-server --features testing --lib
  pki::` now finishes in 20.82s, 146 passed. The `--skip 'pki::'` workaround is no longer needed.

## Exit checks

Run against the final tree. Other agents were writing in `rustak-server/src/{pki,stream,cot_store}`,
`e2e/`, `interop/` and `rustak-client/` throughout; every check below covers the whole
`rustak-server` crate and was green including their work.

```
$ cargo test -p rustak-server --features testing --lib marti::
running 100 tests
test result: ok. 100 passed; 0 failed; 0 ignored; 0 measured; 792 filtered out; finished in 1.26s

$ cargo test -p rustak-server --features testing --lib config::
test result: ok. 92 passed; 0 failed; 0 ignored; 0 measured; 800 filtered out; finished in 0.01s

$ cargo test -p rustak-server --features testing --test marti_contract
running 14 tests
test an_unmatched_marti_path_is_our_json_rather_than_the_admin_ui ... ok
test every_refusal_carries_all_three_fields_of_the_error_shape ... ok
test every_route_answers_with_the_status_and_content_type_it_promised ... ok
test nothing_in_the_marti_surface_ever_redirects ... ok
test the_api_version_header_reaches_the_handlers_in_either_spelling ... ok
test the_admin_api_is_untouched_by_the_marti_scope ... ok
test roles_report_read_only_from_channel_membership_rather_than_a_flag ... ok
test the_cloudtak_setup_gate_answers_before_anything_is_configured ... ok
test the_enveloped_routes_carry_the_type_strings_their_clients_match_on ... ok
test the_version_probe_is_the_product_string_atak_matches_on ... ok
test the_unenveloped_routes_are_left_unenveloped ... ok
test a_crash_report_is_accepted_whether_or_not_it_is_kept ... ok
test the_server_config_is_what_ataks_version_parser_reads ... ok
test a_preflight_is_refused_until_an_operator_asks_for_cross_origin_access ... ok
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.82s

$ cargo test -p rustak-server --features testing --test bootstrap
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.02s

$ cargo test -p rustak-server --features testing --doc
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo test -p rustak-server --features testing --lib -- --skip 'pki::'
test result: ok. 744 passed; 0 failed; 1 ignored; 0 measured; 147 filtered out; finished in 8.31s

$ cargo test -p rustak-server --features testing --lib pki::
test result: ok. 146 passed; 0 failed; 1 ignored; 0 measured; 745 filtered out; finished in 20.82s
# The hang M2-02 recorded no longer reproduces; that brief's owner has since
# landed. `--skip 'pki::'` was not needed for any check above.

$ cargo test -p rustak-server --features testing
     Running unittests src/lib.rs (target/debug/deps/rustak_server-…)
running 892 tests
test result: ok. 890 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 16.05s
     Running unittests src/main.rs (target/debug/deps/rustak-…)
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/bootstrap.rs (target/debug/deps/bootstrap-…)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.72s
     Running tests/marti_contract.rs (target/debug/deps/marti_contract-…)
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.61s
   Doc-tests rustak_server
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
# The whole crate, `pki::` included, with no skip. 892 unit tests where M2-02
# recorded 642 excluding pki.

$ cargo run -p rustak-server -- --config config.example.toml --check
config.example.toml is valid: rustak would listen on 0.0.0.0:8446, with data in ./data.
exit=0
# The documented `[marti]` section is one `rustak --check` accepts; the
# example-file tests in `config/mod.rs` assert both directions of that.

$ cargo clippy -p rustak-server --all-targets --all-features -- -D warnings
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.67s
# Clean, including `pki::`. No pre-existing lints remain in this crate.

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server --no-deps
 Documenting rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.36s
   Generated target/doc/rustak_server/index.html and 1 other file

$ cargo fmt -p rustak-server --check
(no output)

$ ./scripts/check-file-length.sh
(no output, exit 0)
```

`check-file-length.sh` reads `git ls-files`, so it does not yet see this brief's untracked files.
They were counted with the same `awk` the script uses; the table at the top of this file has the
numbers, and the largest is `marti/response.rs` at 208 (limit 300).

### What the contract tests actually assert

- **Content type, byte for byte, over the real `App`.** Every one of the 27 rows of
  `tests/marti_contract.rs::CONTRACT` — including the refusals — with an explicit assertion that no
  answer contains `charset`, because node-tak's check is `header === 'application/json'` and a
  parameter suffix hands the caller a raw string that most of its call sites index straight into.
- **No `3xx`, four ways.** Trailing slash, doubled slash, dot segment, and two unsupported methods,
  against every route. A redirect is the one failure that a TAK client reports as success.
- **The `404` is ours, not the admin UI's.** Four unmounted paths under `/Marti` and `/files/api`
  answer JSON with `status: "NOT_FOUND"`, `code: 1` rather than the single-page shell.
- **Every refusal carries all three fields**, `message` included when it is empty, so a client
  reading `body.message` finds a string rather than `undefined`.
- **ATAK's `ServerVersion` expectations**: envelope `version` parses as the integer `3`,
  `type == "ServerConfig"`, `data.version` is a string, `data.api == "3"`, `data.hostname` has no
  port.
- **CloudTAK's setup gate** answers before anything is configured, with an integer
  `uploadSizeLimit` and no envelope.
- **The unenveloped three stay unenveloped** (`/util/user/roles`, `/util/isAdmin`, `/api/video`) —
  asserted by the absence of `nodeId`, because "fixing" one into an envelope is the natural mistake.
- **The admin API is untouched** by the new scope: `/api/v1/health` still `200`s, `/api/v1/me` still
  `401`s, `/robots.txt` still resolves.
