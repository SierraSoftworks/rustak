# M3-02 — Device profiles, `.pref` writer, package builders, config packages — complete

Brief: `.claude/plan/briefs/M3-02-device-profiles.md`
Read first: `conventions.md`; design 04 §5.3, §6, §8.1/§8.2; `compat/profiles.md`,
`compat/enrollment.md`, `compat/files.md` §10; research `06` §10, `07` §2; status `M0-07`,
`M2-01`, `M2-03`, `M2-04`.

## What was built

Three new module trees and two new DTO files. Everything ATAK pulls for itself now comes from
`profiles::ProfileService`, and the `204` stubs M2-03 left in `marti/tls.rs` are gone.

| File | Functional lines (limit 300) | Contents |
|---|---:|---|
| `rustak-api/src/profile.rs` | 156 | `PrefClass` (five Java classes + `accepts`), `PrefEntry`, `Profile`, `ProfileCreate`, `ProfileUpdate`, `ProfileFile`, `PrefCatalogEntry` |
| `rustak-api/src/config_package.rs` | 47 | `ConfigPackageVariant`, `ConfigPackageRequest` |
| `rustak-server/src/files/package.rs` | 282 | `Manifest`, `ContentEntry`, `RoleXml`, `write_manifest`, `read_manifest`, `parse_manifest`, `escape` |
| `rustak-server/src/profiles/mod.rs` | 16 | the module tree and its re-exports |
| `rustak-server/src/profiles/prefs.rs` | 101 | `PrefGroup`, `UserSettings`, `render`, `enrollment_defaults`, `APP_PREFS`/`LEGACY_APP_PREFS`/`COT_STREAMS` |
| `rustak-server/src/profiles/builder.rs` | 126 | `ProfileFileData`, `build_profile_package`, `build_multifile_package`, `write_package`, `write_zip`, `guess_content_type` |
| `rustak-server/src/profiles/config_package.rs` | 144 | `ConfigPackageInput`, `build_wintak_atak`, `build_itak` |
| `rustak-server/src/profiles/model.rs` | 92 | `ProfileRow`, `ProfileFileRow`, `NewProfile`, `Delivery`, `visible_to` |
| `rustak-server/src/profiles/repo.rs` | 292 | `ProfilesRepo`: CRUD, `for_delivery`, files, prefs |
| `rustak-server/src/profiles/service.rs` | 276 | `ProfileService`, `Assembled`, `preferred_host`, group matching, `Last-Modified` |
| `rustak-server/src/profiles/catalog.rs` | 141 | the curated 20-key preference catalogue |
| `rustak-server/src/marti/profiles.rs` | 295 | the §6.4 endpoint table |
| `rustak-server/src/web/api/profiles.rs` | 224 | `/api/v1/profiles` CRUD, prefs, preview, `pref-catalog` |
| `rustak-server/src/web/api/profile_files.rs` | 188 | `/api/v1/profiles/{id}/files*` (multipart upload, download, delete) |
| `rustak-server/src/web/api/config_packages.rs` | 159 | `POST /api/v1/config-packages` |
| `rustak-server/migrations/0009_profile_prefs.sql` | — | `profile_prefs(profile_id, key, class, value, position)`, `STRICT, WITHOUT ROWID` |
| `rustak-server/tests/profiles_contract.rs` | — (`tests/` exempt) | 14 in-process contract tests over the real `App` |
| `docs/compat/profiles.md` | — | the manual ATAK/WinTAK/iTAK import checklist |

