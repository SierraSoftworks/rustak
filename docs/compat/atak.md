# ATAK-CIV — manual compatibility checklist

Everything on this page is a **manual gate**. rustak's automated suites assert the wire protocol
against ATAK's own networking core (`commoncommo`, nightly, in `interop/eud/`) and against fake
EUDs on every PR, but nothing in CI runs the actual ATAK app — its UI, its import prompts, its
Channels overlay, its Data Sync plugin — so whether a real device *behaves* as expected is checked
by a person before a release that touches enrolment, streaming, channels, chat, packages, missions
or profiles.

Run through this once per release, against a fresh rustak and a fresh (or factory-reset) ATAK-CIV
install, and record the result (ATAK version, date, outcome per section) in the release notes. Every
section is written to be independently skippable if a release only touched one area; the whole page
runs in under an hour on a phone that already has ATAK installed.

## 0. What is already covered automatically

Do not re-check these by hand; they fail the build (or the nightly job) if they break.

| Covered | Where |
|---|---|
| TAK Protocol v1 negotiation, XML/protobuf framing, ping/pong, group-reachability routing, flow tags | `rustak-server/tests/stream_routing.rs`, `stream_session.rs`, `stream_channel_state.rs` |
| GeoChat delivery, plus the `b-t-f-s` bounce for an undeliverable direct message | `stream_routing.rs` (landed in M1-08) |
| Enrolment: CSR/CN validation, `signClient/v2` status-code mapping, certificate issuance parameters | `rustak-server/tests/enroll_flows.rs`, `enroll_oauth.rs` |
| Channels: `Group` JSON shape, `bitpos` assignment, `PUT …/active`, `t-x-g-c` emission rule | `rustak-server/tests/marti_channels.rs` |
| Mission CRUD, subscribe, contents, changes (squashed/full), tokens, `<dest mission>` routing | `rustak-server/tests/missions_flow.rs`, `missions_extras.rs`, `mission_dest.rs`, `mission_squash.rs` |
| Enterprise Sync upload/search/content, `b-f-t-r`/`b-f-t-a` construction, manifest round-trip | `rustak-server/tests/sync_contract.rs` |
| Device-profile bytes, zip layout, `304`/`If-Modified-Since` semantics | `rustak-server/tests/profiles_contract.rs` — this is also the subject of [`profiles.md`](profiles.md), a manual gate in its own right; §2 and §7 below point at it rather than repeating it |
| Certificate revocation closing a live session and refusing a future handshake | covered in the certificate-lifecycle integration tests added in M2-08 |
| The wire protocol against ATAK's **own** networking core, nightly: enrolment, TLS, negotiation, ping/pong, SA and chat routing, mission-package transfer | `interop/eud/` (see [`docs/interop.md`](../interop.md) → "The EUD harness") — this proves the bytes on the wire are right; it does not touch the ATAK **app**, which has no UI in that harness |

## 1. QR enrolment

1. In the admin UI, open **Account** (`/admin/users/{username}`) → **Credentials** — or, signed in as
   the enrolling user, **Credentials** (`/admin/credentials`) — and mint a one-time enrolment token.
   The QR code and the `tak://com.atakmap.app/enroll?...` link are shown once; note them before
   navigating away.
2. On the device: ATAK → the server/network icon → **+** → **Quick Connect**, and scan the QR (or
   type host/username/token by hand).
3. [ ] No `SERVER_NOT_TRUSTED` dialog appears. Quick Connect verifies rustak's public listener
   (`:8446`) against **public CAs with hostname verification on** — if the listener is serving the
   `internal` CA rather than ACME or a real certificate, this is the expected failure, and there is no
   "trust anyway" prompt to click through. Switch the public listener to ACME (or import the CA into
   the device out of band) before re-running this section.
4. [ ] Enrolment completes, and **Settings → Network Preferences → Network Connections** shows a new
   entry with connect string `<host>:8089:ssl`, enabled.
5. [ ] The enrolment device profile is fetched immediately, before the stream reconnects. Check
   **Activity** (`/admin/activity`, category *Enrollment*, then *Profile*) for the token being spent
   and the profile being delivered.
6. [ ] **Devices** (`/admin/devices`) shows the new device: callsign, uid, a certificate in state
   *Active*.
