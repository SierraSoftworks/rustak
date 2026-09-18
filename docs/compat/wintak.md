# WinTAK — manual compatibility checklist

Everything on this page is a **manual gate**, and more of it is unverified than
[`atak.md`](atak.md): rustak's research corpus (`.claude/plan/research/`) was built by reading
ATAK-CIV, TAK Server, CloudTAK and OpenTAKServer source directly — WinTAK is closed-source and none
of it was read. What follows assumes WinTAK, like ATAK-CIV, implements the **published** TAK
Protocol v1 (the wire-level `protocol.txt` both clients ship, not either client's private
implementation) and TAK Server's documented Marti HTTP surfaces. Anything specific to WinTAK's own
behaviour — trust-prompt wording, which preferences exist, whether its Data Sync UI matches ATAK's —
is marked **to confirm** below rather than asserted.

Run this after [`atak.md`](atak.md) passes for a release, on the same rustak. Where a step is
identical to ATAK's, it says so instead of repeating the wire detail; only run the ATAK checklist's
"how to do it" steps with WinTAK's own UI. Record the result (WinTAK version, date, outcome per
section) in the release notes.

## 0. What is already covered automatically

The same automated coverage as [`atak.md`](atak.md) §0 applies here for anything that is protocol,
not client: TAK Protocol v1 negotiation and framing, group-reachability routing, mission CRUD and
notifications, Enterprise Sync, and device-profile bytes are all asserted against the *specification*
and against ATAK's own networking core (`interop/eud/`), not against WinTAK. **No automated suite in
this project drives WinTAK** — there is no WinTAK equivalent of `commoncommo` this project can build
or run. Everything in this document is therefore a first-party check, not a confirmation of something
CI already covers a different way.

## 1. QR enrolment

**To confirm**: whether WinTAK supports scanning or typing a `tak://com.atakmap.app/enroll?...` URL
at all (it is a desktop client; a camera-based QR scan may not exist in its UI, or it may accept the
same URL pasted or typed into a "Quick Connect"-equivalent dialog). If WinTAK has an equivalent
enrolment path:

1. Mint a one-time enrolment token the same way as [`atak.md`](atak.md) §1 step 1.
2. Enrol WinTAK against rustak using whatever equivalent flow it offers (typed host/username/token is
   the most likely path if there is no QR scanner).
3. [ ] **To confirm**: does WinTAK verify rustak's public listener certificate the same way ATAK's
   quick-connect does (public CAs + hostname check, no "trust anyway" prompt)? If rustak's `:8446` is
   serving the `internal` CA rather than ACME, this is the most likely first failure point either way.
4. [ ] The certificate is issued and the device appears in **Devices** (`/admin/devices`) and
   **Activity** (`/admin/activity`, categories *Enrollment* then *Pki*) exactly as in `atak.md` §1
   steps 4–7 — this part is server-side and does not depend on WinTAK's own behaviour.
5. [ ] **To confirm**: does WinTAK read and honour `deviceProfileEnableOnConnect` the way ATAK-CIV
   does, or does it always fetch connect-time profiles? If profiles never seem to arrive on WinTAK
   even though the enrolment profile set the preference, this is the first thing to check.

## 2. Manual config package import

