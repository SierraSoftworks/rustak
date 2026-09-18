# Streaming (CoT over :8089) — wire contract

**Purpose.** What must be true of the `stream/` TLS listener (`:8089`) for ATAK-CIV and CloudTAK to
stay connected, negotiate correctly, see each other's traffic, and disconnect cleanly. This is the
contract for `rustak-cot::codec`, `rustak-cot::xml`, `rustak-cot::proto`, and `rustak-server::stream`.

Baseline: `plan.md` Appendix A.1. This file expands it; do not contradict it.

## 1. Transport

| | |
|---|---|
| Port | `:8089`, TLS 1.2/1.3, **client cert always required** (rustak has no plaintext or anonymous stream input — see `conventions.md`) |
| ALPN | none |
| Cipher list | none enforced by ATAK on the stream socket (the `DEFAULT:!ECDH` restriction only applies to ATAK's mission-package HTTPS client against `:8443`, not `:8089` — verified 07 §3.2, corroborated by `plan.md` TLS decision) |
| Principal | client cert CN → user (see `enrollment.md`); CloudTAK and ATAK both authenticate the stream by client cert only once enrolled |

rustak does **not** implement the TAK Server `<auth><cot username password uid/></auth>` credential
handshake (that is TAK Server's `auth="file"`/`"ldap"` input mode). Every `:8089` connection
presents a client cert obtained via enrollment (`enrollment.md`); there is no anonymous or
password-only stream path. Background on the message TAK Server accepts, in case a client sends one
unprompted: it must be the **first bytes**, root element `<auth>` with a single child `<cot
username= password= uid= [callsign=]/>`; no reply is ever sent, success or failure; a server that
doesn't understand it should simply not react to it (verified 05 §2.3, §1.6). ATAK never sends this
document over a cert-authenticated connection (07 §3.3) — CloudTAK never sends it at all (03 §4.1).

## 2. XML framing

- **Inbound**: scan for the literal token `</event>`; on each occurrence, find the last `<event`
  before it and parse that substring. **Bytes before the first `<event` are silently discarded** —
  this is intentional tolerance, not a bug to fix. No other delimiter (not the XML declaration, not
  whitespace) may be assumed. A single message larger than **8 MiB** is dropped (but still consumed).
  Verified 05 §4; corroborated independently by CloudTAK's own reader, which uses the same
  find-`</event>` regex and explicitly requires a matching **open and close tag** — a self-closing
  `<event .../>` will **never** match and must not be emitted (03 §4.2, §8.11.4).
- **Outbound**: `<?xml version="1.0" encoding="UTF-8"?>` + `\n` + `<event …>…</event>`, **no
  trailing newline**, messages laid head-to-tail with no separator between them. Never self-close
  `<event/>` (`conventions.md`).
- Control characters `U+000B`–`U+001F` and `U+007F`–`U+009F` should not appear in emitted XML;
  CloudTAK strips them before parsing, so their presence is harmless there but still invalid CoT.

## 3. Protobuf framing (TAK Protocol v1)

```
frame := 0xBF <LEB128 varint payload-length> <payload: serialised TakMessage>
```

- `0xBF` is the magic byte. The mesh/UDP frame shape (`0xBF <varint version> 0xBF <payload>`) is
  **different** and never appears on a stream socket — don't share decoder state between the two.
- Max frame ATAK will read is **64 KiB**; a bad magic byte causes ATAK to resync by skipping bytes
  looking for the next `0xBF` (07 §3.5). rustak should do the same on inbound: skip-and-resync, never
  hang or close on a single bad byte.
- **Outbound size cap**: if an encoded `TakMessage` would exceed **64 KiB**, substitute a `b-f-t-r`
  pointer to `GET /Marti/api/cot/xml/{uid}` instead of sending the oversized frame (05 §3.6; see
  `files.md` for the `b-f-t-r` shape). Inbound has no size cap beyond the 64 KiB frame ceiling.
- Field numbers (clean-room `.proto`, no text or comments copied from any GPL source — numbers and
  types are facts, verified against both `atak-civ`'s `commoncommo` protos and TAK Server's server
  protos, which are identical on the shared subset — 07 §12.1):

  `TakMessage`: `1 TakControl takControl`, `2 CotEvent cotEvent` (rustak does not need
  `submissionTime`/`creationTime` — those are TAK-Server-internal fields ATAK ignores; omit them).

  `CotEvent`: `1 string type`, `2 string access`, `3 string qos`, `4 string opex`, `5 string uid`,
  `6 uint64 sendTime` (ms), `7 uint64 startTime` (ms), `8 uint64 staleTime` (ms), `9 string how`,
  `10 double lat`, `11 double lon`, `12 double hae` (999999 = unknown), `13 double ce` (999999 =
  unknown), `14 double le` (999999 = unknown), `15 Detail detail`, `16 string caveat`,
  `17 string releasableTo` (note: proto field is spelled `releaseableTo` in atak-civ's own `.proto` —
  keep the XML attribute `releasableTo` correct and pick either proto spelling consistently).

  `Detail`: `1 string xmlDetail`, `2 Contact contact`, `3 Group group`,
  `4 PrecisionLocation precisionLocation`, `5 Status status`, `6 Takv takv`, `7 Track track`,
  `8 repeated ExtensionEncodedDetail extensionDetails` (rustak: encode/decode but treat as opaque —
  no registered extension IDs are in scope for M1).

  Leaves: `Contact{1 endpoint, 2 callsign}`, `Group{1 name, 2 role}`,
  `PrecisionLocation{1 geopointsrc, 2 altsrc}`, `Status{1 uint32 battery}`,
  `Takv{1 device, 2 platform, 3 os, 4 version}`, `Track{1 double speed, 2 double course}`.

### XML ↔ protobuf conversion rules

A child of `<detail>` is promoted to its typed submessage **only if its attribute set matches
exactly** (extra or missing attributes ⇒ stays in `xmlDetail`):

| Element | Required attrs | Optional | Attr-count gate |
|---|---|---|---|
| `<contact>` | `callsign` | `endpoint` | 1 (no endpoint) or 2 (with endpoint), no others |
| `<__group>` | `name`, `role` | — | exactly 2, no others |
| `<precisionlocation>` | `geopointsrc`, `altsrc` | — | exactly 2 |
| `<status>` | `battery` | — | exactly 1 |
| `<takv>` | `device`, `platform`, `os`, `version` | — | exactly 4 |
| `<track>` | `speed`, `course` | — | exactly 2 |

Whatever doesn't match is concatenated as raw child XML (no `<detail>` wrapper, no XML header) into
`xmlDetail`. Decoding back to XML: emit the typed elements for whichever submessages are set, then
if `xmlDetail` is non-empty, parse it and merge its children in — **on a name collision, the
`xmlDetail` element wins** and the typed element is dropped (verified 05 §12.2, corroborated 07
§4.4). `<detail>` is always present on the wire, even if empty. Sub-second time precision is lost on
any proto round-trip (times are millisecond `uint64`, but TAK Server's own second-resolution
`DateUtil.toCotTime` is *not* something rustak needs to replicate — emit full millisecond CoT time
strings; see `conventions.md` date-format rule).

## 4. Connect sequence

1. TLS handshake completes; client cert → principal (`enrollment.md`).
2. Server replays reachable peers' **latest SA** as plain XML `<event>` messages (see §6 for the
   reachability rule). This happens **before** any negotiation offer.
3. Server sends **exactly one** `t-x-takp-v` negotiation offer (own words, verified shape — 05
   §3.2, structurally confirmed against 07's independent client-side re-derivation):
   ```xml
   <event version="2.0" uid="{fresh-uuid}" type="t-x-takp-v" time="{t}" start="{t}" stale="{t+60s}" how="m-g">
     <point lat="0.0" lon="0.0" hae="0.0" ce="999999" le="999999"/>
     <detail><TakControl>
       <TakProtocolSupport version="1"/>
       <TakServerVersionInfo serverVersion="{rustak-<semver>}" apiVersion="3"/>
     </TakControl></detail>
   </event>
   ```
   `stale` is `time + 60s`. **Do not send this to a CloudTAK connection's negotiation path** — see §5.
4. If the client wants protobuf, it sends `t-x-takp-q` **once**, reusing the offer's `uid`, then
   stops sending anything until it gets a response (both ATAK and the `pyTakStreamingProto`
   reference client behave this way — 05 §3.3, 07 §11.2):
   ```xml
   <event version="2.0" uid="{same uid as offer}" type="t-x-takp-q" time="{t}" start="{t}" stale="{t+60s}" how="m-g">
     <point lat="0.0" lon="0.0" hae="0.0" ce="999999" le="999999"/>
     <detail><TakControl><TakRequest version="1"/></TakControl></detail>
   </event>
   ```
   Only the XPath `detail/TakControl/TakRequest/@version == "1"` matters; anything else in the
   message is ignored. A missing/unparseable version node means: do not switch, and do not answer —
   the client will time out at 60s and stay in XML forever (05 §3.3, 07 §3.4). That silent-hang
   behaviour is the **correct fallback**, not a bug to fix.
5. Server answers with `t-x-takp-r`, same `uid`:
   ```xml
   <event version="2.0" uid="{same uid}" type="t-x-takp-r" time="{t}" start="{t}" stale="{t+60s}" how="m-g">
     <point lat="0.0" lon="0.0" hae="0.0" ce="999999" le="999999"/>
     <detail><TakControl><TakResponse status="true"/></TakControl></detail>
   </event>
   ```
   `status` is the literal string `"true"` or `"false"`. On `"true"` both directions switch to
   protobuf framing **immediately** — the server must never emit XML again on that connection. On
   `"false"` (or if the server chooses not to support protobuf for this client) both sides stay in
   XML and the client may retry.
6. No client request within the offer's 60s stale window ⇒ stay in XML forever; this is normal for
   CloudTAK (which never sends `t-x-takp-q`) and for any client that doesn't implement TAK Protocol
   v1.

## 5. CloudTAK-specific streaming behaviour

CloudTAK (`node-tak`) never negotiates protobuf at all: `sendClient`. Its reader only understands
full `<event>…</event>` XML pairs (found via a `</event>`-anchored regex that also requires the
opening `<event`), never sends `t-x-takp-q`, and its TCP write path joins events with `\n`. Sending
it a `t-x-takp-v` offer is harmless (it stores `serverVersion` from
`detail/TakControl/TakServerVersionInfo/@serverVersion` and otherwise ignores it — 03 §4.3), but
**never** attempt to switch a CloudTAK connection to protobuf; it has no negotiation reply path and
raw `0xBF`-prefixed bytes would corrupt its control-character stripping. Verified in 03 §4.2.

## 6. Keepalive

- The client pings after **15s** of silence and repeats every **4.5s**; if it hears nothing for
  **25s** it disconnects and reconnects (07 §3.6; CloudTAK's ping is simpler — a flat 5s interval,
  03 §4.3). Ping shape (type `t-x-c-t`, no detail):
  ```xml
  <event version="2.0" uid="{device-uid}-ping" type="t-x-c-t" how="m-g" time="{t}" start="{t}" stale="{t+10s}">
    <point lat="0.0" lon="0.0" hae="0.0" ce="999999" le="999999"/>
  </event>
  ```
- Server reply, sent **directly to the pinging connection only** (not broadcast, no flow tag, no
  group check), own-words rendering of the verified template (05 §5.4):
  ```xml
  <event version="2.0" uid="takPong" type="t-x-c-t-r" how="h-g-i-g-o" time="{t}" start="{t}" stale="{t+20s}">
    <point lat="0" lon="0" hae="0" ce="9999999" le="9999999"/>
  </event>
  ```
  **No `<detail>` element at all.** `uid` is the literal string `takPong`, not derived from the
  ping's uid. CloudTAK detects a pong purely by `type == "t-x-c-t-r"` — it does not check `uid` (03
  §4.3), so this shape is safe for both clients.
- A control message (see §7) with no `<point lat=…>` attribute must be **silently dropped**, not
  errored (05 §5.2).

## 7. Control messages

These types are **consumed at ingest and never relayed** to other clients (05 §5.1–§5.3):

| Type | Action |
|---|---|
| `t-x-c-t` | reply with pong (§6); do not relay |
| `t-x-c-t-r` | no-op (some clients echo a stray one back; ignore) |
| `t-x-takp-q` | handled by the negotiation state machine (§4); never relayed |
| `t-x-c-i-e` / `t-x-c-i-d` | set/clear incognito on the sender's subscription — see below |
| `t-x-c-m` | metrics report (`app_framerate`, `battery`, …); safe to parse best-effort or ignore |
| `t-x-c-f` | client-side geospatial filter update; safe to ignore for M1 |
| anything else unrecognised as a normal CoT type | treat as a no-op, never error the connection |

**Incognito**: once a client sends `t-x-c-i-e`, its subsequent non-control messages are dropped at
ingest *unless* they carry at least one `<marti><dest callsign="…"/></marti>` — i.e. an incognito
client only reaches explicitly-addressed recipients. `t-x-c-i-d` clears it. Incognito subscriptions
are also skipped by latest-SA replay (§4 step 2). Verified 05 §5.5.

`t-x-d-d` and `t-x-g-c` are **not** control types in this sense — the server *generates* them (§9)
and a client-sent one is brokered like ordinary CoT.

## 8. Routing and reachability

`<marti><dest …/>` children select recipients; each `<dest>` is matched against **one** attribute,
checked in this order, first match wins — the raw structure is verified 05 §6.1:

1. `callsign` — explicit list by callsign. If any entry in the list is the literal string
   `"All Streaming"`, **discard the whole callsign list** and fall back to implicit (group) broadcast
   — this is how ATAK's "post to all" option degrades.
2. `publish` — not meaningful to implement; a `<dest publish=…>` message should simply match nobody
   (this mirrors TAK Server's own unimplemented state, not a gap rustak needs to fill).
3. `uid` — explicit list by CoT/subscription uid.
4. `mission` (+ optional `path`, `after`) — route into a Data Sync; see `missions.md` §"CoT dest
   routing". `after` is only meaningful when `path` is also present.
5. `mission-guid` (+ optional `path`, `after`) — same, addressed by GUID.
6. `group` — the message's group set is **replaced** by the named group; reject with an error if the
   sender does not hold that group with `IN` direction.

After processing, **`<marti>` is always stripped before relay** — receivers, including the sender's
own reachable peers, never see it (05 §6.2).

**Reachability rule** (the one fact every other routing behaviour depends on — verified 05 §6.3,
independently corroborated in 06 §5.7 with matching semantics):

> Delivery from sender `S` to receiver `R` is allowed **iff** there exists a group `G` such that `S`
> holds `G` with direction `IN` **and** `R` holds `G` with direction `OUT`.

`IN` = "may publish into this group"; `OUT` = "may receive from this group". This is **not**
symmetric and it is **not** set intersection — compute it exactly as stated, per (sender, candidate)
pair. Implicit (group) broadcast additionally **excludes the sending connection itself**; explicit
addressing (uid/callsign) does not self-exclude and still applies the reachability check.

Every relayed message gets a flow tag added as an XML attribute under a `_flow-tags_` element,
keyed by rustak's own server id:
```xml
<detail>…<_flow-tags_ rustak-{server-id}="{iso8601-ms-time}"/></detail>
```
If the inbound message already carries `_flow-tags_` with **this server's** key, drop it (loop
suppression) rather than re-tagging. Verified 05 §5.8.

## 9. Disconnect and group-change notifications

On disconnect, send `t-x-d-d` to every peer reachable *from* the disconnecting user (own-words
rendering, verified 05 §6.6):
```xml
<event version="2.0" uid="{fresh-uuid}" type="t-x-d-d" how="h-g-i-g-o" time="{t}" start="{t}" stale="{t+20s}">
  <point lat="0" lon="0" hae="0" ce="9999999" le="9999999"/>
  <detail><link relation="p-p" uid="{clientUid}" type="{last-SA-event-type}"/></detail>
</event>
```
Only sent when the disconnecting subscription had both a callsign and a clientUid set (i.e. it had
sent at least one SA message).

On a group-membership change (`groups.md`), send `t-x-g-c` to the user's **other** devices (never
back to the device whose change triggered it):
```xml
<event version="2.0" uid="{fresh-uuid}[.{clientUid}]" type="t-x-g-c" how="h-g-i-g-o" time="{t}" start="{t}" stale="{t+20s}">
  <point lat="0" lon="0" hae="0" ce="9999999" le="9999999"/>
  <detail><link relation="p-p"/></detail>
</event>
```
`.{clientUid}` is appended to `uid` only when the change identifies the originating device (see
`groups.md` for when that happens). Both ATAK and CloudTAK react by clearing that server's map
items and re-fetching `/Marti/api/groups/all?sendLatestSA=true` (07 §5.3; 03 §4.3). Verified 05
§5.5/§7.4.