7. [ ] **Activity**, category *Pki*, shows the certificate issued; category *Enrollment* shows the
   token consumed.
8. [ ] On the device, **Settings → Show all preferences → `deviceProfileEnableOnConnect`** is **on**.
   If it is not, every check in §3 and §6 below that depends on a live connect-time profile push will
   silently do nothing — this is the single most important item ATAK's enrolment profile must set
   (see [`profiles.md`](profiles.md) §1).

## 2. Manual config package import

This is the same manual gate as [`profiles.md`](profiles.md) §4 (`POST /api/v1/config-packages`,
`variant: "wintak_atak"`, for a device that cannot scan a QR) — run that section here rather than
duplicating it, and record its pass/fail against this release the same as every other section on
this page.

## 3. Channels UI

Prereq: an enrolled device from §1 with `prefs_enable_channels` turned on.

1. In **Channels** (`/admin/groups`), create two channels: e.g. `Blue` (grant the enrolled account
   both `IN` and `OUT`) and `Red` (grant `OUT` only).
2. On the device, open the Channels overlay (map overlays → Channels).
   - [ ] Both channels are listed. Neither silently disappears — a channel with no `bitpos` would
         (`groups.md` §1), but that is already covered by `marti_channels.rs`; you should never
         observe it here.
3. Toggle `Red` off on the device.
   - [ ] **Activity** (category *Stream*, or check the server log) shows `PUT /Marti/api/groups/active`
         with a `clientUid`. If you have a second device enrolled on the same account, it should
         receive a `t-x-g-c` notice and clear its `Red`-channel map items — the toggling device itself
         does **not** get notified of its own change.
4. From **Channels**, remove the device's membership of `Red` entirely while it is connected (an
   admin-side change, not something the device asked for).
   - [ ] The device receives `t-x-g-c` unprompted, clears every map item that arrived on `Red`, and
         re-fetches `GET /Marti/api/groups/all?useCache=true&sendLatestSA=true` — you should see its
         `Red` items vanish without touching the device.

## 4. Chat

1. Enrol two devices/accounts sharing at least one channel in both directions (§3).
2. Send a group message ("All Chat Rooms") from device A.
   - [ ] Device B receives it in the same room.
3. Send a direct message from A to B, addressed to B's callsign.
   - [ ] B receives it as a DM, not in the group room.
