# M2-08 — Identity admin API gaps found by the UI (certificates, user detail, members, active groups) — complete

Brief: `.claude/plan/briefs/M2-08-identity-api-gaps.md`
Read first: `conventions.md`; status `M2-07-admin-ui-identity.md` ("Server endpoint gaps"),
`M2-02-identity-credentials.md`, `M2-01-pki-issuance.md`, `M2-06-marti-groups-contacts.md`;
`rustak-server/src/web/api/{users,devices,credentials,groups,users_groups}.rs`;
`rustak-api/src/{certificate,device,user,group,credential}.rs`.

Every one of M2-07's seven server gaps is closed except the cosmetic seventh (`POST /users`
answering `200` rather than `201` — it already answered `201`, see below). No `git`/`but` command
was run. `rustak-ui/**` was not edited: every shape this brief changed is additive, so the UI still
compiles and the follow-up brief can use the new fields when it wants them.

## What was built

### The certificate endpoints (gap 1, the one the brief called biggest)

| Route | Who | Answers |
|---|---|---|
| `GET /api/v1/certificates` | administrator, or your own | `[Certificate]`, newest first, paged |
| `GET /api/v1/certificates/{id}` | administrator, or the owner | one `Certificate` |
| `POST /api/v1/certificates/{id}/revoke` | administrator | the revoked `Certificate`, or `409` |

The listing narrows by `username`, `device_uid`, `state=active|revoked|expired` and `kind`, with
`page`/`limit` (default 100, ceiling 200). Every filter is "and also", so one device's revoked
certificates is one request.

Revoking goes through `Pki::revoke` → `pki::revoke::revoke`, which writes the row, calls
`RevocationCache::note_revoked` — the set `RustakClientVerifier::verify_client_cert` consults on
every handshake (`pki/tls/client_verifier.rs:125`) — and runs the hooks, one of which is the
`disconnect_by_fingerprint` the stream listener registers at start-up (`stream/mod.rs:167`). So the
handler is four calls long and there is exactly one implementation of "take a certificate back".

### `GET /api/v1/users/{username}` and a patch that writes the profile (gaps 2 and 3)

`web/api/users.rs` gained `get`, which is administrative **or your own** and answers the same
`User` the listing does — so a page that opens on one account no longer reads the whole listing and
filters it, and can tell `404` from `403`.

The patch now actually writes the display name. **It never did**: `UserPatch::display_name` has
existed since M0-03 and `web/api/users.rs` handled only `disabled` and `is_admin`, so a UI that sent
one had it silently discarded. `db/repos/users/profile.rs` is the write, and `UserPatch::email` is
the new field beside it.

### `GET /api/v1/groups/{name}/members` (gap 4) and `GET /api/v1/devices/{uid}/active-groups` (gap 5)

The members listing is one row per member per direction — the mirror image of
`GET /users/{u}/groups` — and turns M2-07's N+1 (`/users` plus one `/users/{u}/groups` per account)
into one request. The device's channel state is the same shape its `PUT` answers with, so a page can
read what it is about to change instead of writing in order to find out.

### `GET /api/v1/credentials?all=true` (gap 6)

Administrative, paged (default 100, ceiling 200), and refused when combined with `username=`.
An absent `username` still means "mine", for everybody including an administrator.

## Files

**New (3):**
`rustak-server/src/web/api/certificates.rs` (222 functional lines),
`rustak-server/src/db/repos/certificates/list.rs` (69),
`rustak-server/src/db/repos/users/profile.rs` (41).

**Changed (14):**

| File | Lines | What |
|---|---:|---|
| `rustak-api/src/certificate.rs` | 200 | `Certificate.device_uid`, `Certificate.revocation_reason`, `Certificate::state`, `CertificateState`, `RevocationReason`, `RevokeCertificateRequest` |
| `rustak-api/src/user.rs` | 143 | `UserPatch.email` as a tri-state, its `tri_state` deserializer, `is_empty` |
| `rustak-api/src/group.rs` | 143 | `GroupMember` |
| `rustak-api/src/lib.rs` | 63 | the re-exports for all of the above |
| `rustak-server/src/db/repos/certificates.rs` | 294 | `pub mod list;` and the doc line saying why |
| `rustak-server/src/db/repos/users.rs` | 293 | `pub mod profile;` and the doc line saying why |
| `rustak-server/src/db/repos/credentials.rs` | 235 | `list_all(include_revoked, page)` |
| `rustak-server/src/db/repos/mod.rs` | 109 | `CertificateFilter` and `ProfileChange` re-exports |
| `rustak-server/src/identity/devices.rs` | 88 | `usernames` made `pub` (it was already the "one read, not one per row" helper) |
| `rustak-server/src/web/api/users.rs` | 207 | `get`, the profile write, the audit detail |
| `rustak-server/src/web/api/devices.rs` | 138 | `active_groups` |
| `rustak-server/src/web/api/groups.rs` | 159 | `members` |
| `rustak-server/src/web/api/credentials.rs` | 234 | `all=true`, paging |
| `rustak-server/src/web/api/mod.rs` | 122 | six route lines and six rows in the route-table test |

