# R-02 — Independent compatibility review: wire contracts against the verified research

**Reviewer.** Read-only pass over `rustak-cot/src/**` and `rustak-server/src/{stream,marti,missions,files,profiles,identity,pki,auth,cot_store}/**`, compared byte for byte against
`.claude/plan/compat/*.md` and, where the digest and the code disagreed, against the authoritative
reports `research/{03,05,06,07}`. No source file was edited and no `git`/`but` command was run.
`cargo test -p rustak-cot` and the `mission_dest` / `stream_store` integration suites were run and
are green — every finding below is a gap in what is asserted, not a regression in what is.

**Precedence used.** Per `compat/README.md`, `research/05`, `06`, `07` and `03` are authoritative and
the `compat/*.md` digest is a convenience. Where the digest contradicts its own cited source, the
source wins and the digest is listed as a documentation defect (§"Contract defects" below).

**Ordering.** By client impact: what breaks a verified client's core flow, then what degrades it,
then parity drift no verified client reads.

---

## Findings

### CRITICAL

#### C1. `GET /missions/{n}/subscription?uid=` mints a mission token for an unauthenticated caller

`rustak-server/src/marti/missions/subscription.rs:90-102`

```rust
pub async fn get(ctx: MissionCtx, reference: MissionRef, query: CiQuery) -> MartiResult {
    let mission = ctx.service.resolve(&reference).await?;
    let found = ctx.service.subscription(&mission, &client_uid(&query)?).await?
```

Every sibling handler in the file goes through `allowed(ctx, reference, permission)`
(`subscription.rs:358-372`), which resolves a role and calls `require(...)`. This one calls
`resolve` directly — there is no permission check at all. And `MissionService::subscription`
(`rustak-server/src/missions/subscriptions.rs:213-239`) does not read a stored token, it **mints a
fresh one**:

```rust
let token = self.tokens().await?.issue(
    &row.subscription_uid, TokenType::Subscription, &mission.name, mission.guid, None,
)?;
```

`MartiPrincipal` treats anonymous as a valid answer by design (`rustak-server/src/marti/principal.rs:1-18,
108-113`), `MissionCtx::from_request` never requires an identity
(`rustak-server/src/marti/missions/mod.rs:65-91`), and the whole Marti surface is mounted on the
**public** listener as well as the mTLS one (`rustak-server/src/web/server.rs:68`, `:98`). So an
unauthenticated request naming a mission and a subscriber's client uid returns a token carrying that
subscription's role — `MISSION_OWNER` for the creator's. Replayed as `MissionAuthorization: Bearer …`
it resolves through `rustak-server/src/missions/roles.rs:219-232` and grants delete, set-password,
set-role, and read of a password-protected or invite-only mission.

- **Contract:** `compat/missions.md` §9 (the endpoint) and §10 (the token *is* the credential;
  `/subscriptions/roles` exists as the deliberately token-free listing, and
  `rustak-server/tests/missions_flow.rs:254` asserts that distinction). Research `06` §7.13.
- **Client impact:** all clients; most directly exploitable against CloudTAK, whose Data Sync owner
  subscription uid (`connection-{id}-data-{id}`) is predictable (research `03` §5.3).
- **Note:** this also falls inside R-01's brief. Flagged here because it is a divergence from the
  §9/§10 role model, not only a hardening gap.

---

### HIGH

#### H1. The `/Marti/api/cot/**` query surface is not implemented — CloudTAK 404s and the oversize fallback points at a dead URL

No route under `/Marti/api/cot/` exists. `rustak-server/src/marti/mod.rs:133-241` registers
`/injectors/cot/uid` and nothing else; the CoT store is reachable only from the admin API
(`rustak-server/src/web/api/cot.rs:52-54`, `/api/v1/cot/{uid}`). Requests fall through to
`unmatched` and get a JSON `404`.

Two consequences, both verified:

1. **CloudTAK.** node-tak's `query.ts` exposes `single(uid)` → `GET /Marti/api/cot/xml/{uid}` and
   `history(uid, …)` → `GET /Marti/api/cot/xml/{uid}/all?…`, both parsed to GeoJSON by node-cot
   (research `03` §3.14). Neither answers. `/cot`, `/cot/sa` and `/cot/matchUid` are equally absent
   (research `06` §"CotApi", lines 1736-1766; scoped in `plan.md` §335).
2. **ATAK.** The >64 KiB substitution builds its pointer at
   `{public_url}/Marti/api/cot/xml/{uid}` (`rustak-server/src/stream/writer.rs:186-192`), so the
   `b-f-t-r` ATAK receives resolves to a 404 and the substituted message is lost with a
   user-visible failed transfer. Research `05` §3.6 and §9 pin that URL as the whole point of the
   substitution.

This gap is in neither `.claude/plan/backlog.md` nor the interop probe list
(`interop/node-tak/src/surfaces.ts:29-69`), so nothing currently reports it. `cot_store/mod.rs:6`
and `cot_store/latest.rs:3` both describe themselves as what that endpoint answers from, so the
store side is ready and only the routes are missing.

**Severity:** high for both CloudTAK and ATAK.

#### H2. `/Marti/api/clientEndPoints` leaks the devices of accounts the caller cannot reach

`rustak-server/src/marti/contacts.rs:286-311`

```rust
let owner = match owners.get(&device.user_id) {
    Some(owner) => owner.clone(),          // <- cache hit: visibility never re-checked
    None => {
        ...
        let visible = who.is_admin() || viewer.is_some_and(|viewer| can_reach(&groups, viewer));
        owners.insert(device.user_id, owner.clone());   // <- inserted before the check
        if !visible { continue; }
        owner
    }
};
```

The per-account memo is populated *before* `if !visible { continue; }`, and the hit arm has no
visibility test. So for an unreachable account the **first** disconnected device is correctly
skipped and **every subsequent one** is emitted — carrying that account's `username`, the device
`uid`, its `callsign` and `lastEventTime`. Any account with two or more enrolled devices leaks.
`matches(&owner.1, wanted)` at `:313` is the `group=` query filter only and returns `true` when no
filter was supplied (`contacts.rs:237-245`), so there is no second line of defence.

- **Contract:** `compat/contacts.md` §1-§2 (the `OUT`-direction reachability filter), citing
  research `06` §6.2 and the `05` §6.3 reachability rule.