The `wintak_atak` variant of `POST /api/v1/config-packages` exists specifically because WinTAK is
expected to import the same package format ATAK does — this is the one area of this document backed
by a design decision, not a guess. Run [`profiles.md`](profiles.md) §4 step 7 ("Repeat on WinTAK with
the same file") here:

1. [ ] WinTAK shows one import prompt and, after accepting, a new server entry appears in its network
   settings.
2. [ ] The entry's connect string is `<host>:<stream port>:ssl` and is enabled.
3. [ ] **To confirm**: does WinTAK re-home `truststore.p12` to a fixed location the way ATAK's
   certificate sorter does, or does it read the `.pref`'s `caLocation` value directly from wherever
   the package extracted it? If import succeeds but the connection never trusts the server, this is
   the first thing to check.
4. [ ] Connecting prompts for a username and client password (the enrolment variant sets
   `enrollForCertificateWithTrust0=true`/`useAuth0=true`); after entering them, the device enrols and
   connects.

## 3. Channels UI

**To confirm** throughout: whether WinTAK has a Channels overlay at all, and if so whether it is
gated by the same `prefs_enable_channels`/`prefs_enable_channels_host-<host>` preference convention
ATAK-CIV uses (`groups.md` §4) or by something WinTAK-specific.

1. Run [`atak.md`](atak.md) §3 steps 1, 3 and 4 (create two channels with different directions, then
   toggle and remove membership from the admin side) against a WinTAK client instead of ATAK.
2. [ ] Both channels are visible in whatever channel-selection UI WinTAK offers.
3. [ ] Toggling a channel off from WinTAK produces the same `PUT /Marti/api/groups/active` call
   observable in **Activity** — the wire contract does not distinguish clients, so this should behave
   identically even if the UI around it does not.
4. [ ] An admin-side membership removal while WinTAK is connected still delivers `t-x-g-c` and the
   client still re-fetches groups — **to confirm** whether WinTAK reacts to the notification the same
   way ATAK does (clearing map items, refreshing the list) or ignores it until a manual refresh.

## 4. Chat

**To confirm**: whether WinTAK's chat UI exists and uses the same `b-t-f` GeoChat convention ATAK
does, or a different one.

1. Run [`atak.md`](atak.md) §4 between a WinTAK client and an ATAK device sharing a channel.
2. [ ] Group and direct messages cross between the two clients in both directions.
3. [ ] An undeliverable direct message from WinTAK comes back marked failed (the `b-t-f-s` bounce is
   server-side and does not depend on which client sent the original message) — **to confirm**
   whether WinTAK's UI actually surfaces that bounce to the operator.

## 5. Package send / receive

1. Run [`atak.md`](atak.md) §5 between a WinTAK client and an ATAK device.
2. [ ] The transfer completes through the server (not peer-to-peer) exactly as in the ATAK checklist —
   this is server-side behaviour and does not depend on WinTAK.
3. [ ] **To confirm**: whether WinTAK's package browser reads `/Marti/sync/search` the same way, and
   whether it shows the same fields (name, size, submission time).

## 6. Data Sync (missions)

1. Run [`atak.md`](atak.md) §6 with WinTAK subscribed to the mission alongside an ATAK device.
2. [ ] Content (marker, file, log entry) added from either client appears live on the other — again,
   this is server-push behaviour and should not depend on which client is which.
3. [ ] **To confirm**: WinTAK's own Data Sync UI — whether it exists, and whether it re-subscribes
   automatically on reconnect.

## 7. Device profile application

Run [`profiles.md`](profiles.md) §§1–3 with WinTAK. **To confirm** for every check: whether WinTAK
applies delivered `.pref` settings the same way ATAK-CIV does (same preference group name,
`class="class java.lang.…"` requirement, etc.) — nothing about WinTAK's `.pref` importer has been
read from source.

## 8. Disconnect / reconnect

Run [`atak.md`](atak.md) §8 with a WinTAK client. The server-side behaviour (25-second timeout,
`t-x-d-d`, latest-SA replay on reconnect) is identical regardless of which client is on the other end;
only the client-visible symptoms (does WinTAK show a disconnected indicator, does it reconnect
automatically) are WinTAK-specific and unverified.

## 9. Revocation

Run [`atak.md`](atak.md) §9 with a WinTAK client. The TLS-handshake-level refusal is server-side and
identical for every client; **to confirm** is only how WinTAK's UI reports the failed reconnection
attempt to the operator.

## Known gaps

Same as [`atak.md`](atak.md) — video, ExCheck, federation, QUIC and plaintext/anonymous streaming are
out of scope for every client, not just ATAK. Additionally for WinTAK specifically:

- **No automated coverage exists or is planned.** There is no WinTAK equivalent of `commoncommo` this
  project can build from source, so this checklist is the only gate WinTAK compatibility has, and it
  depends entirely on someone running it with a real WinTAK licence and install.
- **iTAK-specific quirks are explicitly out of scope for this document** — see [`itak.md`](itak.md).

## Verified in

- Every step marked with a plain checkbox and no "to confirm" restates a server-side behaviour that
  is verified for ATAK-CIV in [`atak.md`](atak.md) and does not depend on the client, per
  `.claude/plan/compat/*.md`.
- Every step marked **to confirm** has no verified source (WinTAK is closed-source and outside this
  project's research corpus) and must be settled by running this checklist; record the outcome here
  once observed, the same way [`profiles.md`](profiles.md) records what M3-02 settled by hand.
- `plan.md` "Decisions" table — WinTAK is in scope for the config-package variant by design; nothing
  else about it was researched from source.