Plus this status file. Nothing under `marti/**`, `missions/**`, `files/**`, `profiles/**`,
`stream/**`, `auth/**`, `testing/**`, `interop/**` or `rustak-ui/**` was touched.

## Decisions worth recording

### `fingerprint` was not renamed to `fingerprint_sha256`

The brief asks for `fingerprint_sha256` "if missing". It is not missing under another name: the
field is called `fingerprint`, its doc comment has said "the SHA-256 fingerprint of the DER
encoding, as lower-case hexadecimal" since M0-03, and it is what the revocation cache, the
`require_known_cert` check and `pki::pem::sha256_fingerprint` all mean. Renaming it would be a wire
change that breaks `rustak-ui`, the fixtures and every stored expectation to say the same thing in
more letters. `not_after`, `revoked_at` and `username` were likewise already there. The two that
were genuinely missing — `device_uid` and `revocation_reason` — were added.

### Revoking is administrative; reading one certificate is not

The brief's default is "admin unless noted", and only the listing is noted. Reading one certificate
follows the listing rather than the default, because a listing you may read and a row you may not is
an inconsistency with no security value — the rule is `subject::owns`, the same one credentials and
devices use, and it is written once.

Revoking stays administrative even over your own certificate. It drops live connections, it cannot
be undone, and the self-service way to take back your own access already exists and is narrower:
`DELETE /api/v1/credentials/{id}` revokes the certificates that credential bought. A certificate
belonging to **nobody** — the authority's own, a listener's — is administrative to read as well,
because there is no owner for the self-service rule to let through.

### Revoking is `POST /{id}/revoke`, not `DELETE /{id}`

M2-07's gap 1 suggested `DELETE`. With `require_known_cert` on — the default — the register *is*
the list of who may connect, so deleting the row would make the certificate **unrevokable** rather
than revoked: nothing would be left to refuse it a second time, and `RevocationCache::reload` would
drop it from both sets. The row therefore stays and gains a revocation, and the verb says which of
the two happened. `DELETE` on this collection is deliberately not served.

### The state filter is decided in SQL, and the three states partition the register

"Active", "revoked" and "expired" are a function of the row *and of the clock*. Filtering after a
page had been read would return short pages — twenty asked for, eleven returned, no way for the
caller to tell that from the end of the list — so the predicate is in the `WHERE` clause with the
instant bound from Rust, as every other timestamp here is.

Revocation outranks expiry in both directions: `state=revoked` still lists a certificate that has
since run out (an audit is looking for exactly that one), and `state=expired` excludes the revoked
ones. `db/repos/certificates/list.rs` and `rustak-api`'s `Certificate::state` each carry a test
saying so.

### `email` is a tri-state and `display_name` is not

`UserPatch`'s existing rule is "send an empty string to clear", which keeps the difference between
unchanged and cleared off the absent/null distinction. That is right for a display name — `""` is
not a name — and wrong for an email: a form that posts `""` for a box somebody emptied would store
`""` as an address. So `email` is `Option<Option<String>>` through a four-line `tri_state`
deserializer (`#[serde(default)]` supplies the outer `None`; the function is only called when the
key is present, and wrapping in `Some` is what separates cleared from unchanged), and `display_name`
is left as it was. Both clear on whitespace-only input at the handler.

No `serde_with` dependency was added: `rustak-api` compiles for `wasm32-unknown-unknown` beside the
UI, and one function is cheaper than a crate.

### `GroupMember.active` is the *selection*, not the membership

A membership is a right; whether it is switched on is a preference. An administrator looking at a
channel nobody seems to be talking on wants to see which of the two is the reason, so each row
carries both. The value comes from `marti::channels::selection(context, user_id, None)` — M2-06's
account-level layer, which is the only one that answers for a member with no device — cached per
account inside the handler, so a channel with fifty members costs fifty reads rather than a hundred.
A member who has never said anything is `true`, which `GroupMember`'s serde default also encodes.