4. Send a direct message from A to a callsign that cannot currently be reached (temporarily drop A's
   or the target's shared `IN`/`OUT` channel, or address an offline callsign).
   - [ ] A gets the message back marked failed/undelivered rather than silence — the server bounces an
         undeliverable direct GeoChat as `b-t-f-s` (landed in M1-08). If it disappears instead, that is
         a regression, not an expected gap.
5. [ ] **Activity** (category *Stream*) shows the routed messages, useful for checking a delivery that
   looked wrong.

## 5. Package send / receive (peer file-share, not Data Sync)

1. From device A, use "Send" on a map item or file, addressed to device B, with both connected to
   rustak (`b-f-t-r`).
   - [ ] The transfer completes. The upload lands on rustak first (`POST /Marti/sync/missionupload`)
         and B's incoming offer points at rustak's `/Marti/sync/content`, not at A directly — to prove
         this rather than assume it, disconnect A's network mid-transfer; B should still finish, since
         it is pulling from the server, not from A.
   - [ ] **Data packages** (`/admin/packages`) lists the uploaded package; **Activity** (category
         *Package*) shows the upload.
2. From device B, search for the package (`GET /Marti/sync/search?keywords=missionpackage`) rather
   than opening the pushed link.
   - [ ] It appears with a valid submission time, and downloading it via the search result succeeds —
         this is a different client code path from the peer-pushed download in step 1.
3. [ ] Removing the package from **Data packages** makes it disappear from a fresh search.

## 6. Data Sync (missions)

1. Create a mission from **Missions** (`/admin/missions`), or from the device's Data Sync UI if the
   account's channel allows creating missions.
   - [ ] **Activity** (category *Mission*) shows the creation; the mission appears in the device's
         Data Sync list.
2. Subscribe the device to it from ATAK's Data Sync UI.
   - [ ] The subscribe succeeds and the mission's existing content appears on the map.
3. Add a marker while subscribed (send it to the mission, not to broadcast).
   - [ ] It appears for every other subscriber. **Mission** (`/admin/missions/{guid}`) shows an
         `ADD_CONTENT` change, and a second connected subscriber should see the marker arrive live
         (a pushed `t-x-m-c`), without touching Data Sync's manual refresh.
4. Attach a file to the mission from the Data Sync UI.
   - [ ] It appears in the mission's file list for every subscriber, and as mission content in
         **Mission**.
5. Add a log entry from the device.
   - [ ] It appears in the mission's log for every subscriber.
6. Disconnect and reconnect the device's stream (§8) while subscribed.
   - [ ] **To confirm**: whether ATAK's Data Sync plugin re-subscribes automatically on reconnect, or
         needs a manual re-open of the mission. (CloudTAK is documented to re-subscribe automatically;
         the ATAK app's own behaviour here has not been verified from source.)
7. Delete the mission from **Missions**.
   - [ ] The device sees it disappear from the Data Sync list (`t-x-m-d`).

## 7. Device profile application

Run [`profiles.md`](profiles.md) §§1–3 in full here (enrolment profile, connection profile with
apply-on-connect, tool profile + `relativePath`) — this checklist only marks the milestone at which
that page's gate must be re-run, not a second copy of it.

## 8. Disconnect / reconnect

1. With a device connected and at least one other device sharing a channel, cut the first device's
   network (airplane mode) without a clean shutdown.
   - [ ] Within about 25 seconds the server notices; the other device receives `t-x-d-d` and removes
         the departed device's marker.
2. Restore connectivity.
   - [ ] The device reconnects using its existing certificate (no re-enrolment), and every currently
         reachable peer's latest SA is replayed to it immediately.
   - [ ] **Clients** (`/admin/clients`) shows the drop and the reconnect; **Activity** (category
         *Stream*) shows both.
3. Reconnect a device whose channel membership changed while it was offline (e.g. §3 step 4).
   - [ ] The change is already in effect — groups arrive via the normal on-connect
         `GET /groups/all` call, since there was no live connection for a `t-x-g-c` to reach.

## 9. Revocation

1. From **Devices** or **Account** → the device's certificate panel, revoke the connected device's
   certificate with a reason.
   - [ ] The live session drops immediately — check **Clients**, it should disappear.
   - [ ] **Activity** (category *Pki*) shows the revoke and the reason.
2. Try to reconnect the same device with its now-revoked certificate (do not re-enrol).
   - [ ] The TLS handshake itself is refused; the device never gets far enough to send any CoT. This
         is enforced at the handshake, not after a connection is accepted.
3. Re-enrol the same account with a fresh token.
   - [ ] A new certificate is issued and the device works again; the old certificate stays revoked.

## Known gaps

Confirm these *do not* work, so nobody designs a feature around them:

- **Video, ExCheck.** Not implemented. ATAK's video and ExCheck screens will show nothing, or fail to
  connect, if pointed at rustak.
- **Federation, QUIC.** rustak has no server federation and no QUIC transport. A connect string
  ending `:quic` will fail — ATAK's own QR parsing forces `ssl` for anything but a literal `quic`
  suffix, so this should not come up by accident (`enrollment.md` §5).
- **Plain TCP / anonymous stream.** rustak has no plaintext `:8087`/`stcp` listener and no `<auth>`
  credential handshake — a connect string using `tcp` instead of `ssl` simply never connects. This is
  intentional (`plan.md` "Secure by default").
- **`useStreamingGroup`, directory-backed profiles.** See [`profiles.md`](profiles.md) §7 — the same
  known gaps apply here, since profiles is the same subsystem.

## Verified in

- `research/07-atak-client-verified.md` — authoritative for every ATAK-side behaviour cited above.
- `.claude/plan/compat/{streaming,enrollment,groups,missions,files,profiles}.md` — the wire contracts
  each section's expected outcome is drawn from.
- `.claude/plan/status/M1-08-chat-bounce.md`, `M2-09-eud-interop-scenarios.md` — what the automated
  suites already prove, and what they explicitly cannot (the app UI).
- Items marked **to confirm** have no verified source and must be settled by running this checklist;
  update this page with the outcome once observed.