- **Client impact:** ATAK, CloudTAK, WinTAK, iTAK all display the foreign contacts. Nothing errors —
  they simply show data they should not have.
- **Why no test catches it:** `rustak-server/tests/marti_channels.rs:377-415` asserts the invisible
  account `eve` is absent, but enrols `eve` with exactly one device, so the cache-hit path is never
  taken. Adding a second device for `eve` fails that assertion.
- The connected half (`Hub::snapshot_for`, `rustak-server/src/stream/hub.rs:403-416`) is correct;
  only the disconnected half is affected.

#### H3. `<dest mission=…>` relay bypasses the `IN`/`OUT` reachability check

`rustak-server/src/stream/dest.rs:280-282`

```rust
uids.iter()
    .flat_map(|uid| hub.handles_for_uid(uid))
    .collect()
```

`Hub::handles_for_uid` (`rustak-server/src/stream/hub.rs:308-316`) is a bare index lookup with no
`can_reach` filter — unlike `resolve_callsigns`, `resolve_uids`, `reachable_in_group` and
`reachable_from`, which all apply one. `MissionService::subscribers_for`
(`rustak-server/src/missions/subscriptions.rs:267-292`) excludes the sender and filters to connected
subscribers, but likewise applies no group filter. So the raw CoT relay to mission subscribers
reaches every connected subscriber regardless of channel membership.

- **Contract:** `compat/missions.md` §11 item 2 — "Recipients = every **connected** client currently
  subscribed to that mission, **minus the sender**. Still subject to the normal `IN`/`OUT`
  reachability check." Cross-referenced to `streaming.md` §8 and research `05` §6.3/§7.
- **Not the same as §12.** The `t-x-m-*` notifications *are* contractually allowed to bypass the
  broker, and `stream/notify.rs:78-90` correctly does so. Only the step-2 raw relay is wrong.
- **Client impact:** any client — a subscriber with no channel overlap with the sender receives
  position and chat traffic the channel model says it must not see.
- **Why no test catches it:** `rustak-server/tests/mission_dest.rs` puts every party in the same
  `blue` channel with `Direction::Both` (`:362`, `:369`, `:376`), so the divergent case never runs.

#### H4. `DELETE /missions/{n}/subscription?uid=` has no role check

`rustak-server/src/marti/missions/subscription.rs:113-125` — `resolve` then `unsubscribe`, no
`allowed(...)`. Any caller, anonymous included, can unsubscribe any device from any mission. Combined
with rustak's recorded decision to delete the row rather than honour `disconnectOnly`
(`compat/missions.md` §16), the victim loses the subscription until it re-subscribes, and CloudTAK
only re-subscribes on stream reconnect (research `03` §5.3; `compat/cloudtak.md` §15), so a Data Sync
stays dark until then.

#### H5. `GET`/`DELETE /missions/logs/entries/{id}` have no role check

`rustak-server/src/marti/missions/logs.rs:75-85` and `:93-105`. `logs::listing` (`:133-140`) correctly
requires `MISSION_READ` and `logs::write` (`:160-178`) `MISSION_WRITE`, but reading or deleting an
entry by id bypasses both, including for password-protected and invite-only missions
(`compat/missions.md` §13 "Logs", §9 role model). Ids are UUIDs, so a leaked id is needed — high
rather than critical.

---

### MEDIUM

#### M1. The oversize `b-f-t-r` substitute goes stale in 10 s where TAK Server allows 100 s

`rustak-cot/src/detail/fileshare.rs:16` (`FILESHARE_VALIDITY: Duration = Duration::from_secs(10)`),
applied at `:188-201` and used only by `rustak-server/src/stream/writer.rs:200`.

Research `05` §9 pins the **server-generated** shape (`CommonUtil.getFileTransferCotMessage`) at
`stale='{t+100s}'`, and explicitly says that shape "is also the substitute emitted when an outbound
protobuf message exceeds 64 KiB". `compat/files.md` §9 documents `+10s`, but that is ATAK's own
client-emitted offer (research `07` §6.3) — the digest conflates the two templates. A 10-second
window is the time ATAK has to notice the pointer and start the fetch before the event is stale.

Three smaller divergences in the same substituted message, all against research `05` §9:

- **`<ackrequest>` is added unconditionally** (`fileshare.rs:195-199`). The server-generated template
  has none, and `compat/files.md` §9 itself says it "is only present when the sender wants a
  receipt". ATAK will send a `b-f-t-a` nobody consumes.
- **`hae="0.0"`** — `Point::zero()` (`rustak-cot/src/event.rs:59-67`) leaves `hae` at 0 while the
  template uses `9999999.0`, so a receiver plots the pointer at sea level rather than at unknown
  altitude. `Point::UNKNOWN_HAE` exists at `event.rs:34` and is unused here. The fix belongs in
  `fileshare_pointer`, not in `zero()`, which pings/pongs/notices share correctly.
- **`senderCallsign` is empty** when the event has no callsign
  (`stream/writer.rs:195`, `event.callsign().unwrap_or_default()`); TAK Server uses the authenticated
  user's name or the literal `"takserver"`.

**Client impact:** ATAK, on the >64 KiB path only. Compounds with H1 — even a message fetched in time
hits a 404.

#### M2. OIDC group-suffix stripping truncates at the last occurrence, not the first

`rustak-server/src/identity/groups.rs:216-223` uses `str::strip_suffix`, which removes the trailing
occurrence. Research `06` line 580 is explicit: `substring(0, indexOf(suffix))` "uses the **first**
occurrence, so `A_READ_B_READ` truncates at the first `_READ`", and `compat/oauth.md` §5 restates it.
A directory group `A_READ_B_READ` therefore becomes channel `A` on TAK Server and `A_READ_B` on
rustak.

**Client impact:** any OIDC-federated client (ATAK/WinTAK via `/login/*`; CloudTAK once it gains an
OIDC path) lands in a differently named channel than the same directory yields against TAK Server —
wrong CoT visibility, not a parse failure. Direction mapping (`_READ` → `OUT`, `_WRITE` → `IN`) and
the read-before-write precedence are both correct.

#### M3. `t-x-g-c` is emitted on every `PUT /groups/active`, including the no-`clientUid` case