This is the one place `web::api` reaches into `marti::`. The alternative was to copy the
`marti-channels` partition name and the `active-<id>` key format into a second file, which is a
duplication that would drift silently; `MartiError` only ever fails here as `Internal`, so the
conversion is one function (`groups::refused`).

### Two repositories gained a child module rather than lines

`db/repos/certificates.rs` was at 293 functional lines and `db/repos/users.rs` at 292, against
`conventions.md`'s limit of 300. Rather than deleting `list_of_kind` and `expiring_before` (unused
outside their file today, but plainly there for the renewal job a later brief brings), each file
declares one child module: `certificates/list.rs` for the narrowed listings and `users/profile.rs`
for the display-name-and-email write. A child module sees its parent's private `COLUMNS`,
`from_row` and `db`, so nothing had to be widened, and `repos/resources/` is the existing precedent
for the shape.

### The profile write binds a flag per field

`COALESCE(?2, display_name)` cannot express "set it to nothing", and absent / `null` / a value are
three instructions. Each field is therefore bound twice — whether it is being written, and what to
write — as `CASE WHEN ?2 THEN ?3 ELSE display_name END`.

### `Pki::revoke` already writes the audit entry, so the handler does not

`pki::revoke::revoke` records `certificate.revoked` under `AuditCategory::Pki` with the actor, the
fingerprint, the serial, the reason and the `clientUid`, and distinguishes `Skipped` from `Success`
for a second attempt. A second entry from the handler would be a duplicate with less in it.

## Deviations from the brief, and why

1. **`fingerprint_sha256` is `fingerprint`.** See above. `not_after`, `revoked_at` and `username`
   were already present too; `device_uid` and `revocation_reason` were added.
2. **The revoke → handshake-failure path is a unit test, not a new integration test.**
   `revoking_refuses_the_certificate_at_the_next_handshake_and_closes_what_holds_it` builds a real
   authority with `Pki::load`, primes the cache the way `Pki::enroll` does, revokes through the
   endpoint, and asserts `RevocationCache::is_acceptable` answers `Err(CertRejection::Revoked)` —
   which is the exact call `RustakClientVerifier::verify_client_cert` makes on every handshake — and
   that a registered hook fired with that fingerprint, which is the exact mechanism the stream
   listener disconnects by. `tests/enroll_flows.rs` is not in this brief's file list and another
   agent is working near it, so the assertion was made where it is cheapest and no less precise.
3. **Two repository files outside the brief's list were touched.** `db/repos/users.rs` and
   `db/repos/credentials.rs` each gained one thing the brief's deliverables cannot be built without:
   the profile write (item 2 asks for an email that can be set and cleared) and `list_all` (item 4
   asks for an installation-wide listing). Both are additive; neither changes an existing signature.
4. **`identity/devices.rs::usernames` was made `pub`.** One word. It is exactly the read helper the
   brief allows — "one read rather than one per row" — and the certificate and credential listings
   want the same map for the same reason.
5. **`web/api/groups.rs` imports `marti::channels`.** See "`GroupMember.active`" above.
6. **M2-07's gap 7 needed nothing.** `POST /api/v1/users` already answers `201 Created` with the
   `User` (`json_with(StatusCode::CREATED, …)`); the gap note was written against an older reading.
   No `Location` header was added: nothing asks for one and it would be a wire change for its own
   sake.
7. **`rustak-ui` was not edited.** Every change is additive — new fields with serde defaults, a new
   DTO, a new optional patch field — so `trunk build` is clean without touching the client. The UI
   agent picks these up in its own brief.

## Tests

98 tests pass across the five route files this brief owns and 11 across the two new repository
modules. 33 of the tests are new: 5 in `rustak-api`, 9 in the two repository modules and 19 across
the route files.

| Where | New tests |
|---|---|
| `rustak-api/src/certificate.rs` | the revoked-and-expired precedence, the state and reason wire forms, the default revocation reason, the field-list guard updated for `device_uid` |
| `rustak-api/src/user.rs` | absent / `null` / a value are three different patches, and each serialises back to itself |
| `rustak-api/src/group.rs` | a member with no stated preference is on |
| `db/repos/certificates/list.rs` | the unfiltered order, each state, the revoked-that-ran-out case, the three filters combining, paging as a window |
| `db/repos/users/profile.rs` | leave alone, clear, write nothing, no such account |
| `web/api/certificates.rs` | own vs everybody's, the three states, the device filter and a stranger's device, one certificate's authz matrix including the server's own, revoke → handshake refusal + hook + audit, revoke twice → `409`, non-admin → `403`, the page cap |
| `web/api/users.rs` | self/administrator/stranger on one account, `404` vs `400`, set and clear both fields, an email alone is a change |
| `web/api/devices.rs` | read before write, read equals write, a stranger's state is refused |
| `web/api/groups.rs` | one row per direction, a switched-off member, administrative and `404` |
| `web/api/credentials.rs` | the whole installation's with owners and no secrets, paging, administrative, not combinable with a name |
| `web/api/mod.rs` | six more rows in `PROTECTED`, so each new route is proved to answer `401` without a session |

