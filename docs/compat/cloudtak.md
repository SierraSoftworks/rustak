# CloudTAK — manual compatibility checklist

CloudTAK is the one client with a full-stack automated suite (`interop/cloudtak/`, nightly, driving
CloudTAK's own published container and REST API — see [`docs/interop.md`](../interop.md) → "The
CloudTAK stack"). That suite already proves the wire-level round trip: server configuration, login,
channels, Data Sync create/subscribe/marker/file, changes and a package share, plus a Playwright
smoke of the login page, the map and the Data Sync menu. **Run that job first** (it costs a
`run-cloudtak`-labelled PR or a `workflow_dispatch`) — a manual pass on this page is not a substitute
for it, and if the automated suite has never gone green for this checkout, that is the thing to fix,
not this page.

What this page is for instead: things an API-level assertion cannot see — whether a marker actually
*renders* on the map, whether a file preview works, whether the operator sees a sane error instead of
a stuck spinner — and confirming the documented CloudTAK-side gotchas (TLS trust, the `!ECDH`-free
listener, force-activated channels) against a real CloudTAK, not just against fixtures. Run this once
per release that touches the Marti surface, using a real CloudTAK instance (the compose stack in
`interop/cloudtak/`, or a hand-run one) and a fresh rustak. Record the result (CloudTAK version, date,
outcome per section) in the release notes.

## 0. What is already covered automatically

Do not re-check these by hand.

| Covered | Where |
|---|---|
| `GET /files/api/config` connectivity smoke test, the password grant, `signClient/v2` enrolment | `interop/cloudtak/src/{enroll,api}.ts` steps, and `rustak-server/tests/enroll_oauth.rs` |
| JWT byte-alignment and flat-claims shape CloudTAK's non-verifying parser depends on | `rustak-server/tests/oauth_flows.rs` |
| `Content-Type: application/json` exactness, never-3xx, envelope shapes | `rustak-server/tests/marti_contract.rs` |
| Channels: `Group` JSON, `bitpos`, force-activate-all-groups before mission creation | `rustak-server/tests/marti_channels.rs`, `interop/cloudtak/src/steps-setup.ts` |
| Data Sync: create (`201`+`token`+`guid`), subscribe, contents (marker + file), changes, package share | `rustak-server/tests/missions_flow.rs`, `sync_contract.rs`, `interop/cloudtak/src/steps-datasync.ts` |
| The Playwright smoke: login page, map canvas, Data Sync menu | `interop/cloudtak/src/smoke.ts` |
| The full round trip against CloudTAK's own published container, nightly | `interop/cloudtak/` (see [`docs/interop.md`](../interop.md) → "The CloudTAK stack") |

## 1. Server configuration and first login

CloudTAK has no QR-scan equivalent — it is configured with the three URLs and a username/password,
and its own first `PATCH /api/server` call does more than a smoke test (`.claude/plan/compat/
cloudtak.md` §2): it validates a certificate over mTLS, runs the password grant, mints its own
enrolment certificate, and makes that account CloudTAK's system administrator, all in one call.

1. In rustak, mint an ordinary account with a **client password** (`Credentials` on that account, not
   the server's own administrator — CloudTAK should never be handed rustak's admin identity, since
   that would hide an authorisation bug that bites every other CloudTAK user).
2. In CloudTAK's setup wizard, enter the three URLs (`ssl://host:8089`, `https://host:8443`,
   `https://host:8446`) and that username/password.
3. [ ] The wizard succeeds and that account becomes CloudTAK's administrator (this is CloudTAK's own
   behaviour, not rustak's — confirm it happens, don't try to prevent it).