`rustak-server/src/marti/groups.rs:277-285` → `identity/members.rs:155-183` → `stream/live.rs:174-182`.
There is no `clientUid` gate and no suppression when the submitted selection equals the stored one.

This is a **recorded deviation** — design `04` D9, restated in the `groups.rs` module docs and
`status/M2-06`. Its stated rationale ("CloudTAK never sends a `clientUid`, so a channel toggled from a
browser would never reach the phone") holds for the admin/browser case. It does **not** hold for
CloudTAK's housekeeping: `compat/groups.md` §5 and research `03` §3.4 show `DataMission.sync` PUTs the
full group list with every `active` forced true before creating a Data Sync. Because
`compat/groups.md` §3 says both clients answer `t-x-g-c` by **clearing that server's map items** and
re-fetching, that housekeeping PUT makes every ATAK on the account blank and reload its map. CloudTAK
guards the call with `if (groups.data.some(g => !g.active))`, so it is bounded rather than
per-mission — hence medium.

**Recommendation:** keep D9, but suppress the notice when the applied selection is unchanged. That
preserves D9's benefit without the side effect. The exclusion rule itself (`uid` suffixed with the
originating `clientUid`, sent to the *other* devices) is implemented and tested correctly.

#### M4. `/Marti/sync/missionupload` ignores the singular `keyword=` parameter

`rustak-server/src/files/upload.rs:89` — `keywords: query.strings("keywords")`. `Upload::parse` is the
only keyword reader for both upload routes and reads the plural spelling only;
`rustak-server/src/marti/sync.rs:156-162` then force-adds `missionpackage`, so a package's stored
keyword set is always exactly `["missionpackage"]`.

`compat/files.md` §5 documents the parameter as `keyword` (citing research `06` §9.6), and research
`03` §3.12 gives node-tak's call shape: `?…&keyword=missionpackage&keyword=…`. The read side already
accepts both spellings (`rustak-server/src/files/search.rs:118-119`), which makes the asymmetry easy
to miss.

**Client impact:** CloudTAK — every extra keyword passed to `Files.uploadPackage` is silently dropped,
so a package uploaded with keywords cannot be found by `/Marti/sync/search?keywords=<that keyword>`.
The package still lists, so this degrades rather than breaks.

#### M5. `/Marti/sync/content?offset=N` answers `206` where TAK Server answers `200`

`rustak-server/src/files/store.rs:115-117`

```rust
pub fn is_partial(&self) -> bool {
    self.offset > 0 || self.length < self.total
}
```

Research `06` line 1479 pins the condition as `length > 0 && offset + length < totalSize`. For an
offset-only request `store::open_range` sets `length = total - offset` (`store.rs:163-168`), so
`offset + length == total` and TAK Server returns `200`; rustak returns `206` plus a `Content-Range`
because `offset > 0`.

**Client impact:** ATAK — research `07` §6.3(b) records that `GetFileTransferOperation` resumes a
failed download by appending `&offset=<n>` and nothing else, which is exactly this case. Whether
ATAK's `TakHttpResponse` accepts a `206` there is not established in the research, so the consequence
is unverified; the divergence from the pinned condition is verified. The `Range:`-header branch is
correct. No test covers the offset-only case (`rustak-server/tests/sync_contract.rs:242-254` only
exercises `offset=3&length=4`).

#### M6. `GET /Marti/api/tls/config` advertises `nameEntry` values the issuer then drops

`rustak-server/src/marti/tls.rs:282` advertises `padded_entries(pki.name_entries(), …)`, while
`rustak-server/src/pki/facade.rs:227,242` issues from the unpadded `self.name_entries()`.
`PkiConfig::subject_entries()` (`rustak-server/src/config/pki.rs:242-256`) returns one entry by
default, so with stock config the server tells the client to build `CN + O=rustak + OU=rustak` and
then issues `CN=<user>, O=rustak`. `warn_on_subject_mismatch` (`rustak-server/src/pki/csr.rs:407`)
fires on every stock ATAK enrolment, and `compat/cloudtak.md` §6's requirement that fixtures exercise
a multi-RDN subject is not met by the default path.

**Client impact:** none verified — ATAK does no subject inspection (research `07` §1.8) and CloudTAK
treats the DN as opaque (research `03` §1.5). Medium because the server contradicts itself and
`compat/enrollment.md` §1/§4 require the advertised and issued subjects to agree.

#### M7. `[pki] name_entries` keys are neither validated at load nor honoured at issuance

`rustak-server/src/config/pki.rs:263-272` rejects only blank values and a literal `CN`;
`rustak-server/src/pki/issue.rs:300-310` recognises only `CN/O/OU/C/L/ST|S` and silently skips the
rest (`warn!` at `:229`). Two failure modes, the same class as the `OU=""` bug `compat/enrollment.md`
§1 records:

- A key OpenSSL does not know is advertised verbatim; commoncommo resolves each `nameEntry` with
  `OBJ_txt2nid` and **aborts CSR generation** on an unknown name (research `07` §1.2), so **every
  ATAK enrolment fails at `status 14`** with nothing said at config load.
- A key OpenSSL knows but `dn_type` does not (`DC`, `E`, `STREET`, `SN`) is advertised, reaches the
  client's CSR, and is then dropped from the issued subject.

**Client impact:** ATAK, catastrophically, but only on a misconfiguration the server currently accepts
silently. A load-time validation against the recognised set would close it.

#### M8. `boundingPolygon` query values are comma-split, destroying the `lat,lon` pairs

`rustak-server/src/missions/model.rs:288` uses `query.strings("boundingPolygon")`, and `CiQuery::strings`
(`rustak-server/src/marti/extract.rs:312-320`) does `.flat_map(|value| value.split(','))`. So
`?boundingPolygon=51.5,-0.12&boundingPolygon=51.6,-0.13` is stored as
`["51.5","-0.12","51.6","-0.13"]` and re-emitted that way from `render.rs:78`.

`compat/missions.md` §4 says `boundingPolygon` is an array of `"lat,lon"` strings; §3's blanket
"accept comma-joined on every multi-value param (`group`, `boundingPolygon`, `keyword`)" cannot hold
at the same time. **The contract contradicts itself** (research `06` §7.2 has the same ambiguity) and
the code implements the clause that corrupts the data. Resolve the contract before changing code.