## Exit checks

```
$ cargo test -p rustak-api
test result: ok. 116 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p rustak-server --features testing --lib -- \
      web::api::certificates web::api::users web::api::groups \
      web::api::devices web::api::credentials
test result: ok. 98 passed; 0 failed; 0 ignored; 0 measured; 1247 filtered out

$ cargo test -p rustak-server --features testing --lib -- \
      db::repos::certificates::list db::repos::users::profile web::api::tests
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 1334 filtered out

$ cargo fmt --all --check
(clean, no output)

$ ./scripts/check-file-length.sh
(exit 0 — and the three new files, which `git ls-files` does not yet see, measure 222, 69 and 41)

$ cd rustak-ui && trunk build
2026-09-18T16:26:00.007840Z  INFO 🚀 Starting trunk 0.21.14
2026-09-18T16:26:00.012080Z  INFO 📦 starting build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.07s
2026-09-18T16:26:06.892762Z  INFO applying new distribution
2026-09-18T16:26:06.894025Z  INFO ✅ success
```

### The three whole-workspace checks fail on other agents' in-progress work

```
$ cargo test -p rustak-server --features testing
test result: FAILED. 1340 passed; 3 failed; 2 ignored
    missions::logs::tests::an_entry_naming_two_missions_is_readable_from_either_and_names_both
    missions::logs::tests::deleting_an_entry_removes_every_row_of_it
    missions::logs::tests::rewriting_an_entry_with_fewer_missions_drops_the_one_it_left_out
      → UNIQUE constraint failed: mission_logs.log_id  (rustak-server/src/missions/logs.rs)

$ cargo clippy --workspace --all-targets -- -D warnings
rustak-core/src/identity/password.rs:332   assertions_on_constants
rustak-server/src/auth/mission_token.rs:235 unnecessary_lazy_evaluations
rustak-server/src/db/repos/missions/changes.rs:155 needless_question_mark
rustak-server/src/missions/{invitations.rs:30, logs.rs:405, logs.rs:453, roles.rs:83, service.rs:202}
rustak-server/src/stream/mission_notify.rs:321 wrong_self_convention

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
rustak-core/src/identity/password.rs:{14,88}  unresolved link to `use_testing_params`
rustak-server/src/marti/missions/mod.rs:19    private-intra-doc-link to `reserved_routes`
rustak-server/src/missions/notify.rs:36       unresolved link to `MissionRoleKind`
rustak-server/src/auth/mission_token.rs:18    redundant explicit link target
```

Every one of these is in a file this brief does not own — the mission and mission-token work, and
`rustak-core/src/identity/password.rs`, which is modified in the working tree by another session.
They were left alone deliberately. Two *were* mine and are fixed: a
`private_intra_doc_links` error for `MAX_PAGE_SIZE` in `web/api/{certificates,credentials}.rs` and a
`cloned_ref_to_slice_refs` warning in the revocation test. Filtered to the files this brief touched,
clippy and rustdoc are both silent, and the whole-workspace commands should pass once the mission
branches land.

## What the UI can now do that it could not

Reading M2-07's deviations back:

- **Deviation 2** ("email is read-only on the Profile tab") — `UserPatch.email` exists and is
  written, `null` clears it.
- **Deviation 4** ("no per-device active channel toggles, because nothing reads the state") —
  `GET /devices/{uid}/active-groups`, so `api::devices::set_active_groups` no longer has to be
  written blind and its `#[allow(dead_code)]` can go.
- **Deviation 6** ("the members table costs N+1 requests") — `GET /groups/{name}/members`, one
  request, with the member count and each member's on/off state.
- **"Certificate #11" / "No certificate" on the Devices tab** — `GET /certificates?device_uid=`
  gives the fingerprint, the expiry, the state and the revoke button behind
  `POST /certificates/{id}/revoke`.
- **The Credentials page** can become an administrator's view of the installation with
  `?all=true`, rather than only the self-service page it had to be.
- **The account page** reads `GET /users/{username}` instead of the whole listing, and can tell a
  name that is not here from one that is not theirs to see.