4. [ ] If rustak's `:8446` listener is serving the `internal` CA rather than a certificate CloudTAK's
   Node runtime already trusts, login **fails outright** unless the CA is mounted into CloudTAK's
   container and named in `NODE_EXTRA_CA_CERTS` — this is the most common CloudTAK bring-up failure
   (`.claude/plan/compat/cloudtak.md` §3, tracked upstream as CloudTAK issue #983). If you hit it,
   that is expected, not a rustak bug — the fix is the environment variable, not the certificate.
5. [ ] **Activity** (`/admin/activity`, category *Enrollment*, then *Pki*) shows the token/password
   spent and the certificate issued for CloudTAK's account.

## 2. Channels UI

1. Grant the CloudTAK account two channels with different directions (as in `atak.md` §3).
2. In CloudTAK's UI, open its channel/group settings.
   - [ ] Both are listed and their direction is represented sensibly in the UI (read-only vs.
         read-write, however CloudTAK phrases it).
3. **Before creating any Data Sync**, CloudTAK is documented to force-activate every channel the
   account holds (`groups.md` §5) — if you toggle a channel off in rustak's **Channels** page and then
   ask CloudTAK to create a Data Sync, expect CloudTAK to silently re-activate it first.
   - [ ] Creating a Data Sync succeeds even when a channel was left inactive going in.
4. Remove the account's membership of a channel from rustak's admin UI while CloudTAK is connected.
   - [ ] CloudTAK reacts to the resulting `t-x-g-c` the same way ATAK does — re-fetching groups and
         updating what it shows — rather than needing a page reload. If it needs a manual refresh to
         notice, that is worth a note, not necessarily a defect (CloudTAK's own reactivity here has
         less verification behind it than ATAK's).

## 3. Chat

CloudTAK maintains its own chat history mirror and associates a `chatRoom` with each Data Sync, so a
GeoChat exchange between CloudTAK and a real EUD is a genuine interop check, not a formality.

1. With CloudTAK and an ATAK device sharing a channel, send a chat message from CloudTAK's UI.
   - [ ] The ATAK device receives it as an ordinary `b-t-f` message.
2. Send a message from the ATAK device back.
   - [ ] It appears in CloudTAK's chat UI.
3. [ ] **To confirm**: CloudTAK's behaviour on an undeliverable direct message (the `b-t-f-s` bounce,
   `atak.md` §4 step 4) — whether its UI surfaces a failure or not has not been checked from source.

## 4. Data Sync — marker, file, log, package

The automated suite already runs this at the API level (§0); this section is for confirming it
*looks* right and behaves sanely under a manual hand.

1. Create a Data Sync from CloudTAK's UI.
   - [ ] **Missions** (`/admin/missions`) shows it; **Activity** (category *Mission*) shows the
         creation.
2. Subscribe an ATAK device to the same Data Sync (`atak.md` §6).
3. Add a marker from CloudTAK's map.
   - [ ] It renders on CloudTAK's own map (not just present in the API response) and appears on the
         subscribed ATAK device.
4. Add a marker from the ATAK device.
   - [ ] It renders on CloudTAK's map without a manual refresh — CloudTAK is documented to listen for
         a live `t-x-m-c` push, not to poll `/changes`.
5. Attach a file to the Data Sync from CloudTAK (the `PUT /api/marti/package` path, not the multipart
   upload — `.claude/plan/status/M4-03-interop-cloudtak-compose.md` decision 6).
   - [ ] The file is visible and previewable from CloudTAK's UI, and appears in **Mission**'s content
         list.
6. Add a log entry from CloudTAK.
   - [ ] It appears in the Data Sync's log for the ATAK device too.
7. Delete the Data Sync from rustak's **Missions** page.
   - [ ] CloudTAK reflects the deletion (`t-x-m-d`) without a manual reload, or at least on its next
         normal refresh — note which, since this has not been characterised precisely for CloudTAK.

## 5. Disconnect / reconnect

CloudTAK maintains its own long-lived stream connection to rustak on the account's behalf, so
"disconnect/reconnect" here means restarting either side of that connection, not a phone losing
signal.

1. Restart rustak (or otherwise force the stream connection down) while CloudTAK has active Data Sync
   subscriptions.
   - [ ] CloudTAK's connection recovers on its own once rustak is back.
   - [ ] CloudTAK **re-subscribes to every one of that connection's Data Syncs** on reconnect — this is
         documented CloudTAK behaviour (`missions.md` §15), so a Data Sync that goes quiet and never
         comes back after a restart is a regression worth chasing, not an expected gap.
2. **Note the retry limitation while you're here**: CloudTAK's own retry logic only retries on
   connection-refused — a transient `5xx` from `PUT …/subscription` during the reconnect window can
   silently drop a Data Sync until CloudTAK's *next* reconnect, not the current one. If step 1 fails
   intermittently, check whether rustak returned a `5xx` during the resubscribe window before treating
   it as a routing bug.
3. [ ] **Clients** (`/admin/clients`) and **Activity** (category *Stream*) show the drop and the
   reconnect.

## 6. Revocation

1. Revoke CloudTAK's enrolment certificate from **Devices** or the account's **Account** page.
   - [ ] CloudTAK's connection drops and it cannot reconnect until it re-enrols.
   - [ ] **Activity** (category *Pki*) shows the revoke.
2. **To confirm**: CloudTAK is documented to re-enrol automatically whenever its stored certificate is
   within 7 days of expiry, using the client password it already holds (`enrollment.md` §7) — that is
   an *expiry* path, not a *revocation* path, so it should **not** paper over a hard revoke. Confirm
   CloudTAK does not silently re-enrol immediately after an explicit revoke; if it does, that is worth
   a bug report against CloudTAK's own re-enrolment trigger, not something to work around in rustak.
3. Revoke the client password itself (rather than the certificate) from the account's **Credentials**
   tab.
   - [ ] CloudTAK's *next* certificate renewal attempt (7 days before expiry, or a manual
         reconfiguration) fails, since the password it has cached is no longer valid; the current live
         connection is unaffected until then, since the certificate — not the password — is what the
         stream and Marti listeners check.

## Known gaps

- **Video.** rustak stubs `GET /Marti/api/video` as an empty list and rejects writes, matching every
  other server CloudTAK is realistically run against (`.claude/plan/compat/cloudtak.md` §8 — CloudTAK's
  own video integration is documented broken against every real TAK Server deployment today). Do not
  expect CloudTAK's video features to do anything against rustak.
- **Federation, QUIC, ExCheck.** Out of scope, same as every other client.
- **MinIO / asset storage, the pmtiles tiler, the events and retention workers, the media server.**
  The automated suite runs CloudTAK with only three of its seven services because nothing this
  project asserts touches the rest (`interop/cloudtak/README.md` → "What is and is not in the
  stack"). If your manual CloudTAK instance has those services enabled, features that depend on them
  (basemaps, video, background ETL) are outside anything rustak's compatibility work covers either
  way.

## Verified in

- `.claude/plan/compat/cloudtak.md`, `groups.md`, `missions.md`, `enrollment.md`, `oauth.md` — the
  wire contracts each section's expected outcome is drawn from.
- `research/03-cloudtak-node-tak-contract.md` — authoritative for CloudTAK's own client behaviour.
- `.claude/plan/status/M4-03-interop-cloudtak-compose.md` — what the automated suite already checks,
  its known first-run risk points, and the package/service decisions cited above.
- Items marked **to confirm** have no verified source and must be settled by running this checklist.