**Client impact:** ATAK/WinTAK/iTAK mission-boundary rendering. CloudTAK's `Mission` TypeBox has no
`boundingPolygon` field (research `03` §3.8), so it is unaffected. The JSON-body path
(`model.rs:321`) is correct.

#### M9. `MissionLayer.uids[]` entries carry only `data`, and `contents` is always empty

`rustak-server/src/marti/missions/layers.rs:228-233` emits `{"data": uid}` and a hard-coded empty
`contents`. TAK Server emits `List<MissionAdd<String>>` — `{data, timestamp, creatorUid, keywords}`
(research `06` §7.15), and CloudTAK's `MissionLayer.uids[]` schema makes `timestamp` and `creatorUid`
**required** (research `03` §3.10). The empty `contents` means a resource filed under a layer never
appears in the tree, even though `mission_contents.layer_uid` exists and `add_content` populates it
(`rustak-server/src/missions/contents.rs:79-97`). Contract: `compat/missions.md` §13 Layers, §4
`MissionAdd<T>`.

#### M10. `GET /missions/{n}/layers/{layerUid}` is not implemented

`rustak-server/src/marti/missions/mod.rs:232-240` registers `PUT …/layers/{uid}/name`,
`PUT …/layers/{uid}/position`, `PUT …/layers/parent` and `GET|PUT|DELETE …/layers`, but not
`GET …/layers/{uid}`; it falls through to the scope default service and answers `404`. TAK Server
serves it (research `06` §7.1, lines 4650/4676) and node-tak's `MissionLayer.get()` calls it
(research `03` §3.10). Contract: `compat/missions.md` §13.

#### M11. `<role>` in `t-x-m-i` / `t-x-m-r` uses a nested `<permission type=…/>` shape TAK does not emit

`rustak-server/src/stream/mission_payload.rs:220-229` produces
`<role type="…"><permissions><permission type="MISSION_READ"/>…</permissions></role>`, pinned in
`rustak-cot/tests/golden/missions/t-x-m-i-invite.xml` and `t-x-m-r-role-change.xml`. Research `05`
§7.5 reads `MissionRole.java:104-105` as `@XmlElement(name="permissions")` on a `Set<String>`, which
JAXB renders as repeated text elements: `<permissions>MISSION_READ</permissions><permissions>…`.
A client reading `role/permissions` text gets nothing from rustak's form.

This is recorded as deviation 5 in `status/M4-02-missions-sync-notify.md` (design `04` §4.8 chosen
over research `05` §7.5) — but **the design document and the research disagree, and per
`compat/README.md` the research wins**. The accepted-deviation rationale does not hold on its own
terms.

**Client impact:** ATAK/iTAK invite handling, if they read the permission list. Not CloudTAK, which
reads the REST role JSON (correct).

#### M12. `[marti] public_host` yields a port-less `senderUrl`, and the shipped example invites it

`rustak-server/src/marti/sync_read.rs:229-234` builds `format!("https://{host}")`.
`config.example.toml:144-148` and `rustak-server/src/config/marti.rs:40-44` both describe the key as
"the host name written into URLs" and show `# public_host = "tak.example.com"`. Following either
produces `https://tak.example.com/Marti/sync/content?hash=…` — port 443, where rustak does not
listen.

`compat/files.md` §5 pins the `missionupload` body as `https://{host}:{marti-port}/Marti/sync/content?hash=…`
and notes "this exact string becomes the `senderUrl` in the `b-f-t-r`"; research `06` §9.6 confirms
`getBaseUrl` is `scheme://host:port`.

**Client impact:** ATAK peer-to-peer package delivery — the receiving EUD fetches `senderUrl` verbatim
(research `07` §6.3(a)) and gets a connection refusal. Same failure class as
`status/CI-01-2026-09-18-mission-package-url.md` chased, now reachable through configuration rather
than the h2 fallback (which is fixed: `web/helpers/request.rs:148-152` reads the authority, which
carries the port). Cheapest fix is the example and doc comment, plus a note that the value may carry
`:port`.

#### M13. `GET /missions/invitations?clientUid=` returns another device's invitation tokens

`rustak-server/src/marti/missions/invitations.rs:197-212` passes the raw query value into
`invite_target` (`rustak-server/src/missions/invitees.rs:59-70`), which takes `client_uid` verbatim
with no check that the caller owns it; the rendered `MissionInvitationJson` carries `token`
(`invitations.rs:55`), and that token is honoured on `PUT …/subscription`
(`subscription.rs:44-56`). TAK Server's endpoint looks equally permissive (research `06` §7.1 line
3029), so this is a **hardening recommendation rather than a divergence** — but the `username`,
`groups` and callsign arms of the same function *are* scoped to the caller, so the `client_uid` arm
is internally inconsistent.

---

### LOW