## 10. Subscription state from SA messages

The **first** inbound message carrying `<contact endpoint>` fixes the subscription's `clientUid` (=
event `uid`) and `callsign`. Every subsequent SA message (non-empty callsign + `contact/@endpoint` +
uid) refreshes: `team` ← `__group/@name`, `role` ← `__group/@role`, `takv` ←
`"{platform}:{version}"`. Any parse failure leaves these as `"unknown"`, not an error. The
streaming-endpoint sentinel used in `<contact endpoint>` for relayed traffic is the literal string
`*:-1:stcp`. Verified 05 §5.7.

## Gotchas

- Never send TAK Protocol v1 negotiation bytes (offer or protobuf frames) to a connection you know
  is CloudTAK — it cannot parse them (§5).
- Never self-close `<event/>` — CloudTAK's reader requires a real open/close pair (§2).
- A control message with no `<point lat>` must be dropped silently, not rejected with an error (§6).
- The pong's `uid` is the constant `takPong`, not `"{ping-uid}-pong"` or similar (§6).
- `<marti>` is stripped from **every** relayed message, even ones the reachability check would have
  rejected outright to a `403`-style error on the HTTP surface — on the stream there is no error
  reply, the message is just not delivered.
- Reachability is `sender.IN ∩ receiver.OUT` per matching group name, not a raw intersection of
  group sets — re-derive per pair, don't cache a flattened "who can see whom" table naively without
  invalidating it on every group-membership change.
- Oversized (>64 KiB) protobuf frames become a `b-f-t-r` pointer, not a truncated or dropped message.

## Verified in

- `research/05-takserver-streaming-auth-verified.md` §§1–8 (framing, auth, negotiation, control
  messages, routing, reachability, disconnect, group-change) — authoritative.
- `research/07-atak-client-verified.md` §§3–4 (client-side negotiation, ping/pong, protobuf spec,
  framing/resync) — authoritative for ATAK's own behaviour.
- `research/03-cloudtak-node-tak-contract.md` §4 (CloudTAK never negotiates; ping/pong; `<marti>`
  usage) — authoritative for CloudTAK.
- `plan.md` Appendix A.1 — baseline digest, expanded here.