**57 tests** added: 43 unit (in each file's single trailing column-0 `#[cfg(test)] mod tests`) and
14 integration. No manifest changes; no new dependencies (`zip` 8.6 with `deflate-flate2-zlib-rs`
was already in the tree).

### Routes as mounted

| Method | Path | Auth | Answer |
|---|---|---|---|
| GET | `/Marti/api/tls/profile/enrollment?clientUid=` | Basic **or** bearer | always `200` + `profile.zip` (see deviation 1) |
| GET | `/Marti/api/device/profile/connection?clientUid=&syncSecago=` | any credential | `204` or `200` + `profile.zip`, `Last-Modified` |
| GET | `/Marti/api/device/profile/tool/{tool}?clientUid=&syncSecago=` | any credential | `204` or `200` + `profile.zip` |
| GET | `/Marti/api/tls/profile/tool/{tool}/file?relativePath=*` | any credential | `404` / `304` / `200` raw / `200` `multiFile` zip |
| GET | `/Marti/api/device/profile/tool/{tool}/file?relativePath=*` | any credential | same handler |
| GET | `/Marti/api/device/profile/{name}/missionpackage` | any credential | that profile's zip, `404` when unknown |
| HEAD | `/Marti/api/device/profile/{name}/missionpackage` | any credential | `200`, empty (TAK's own no-op) |
| GET | `/Marti/api/device/profile` | admin | `ApiResponse` kind `Profile`, the listing |
| GET | `/Marti/api/device/profile/{name}` | admin | `ApiResponse` kind `Profile` |
| GET | `/Marti/api/device/profile/directories` | admin | `501` — rustak stores profile files in the content store |
| GET/POST | `/api/v1/profiles` | admin | list / create |
| GET/PATCH/DELETE | `/api/v1/profiles/{id}` | admin | |
| GET/POST | `/api/v1/profiles/{id}/files` | admin | list / multipart upload (part `file`, optional `filename`) |
| GET/DELETE | `/api/v1/profiles/{id}/files/{file}` | admin | bytes / `204` |
| GET/PUT | `/api/v1/profiles/{id}/prefs` | admin | `[PrefEntry]`, replaced wholesale |
| GET | `/api/v1/profiles/{id}/preview` | admin | the zip a device would receive |
| GET | `/api/v1/profiles/pref-catalog` | admin | `[PrefCatalogEntry]` |
| POST | `/api/v1/config-packages` | admin | zip download |

`marti/mod.rs` gained one `.configure(profiles::routes)` line (replacing the two `tls::` stub
routes) and one `pub mod profiles;`. `web/api/mod.rs` gained two `.configure` lines and three
`pub mod` lines. `lib.rs` gained `pub mod files;` and `pub mod profiles;`.

### The bytes that matter

`.pref` rendering is byte-exact and asserted as such:

```
<?xml version='1.0' standalone='yes'?><preferences><preference version="1" name="com.atakmap.app.civ_preferences"><entry key="…" class="class java.lang.String">…</entry>…</preference></preferences>
```

- single-quoted declaration, no `encoding`;
- no whitespace between elements;
- a `class` attribute on **every** entry (ATAK dereferences it without a null check, so a missing
  one aborts the whole import rather than skipping a key);
- entries render in the order they were stored (`profile_prefs.position`), so two builds of the
  same profile produce the same document;
- `&`, `<`, `>` escaped in text and the five entities escaped in attributes — the reference
  generator escapes nothing, which produces a document its own parser would refuse the first time a
  callsign contains an ampersand.

Zip entries are written with a fixed 1980-01-01 timestamp, so the only thing that varies between
two builds is the minted manifest `uid`.

## Exit checks

```
$ cargo test -p rustak-server --features testing
test result: ok. 1208 passed; 0 failed; 2 ignored        (lib)
test result: ok. 3 passed    (bootstrap)
test result: ok. 10 passed   (enroll_flows)
test result: ok. 10 passed   (enroll_oauth)
test result: ok. 9 passed    (marti_contract)
test result: ok. 14 passed   (marti_groups / contacts)
test result: ok. 14 passed   (profiles_contract)
test result: ok. 11 passed   (stream_routing)
test result: ok. 10 passed   (stream_session)
test result: ok. 8 passed    (stream_store)
test result: ok. 13 passed   (sync_contract)
test result: ok. 5 passed    (doc-tests)

$ cargo test --workspace
exit 0 — 23 targets, every one green (rustak-cot 139 + 249 + 9, rustak-core 49,
rustak-client 9, rustak-api 105, rustak-server as above)

$ cargo clippy --workspace --all-targets -- -D warnings
clean
(It was briefly blocked mid-run by `tests/sync_contract.rs:169`
 `clippy::unnecessary_to_owned` — M3-01's file, since fixed by them.)

$ RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps
clean
(Briefly blocked by `db/repos/resources/row.rs:56`, M3-01's file, since fixed.
 One of mine needed fixing too: see the rustdoc gotcha below.)

$ cargo fmt --all --check
clean

$ ./scripts/check-file-length.sh
clean (and every new file measured by hand: largest is profiles/repo.rs at 292)

$ cargo run -p rustak-server -- --config config.example.toml --check
config.example.toml is valid: rustak would listen on 0.0.0.0:8446, with data in ./data.
```

## Deviations from the brief, and why

1. **The enrolment endpoint never answers `204`.** Design §6.4 says "Empty → 204", but the same
   design also says the delivered set always includes "the server default
   `rustak-enrollment.pref`". That file is the only mechanism that turns on
   `deviceProfileEnableOnConnect`, which defaults to `false` in ATAK and which every connection
   profile silently depends on. Sending it unconditionally is therefore the whole point of the
   endpoint, and it means enrolment is always `200`. There is no configuration key to disable it —
   the brief did not ask for one and `config.example.toml` is a shared file — so
   `ProfileService::enrollment` takes a `defaults: bool` the route currently passes `true`.
   `M2-03`'s `enroll_flows.rs` test that asserted `204` here was updated (see 5).

2. **`marti/headers.rs` exempts `304` from the redirect guard.** `assert_no_redirect` turned every
   `3xx` into a `500`, which made the `If-Modified-Since` half of §6.4 unimplementable. `304`
   carries no `Location` and is the documented answer to a conditional request, so it is now
   exempt; every other `3xx` still fails loudly. This is a six-line change in a file the brief did
   not list as mine, with the reason stated in the module doc and beside the check.

3. **`include_client_cert: true` is refused rather than served.** The brief says the endpoint
   "consumes an existing `ClientPassword`/certificate and never mints one". rustak never holds a
   device's private key (`pki::facade` asserts this: "we never hold a client's key"), so there is
   nothing to assemble a `.p12` keystore from after the fact and the only way to serve the flag
   would be to mint a key pair. The request is refused with a `400` explaining that, and the
   enrolment variant — which is the safer one anyway — is what `false` builds. The builder's
   certificate variant is implemented and tested, so the day a keystore has somewhere to come from
   it is one line. Recorded in `docs/compat/profiles.md` §7.

4. **Two extra files, both splits for the 300-line rule.** `profiles/repo.rs` holds the SQL that
   `profiles/model.rs` would otherwise carry (391 lines combined), and
   `web/api/profile_files.rs` holds the file endpoints that `web/api/profiles.rs` would otherwise
   carry (384 combined). `profiles/catalog.rs` is a third addition: the curated preference
   catalogue is data, and inlining it would have pushed `web/api/profiles.rs` over again.

5. **Two files outside my list were edited.** `marti/headers.rs` (see 2) and
   `rustak-server/tests/enroll_flows.rs`, whose
   `the_profile_endpoints_answer_no_content_until_they_have_something_to_send` asserted the `204`
   stubs this brief was told to delete — two other agents reported it failing with a `401`. It now
   asserts the real contract: the connection fetch is `204` with nothing configured, enrolment is
   `200` because of deviation 1, and an anonymous request is `401` rather than being handed an
   empty profile, because these carry an account's own settings.

   The `401` is correct, and the credential ATAK actually holds at that moment still works:
   `auth::resolve::ENROLLMENT_PREFIX` is `/Marti/api/tls/`, so Basic reaches
   `/Marti/api/tls/profile/enrollment` — which is exactly why the enrolment profile lives under
   `tls/` and the connect-time ones do not.
   `profiles_contract.rs::the_enrolment_profile_is_reachable_with_the_basic_credential_atak_enrolled_with`
   pins that down with a real `ClientPassword`.

6. **`profiles.preferences` (migration 0007) is dead.** It was a JSON blob on the row; the editor
   needs a class per entry and the renderer needs a stable order, so `0009` adds `profile_prefs`
   instead. An applied migration is never edited, so the column stays and is never read or written.
   Noted in the migration's own comment.

7. **`GroupIndex` is built per request.** `ProfileService::group_names` resolves the caller's bit
   positions to names through `db.groups().index()`, which is a read per profile fetch. These
   endpoints fire once per enrolment and once per stream connect, so it is not on any hot path; if
   that changes, the index belongs in a `Cache`.

## For the other agents in this tree

- **M3-01 (files/sync).** I created `rustak-server/src/files/mod.rs` with only `pub mod package;`
  before yours existed; you have since rewritten it and kept the line, which is what was wanted.
  `files/package.rs` is mine and complete — `Manifest`, `read_manifest`/`write_manifest`, tolerant
  reader, nested-prefix handling — so mission archives and `/Marti/sync/upload` manifests can use
  it as is.
  One rustdoc gotcha worth passing on: a `///` outer doc on `pub mod package;` in `files/mod.rs` merges
  with the module's own `//!` docs and makes rustdoc resolve **all** of their intra-doc links in
  the *parent* module's scope — that is why `files/package.rs`'s module doc has to write
  `[`crate::files::package::read_manifest`]` in full.
  `ProfileService::enrollment_packages` reads `resources` directly (`name, hash, submission_time`
  where `install_on_enrollment = 1 AND deleted_at IS NULL`) rather than through your repository, so
  that neither of us blocks the other; swapping it for `ResourcesRepo` later is a three-line change.

- **M2-07 (admin UI).** The DTOs are in `rustak_api::{profile, config_package}` and re-exported
  from the crate root: `Profile`, `ProfileCreate`, `ProfileUpdate`, `ProfileFile`, `PrefEntry`,
  `PrefClass`, `PrefCatalogEntry`, `ConfigPackageRequest`, `ConfigPackageVariant`. `PrefClass`
  carries `label`-free `as_str()`/`ALL`/`accepts()` so a class picker and its validation can be
  built straight from it. `GET /api/v1/profiles/pref-catalog` is the autocomplete source.

- **Anybody touching `marti/mod.rs`.** The profile routes are registered through
  `profiles::routes`, whose internal order matters: `/device/profile/connection`,
  `/device/profile/tool/{tool}/file` and `/device/profile/tool/{tool}` all come before
  `/device/profile/{name}`, which would otherwise swallow them.

## Not done (and deliberately so)

- **`useStreamingGroup`.** TAK Server can take the caller's channels from their streaming
  subscription rather than from the HTTP request. Channels here come from the authenticated
  principal; the seam is `ProfileService::group_names`.
- **`POST /Marti/api/device/profile/{name}/send`** (push a profile to named client UIDs) and the
  `directories*` family. The first needs the stream hub's client registry; the second is a
  filesystem model rustak does not use. Both answer `501` or are simply absent.
- **`/api/v1/profiles/map-sources`.** Design §8.1 lists a built-in catalogue of map-source XML
  templates. The brief's deliverable list does not include it and there is no verified source for
  what those templates should contain, so it is left for the UI brief to specify.
- **Any automated ATAK/WinTAK/iTAK import check.** This stays a manual gate;
  `docs/compat/profiles.md` is the checklist, and it says so at the top.

## Corrections filed

`.claude/plan/compat/profiles.md` gained a "Corrections from the M3-02 implementation" section
covering: `304` not being a redirect; truncating the *delivered* timestamp as well as the stored
one; the per-entry `class` (the reference generator hard-codes `String`); the `<Role name=>` versus
`<Role type=>` disagreement between design 04 §5.3 and `compat/files.md` §10 (we write `name`,
accept either); the unverified iTAK keystore path; and the keystore refusal above.