| # | Finding | File:line | Contract | Client |
|---|---|---|---|---|
| L1 | An `<event>` with no `<point>` is rejected at parse; TAK Server logs it and relays with defaults | `rustak-cot/src/xml/parse.rs:120`, `:137-143` | research `05` §12.2 ("missing … `point` … logged as errors but not fatal"); the control-message drop in §5.2 *is* implemented correctly | none verified — every verified client emits a point; a third-party ETL might not |
| L2 | Missing `hae` defaults to `9999999`, missing `ce`/`le` to `9999999`; TAK defaults `hae` to 0 and `ce`/`le` to `999999` | `rustak-cot/src/xml/parse.rs:148-150`, `event.rs:34-38` | research `05` §12.2 | none — both are "unknown" to a client |
| L3 | `MissionInvitation.createTime` uses padded millis; its sibling `MissionSubscription.createTime` correctly uses unpadded | `rustak-server/src/marti/missions/invitations.rs:54` | research `06` §7.14; `compat/missions.md` lists no invitation date format (gap) | none verified |
| L4 | `MissionChange.logEntry` is never emitted | `rustak-server/src/missions/dto.rs:109-132`, `changes.rs:181-194` | `compat/missions.md` §8; research `06` §7.12 | none — CloudTAK's schema omits it |
| L5 | `t-x-m-c` excludes the author from its recipients | `rustak-server/src/missions/subscriptions.rs:289` | `compat/missions.md` §12 ("every connected subscriber uid"); research `05` §7.3 shows no author exclusion for `t-x-m-c*` | an ATAK device filing content over REST never gets its own `t-x-m-c` back |
| L6 | `<dest mission>` resolves soft-deleted missions; REST reads of the same mission answer `410` | `rustak-server/src/missions/cot.rs:77-93` | `compat/missions.md` §11 | streamed CoT keeps being filed into a tombstone |
| L7 | The nested `mission` on a `201` subscribe carries no changes or logs | `rustak-server/src/marti/missions/subscription.rs:72-75` | research `06` §7.6 (`API_VERSION >= 3`); the route already accepts the window params per `compat/missions.md` §9 | none — CloudTAK reads only `data.token` |
| L8 | UUID-shaped and reserved mission names refused at create | `rustak-server/src/missions/model.rs:157-170` | `compat/missions.md` §1 says explicitly "**Don't special-case this**"; design `04` D7 chose otherwise but §16 does not record it | ATAK/CloudTAK can create UUID-named missions against TAK Server |
| L9 | `allowGroupChange` accepted and ignored; the guard fires whenever `group` is present rather than when the set differs | `rustak-server/src/missions/model.rs:206-207`, `crud.rs:157-167` | `compat/missions.md` §3; not recorded in §16 | none — CloudTAK always sets the flag |
| L10 | `/Marti/sync/missionupload` base64 bodies carry a trailing newline | `rustak-server/src/pki/pem.rs:51` | `compat/enrollment.md` §3; research `06` §3.3 | none — node-tak normalises it, commoncommo tolerates both |
| L11 | Client-cert key usage is `digitalSignature`/`nonRepudiation`(+`keyEncipherment` for RSA); contract pins `digitalSignature`/`keyAgreement`/`nonRepudiation` | `rustak-server/src/pki/issue.rs:253-268` | `compat/enrollment.md` §4; research `06` §3.5 | none — ATAK does no KU inspection (research `07` §1.8). Unrecorded deviation |
| L12 | `notBefore` backdated 5 minutes; TAK uses 720 | `rustak-server/src/pki/issue.rs:98` | `compat/enrollment.md` §4 | an EUD >5 min behind rejects the cert it was just issued. Worth a deliberate decision |
| L13 | `read_only_group` is not removed from the granted set, so the marker group becomes a channel of its own | `rustak-server/src/identity/groups.rs:173-192` | research `06` §4.7 step 5; not pinned by `compat/oauth.md` §5 | OIDC deployments get a spurious channel |
| L14 | `Accept` negotiation on `signClient/v2` differs in precedence and case-sensitivity | `rustak-server/src/marti/tls.rs:82-90` | research `06` §3.3 (lowercases; tests `*/*`/`application/json` before `application/xml`) | none — neither verified client sends a list or unusual casing |
| L15 | `Authorization: Basic` scheme match is case-sensitive | `rustak-server/src/auth/basic.rs:77-79` | RFC 7617 | none — both verified clients send `Basic` |
| L16 | `/Marti/api/files/metadata` defaults `ascending` to `false`; TAK defaults `true` | `rustak-server/src/marti/files.rs:250` | `compat/files.md` §8; research `06` §9.9 | negligible — CloudTAK matches on `entry.Hash` |
| L17 | `Expiration` in the file-manager map carries a `.000` millisecond field | `rustak-server/src/files/legacy.rs:163-170` | `compat/files.md` §8; research `06` §9.9 | none — no verified client reads the key |
| L18 | Profile routes absent from the `PATHS` tables, so a wrong-verb request gets `404` instead of `405` | `rustak-server/src/marti/mod.rs:280-333` | `status/M3-01` asks explicitly for the table to be kept in step | none — the response stays JSON and non-3xx |
| L19 | `DELETE /Marti/api/subscriptions/delete/{uid}` envelope `type` is `java.lang.String`; upstream is `String` | `rustak-server/src/marti/subscriptions.rs:201-204` | research `06` §6.5, which draws the distinction deliberately | none — Tier 4 admin-only |
| L20 | `SubscriptionInfo.port` is a JSON number; upstream is a string | `rustak-server/src/marti/subscriptions.rs:60` | research `06` §6.3 | none — Tier 4 |
| L21 | `/token/access` envelope `type` is `java.lang.String`; upstream is `String` | `rustak-server/src/auth/oauth_server/session.rs:89` | research `06` §4.5 | none — no verified client reads it |
| L22 | `<point>` renders float-formatted doubles (`lat="0.0"`, `ce="9999999.0"`) where the templates show `lat="0"`, `ce="9999999"` | `rustak-cot/src/xml/write.rs:75-81` | `compat/streaming.md` §6/§9, `missions.md` §12, research `05` §5.4/§7.1 | none — every client parses these as doubles. Noted only because the templates pin the spelling |
| L23 | `mission.expiration` is epoch **seconds** in code and epoch **millis** in the contract | `rustak-server/src/missions/dto.rs:52-53`, `model.rs:64-65`, `jobs/mission_expiry.rs:33-36` | `compat/missions.md` §4 says millis; design `04` line 437 says seconds; research `06` says only `Long` | none — nothing on the wire type-checks it. Code is internally consistent; the contract needs settling |

---

## Accepted deviations re-checked

