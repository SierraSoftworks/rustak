# M4-04 — Fixing the Critical/High findings of the wire-compatibility review (R-02)

**Status: complete.** Every Critical and High finding of `reviews/R-02-wire-compat-review.md` is
fixed, the six Mediums the brief put in scope are fixed, the four contract corrections it listed are
made, and the one extra item the coordinator added mid-task (R-01's mission-token guid match) is
fixed. Every exit check is green.

## Dispositions

### Critical

| # | Finding | Disposition |
|---|---|---|
| C1 | `GET …/subscription?uid=` mints a mission token for an unauthenticated caller | **Fixed.** Two changes, either of which alone would have closed it. The route now goes through the shared `allowed(…, Permission::Read)`, and `MissionService::subscription` — which minted a fresh `SUBSCRIPTION` token on every read — is replaced by `stored_subscription`, which reports the row and nothing else. `subscription_json(…, false)` on this route, so no `token` key is rendered at all. |

### High

| # | Finding | Disposition |
|---|---|---|
| H1 | `/Marti/api/cot/**` not implemented; the oversize pointer targets a dead URL | **Fixed.** New `marti/cot.rs` serves all five routes per research `06` §11 and design 04 §7, mounted on both listeners inside the `/Marti/api` scope (so the scope's own JSON `404`/`405` default service applies). `stream/writer.rs`'s `senderUrl` now resolves. |
| H2 | `/Marti/api/clientEndPoints` leaks the devices of unreachable accounts | **Fixed.** The per-account memo in `marti/contacts.rs` now holds an `Owner { username, channels, visible }` — the visibility answer is *part of* the cached value, so a hit asks the same question a miss did. |
| H3 | `<dest mission=…>` relay bypasses the `IN`/`OUT` check | **Fixed.** `stream/dest.rs::mission_recipients` resolves through `Hub::resolve_uids`, which applies `sender.IN ∩ receiver.OUT` per pair exactly as `<dest uid>` does. The `t-x-m-*` notification path is untouched: §12 says it bypasses the broker deliberately. |
| H4 | `DELETE …/subscription?uid=` has no role check | **Fixed.** Behind `MISSION_READ`, not `MISSION_WRITE`: unsubscribing is what a read-only subscriber does when it leaves, and requiring write would strand it. |
| H5 | `GET`/`DELETE /missions/logs/entries/{id}` have no role check | **Fixed.** `GET` needs `MISSION_READ` on **any** of the entry's missions — exactly the set `{n}/log` would already have shown it to, so the two cannot disagree. `DELETE` needs `MISSION_WRITE` on **every** one, because the delete removes it from all of them, which is the rule the write path already applied. |

### Medium — in scope per the brief

| # | Finding | Disposition |
|---|---|---|
| M1 | The oversize `b-f-t-r` goes stale in 10 s, adds an unconditional `<ackrequest>`, `hae="0.0"`, empty `senderCallsign` | **Fixed**, all four. `stream/writer.rs` now builds the **server-generated** template itself (`stale=+100s`, no `<ackrequest>`, `hae=9999999.0`, `senderCallsign` falling back to `takserver`) rather than calling `rustak_cot::detail::fileshare::fileshare_pointer`, which renders ATAK's own client offer. The two are different messages that share a type; `rustak-cot` is left alone and still renders the client template for the client's use. |
| M2 | OIDC suffix stripping truncates at the last occurrence | **Fixed.** `identity/groups.rs` gains `truncate_at_first`: `ends_with(suffix)` is the gate and `find(suffix)` is the cut, which is what `substring(0, indexOf(suffix))` does. `A_READ_B_READ` → `A`. |
| M3 | `t-x-g-c` on every `PUT /groups/active`, including the no-op | **Fixed, keeping D9.** `marti/channels.rs::apply` now compares the effective selection of the account *and of every one of its devices* before and after, and answers whether anything moved; `marti/groups.rs` sends the notice only when it did. |
| M4 | `/Marti/sync/missionupload` ignores the singular `keyword=` | **Fixed** in `marti/sync.rs` (see deviation 2). |
| M5 | `?offset=N` answers `206` where TAK answers `200` | **Fixed.** `marti/sync_read.rs::range` now reports whether a real `Range:` header asked; an `offset`/`length` query uses TAK's pinned condition `length > 0 && offset + length < total`, and a `Range:` header keeps the ordinary `206`. |
| M6 | `tls/config` advertises `nameEntry` values the issuer drops | **Fixed.** `PkiConfig::enrollment_entries()` is the one function that answers both "what do we advertise" and "what do we issue to a device"; `marti/tls.rs` no longer pads on its own. See deviation 3 for why the CA and server certificates keep the unpadded `subject_entries()`. |
| M7 | `[pki] name_entries` keys neither validated nor honoured | **Fixed.** `PkiConfig::validate_name_entries()` refuses any type outside `O, OU, C, L, ST` at config load, with advice explaining both failure modes (commoncommo's `OBJ_txt2nid` abort, and a key OpenSSL knows that `dn_type` does not). |

### The coordinator's extra item (R-01, Medium)

| Finding | Disposition |
|---|---|
| A mission token is accepted on a `MISSION_NAME` **or** `MISSION_GUID` match, so a never-expiring `ACCESS` token for a deleted mission opens its name-reusing successor | **Fixed.** `missions/roles.rs` requires the **guid** to match. The rename-safety the old rule was for is exactly what the guid check gives — a rename changes the name, not the guid — so nothing is lost. `compat/missions.md` §16 records the withdrawal. |

### Deliberately not done

| # | Finding | Why |
|---|---|---|
| M8 | `boundingPolygon` comma-split | **Contract settled, code unchanged — and the code is right.** Research `06` §7.2 line 941 says `boundingPolygon` is a `List<String>` bound by Spring "same rule" as `group`, and Spring splits a comma-containing value for a collection target. So upstream *also* splits it, and rustak matches. The `lat,lon` pairing survives on the JSON-body path, which rustak already gets right. §3 wins over §4 for the query string; both sections now say so. |
| M9, M10, M12, M13 | `MissionLayer.uids[]` fields, `GET …/layers/{uid}`, port-less `senderUrl`, `?clientUid=` invitation listing | Not in the brief's Medium list. M13 the review itself records as "a hardening recommendation rather than a divergence". |
| L1–L23 | The low table | Not in scope. L5 (`t-x-m-c` excludes the author) and L13 (`read_only_group` becomes a channel of its own) are the two most worth a later brief. |

### Contract corrections (item 6)

| Defect | Fixed in |
|---|---|
| 1. Flow tag rendered `rustak-{server-id}` | `compat/streaming.md` §8 — it is `TAK-Server-{server-id}`, the **code was right**, with a dated correction note saying so. |
| 2. One `b-f-t-r` template where there are two | `compat/files.md` §9 — both templates now given side by side, with which is which and who emits each. |
| 3. §3 vs §4 on `boundingPolygon` | `compat/missions.md` §3 — **§3 won**, from research `06` §7.2; §4's row now says "in the body and in the response". |
| 4. `useCache=false` documented but not implemented | `compat/groups.md` §1 — D8 recorded in place. |
| 5. `/contacts/all` described as "recently seen" with a cache | `compat/contacts.md` Purpose/§1 — corrected to live subscriptions only, which is what the code does. |
| 7. "`<detail>` is always present on the wire" | `compat/streaming.md` §3 — scoped to the proto→XML path, which is the only thing research `05` §12.2 claims. |
| M11, the `<role>` shape | **Code changed, deviation withdrawn.** `stream/mission_payload.rs` renders repeated `<permissions>MISSION_READ</permissions>` text elements per research `05` §7.5; the two goldens are re-pinned; `compat/missions.md` §12 and `status/M4-02` deviation 5 both record it. The deviation cited design 04 §4.8 over the research, and `compat/README.md` makes the research authoritative — it did not hold on its own terms. |

Defect 6 (`MissionInvitation.createTime` format, `expiration` unit) is left: the brief listed four
corrections and this was not one of them, and settling the `expiration` unit is a code decision
rather than a documentation one.

## The authorization matrix

`marti/missions/mod.rs` now owns the only place a mission route resolves a role:

```rust
pub(super) async fn resolved(ctx, reference) -> Result<(Mission, Option<MissionRole>), MartiError>;
pub(super) async fn allowed(ctx, reference, permission) -> Result<Mission, MartiError>;
```

Every per-file `readable`/`writable` is now a one-line wrapper over `allowed`, and nothing else in
the scope calls `service.resolve` + `role_for_request` by hand. **Four routes deliberately do not go
through `allowed`**, each documented at its own definition and listed in the test's `PUBLIC` table:

| Route | Why |
|---|---|
| `PUT {n}/subscription` | it is how a caller *acquires* a role; the credential is the password, the standing invitation or the token in the request (§9) |
| `GET {n}/token?password=` | the password **is** the credential |
| `GET {n}/role` | "what may I do here" has to be answerable by somebody who may do nothing |
| `GET {n}` | an `API_VERSION >= 3` caller gets a stripped `200` rather than a `403` (§5), so the refusal is inspected rather than propagated — it uses `resolved` + `require` |

`tests/missions_authz.rs` walks **every** mounted per-mission route against four callers
(anonymous, signed-in stranger, read-only subscriber, owner). The fixture mission is
**password-protected**, because an ordinary mission hands its default role to anybody who asks
(§5/§9) — so a matrix run against a public one asserts nothing, which is the condition C1, H4 and H5
were all reachable under. A route added without a check fails here the day it is added.

## Every test added, and that each one fails without its fix

| Test | Finding | Verified against the unfixed code |
|---|---|---|
| `missions_authz.rs` — 4 matrix cases + 3 named | C1, H4, H5 | the named C1 and H5 cases are written from the finding |
| `missions_authz.rs::a_token_for_a_deleted_mission_does_not_open_its_successor` | R-01 | — |
| `marti_cot.rs` — 11 cases | H1 | every route was a `404` before |
| `stream_store.rs::an_oversize_message_reaches_a_protobuf_peer_as_a_pointer` (extended) | H1, M1 | fetches the `senderUrl` it produced and asserts a `200` carrying the original event; pins `stale − time == 100 s`, the absence of `<ackrequest>`, and `hae` |
| `marti_channels.rs::every_device_of_an_unreachable_account_is_invisible_not_just_the_first` | H2 | ✅ **fails** with `marti/contacts.rs` reverted |
| `mission_dest.rs::a_subscriber_in_another_channel_gets_the_notice_and_not_the_message` | H3 | ✅ **fails** with `stream/dest.rs` reverted |
| `identity/groups.rs::a_repeated_suffix_truncates_at_the_first_one` | M2 | written from research `06` line 580 |
| `marti_channels.rs::a_selection_that_changes_nothing_sends_no_notice` | M3 | ✅ **fails** with `marti/{groups,channels}.rs` reverted |
| `sync_contract.rs::a_package_upload_keeps_the_singular_keywords_it_was_given` | M4 | ✅ **fails** with `marti/sync.rs` reverted |
| `sync_contract.rs` — offset-only and `Range:` cases | M5 | written from research `06` line 1479 |
| `enroll_flows.rs::every_advertised_name_entry_appears_in_the_issued_subject` | M6 | ✅ **fails** with `pki/facade.rs` reverted |
| `config/validate.rs::a_subject_type_the_issuer_cannot_render_is_refused_at_load` | M7 | written from the finding |
| `config/pki.rs` — two cases | M6, M7 | — |
| `marti/cot.rs` — three unit cases | H1 | — |
| `stream/writer.rs::an_oversize_message_becomes_a_pointer_a_client_can_fetch` (extended) | M1 | — |

`interop/node-tak/src/surfaces.ts` gains a `cotQuery` probe, so the suite reports the surface rather
than being silent about it. The node-tak run went from **24 passed / 1 skipped** to **25 passed**:
the skipped case was gated on a surface that now exists.

## An adjacent bug the `<events>` work turned up

`missions::changes::cot_events_xml` — the mission-scoped `{n}/cot` — concatenated whole stored
documents into its `<events>` wrapper, so the body carried an XML declaration in the middle of it and
no parser would accept a mission with anything in it. The only test covered the **empty** case, which
is why it was never seen. `event_element()` now strips the declaration and both `{n}/cot` and the
four new `<events>` routes use it; `marti_cot.rs` asserts `matches("<?xml").count() == 1`.

## Deviations from the brief

1. **Files outside the brief's list.** The brief's list predates the coordinator's revised ownership
   message. Touched anyway, none of them in the security agent's areas:
   * `marti/{groups,channels}.rs` — M3's suppression has nowhere else to live; `identity/members.rs`
     and `identity/active.rs` were **not** touched, because `identity/**` is M5-02's.
   * `config/pki.rs` — M6/M7's rules belong with the configuration they validate, and putting them
     there kept `config/validate.rs` under 300 functional lines.
   * `pki/facade.rs` — **one line**: `name_entries()` returns `enrollment_entries()`. `pki/**` was in
     the original brief's exclusion list and not in the coordinator's revised one; the change is the
     minimum that makes the advertised and issued subjects the same value.
   * `missions/changes.rs`, `stream/mission_payload.rs` — both in `missions/**` / the named stream
     files.
   * `tests/stream_support/mod.rs` — see deviation 4.
2. **M4 fixed in `marti/sync.rs`, not `files/upload.rs`.** The review locates the omission in
   `Upload::parse`, which both upload routes share. `files/**` is not mine, so the singular `keyword`
   is read in the `missionupload` handler — which is the route the brief names and the only one whose
   documented parameter is the singular. `/Marti/sync/upload` still reads only `keywords`, which is
   its own documented spelling. Worth folding into `Upload::parse` when `files/**` is next open.
3. **The CA and server certificate subjects are deliberately *not* padded.** Making
   `subject_entries()` itself pad would have added `OU=rustak` to the certificate authority's own
   subject on every new installation, which nothing asks for and which no client reads. The padding
   is the *enrolment* contract (CloudTAK's `xml-js`, commoncommo's `X509_NAME_ENTRY_create_by_NID`),
   so it lives in `enrollment_entries()` and applies to client certificates and the advertised
   document, which are the two things that have to agree.
4. **A harness bug fixed to make H2's test possible.** `Harness::write_identity` wrote client
   material to `clients/{username}`, so a second enrolment for the same account silently overwrote
   the first's certificate and both `Identity` handles connected as the same device. H2 needs one
   account with two devices, so the path is now `clients/{username}/{uid}`. No existing test depended
   on the old path, and `marti_channels`, `mission_dest`, `stream_*` and `services_flow` all still
   pass.
5. **`misc::send` is recorded as `MISSION_READ`, not write.** The matrix records what the code asks
   for. R-02 did not flag it and changing it is a behaviour decision, not a review finding; it is in
   the table so that a future change to it is a visible one.
6. **`cot_store/query.rs` was left alone.** `/cot/sa`'s bounding box was going to be decided in SQL
   over the `lat`/`lon` columns, but another agent was rewriting that file mid-task. The box is
   applied over the returned page instead, from the `<point>` in the stored XML — which is the same
   pattern the file's own documentation describes for the channel rule, and is bounded by
   `query::MAX_PAGE`. Worth moving into SQL when that file is next open.

## Exit checks

All green.

```
$ cargo fmt --all --check
FMT OK

$ ./scripts/check-file-length.sh
LENGTH OK        (no output; every file is under 300 functional lines)

$ cargo clippy --workspace --all-targets -- -D warnings
CLIPPY OK        (clean)

$ RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps
DOC OK           (clean)

$ cargo test --workspace --no-fail-fast
40 suites, every one `test result: ok`, 0 failures.

rustak_server (lib)   ok. 1682 passed; 0 failed; 2 ignored
acme_directory        ok. 5       api_v1_live        ok. 14
api_v1_packages       ok. 9       bootstrap          ok. 3
enroll_flows          ok. 14      enroll_oauth       ok. 11
marti_channels        ok. 11      marti_contract     ok. 14
marti_cot             ok. 11 (new)  mission_dest     ok. 11
mission_squash        ok. 4       missions_authz     ok. 7 (new)
missions_extras       ok. 14      missions_flow      ok. 14
oauth_flows           ok. 31      profiles_contract  ok. 14
services_flow         ok. 2       stream_channel_state ok. 3
stream_routing        ok. 12      stream_session     ok. 11
stream_store          ok. 8       sync_contract      ok. 15
plus rustak-{api,client,core,cot} and every doc-test.

$ cd interop/node-tak && npm test
ℹ tests 25   ℹ pass 25   ℹ fail 0   ℹ skipped 0
(was 24 passed / 1 skipped; the skipped case was gated on the CoT query surface)

$ cd interop/eud && npm test
ℹ tests 43   ℹ pass 43   ℹ fail 0   ℹ skipped 0
```

### A note on intermediate runs

Three other agents were editing the workspace throughout this brief, so intermediate runs failed on
half-applied states of their work — `enroll_flows` and `services_flow` each failed for a while and a
bisect pointed at `pki/csr.rs` and then at `pki/revoke.rs`. Both were build-state artefacts: once the
tree settled, every suite passes with those agents' changes **in place**, and reverting them changes
nothing. Nothing in this brief was implicated, and no other agent's file was left modified.

## Files

| Area | Files |
|---|---|
| CoT query surface | `rustak-server/src/marti/cot.rs` (**new**), 1 module line + 1 `configure` line + 5 `PATHS`/`PARAMETERISED` entries in `marti/mod.rs` |
| Authorization | `marti/missions/{mod,subscription,logs,changes,contents,crud,invitations,layers,misc}.rs`, `missions/{roles,subscriptions}.rs` |
| Leaks and routing | `marti/contacts.rs`, `stream/dest.rs` |
| Oversize substitution | `stream/writer.rs` |
| `<role>` shape | `stream/mission_payload.rs`, `stream/mission_notify.rs` (tests), 2 goldens |
| `<events>` rendering | `missions/changes.rs` |
| Mediums | `identity/groups.rs`, `marti/{groups,channels,sync,sync_read,tls}.rs`, `config/{pki,validate}.rs`, `pki/facade.rs` (1 line) |
| Tests | `tests/{missions_authz,marti_cot}.rs` (**new**), additive cases in `tests/{marti_channels,mission_dest,sync_contract,stream_store,enroll_flows}.rs`, `tests/stream_support/mod.rs` (harness fix) |
| Contracts | `.claude/plan/compat/{streaming,files,missions,contacts,groups}.md`, `.claude/plan/status/M4-02-missions-sync-notify.md` (deviation 5 only) |
| Interop | `interop/node-tak/src/surfaces.ts` |

No `git` or `but` command was run.