| Deviation | Verdict |
|---|---|
| `compat/streaming.md` §1 — no `<auth>` stream handshake | **Holds.** ATAK never sends one over a cert-authenticated connection (research `07` §3.3), CloudTAK never sends one at all (research `03` §4.1), and `XmlScanner::find_start` (`rustak-cot/src/codec/xml_frame.rs:47-68`) discards it silently as the contract requires. |
| `compat/missions.md` §10 — mission tokens use a dedicated sealed secret | **Holds.** HS256 over a 32-byte secret under `SecretContext::MissionTokenKey` (`rustak-server/src/auth/mission_token.rs:184-196`), not the TLS/RS256 key. Audience binding to one mission is enforced at `missions/roles.rs:219-221`. |
| `compat/cloudtak.md` §8 — video stubbed as an empty list | **Holds.** `GET /Marti/api/video` returns a bare `{"videoConnections": []}` and writes are refused. |
| `compat/oauth.md` §4 — rustak is a full OIDC authority, not an LDAP-bind proxy | **Holds.** Every `/login/*` deviation matches §4's table and M5-01's rationale; CloudTAK never exercises the path. |
| `compat/groups.md` D8 — `useCache` ignored on `/groups/all` | **Holds.** ATAK hardcodes `useCache=true` (research `07` §5 line 469) and CloudTAK passes `useCache: true` (research `03` §3.4), so both verified clients get exactly TAK's behaviour. But D8 is **not recorded in `compat/groups.md` §1**, whose text says the opposite — see contract defects. |
| design `04` D9 — `t-x-g-c` without `clientUid` | **Holds for the stated case, but has an unintended consequence.** See M3. |
| design `04` D12 — `senderUrl` never rewritten | **Holds** for the relay path; the configuration side is M12. |
| design `04` D14 — `.pref` and manifest values XML-escaped where TAK does not | **Holds.** ATAK uses a real parser; escaping is strictly more correct. |
| `status/M2-03` §7 — `401` rather than `400` for `invalid_grant` | **Holds.** node-tak's branch is `[401,403].includes(status)`. |
| `status/M4-02` deviation 5 — nested `<permission>` elements in `<role>` | **Does not hold.** See M11 — it cites design `04` §4.8 over research `05` §7.5, and the research is authoritative. |

---

## Contract defects (the digest contradicts its own cited source)

These need a `compat/*.md` edit, not a code change. Each is a trap for the next implementer.

1. **`compat/streaming.md` §8 renders the flow tag as `<_flow-tags_ rustak-{server-id}="…">`.**
   Research `05` §5.8 line 409 pins the attribute name as `"TAK-Server-" + serverId`, and research
   `07` §875 corroborates it from a packet capture. **The code is right**
   (`rustak-cot/src/detail/flow_tags.rs:18-20`) and the digest is wrong.
2. **`compat/files.md` §9 gives one `b-f-t-r` template at `stale=+10s`.** Research `05` §9 and `07`
   §6.3 describe two different templates — ATAK's client offer (`+10s`) and the server-generated one
   (`+100s`, no `<ackrequest>`, `hae="9999999.0"`). The digest conflates them, which is how M1 got in.
3. **`compat/missions.md` §3 and §4 contradict each other on `boundingPolygon`** — see M8.
4. **`compat/groups.md` §1 states the `useCache=false` behaviour that design `04` D8 deliberately
   does not implement.** Record D8 in §1.
5. **`compat/contacts.md` Purpose/§1 say `/contacts/all` is "connected-or-recently-seen" backed by a
   last-known cache.** Research `06` §6.1 shows upstream `getAllContactsLite` returns live
   subscriptions only, which is what the code does. Correct the wording, not the code.
6. **`compat/missions.md` lists no date format for `MissionInvitation.createTime`** (L3), and §4's
   `expiration` unit disagrees with design `04` (L23).
7. **`compat/streaming.md` §3's "`<detail>` is always present on the wire, even if empty"** is a fact
   about TAK Server's *proto→XML* output (research `05` §12.2), not a universal rule — §6's own pong
   template has no `<detail>` at all. The code is right to omit it; the sentence should be scoped.

---

## Contract tests I would add

Ordered by the finding they would have caught.

1. **Route-presence test for the Marti CoT surface** (H1). Extend the existing route-table test in
   `rustak-server/src/marti/mod.rs` with `/Marti/api/cot/xml/{uid}`, `…/all`, `/cot`, `/cot/sa`,
   `/cot/matchUid`, asserting `401`/`200` rather than the default-service `404`. Add
   `/Marti/api/cot/xml/{uid}` to `interop/node-tak/src/surfaces.ts` so the probe reports it.
2. **End-to-end oversize substitution** (H1 + M1). Extend
   `rustak-server/tests/stream_store.rs::an_oversize_message_reaches_a_protobuf_peer_as_a_pointer` to
   `GET` the `senderUrl` it produced and assert a `200` with the original event, and pin
   `stale - time == 100s` plus the absence of `<ackrequest>`.
3. **`clientEndPoints` with a two-device invisible account** (H2). Enrol a second device for `eve` in
   `rustak-server/tests/marti_channels.rs:377-415`; the existing assertion then fails.
4. **Mission relay reachability** (H3). A `mission_dest.rs` case where the subscriber holds a
   *different* channel from the sender, asserting the raw CoT does **not** arrive while the
   `t-x-m-c` notification does.
5. **Authorization matrix over every mission route** (C1, H4, H5, M13). A table-driven test that
   walks each registered mission route anonymously and asserts `401`/`403`, with an explicit
   allow-list for the genuinely public ones. This catches the whole class rather than the three
   instances found.
6. **Golden `.xml` for the server-generated `b-f-t-r`** alongside the existing
   `rustak-cot/tests/golden/fileshare.xml`, pinned to research `05` §9's shape.
7. **OIDC suffix property test** (M2): `A_READ_B_READ` → `A`, plus the `_WRITE` and bare cases.
8. **`t-x-g-c` suppression** (M3): a `PUT …/active` that changes nothing emits no notice.
9. **`keyword=` round trip** (M4): upload with `?keyword=x`, then find it via
   `/Marti/sync/search?keywords=x`.
10. **Offset-only range** (M5): `?offset=3` with no `length` asserts `200`, not `206`.
11. **`tls/config` ↔ issued-subject agreement** (M6, M7): a property test that every `nameEntry` the
    config advertises appears in the issued subject, and a config-load test that rejects a key
    `dn_type` does not recognise.
12. **`MissionLayer` schema conformance** (M9): validate the layer response against CloudTAK's
    required `timestamp`/`creatorUid`.
13. **`boundingPolygon` round trip** (M8) — once the contract's self-contradiction is settled.
14. **Negative framing corpus** for `rustak-cot`: an `<event>` with no `<point>`, an `<events>`
    wrapper, a stray `<auth>` document, and a `0xBF` byte inside an XML payload, asserting the
    documented tolerance rather than a drop (L1).

---

## Manual checks that remain necessary

These cannot be settled from source and need a real client or a packet capture.

1. **Does ATAK accept a `206` on the `&offset=` resume path?** (M5.) The research does not say; only a
   real `GetFileTransferOperation` resume against both servers answers it.
2. **Byte-exact dom4j output** — declaration quoting, the newline after the declaration, the absence
   of a trailing newline. Research `05` "Things I could not confirm" item 2 flags this as inferred
   from dom4j defaults, not observed. rustak hard-codes it
   (`rustak-cot/src/xml/mod.rs:29`, `xml/write.rs:22-23`); a capture against a real TAK Server would
   confirm it.
3. **WinTAK and iTAK** are unverified throughout — every "client impact" line above that names them
   is inferred from ATAK's behaviour. The `.pref` `class` literals, the enrolment `nameEntry` set and
   the `<role>` permission shape (M11) are the three places where a WinTAK/iTAK difference would be
   most likely and most expensive.
4. **Whether ATAK/iTAK read `role/permissions` from `t-x-m-i`** (M11). If neither does, M11 drops to
   low and the M4-02 deviation can be re-recorded against the research rather than contradicting it.
5. **`t-x-takp-v` against a real ATAK build** — the negotiation templates are byte-pinned in unit
   tests against a shape re-derived from source, never against a capture. The `serverVersion` string
   (`rustak-`+semver) is displayed by CloudTAK and worth eyeballing once.
6. **A CloudTAK Data Sync end-to-end** exercising `<dest mission>` from an ATAK while a second ATAK
   in a different channel is subscribed (H3), which is the cheapest confirmation that the
   reachability fix is correct once made.

---

## Coverage — checked and found correct

**CoT XML (`rustak-cot/src/xml/**`).** Declaration + `\n` + `<event …>`, no trailing newline,
messages head-to-tail with no separator (`codec/mod.rs:288-294`); `<event>` is never self-closed and
the writer's own test asserts it; attribute order matches the documented envelope order including the
optional `access`/`qos`/`opex`/`caveat`/`releasableTo` tail; values XML-escaped; control characters
`U+000B`–`U+001F` and `U+007F`–`U+009F` stripped with `\t`/`\n` retained in character data only;
CDATA `]]>` split safely and comment `--` neutralised; doubles rendered shortest-round-trip and never
`NaN`/`inf`. Inbound framing scans for the literal `</event>`, discards bytes before the first
`<event`, refuses to match `<events>`, resumes correctly across a split `</event>`, and consumes an
8 MiB-over message before reporting it.

**Protobuf (`rustak-cot/src/{proto,codec}/**`).** `0xBF` + LEB128 length + `TakMessage`; mesh framing
never shared; resync-on-bad-magic with a counter rather than a hang; `MAX_PROTO_PAYLOAD = 65_536`,
`MAX_MESSAGE = 8 MiB`. Field numbers in `rustak-cot/proto/tak_protocol_v1.proto` match the reference
`@tak-ps/node-cot` protos message for message on the shared subset (independently compared), with 3
and 4 reserved on `TakMessage`. Promotion gating is exactly the documented attribute-count rule;
non-promoted children are concatenated into `xmlDetail` with no wrapper; on decode the typed elements
are emitted first and an `xmlDetail` element of the same name **wins**, per research `05` §12.2.

**Negotiation and connect sequence.** Register → replay latest SA (plain XML) → exactly one
`t-x-takp-v` (`stream/connection.rs:132-138`); the offer is `None` on a second call; `t-x-takp-q` with
a missing or unreadable version gets **no answer at all**; `t-x-takp-r` carries the literal
`"true"`/`"false"` and the switch happens on the writer task in the same step as the answer, so no XML
can follow it (`stream/writer.rs:114-130`). Negotiation `<point>` correctly uses the `999999` sentinel
while pings/pongs/notices use `9999999`. CloudTAK is never switched, because it never asks.

**Control messages and keepalive.** The set is exactly the eleven types, matched
case-insensitively for classification (`rustak-cot/src/types.rs:75-99`) and case-**sensitively** for
action, so a shouted `T-X-C-T` is consumed without a pong — matching TAK Server. The `t-b` family is
ignored: no XPath filter, no dial-out. The fall-through is a no-op, not
`deleteSubscription(c.getUid())`. The pong is `uid="takPong"`, `how="h-g-i-g-o"`, stale +20 s, **no
`<detail>`**, sent straight to the pinging connection with no flow tag and no channel check. Incognito
drops non-control traffic unless a `<dest callsign>` is present and is skipped by SA replay.

**Routing.** `<dest>` precedence is callsign → publish → uid → mission → mission-guid → group, first
match wins, with `after` read only alongside `path` (`rustak-cot/src/detail/marti.rs:136-154`).
`"All Streaming"` discards the whole callsign list (`stream/dest.rs:223-228`) and a lone one degrades
to broadcast. `<dest publish>` matches nobody but still counts as explicit. `<marti>` is stripped from
**every** relayed message (`stream/router.rs:129`). Reachability is `sender.IN ∩ receiver.OUT` per
matching bit (`rustak-core/src/identity/groups.rs:219-224`), applied per pair, with implicit broadcast
self-excluding and explicit addressing not — and explicit addressing still asking the channel
question. Flow tags are added once, refreshed rather than duplicated, and a message already carrying
this server's tag is dropped.

**GeoChat bounce.** The bounce is the sender's own message with only `type` changed, `<marti>` and
this server's flow tag removed, other servers' tags left alone, delivered straight down the sender's
connection with no flow tag of its own. It fires only when the sender named people (`<dest callsign>`
or `<dest uid>`, and no mission on the list), never on a broadcast, never on a `b-t-f-s`, and the
receipts `b-t-f-d`/`-r`/`-p` do bounce (`rustak-cot/src/msgs.rs:173-207`, `stream/dest.rs:181-183`).

**Disconnect and group change.** `t-x-d-d` carries `<link relation="p-p" uid= type=/>` in that
attribute order, is sent only for a subscription that had both a callsign and a clientUid, and is
computed after unregistration so it never reaches its own subject. `t-x-g-c` carries a bare peer link
and goes to the account's other devices, with `.{clientUid}` appended to the uid when one is named.
Both use `how="h-g-i-g-o"` and a 20 s stale.

**Subscription state.** The first message carrying a `<contact endpoint>` fixes `clientUid` and
`callsign`; later SA messages refresh team/role/takv, leaving a field the client did not send alone;
parse failures yield `"unknown"`, never an error. `*:-1:stcp` is the sentinel
(`rustak-cot/src/detail/contact.rs:8`).

**Marti envelope and headers.** `version:"3"`, per-endpoint `type` strings verified against research
`06` §1.2 for every endpoint in scope, `data`/`messages` omitted rather than null, `nodeId` stable.
Content-Type is a `HeaderValue::from_static("application/json")` written explicitly on every JSON
response — `HttpResponse::json` is used nowhere, so no `; charset=utf-8` can appear. The `text/json`
exception is applied to exactly two routes (`POST /Marti/sync/upload`, `GET /Marti/sync/search`) and
nowhere else, matching the two node-tak call sites with a `JSON.parse` fallback. `NormalizePath` is
installed nowhere; each of `/Marti`, `/files/api` and `/oauth` carries its own JSON `404`/`405`
default service; `marti/headers.rs:135-158` turns any escaped `3xx` into a `500`, exempting only the
`304` on `marti/profiles.rs:200`. The single `HttpResponse::Found` in the tree
(`auth/oauth_server/authorize.rs:268`) is deliberately mounted outside those scopes.

**Dates.** Three formatters plus the Java `Date.toString()` form, each used where its contract says:
padded `.SSS'Z'` for `/Marti/GetTime`, legacy `SubmissionDateTime`, `Mission.createTime`/`lastEdited`,
`MissionAdd.timestamp`, `MissionChange.timestamp`/`serverTime` and `LogEntry.servertime`/`dtg`/`created`;
unpadded `.S'Z'` for `ClientEndpoint.lastEventTime`, `MissionSubscription.createTime` and modern
`Resource.submissionTime`; bare `yyyy-MM-dd` for `Group.created`; `EEE MMM dd HH:mm:ss UTC yyyy` for
the `Time` key of `/Marti/api/files/metadata`. `servertime` and `serverTime` are correctly kept
distinct. All render a literal `Z`.

**Group and contact JSON.** `bitpos` is a `u32` (structurally non-negative), `created` date-only on the
way out and epoch-millis-tolerant on the way in, `direction` only ever `IN`/`OUT`, `active` always
present. `/contacts/all` is a bare array with all seven keys always present and never null.
`/clientEndPoints` sets the no-store cache headers, never serialises `groups`, uses only the two
literal `lastStatus` values, and hard-`403`s an unreachable `group=` filter.

**Enterprise Sync.** All 22 legacy Title-case `Metadata` keys at the contract's exact capitalisation,
`EXPIRATION` the sole all-caps one, `Size`/`PrimaryKey` as strings, the four array-valued fields as
arrays, absent values omitted rather than null, `resultCount` a real JSON number. `Files` is a
`BTreeMap<String, String>` — every value a string. `MANIFEST/manifest.xml` at that exact case with
`<MissionPackageManifest version="2">`, `<Configuration>`/`<Parameter name= value=>` and
`<Contents>`/`<Content zipEntry= ignore=>`, no inter-element whitespace, values escaped.

**Device profiles and `.pref`.** All five endpoints at the exact paths with no
`/device/profile/enrollment` alias; `204`/`200`/`304` semantics correct including `tool_exists`
distinguishing `404` from `304`; `Last-Modified` RFC 1123 truncated to whole seconds on both sides.
The `.pref` bytes are exactly `<?xml version='1.0' standalone='yes'?><preferences><preference
version="1" name="com.atakmap.app.civ_preferences">…`, single-quoted declaration, no `encoding`, no
whitespace; every one of the five `class` literals carries the redundant `class ` token; `cot_streams`
uses only keys from research `07` §2.5, indexed with `0`, with US-English `cache_creds_both` and no
`username`/`password`.

**Enrolment.** `ns2:certificateConfig` with the literal non-URI `xmlns:ns2`, `standalone="yes"`,
`<nameEntries>` always present, and `padded_entries` guaranteeing ≥2 entries and never an empty value
(the commoncommo `status 14` regression is fixed and unit-tested). `signClient/v2` defaults to JSON,
serves XML only for an explicit XML `Accept`, always `200` never `201`, keys `signedCert`/`ca0`/`ca1…`,
bare base64 with no PEM armour. v1 is PKCS#12 with alias `signedCert`/`ca0…`, password `atakatak`.
Status mapping matches ATAK's `enrollmentmanager.cpp:1008-1030`, CN compare case-insensitive, one-time
tokens spent by `signClient` only. CSR ingestion accepts PEM (both banners), bare base64 and raw DER.

**OAuth and JWT.** `/oauth/token` is form-encoded only, unknown fields tolerated, body exactly
`{access_token, token_type:"Bearer", expires_in}` with no `refresh_token` and no `scope`. The JWT
header is pinned at the literal 27-byte `{"alg":"RS256","typ":"JWT"}`, encoded once and concatenated
manually so no serialiser can reorder it; `aud` is a string not an array; all claims are flat scalars;
`issue` refuses to mint a payload containing more than one `}`; the byte alignment was walked for all
three payload-length residues, so node-tak's `split('}')[1]` recovers the claims in every case. `sub`
is the lowercased username, which is also the CN in the issued certificate.

**Missions.** Every envelope `type` string across all eleven families; `Mission`'s eight array fields
declared as plain `Vec<_>` with no `skip_serializing_if`, so they serialise as `[]` and no
`Option<Vec<_>>` can leak a `null`; `token`/`ownerRole` only on the `201` create; subscribe returns
**`201`** with the FQCN type and a `SUBSCRIPTION` token; the subscribe auth order
(password → token → standing invitation → `403`) including the §9 edge case that a stray `password`
on an unprotected mission is a `403`; `MissionChange` field-for-field with `isFederatedChange` always
present and `contentHash` correctly absent; the `t-x-m-*` type table including `t-x-m-r` →
`mission/@type="INVITE"`; `<mission>` attributes versus `<MissionChanges>`/`<MissionChange>` child
elements with `<details>` back to attributes and `<location>` as a child; `serverTime` correctly
absent from the XML; one `MissionChange` per event so CloudTAK's `missionChanges.length === 1` branch
fires; `mission_layers` the only snake_case key; `squashed` defaulting `true` on `/changes` and
`false` on `?changes=true`; `/cot` answering `<events>` even when empty, never `404`, with `<marti>`
stripped.
