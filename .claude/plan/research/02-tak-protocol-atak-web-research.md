I have comprehensive primary-source coverage. Here is the report.

---

# ATAK-CIV ↔ TAK Server wire compatibility specification

**Primary sources used** (both repos read directly via the GitHub API at `main`, Feb 2026 push):
- `TAK-Product-Center/atak-civ` — https://github.com/TAK-Product-Center/atak-civ
- `TAK-Product-Center/Server` — https://github.com/TAK-Product-Center/Server
- Mirror (same content, older): `deptofdefense/AndroidTacticalAssaultKit-CIV`

**Read this first — three corrections to your premises:**

1. **Stream framing has no trailing magic byte.** It is `0xBF <varint length> <payload>`. The `0xBF <varint> 0xBF` form is *mesh/datagram* framing only. Verified in three independent places (spec text, C++ encoder, Python reference client) — details in §A.2.
2. **ATAK does not use the JSON form of `signClient/v2`.** Its native enrollment client sends `Accept: application/xml` and parses `<enrollment><signedCert>…</signedCert><ca>…</ca></enrollment>`. The `ca0`/`ca1` JSON keys exist but ATAK never requests them. Details in §B.1.
3. **The Data Sync / Mission *client* is not in the public atak-civ repo.** Section C is therefore reconstructed from the server side only, and I flag exactly what that means for confidence.

Also flagged up front because it may change your design: **both repos are GPL-3.0**, including the `.proto` files. See §Licensing.

---

## A. Streaming protocol

### A.1 Ports and transports

From `SslNetCotPort.java` ([source](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/comms/SslNetCotPort.java)) — ATAK's own definitions of the three HTTP port roles:

| Port | Constant | Auth model |
|---|---|---|
| 8080 | `SERVER_API_PORT_UNSECURE` | plain HTTP Marti |
| 8443 | `SERVER_API_PORT_SECURE` | HTTPS **with client certificate** |
| 8446 | `SERVER_API_PORT_CERT_ENROLLMENT` | HTTPS with **HTTP Basic auth** (no client cert) |

All three are settable at runtime (`setSecureServerApiPort` etc.), so do not hardcode them server-side as the only valid values — but these are the defaults ATAK assumes.

CoT streaming ports from `CoreConfig.example.xml` ([source](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/example/CoreConfig.example.xml)):

```xml
<input _name="stdssl"  protocol="tls"  port="8089" coreVersion="2"/>
<input _name="quic"    protocol="quic" port="8090"/>
<!-- <input _name="stdtcp"    protocol="tcp"  port="8087" auth="anonymous"/> -->
<!-- <input _name="streamtcp" protocol="stcp" port="8088" auth="anonymous"/> -->
<connector port="8443" _name="https"/>
<connector port="8446" clientAuth="false" _name="cert_https"/>
```

Note the shipped default only enables **8089 (TLS)**; plaintext 8087/8088 are commented out. `stcp` (8088) and `tcp` (8087) are distinct protocol names in TAK Server's config.

### A.2 Framing

**Protocol Version 0 (legacy XML)** — authoritative text from `commoncommo/core/impl/protobuf/protocol.txt` ([source](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/protocol.txt), duplicated at [`takproto/README.txt`](https://github.com/TAK-Product-Center/atak-civ/blob/main/takproto/README.txt)):

> "The TCP stream is comprised of one CoT `<event>` after another. Messages are delimited and broken apart by searching for the token `</event>` and breaking apart immediately after that token. When sending, messages must be prefaced by XML header (`<?xml … ?>`), followed by a newline, followed by the complete XML `<event>`. TAK servers require that no arbitrary newlines follow the `</event>` end of message and that the next character immediate commences the next `<?xml … ?>` header."

Implementation rules for your server:
- **Receive:** scan for the literal byte sequence `</event>` (8 bytes). commoncommo does exactly this — `MESSAGE_END_TOKEN[] = "</event>"` in [`streamingsocketmanagement.cpp`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/streamingsocketmanagement.cpp). There is **no** length prefix and **no** newline delimiter. You must be robust to `</event>` appearing inside CDATA or attribute values in principle — TAK itself is not, so neither clients nor server emit such content.
- **Send:** `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n<event …>…</event>` with **no trailing newline**. Emit the next message's XML declaration as the immediately following byte.

**Protocol Version 1 — streaming framing.** From `protocol.txt`:

```
TAK Protocol Streaming Header: <magic byte> <message length>
  <magic byte>     = 0xBF
  <message length> = protobuf unsigned varint, byte count of the payload that follows
```

The version identifier is **omitted** from the streaming header (it is fixed by negotiation). Corroborated by the encoder in [`takmessage.cpp`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/takmessage.cpp):

```cpp
case HEADER_LENGTH:
    headerBuf[0] = TAKPROTO_MAGIC;                              // 0xbf
    size_t n = InternalUtils::varintEncode(headerBuf + 1, …);
    headerlen = n + 1;                                          // <-- 1 + varint, no trailer
```

versus the mesh header in the same switch:

```cpp
case HEADER_TAKPROTO:
    headerBuf[0]     = TAKPROTO_MAGIC;
    size_t n         = InternalUtils::varintEncode(headerBuf + 1, …);  // version
    headerBuf[1 + n] = TAKPROTO_MAGIC;
    headerlen = n + 2;                                          // 0xbf 0x01 0xbf for v1
```

And the decoder state machine in `streamingsocketmanagement.cpp` is `PROTO_HDR_MAGIC → PROTO_HDR_LEN → PROTO_DATA`, with no fourth state for a trailing byte. TAK Server's own Python load-test client agrees — [`create_proto.py`](https://github.com/TAK-Product-Center/Server/blob/main/src/testing/load_test/create_proto.py):

```python
MAGIC_BYTE = b'\xbf'
def serialize(self):
    return MAGIC_BYTE + get_size_bytes(msg) + msg
```

**Resync behaviour you should match:** if the byte where a magic is expected is not `0xBF`, commoncommo logs and *skips forward byte-by-byte until it finds one* rather than dropping the connection. It also rejects a decoded length greater than its RX buffer and rescans. A tolerant server should do likewise rather than erroring out.

**Varint:** standard protobuf unsigned LEB128, capped at 10 bytes / 2⁶³−1 (`protocol.txt`, "TAK Protocol Varint Encoding").

### A.3 Protocol negotiation (server-driven)

Three CoT types, named in TAK Server's [`StreamingProtoBufOrCoTProtocol.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/src/main/java/com/bbn/marti/nio/protocol/connections/StreamingProtoBufOrCoTProtocol.java):

```java
TAK_ANNOUNCE_TYPE  = "t-x-takp-v";   // server → client
TAK_REQUEST_TYPE   = "t-x-takp-q";   // client → server
TAK_RESPONSE_TYPE  = "t-x-takp-r";   // server → client
```

Your premise omitted these `type=` values — they matter, because ATAK dispatches on them.

**Step 1 — server announces (MUST be sent at most once per connection).** TAK Server's actual emitted string:

```xml
<?xml version='1.0' encoding='UTF-8' standalone='yes'?>
<event version='2.0' uid='<negotiationUuid>' type='t-x-takp-v'
       time='…' start='…' stale='…' how='m-g'>
  <point lat='0.0' lon='0.0' hae='0.0' ce='999999' le='999999'/>
  <detail><TakControl><TakProtocolSupport version='1'/></TakControl></detail>
</event>
```

`<TakProtocolSupport>` may optionally carry `<DetailExt id="…"/>` children, or a single `<DetailExt supportsAll="true"/>` meaning "I relay any extension opaquely". A store-and-forward server like yours should consider advertising `supportsAll="true"`. Multiple `<TakProtocolSupport>` elements (one per version) are permitted inside the single `<TakControl>`.

**Step 2 — client requests.** ATAK replies **reusing the server's `uid`** (`CoTMessage` constructor in [`cotmessage.cpp`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/cotmessage.cpp) takes `msg->getEventUid()`):

```xml
<event … uid='<same negotiationUuid>' type='t-x-takp-q' …>
  <detail><TakControl><TakRequest version="1"><DetailExt id="…"/></TakRequest></TakControl></detail>
</event>
```

Exactly one `<TakRequest>`. `id='*'` is **not** valid on a client `<DetailExt>`.

**Step 3 — server responds:**

```xml
<event … uid='<same negotiationUuid>' type='t-x-takp-r' …>
  <detail><TakControl><TakResponse status='true'/></TakControl></detail>
</event>
```

**Switchover rules (these are the ones that break implementations):**
- On `status='true'` the **server MUST NOT send any further CoT XML**. The very next byte it writes must be `0xBF`. commoncommo sets `protoState = PROTO_HDR_MAGIC` the instant it parses the response.
- The client also switches immediately and in both directions — one negotiated version for the whole connection.
- Between sending `t-x-takp-q` and receiving the response, **the client stops sending** but keeps reading. The server may keep sending XML right up until it processes the request.
- On `status='false'`, both sides continue in XML and the client may retry.
- Timeouts: commoncommo uses `PROTO_TIMEOUT_SECONDS = 60.0f`. The spec requires the server to watch for `t-x-takp-q` for at least 60 s after announcing, extended another 60 s after any `false` response.
- If the client times out with no response it **disconnects** — an indeterminate negotiation is fatal.

**Ordering vs. auth:** per `protocol.txt` step 1, if the server requires authentication the auth XML message MUST be the client's first message; the server MAY send CoT XML (including, presumably, the announcement) while awaiting it.

### A.4 Ping / pong keepalive

Constants from [`cotmessage.cpp`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/cotmessage.cpp):

```cpp
const char TYPE_PONG[] = "t-x-c-t-r";
const char TYPE_PING[] = "t-x-c-t";
const char HOW_PING[]  = "m-g";
const float STALE_TIME_PING = 10.0f;
```

Client-side timing from `streamingsocketmanagement.cpp`:

```cpp
const float RX_STALE_SECONDS      = 15.0f;  // no RX for this long → send ping
const float RX_STALE_PING_SECONDS =  4.5f;  // repeat ping this often while still silent
const float RX_TIMEOUT_SECONDS    = 25.0f;  // no RX for this long → tear down & reconnect
const char *PING_UID_SUFFIX = "-ping";      // ping uid = <ourUid>-ping
```

**Implementation requirement: your server must send *something* at least every 25 seconds, and must answer `t-x-c-t` promptly.** Any inbound byte that yields a message resets `lastRxTime` — it does not have to be a pong. But ATAK sends pings at 15 s and drops at 25 s, leaving only a 10 s window.

TAK Server's reply, verbatim from `sendPong()` in [`SubmissionService.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/src/main/java/com/bbn/marti/service/SubmissionService.java):

```xml
<event version='2.0' uid='takPong' type='t-x-c-t-r' how='h-g-i-g-o'
       time='<now>' start='<now>' stale='<now+20s>'>
  <point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/>
</event>
```

The uid is the literal string `takPong` — not an echo of the ping uid.

Both sides discard ping/pong before app dispatch: commoncommo drops any message where `isPong()`, and ATAK's [`CotMapComponent.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/cot/CotMapComponent.java) treats both types as an explicit no-op ("essentially a no-op as the AbstractStreaming instance will have updated the last time for that connection").

**Server-side control types** TAK Server intercepts rather than routing (`SubmissionService.controlMsgTypes`): `t-b`, `t-b-a`, `t-b-c`, `t-b-q`, `t-x-c-f`, `t-x-c-t`, `t-x-c-t-r`, `t-x-takp-q`, `t-x-c-m`, `t-x-c-i-e`, `t-x-c-i-d`. The last two are **incognito enable/disable** — a client that goes incognito is skipped when building latest-SA replays. `t-x-c-m` is a metrics message. These are worth implementing as at-least-no-ops.

### A.5 What ATAK sends on connect (SA / PLI)

Real captures from [`commoncommo/core/atakcotcaptures.txt`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/atakcotcaptures.txt) — this file is the closest thing to a ground-truth corpus in the repo:

```xml
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<event version="2.0" uid="ANDROID-48:5A:3F:49:93:24" type="a-f-G-U-C"
       time="2016-01-13T20:43:37.842Z" start="2016-01-13T20:43:37.842Z"
       stale="2016-01-13T20:44:04.842Z" how="h-e">
  <point lat="37.954121501941216" lon="-87.97179289978664"
         hae="80.85387849113384" ce="9999999" le="9999999"/>
  <detail>
    <contact endpoint="192.168.167.167:4242:tcp" callsign="GEMINI"/>
    <uid Droid="GEMINI"/>
    <__group role="Team Member" name="Cyan"/>
    <status battery="0"/>
    <track course="114.66817017909176" speed="0.0"/>
    <precisionlocation geopointsrc="User" altsrc="DTED0"/>
  </detail>
</event>
```

Notes that matter for a server implementation:

- **`uid`** is the device UID (`ANDROID-<MAC>` historically; modern ATAK uses a generated UID). Per the header note in the capture file, from Shawn (TPC): *"UIDs in messages are for the message. They don't identify the event originator. Only consider unique id for a contact if the contact info is present."* Treat `<contact callsign=>` presence as the signal that this event identifies a contact.
- **`endpoint`** over a streaming connection is the literal `*:-1:stcp` — `CoTMessage::STREAMING_ENDPOINT("*:-1:stcp")` in `cotmessage.cpp`. Over mesh it is `ip:port:proto`. Other sentinels in the same file: `tcpsrcreply`, `udpsrcreply`, `quicsrcreply`.
- `type` is `a-f-G-U-C` (ground combat unit) or `a-f-G-U-C-I` depending on role.
- `ce`/`le`/`hae` unknown sentinel is `9999999` in SA messages but `999999` in the negotiation templates — both appear; don't validate strictly.
- `<status battery=>` is an integer percentage; `0` is legitimately sent.
- `_flow-tags_` is added **by the server** (`<_flow-tags_ takServer1="2016-01-14T19:16:48.750Z"/>` appears in the through-server captures). TAK Server has a `FlowTagFilter` and uses this for federation loop prevention. Your server should add its own flow tag and drop messages already carrying it.

### A.6 Protobuf mapping

`.proto` files live at **`commoncommo/core/impl/protobuf/`** in atak-civ, with a byte-identical copy at **`takproto/`**:

| File | Message | Fields |
|---|---|---|
| [`takmessage.proto`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/takmessage.proto) | `TakMessage` | `takControl=1`, `cotEvent=2` (both optional) |
| [`cotevent.proto`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/cotevent.proto) | `CotEvent` | see below |
| [`detail.proto`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/detail.proto) | `Detail` | `xmlDetail=1`, `contact=2`, `group=3`, `precisionLocation=4`, `status=5`, `takv=6`, `track=7`, `extensionDetails=8` |
| [`contact.proto`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/contact.proto) | `Contact` | `endpoint=1` (opt), `callsign=2`, `altendpoints=3` (opt) |
| [`group.proto`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/group.proto) | `Group` | `name=1`, `role=2` |
| [`precisionlocation.proto`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/precisionlocation.proto) | `PrecisionLocation` | `geopointsrc=1`, `altsrc=2` |
| [`status.proto`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/status.proto) | `Status` | `battery=1` (uint32) |
| [`takv.proto`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/takv.proto) | `Takv` | `device=1`, `platform=2`, `os=3`, `version=4` |
| [`track.proto`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/track.proto) | `Track` | `speed=1`, `course=2` (double) |
| [`takcontrol.proto`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/protobuf/takcontrol.proto) | `TakControl` | `minProtoVersion=1`, `maxProtoVersion=2`, `contactUid=3`, `extensionIds=4` |

`CotEvent` field numbers — note the **non-contiguous numbering**, `caveat` and `releasableTo` were appended later at 16/17:

```
type=1  access=2  qos=3  opex=4  uid=5
sendTime=6  startTime=7  staleTime=8   (uint64 ms since Unix epoch)
how=9
lat=10 lon=11 hae=12 ce=13 le=14       (double; 999999 = unknown)
detail=15
caveat=16  releasableTo=17
```

**Typed vs. `xmlDetail` — the exact rules** (from the comment block in `detail.proto`, which is normative):

Six children are lifted into typed fields: `<contact>`, `<__group>`, `<precisionlocation>`, `<status>`, `<takv>`, `<track>`. Everything else stays in `xmlDetail`.

Sender rules:
1. Remove the child elements used to populate typed fields. **If the same child element appears more times than the field can hold, or any mapping error occurs, do not remove it and do not populate the typed field** — fall back to XML.
2. If nothing remains under `<detail>`, leave `xmlDetail` empty.
3. Serialise the remaining tree as UTF-8, **strip the `<detail>`/`</detail>` tags and the XML header**, put the fragment in `xmlDetail`.

Receiver rules: wrap `xmlDetail` in `<detail>…</detail>`, prepend an XML header, parse, then merge in XML equivalents of the typed messages. **Conflict rule: if a sender misbehaved and data for the same element is in both places, the `xmlDetail` copy wins and the typed message is ignored.**

"Required unless otherwise noted" in the per-message comments means: if a required attribute is missing, the conversion **must be rejected** and the whole element left as opaque XML. E.g. a `<contact>` with no `callsign` must not become a `Contact` message.

**Whole-element rule (called out in capitals in the source):** "WHOLE ELEMENTS MUST BE CONVERTED TO MESSAGES. Do not try to put part of the data from a given element into one of the messages and put other parts of the data in an element of xmlDetail!" This bites in practice — a real `<contact>` often carries `phone=` (seen in the captures), which has no protobuf field. Per this rule, such a `<contact>` must go to `xmlDetail` **whole**, not be split.

**Detail extensions** (`Detail.extensionDetails`, `ExtensionEncodedDetail{extensionId, data}`): IDs are centrally registered with TPC. "Implementers MUST NOT use IDs in public deployments which have not been registered." For your server the safe posture is: advertise `<DetailExt supportsAll="true"/>`, store and forward the `extensionDetails` bytes opaquely, never synthesise your own.

TAK Server has its own copies at [`src/takserver-protobuf/src/main/proto/`](https://github.com/TAK-Product-Center/Server/tree/main/src/takserver-protobuf/src/main/proto) with a **different package name** and extra files (`binarypayload.proto`, `fig.proto`, `message.proto`, `missionannouncement.proto`, `streaminginput.proto`) used for federation/clustering, not the EUD wire. Use the atak-civ set for client compatibility.

### A.7 Routing semantics

The authoritative class is [`StreamingEndpointRewriteFilter.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/src/main/java/com/bbn/cot/filter/StreamingEndpointRewriteFilter.java).

**`<marti><dest …/></marti>` — eight recognised attributes**, more than you listed:

```java
UID_ATTR="uid"  CALLSIGN_ATTR="callsign"  PUBLISH_ATTR="publish"
MISSION_ATTR="mission"  MISSION_ATTR_GUID="mission-guid"
PATH_ATTR="path"  AFTER_ATTR="after"  GROUP_ATTR="group"

DEST_XPATH = "/event/detail/marti/dest[@callsign or @publish or @uid or
              @mission or @path or @after or @mission-guid or @group]"
```

Behaviours to implement:
- Each matching `<dest>` is **detached** from the document, and finally **the entire `<marti>` element is removed** before the message is relayed. Recipients never see the addressing block.
- `@callsign` → add to explicit callsign list. **Special case: the literal callsign `"All Streaming"` suppresses explicit addressing** and the message broadcasts (`if (callsignList.size() > 0 && !callsignList.contains("All Streaming"))`).
- `@uid` → explicit UID list.
- `@mission` / `@mission-guid` → mission-scoped publish; this is how ATAK writes CoT into a Data Sync mission over the stream.
- `@group` → **security-checked**: the server hydrates the named group with `Direction.IN` and, if the sender is not already a member, throws `ForbiddenException("illegal attempt to set group … for uid …")`. A client cannot escalate into a channel it isn't in. Implement this check.
- `@path` / `@after` are used for ordering content within mission layers.

**`__serverdestination`** is a *server-added* detail on the receive side, seen in the GeoChat capture:

```xml
<__serverdestination destinations='192.168.79.115:4242:tcp:ANDROID-88:32:9B:40:EC:D0'/>
```

Format is `ip:port:proto:uid`. It tells the receiving client how to reach the sender directly. Note it appears in a *multicast* capture in that file; over a streaming connection the endpoint is `*:-1:stcp`, so its usefulness is limited. I did **not** find the server-side code that writes it (my code search for it was rate-limited before completing) — treat this as lower confidence and optional.

**`__group`** — TAK Server reads `//detail/__group/@name` and `@role` in [`MessageConversionUtil.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/src/main/java/com/bbn/marti/util/MessageConversionUtil.java) purely to populate the contact/SA record (team colour + role). **It is not the channel-membership mechanism** — that is the group bit-vector derived from authentication (§E). This is a common misreading; `__group name="Cyan" role="Team Member"` is the *team colour*, not a TAK Server channel.

**GeoChat `b-t-f`** — real capture:

```xml
<event version='2.0'
  uid='GeoChat.ANDROID-88:32:9B:40:EC:D0.All Chat Rooms.b4936fac-b0ca-4fd3-892d-35596f0dc749'
  type='b-t-f' time='…' start='…' stale='…' how='h-g-i-g-o'>
  <point lat='0.0' lon='0.0' hae='9999999.0' ce='9999999' le='9999999'/>
  <detail>
    <__chat id='All Chat Rooms' chatroom='All Chat Rooms'>
      <chatgrp id='All Chat Rooms' uid0='ANDROID-88:32:9B:40:EC:D0' uid1='All Chat Rooms'/>
    </__chat>
    <link relation='p-p' type='a-f-G-U-C' uid='ANDROID-88:32:9B:40:EC:D0'/>
    <remarks source='BAO.F.ATAK.ANDROID-88:32:9B:40:EC:D0' time='…'>Roger</remarks>
    <__serverdestination destinations='192.168.79.115:4242:tcp:ANDROID-88:32:9B:40:EC:D0'/>
    <precisionlocation geopointsrc='???' altsrc='???'/>
  </detail>
</event>
```

Structure: the event `uid` is `GeoChat.<senderUid>.<room>.<messageUuid>`. For the "All Chat Rooms" broadcast, `__chat/@id` and `@chatroom` are both the literal `All Chat Rooms`, and `chatgrp/@uid1` is also that literal. For a direct message, `uid1` is the recipient's UID and `chatroom`/`id` become the recipient callsign. `remarks/@source` uses the `BAO.F.ATAK.<uid>` convention. Note this capture lacks `groupOwner=`, `senderCallsign=` and `parent=` — those attributes do exist in current ATAK but are **not** present in this 2015-era capture, so I could not verify their exact semantics from primary source. Treat those three as unverified.

Server handling: GeoChat is routed like any other CoT — by `<marti><dest>` if present, otherwise broadcast within the sender's group intersection. TAK Server does have a `ChatMessage` model (`com.bbn.marti.remote.socket.ChatMessage`) but the routing path is the generic one.

**Deletes `t-x-d-d`** — TAK Server's template from [`DistributedSubscriptionManager.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/src/main/java/com/bbn/marti/service/DistributedSubscriptionManager.java):

```xml
<event how='h-g-i-g-o' type='t-x-d-d' version='2.0' uid='<generated>'
       time='<now>' start='<now>' stale='<now+20s>'>
  <point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/>
  <detail><link relation='p-p' uid='<uidToDelete>' type='<typeOfDeleted>'/></detail>
</event>
```

The deleted object's UID is in `detail/link/@uid`, **not** the event uid. `SubmissionService` also inspects `t-x-d-d` to call `federationManager.removeLocalContact(link.uid)`. Your server should relay these and use them to expire contacts.

There is a parallel **`t-x-g-c`** (group change) message with the same shape, sent to tell a client its channel assignment changed — see §E.

### A.8 Echo, and what to send on connect

**Echo:** TAK Server does not echo a message back to the originating subscription. `SubmissionService.processControlMessage`'s `default:` branch is `subMgr.deleteSubscription(c.getUid())` — i.e. unknown control types drop the subscription, which is worth knowing as a hazard. For data messages the broker excludes the source handler. **Do not echo the sender's own SA back to it** — ATAK will render a ghost of itself.

**On connect, TAK Server replays the latest SA of every reachable peer.** `MessagingUtilImpl.sendLatestReachableSA(User destUser)` ([source](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/src/main/java/com/bbn/marti/groups/MessagingUtilImpl.java)):

1. Compute reachable users via `CommonGroupDirectedReachability` (group intersection, respecting IN/OUT direction).
2. For each reachable subscription: **skip if `sub.incognito`**, otherwise take `sub.getLatestSA()` and send it to the new client.
3. Also `sendLatestFeedEventsToSub(destSubscription)` for data feeds.

So your server must **cache the most recent SA/PLI per connected client** and replay that set to each newly-authenticated client. This is the "CoT repeater" behaviour you asked about — it is a *latest-value cache per subscription*, not a history replay. The same routine is re-run on group changes (`sendUpdatedGroupsLatestSA`) and there is an explicit `GET /Marti/api/groups/all?sendLatestSA=true` hook to trigger it (§E).

On disconnect, `messagingUtil().sendDisconnect(sub.getLatestSA(), sub)` emits a `t-x-d-d` for the departing client.

### A.9 TLS specifics

**Client cert is required on 8089.** TAK Server's TLS inputs run `X509AuthCodec`/`SslCodec` (`SubmissionService` imports both).

**How ATAK verifies the server.** commoncommo does its own verification, not OpenSSL's, because it holds certs in memory:

```cpp
sslCtx = SSL_CTX_new(SSLv23_client_method());
if (sslCtx)
    // "Be certain openssl doesn't do anything with its internal verification
    //  as we will do our own (internal verify cannot be made to work with in-memory certs)"
    SSL_CTX_set_verify(sslCtx, SSL_VERIFY_NONE, NULL);
```

and per-connection it builds an `X509_STORE` from the enrollment truststore CAs. `SSLv23_client_method()` = negotiate the highest mutually-supported TLS version, so **TLS 1.2 and 1.3 both work** provided the linked OpenSSL supports them. There is no explicit minimum-version floor in commoncommo.

**Cipher restriction — plan for this.** In `configSSLForConnection`:

```cpp
// Disable ECDH ciphers as demo.atakserver.com fails when they are
// enabled.  See:  https://bugs.launchpad.net/ubuntu/+source/openssl/+bug/1475228
// for similar issues with other servers.
SSL_CTX_set_cipher_list(sslCtx, "DEFAULT:!ECDH");
```

In OpenSSL cipher-string grammar, the `ECDH` alias covers **fixed *and* ephemeral** ECDH suites, so `!ECDH` excludes ECDHE too. Practical consequence: under **TLS 1.2** this ATAK code path offers only non-ECDHE key exchange (RSA / DHE). Under **TLS 1.3** `SSL_CTX_set_cipher_list` does not govern the TLS 1.3 ciphersuite list, so TLS 1.3 is unaffected. **Recommendation: either support TLS 1.3, or keep a TLS 1.2 DHE (or plain RSA) suite enabled**, otherwise this code path cannot complete a handshake. I have flagged this as an inference from OpenSSL's documented cipher-alias semantics rather than something the TAK source states outright — worth an empirical test against a real device early in your build.

Note this applies to the **commoncommo/native** path. ATAK's Java HTTP client (`CertificateManager.getSockFactory`) is a separate stack with its own defaults and is not subject to this cipher list.

**QUIC** (port 8090) uses ALPN `takstream` — `ALPN_STREAMING[] = {0x09,'t','a','k','s','t','r','e','a','m'}` in `streamingsocketmanagement.cpp`, and the client **fails the handshake** if the server does not negotiate it. I traced the two usage sites (lines ~2383, ~2570) and both are inside `StreamingQuicConnection`, so **ALPN is QUIC-only — do not require or offer `takstream` on the TLS 8089 listener.** QUIC is out of scope for a first implementation.

**Server-side TLS config**, from [`CoreConfig.xsd`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-common/src/main/xsd/CoreConfig.xsd):

```xml
<xs:element name="tls">
  <xs:attribute name="keystore"   use="required"/>  <!-- e.g. JKS -->
  <xs:attribute name="keystoreFile" use="required"/>
  <xs:attribute name="keystorePass" use="required"/>
  <xs:attribute name="truststore" use="required"/>
  <xs:attribute name="truststoreFile" use="required"/>
  <xs:attribute name="truststorePass" use="required"/>
  <xs:attribute name="context" default="TLSv1.2"/>
  <xs:attribute name="ciphers"/>
</xs:element>
```

Default `context` is `TLSv1.2`. There is a separate `webCiphers` attribute for the HTTPS connectors.

**Cert → user → groups mapping:** §E.

---

## B. Certificate enrollment

**Where the code is:** the Java class `CertificateEnrollmentClient` ([source](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/net/CertificateEnrollmentClient.java)) is **only UI and key/cert storage**. All HTTP is in native commoncommo: [`enrollmentmanager.cpp`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/enrollmentmanager.cpp). I confirmed this by searching the whole repo for `signClient` and `Marti/api/tls` — the only hit is `enrollmentmanager.cpp`. Supporting classes: `CertificateConfigRequest.java`, `certconfig/CertificateConfig.java`, `certconfig/NameEntry.java`, `enrollmentmanager.h`, `cryptoutil.cpp`.

### B.1 The exact HTTP sequence

Base URL construction (`EnrollmentRequest::createURLRequest`):

```cpp
ss << "https://" << host << ":" << port << "/Marti/api/tls/";
```

The three enrollment steps are `ENROLL_STEP_KEYGEN` (local, no HTTP), `ENROLL_STEP_CSR`, `ENROLL_STEP_SIGN`.

**Step 0 — local keygen.** `cryptoutil.cpp` ([source](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/cryptoutil.cpp)): `EVP_PKEY_CTX_new_from_name(NULL, "RSA", NULL)`, `bits = 2048`, exponent `RSA_F4` (65537). **RSA-2048 only — no EC.** TAK Server agrees: `Constants.KEY_TYPE="RSA"`, `CERTBITS=2048`.

**Step 1 — `GET /Marti/api/tls/config`**, Basic auth, `Accept: application/xml`.

Response parsed by `parseCsrDoc`. Root element **must** be `certificateConfig`; it must contain a `nameEntries` child; each `nameEntry` contributes `@name`/`@value`. TAK Server's `getConfig()` marshals it with `QName("com.bbn.marti.config", "certificateConfig")` — hence the `ns2:` prefix you see on the wire. Minimal valid response:

```xml
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<ns2:certificateConfig xmlns:ns2="com.bbn.marti.config">
  <nameEntries>
    <nameEntry name="O" value="TAK"/>
    <nameEntry name="OU" value="TAK"/>
  </nameEntries>
</ns2:certificateConfig>
```

**Subject composition** — this is exact, from `parseCsrDoc`:

```cpp
// Before adding CSR config info, add our CN
entries.push_back(std::pair<std::string,std::string>("CN", user));
// then every <nameEntry name= value=> in document order
```

So the subject is **`CN=<username>` first, then the server's nameEntries in document order**. The `name` values are resolved via OpenSSL `OBJ_txt2nid`; an unknown name aborts CSR generation. Use standard short names (`O`, `OU`, `C`, `ST`, `L`).

CSR is signed with **SHA-256** (`X509_REQ_sign(req, pkey, EVP_sha256())`).

**Step 2 — `POST /Marti/api/tls/signClient/v2?clientUid=<uid>&version=<clientVersion>`**

URL built as:

```cpp
ss << "signClient" << "/v2" << "?clientUid=" << ourUid;
if (!clientVersionInfo.empty()) ss << "&version=" << clientVersionInfo;
```

Headers set in `EnrollmentURLRequest::curlExtraConfig`:

```cpp
CURLOPT_USERNAME / CURLOPT_PASSWORD     // HTTP Basic
"Accept: application/xml"
"Content-Type: application/octet-stream"
```

(A bearer-token header path also exists in the same function, for token-based enrollment.)

**Body:** the PEM CSR **with the BEGIN/END banners stripped**:

```cpp
std::string pemHeader("-----BEGIN CERTIFICATE REQUEST-----\n");
std::string pemFooter("-----END CERTIFICATE REQUEST-----\n");
// both replaced with "" before upload
```

So the body is base64 with embedded newlines, no armour.

**`version` parameter is semantically significant on the server.** `CertManagerApi.signClientCertV2` passes `version != null` to `certManagerService.signClient(clientUid, version != null, base64CSR)`, with the comment *"TAK 4.4 clients that support Channels will pass in the version parameter"*. Presence of the parameter — not its value — flags a Channels-capable client.

**Response — content-negotiated.** Verbatim from [`CertManagerApi.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/tak/tls/CertManagerApi.java):

- `Accept` absent, `*/*`, empty, or `application/json` → JSON:
  ```json
  { "signedCert": "<base64, no armour>", "ca0": "<base64>", "ca1": "<base64>" }
  ```
- `Accept: application/xml` → XML (**this is what ATAK gets**):
  ```xml
  <?xml version="1.0" encoding="UTF-8"?>
  <enrollment>
    <signedCert>MIIC…</signedCert>
    <ca>MIID…</ca>
    <ca>MIID…</ca>
  </enrollment>
  ```
- Anything else → `400 Bad Request`.

**Note the asymmetry: in XML the CA elements are all named `<ca>`, not `ca0`/`ca1`.** ATAK's parser (`parseSigningXML`) iterates *all* children of `<enrollment>`, treats the one named `signedCert` as the client cert and **every other child, whatever its name, as a CA**. So `<ca0>`/`<ca1>` would also work — but emit `<ca>` to match the reference server.

All PEM bodies are `Util.certToPEM(cert, false)` — the `false` means **no header/footer**. ATAK compensates, and the comment documents the quirk precisely:

```cpp
// TAK Server does not include PEM header/footer but openssl requires it.
// Also TAK server PEM string may or may not end with a newline, and
// OpenSSL requires precisely: \n{FOOTER HERE}\n
```

Your server should emit bare base64. ATAK tolerates a trailing newline or not.

**Step 3 — ATAK builds two PKCS#12 stores locally.** `parseSigningXML` calls `crypto->generateKeystore(...)` twice:
- **Client keystore**: private key + signed cert + full CA stack, friendly name `"TAK Client Cert"`, encrypted with a **generated** `clientCertPassword`.
- **CA truststore**: CA stack only, no key, encrypted with a generated `enrolledTrustPassword`. CA entries get aliases `enrollCaResult1`, `enrollCaResult2`, …

PKCS#12 params (`cryptoutil.cpp`): `NID_pbe_WithSHA1And3_Key_TripleDES_CBC` for keys, `NID_pbe_WithSHA1And40BitRC2_CBC` for certs — legacy algorithms, which is why commoncommo explicitly loads OpenSSL's legacy provider. **The server never produces these p12s** — it returns PEM and the client assembles them.

**Does the server ever return a p12?** Yes — the **legacy v1** endpoint does:

```java
@RequestMapping(value = "/tls/signClient", method = RequestMethod.POST)
ResponseEntity<byte[]> signClientCert(clientUid, version, @RequestBody String base64CSR) {
    KeyStore keyStore = KeyStore.getInstance("pkcs12");
    keyStore.setCertificateEntry("signedCert", cert.getX509Certificate());
    int ndx = 0;
    for (X509Certificate ca : cert.getX509CertificateChain())
        keyStore.setCertificateEntry("ca" + ndx++, ca);
    keyStore.store(bos, DEFAULT_PASSWORD.toCharArray());   // "atakatak"
    // Content-Type: application/octet-stream
}
```

**`DEFAULT_PASSWORD = "atakatak"`.** Aliases `signedCert`, `ca0`, `ca1`, … — so the `ca0`/`ca1` naming originates here, in the v1 p12, and carried over into the v2 JSON. Current ATAK uses v2 exclusively; implement v1 only for older clients or third-party tools.

**Also present** (used by the web UI / admin tooling, **not** by ATAK):
- `GET /Marti/api/tls/makeClientKeyStore?cn=&clientUid=&password=` (default password `atakatak`)
- `GET /Marti/api/tls/makeClient?cn=`
- `POST /Marti/api/tls/signServer?base64CSR=&issuerDN=`

### B.2 Issued-certificate requirements

What I could verify from primary source: RSA-2048, SHA-256 CSR, subject `CN=<username>` + server nameEntries. TAK Server persists the issued cert via `takCertRepository.save(cert)` — this matters because revocation and cert→user mapping key off the stored record.

What I **could not** verify from the source I read: whether ATAK enforces `extendedKeyUsage=clientAuth`, a SAN, or any particular validity window. commoncommo's `parseSigningXML` only calls `stringToCert()` and stuffs the result into a PKCS#12 — **it performs no policy validation on the issued cert**. The server side is the constraining party at connection time. Practical guidance: issue with `EKU=clientAuth`, `keyUsage=digitalSignature,keyEncipherment`, a `subjectKeyIdentifier`/`authorityKeyIdentifier` pair, and no SAN requirement (TAK identifies by CN). **I am stating this as a recommendation, not a verified ATAK requirement.**

### B.3 QR-code enrollment

Verified verbatim in [`CotMapComponent.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/cot/CotMapComponent.java):

```java
/**
 * Responsible for processing URI "com.atakmap.app/enroll such as
 * tak://com.atakmap.app/enroll?host=takserver.com&username=foo&token=user_token
 */
if (u != null && "com.atakmap.app/enroll".equals(u.getHost() + u.getPath())) {
    final String host     = u.getQueryParameter("host");
    final String username = u.getQueryParameter("username");
    final String token    = u.getQueryParameter("token");
    if (host == null || username == null || token == null) return;
    // → confirmation dialog → CertificateEnrollmentClient.onEnrollmentOk(
    //      ctx, host, "", host, username, token, -1L)
}
```

- **Exactly three parameters: `host`, `username`, `token`. All three are mandatory** — a missing one is a silent no-op.
- **No port parameter.** Ports come from `SslNetCotPort` defaults (8446 for enrollment). You cannot move the enrollment port via QR.
- The `token` is passed where a password would go — it is the enrollment credential presented via HTTP Basic.
- ATAK **always shows a confirmation dialog** naming the username and host before enrolling. Scanning alone never enrolls silently.
- Registered in [`AndroidManifest.xml`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/AndroidManifest.xml) as `<data android:scheme="tak" />`.

**Import QR exists too**, in [`ImportExportMapComponent.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/importexport/ImportExportMapComponent.java):

```
tak://com.atakmap.app/import?url=http%3A%2F%2Fwebaddress.com%2Ffile%2Ffile.zip
tak://com.atakmap.app/import?url=https%3A%2F%2Fdownload.osgeo.org%2F…%2Ftjpeg.tif
```

Single URL-encoded `url` parameter; ATAK fetches and runs it through the import manager (so data packages, `.pref`, KML, imagery all work).

**Untrusted server cert on first contact.** commoncommo has an explicit `SERVER_NOT_TRUSTED` error state. `CertificateEnrollmentClient.onEnrollmentErrored` handles it:

```java
case SERVER_NOT_TRUSTED: {
    message = "The TAK Server's identity could not be verified";
    // Quick-connect path: removeStreaming(...) and report QUICK_CONNECT_ERROR
    // Manual path: AlertDialog titled R.string.server_auth_error
    return;   // "This case is self-handled; we are done"
}
```

**ATAK does not offer a "trust this certificate anyway" prompt here — it fails and aborts.** There is a separate "enroll with trust" flow (`enrollForCertificateWithTrust` / `enrollUseTrust` preferences) where a truststore is supplied out of band. For quick-connect, the code requires the server to return trust material: `"no enrollment trust store and none given in enrollment setup!"` → `"Server did not return trust configuration"`. **Design implication: your enrollment endpoint must return the full CA chain, and either the device must already hold your CA or you must ship it in an enrollment profile.** A bare self-signed server cert with no prior trust will not enroll.

One relaxation: hostname verification *is* optional on the enrollment port. `DeviceProfileOperation` passes `profileRequest.isAllowAllHostnames()` for `CERT_ENROLLMENT`, but hardcodes `true` (verify) for `SECURE`.

### B.4 Device profiles

All from [`DeviceProfileOperation.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/net/DeviceProfileOperation.java) — the exact path-building code:

| Trigger | Method + path | Port | Auth |
|---|---|---|---|
| `onEnrollment` | `GET /Marti/api/tls/profile/enrollment?clientUid=<uid>` | 8446 | Basic |
| `onConnect` | `GET /Marti/api/device/profile/connection?syncSecago=<n>&clientUid=<uid>` | 8443 | client cert |
| tool (on demand) | `GET /Marti/api/device/profile/tool/<tool>?clientUid=<uid>[&syncSecago=<n>]` | 8443 | client cert |
| tool file | `GET /Marti/api/tls/profile/tool/<tool>/file?relativePath=/a&relativePath=/b&clientUid=<uid>[&syncSecago=<n>]` | 8446 | Basic |

The source comment confirms the split: *"api/tls allows basic auth (no client cert) on port 8446"*.

Path handling detail: spaces are replaced with `%20` only — the `URLEncoder.encode` call is commented out in the shipped source, so **other special characters in `relativePath` are sent raw**. Be lenient when parsing.

**Response contract:**
- **200** + body → processed. If `Content-Type: application/zip` (`ResourceFile.MIMEType.ZIP.MIME`) it's treated as a **Mission Package** and run through `MissionPackageExtractorFactory.Extract(...)`, then deleted (when `autoImportProfile`).
- **204 No Content** → nothing available, **not an error**. Use this for "no profile configured".
- **304 Not Modified** → honoured, in response to `If-Modified-Since`.
- Anything else → `ConnectionException`.

Caching headers: request `If-Modified-Since`, response `Last-Modified`. Source note: *"TAK Server only returns files which have been modified since this date. If multiple paths are specified, only those which have been modified since this date are returned"* and *"TAK Server returns last mod time for newest file"*.

Server side: [`ProfileAPI.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/device/profile/api/ProfileAPI.java), models `Profile`, `ProfileFile`, `ProfileDirectory`, `PreferenceFile`.

### B.5 The `.pref` XML format

**Your assumed format is close but the preference-group names for connections are wrong**, and that will break auto-configuration.

Exact emitter from [`PreferenceControl.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/app/preferences/PreferenceControl.java):

```xml
<?xml version='1.0' standalone='yes'?>
<preferences>
  <preference version="1" name="<SharedPreferences group name>">
    <entry key="<key>" class="<java.lang.Object.getClass().toString()>">value</entry>
    <entry key="<setKey>" class="class java.util.HashSet">
      <element>a</element>
      <element>b</element>
    </entry>
  </preference>
</preferences>
```

The `class` attribute is `value.getClass().toString()`, so it is literally `class java.lang.String`, `class java.lang.Boolean`, `class java.lang.Integer`, `class java.lang.Long`, `class java.util.HashSet`.

**Group names are dispatched on import:**

```java
switch (name) {
  case "cot_inputs":  loadConnectionHolder(connections[0], preference); break;
  case "cot_outputs": loadConnectionHolder(connections[1], preference); break;
  case "cot_streams": loadConnectionHolder(connections[2], preference); break;
  default:            loadSettings(preference, name, retval);           break;
}
```

**Streaming server connections go in `<preference name="cot_streams">`, not in the app preferences group.** The indexed keys inside it, exactly as read:

```
count                              (total number of connections)
description<j>
connectString<j>                   (TAKServer.CONNECT_STRING_KEY)
enabled<j>                         (boolean)
useAuth<j>                         (boolean)
compress<j>                        (boolean)
cacheCreds<j>
caPassword<j>
clientPassword<j>
caLocation<j>
certificateLocation<j>
enrollForCertificateWithTrust<j>   (boolean)
enrollUseTrust<j>                  (boolean, optional)
expiration<j>                      (long, -1 = never)
```

Note `caPassword` / `clientPassword` **are indexed** (`caPassword0`), contrary to your list.

General app preferences (`locationCallsign`, `locationTeam`, `atakRoleType`, …) go in a group named after the package — `DEFAULT_PREFERENCES_NAME = _context.getPackageName() + "_preferences"`. Legacy names `com.atakmap.app_preferences`, `com.atakmap.civ_preferences`, `com.atakmap.fvey_preferences` are accepted and rewritten to the current package. Your `com.atakmap.app.civ_preferences` guess is not in that legacy list — I'd use the plain package-based name, but note this is the one area where I'd verify empirically.

**Export excludes** `locationCallsign` and `bestDeviceUID` (plus two credential keys) — but **import does not**, so a server-pushed profile *can* set `locationCallsign`. On completion ATAK broadcasts `com.atakmap.app.PREFERENCES_LOADED`.

`.pref` is also a registered import type (`<data … android:pathPattern=".*\\.pref" />`), so it can be delivered inside a data package or via the `import` QR.

### B.6 OAuth / Keycloak

Server side, [`OAuthApi.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/oauth/OAuthApi.java):

| Endpoint | Purpose |
|---|---|
| `GET /login/auth` | begin authorization-code flow |
| `GET /login/redirect?code=&state=` | OIDC callback |
| `GET /login/refresh` | refresh |
| `GET /login/authserver` | returns the configured auth-server **name** |
| `GET /login/.well-known/openid-configuration` | returns `{authorization_endpoint, token_endpoint}` |
| `GET /token/access` | current access token |
| `GET|POST /logout` | logout |

Plus password-grant support: `PasswordGrantAuthenticationConverter` / `Provider` / `SuccessHandler`, `AccessTokenResolver`, `BearerTokenAuthenticationFilter`, `JdbcOAuth2AuthorizationService`, and `OAuthAuthenticator` on the messaging side. Token admin at `GET /Marti/api/token`, `DELETE /Marti/api/token/{token}`, `DELETE /Marti/api/token/revoke/{tokens}`.

commoncommo's enrollment request supports a bearer-token header (the `tokenHdr` branch in `curlExtraConfig`), which is the hook for token-based enrollment — consistent with the QR `token` parameter.

**What I could not verify:** whether ATAK 5.x performs a *browser-based* OIDC login during enrollment. I found no `authServer` handling and no OIDC/browser-redirect code in the atak-civ client tree. The evidence supports **bearer-token enrollment** (server-issued token, delivered by QR or typed), not an in-app OIDC dance. **Minimum for your server: HTTP Basic on 8446.** Treat OAuth as optional and verify against a real client before investing.

---

## C. Data Sync / Mission API

### C.1 Important caveat on confidence

**The ATAK-side Data Sync client is not in the public atak-civ repository.** Evidence:
- No `com/atakmap/android/mission*` package exists (only `missionpackage`, which is data packages — a different feature).
- Repo-wide code searches for `api/missions`, `t-x-m-c` and `Marti/sync` return **zero** ATAK Java hits (`Marti/sync` matches only the capture text file).

So §C is derived from the **server** implementation, which is authoritative for what a server must accept but does **not** tell you which subset ATAK actually calls, nor the exact headers ATAK sends. Where I mark something "ATAK-hit" below, that is inferred from the endpoint's shape and from TAK Server's own Python client ([`mission_api.py`](https://github.com/TAK-Product-Center/Server/blob/main/src/testing/load_test/mission_api.py)), not observed from ATAK source. **Plan to capture traffic from a real device to pin down the exact call sequence.**

### C.2 Response envelope

[`ApiResponse.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/cot/search/model/ApiResponse.java):

```java
private String version;      // Constants.API_VERSION = "3"
private String type;         // e.g. "Mission", "MissionChange", "com.bbn.marti.remote.groups.Group"
private T data;
private List<String> messages;
private final String nodeId;
```

`@JsonInclude(NON_NULL)`, so `messages` is omitted when empty. The `type` is usually `Class.getSimpleName()` — but **not always**: the Groups API uses the fully-qualified `com.bbn.marti.remote.groups.Group`, and ATAK depends on that exact string (`ServerGroup.GROUP_LIST_MATCHER`).

**API version header.** `MissionServiceDefaultImpl.getApiVersionNumberFromRequest`:

```java
String requestedApiVersionNumber = request.getHeader(Constants.API_VERSION_HEADER);  // "API_VERSION"
if (requestedApiVersionNumber != null) return Integer.parseInt(...);
return 2;   // default when absent
```

Clients send an **`API_VERSION` request header**; absent means 2. This changes behaviour — e.g. `createMissionSubscription` uses `getMission()` vs `getMissionByNameCheckGroups()` depending on whether it's ≥ 4. Support at least 2 and 3.

### C.3 Core mission endpoints

From [`MissionApi.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/sync/api/MissionApi.java) (~4950 lines; ~90 mappings). The ones that matter:

```
GET    /Marti/api/missions?passwordProtected=&defaultRole=&tool=
GET    /Marti/api/missions/{name}?password=&changes=&logs=&secago=&start=&end=
GET    /Marti/api/missions/guid/{guid}
PUT    /Marti/api/missions/{name}?creatorUid=&group=&description=&tool=&...
POST   /Marti/api/missions/{name}
DELETE /Marti/api/missions/{name}
PUT    /Marti/api/missions/{missionName}/copy
GET    /Marti/api/missions/{name}/archive
POST   /Marti/api/missions/{name}/send
```

**`tool` defaults to `"public"`.** This is easy to miss and will make missions invisible:

```java
if (tool != null) missions = missionService.getAllMissions(passwordProtected, defaultRole, tool, groups);
else              missions = missionService.getAllMissions(passwordProtected, defaultRole, "public", groups);
```

**Archive** returns a zip with `Content-Disposition: attachment; filename="<name>".zip` (note the odd quote placement — that's verbatim in the source).

**Contents:**
```
PUT    /Marti/api/missions/{name}/contents?creatorUid=     body: MissionContent JSON
DELETE /Marti/api/missions/{name}/contents?hash=&uid=
PUT    /Marti/api/missions/{name}/contents/missionpackage
```
`MissionContent` = `{"hashes":[…],"uids":[…],"paths":[…]}`; at least one must be non-empty or `IllegalArgumentException`. Test-client shape confirmed in `mission_api.py`: `json.dumps({"hashes": hashes, "uids": uids})` with `Content-Type: application/json`.

**CoT export:**
```
GET /Marti/api/missions/{name}/cot?path=
GET /Marti/api/missions/guid/{missionGuid}/cot?path=
```
Returns `Content-Type: application/xml` — the body is `missionService.getCachedCot(...)`. I did **not** read `getCachedCot`, so I cannot confirm from source that the root element is `<events>`; that is the conventional Marti shape but is **unverified**.

**Changes:**
```
GET /Marti/api/missions/{name}/changes?secago=&start=&end=&squashed=true
```
`squashed` defaults to **`true`**. Returns `ApiResponse<Set<MissionChange>>` with `type = "MissionChange"`.

**Change types** ([`MissionChangeType.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-plugins/src/main/java/com/bbn/marti/remote/sync/MissionChangeType.java)) — note the comment, the ordinals are persisted:

```java
// The ordinal of these types matters at the the database level!
public enum MissionChangeType {
    CREATE_MISSION, DELETE_MISSION, ADD_CONTENT, REMOVE_CONTENT,
    CREATE_MISSION_FEED, DELETE_MISSION_FEED
}
```

Also: `/keywords`, `/keywords/{keyword}`, `/uid/{uid}/keywords`, `/content/{hash}/keywords`, `/log`, `/logs/entries`, `/all/logs`, `/kml`, `/password`, `/expiration`, `/externaldata`, `/feed`, `/maplayers`, `/layers`, `/parent`, `/children`, `/contacts`, and `GET /Marti/api/sync/search`. Nearly every name-based route has a `/missions/guid/{guid}/…` twin — **support both**, since newer clients prefer GUIDs.

### C.4 Mission object

[`Mission.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-plugins/src/main/java/com/bbn/marti/sync/model/Mission.java), `@JsonInclude(NON_NULL)`:

```java
String name, description, chatRoom, baseLayer, bbox, boundingPolygon,
       path, classification, tool, creatorUid, guid, token, passwordHash;
Date createTime, lastEdited;          // @JsonFormat pattern = COT_DATE_FORMAT_PAD_MILLIS
Set<String> keywords;
Set<Mission> children;                // @JsonIgnore
Set<ExternalMissionData> externalData;
Set<MissionFeed> feeds;
Set<MapLayer> mapLayers;
NavigableSet<String> groups;
MissionRole ownerRole, defaultRole;
Long expiration;
String groupVector;                   // @JsonIgnore — never serialise
```

Serialisation subtleties:
- **`uids` and `contents` are serialised from the *Add* lists, not the raw sets.** `getUids()`/`getContents()` are `@JsonIgnore`; `@JsonProperty("uids")` is on `getUidAdds()` → `List<MissionAdd<String>>` and `@JsonProperty("contents")` on `getResourceAdds()` → `List<MissionAdd<Resource>>`. `MissionAdd<T>` = `{data, timestamp, creatorUid, keywords}`. Your JSON must match this shape, not a flat array of strings.
- `passwordProtected` is a computed `@JsonProperty` (derived from `passwordHash != null`), not a stored column.
- `groupVector` is `@JsonIgnore` — leaking it is an information disclosure.
- Date format: `Constants.COT_DATE_FORMAT_PAD_MILLIS = "yyyy-MM-dd'T'HH:mm:ss.SSS'Z'"`. There is also an unpadded `COT_DATE_FORMAT = "yyyy-MM-dd'T'HH:mm:ss.S'Z'"` — **emit padded**.

### C.5 Roles, permissions, subscriptions

[`MissionRole.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-plugins/src/main/java/com/bbn/marti/sync/model/MissionRole.java):

```java
public enum Role { MISSION_OWNER, MISSION_SUBSCRIBER, MISSION_READONLY_SUBSCRIBER }
public static final Role defaultRole = Role.MISSION_SUBSCRIBER;
```

Serialised via `@JsonProperty("type")` on `getRole()` — so the wire field is **`type`**, not `role`. `permissions` is a `Set<MissionPermission>`.

[`MissionPermission.Permission`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-plugins/src/main/java/com/bbn/marti/sync/model/MissionPermission.java):

```java
MISSION_READ, MISSION_WRITE, MISSION_DELETE, MISSION_SET_ROLE,
MISSION_SET_PASSWORD, MISSION_UPDATE_GROUPS, MISSION_MANAGE_FEEDS,
MISSION_MANAGE_LAYERS
```

The permission set is what's enforced (`@PreAuthorize("hasPermission(#request, 'MISSION_READ')")`); the three role names are just named bundles. Conventional mapping: owner = all; subscriber = READ+WRITE; readonly-subscriber = READ.

Subscription endpoints:

```
PUT    /Marti/api/missions/{missionName}/subscription?uid=&topic=&password=&secago=&start=&end=
GET    /Marti/api/missions/{missionName}/subscription?uid=
POST   /Marti/api/missions/{missionName}/subscription
DELETE /Marti/api/missions/{missionName}/subscription?uid=
GET    /Marti/api/missions/{missionName}/subscriptions
GET    /Marti/api/missions/{missionName}/subscriptions/roles
GET    /Marti/api/missions/all/subscriptions
GET    /Marti/api/missions/all/subscriptions/guid
GET    /Marti/api/missions/{missionName}/role
PUT    /Marti/api/missions/{missionName}/role
GET    /Marti/api/missions/{missionName}/token
```

`PUT …/subscription` returns **201 Created** with `ApiResponse<MissionSubscription>`.

### C.6 Mission tokens — exact auth mechanism

From `MissionServiceDefaultImpl.getRoleFromToken` ([source](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/sync/service/MissionServiceDefaultImpl.java)):

```java
if (commonUtil.isAdmin(request))
    return missionRoleRepository.findFirstByRole(MissionRole.Role.MISSION_OWNER);

// if the request has an Authorization header, get the role from the token
String authorization = request.getHeader("MissionAuthorization") != null ?
        request.getHeader("MissionAuthorization") : request.getHeader("Authorization");
if (authorization == null) return null;
if (!authorization.startsWith("Bearer ")) { … return null; }
```

**Two header names, `MissionAuthorization` taking precedence over `Authorization`** — this matters because mission tokens must coexist with OAuth bearer tokens on the same request. Support both.

Token format ([`MissionTokenUtils.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/sync/service/MissionTokenUtils.java)): a JWT signed **HS256** with:

```java
.claim(tokenType.name(), id)          // INVITATION | SUBSCRIPTION | ACCESS
.claim(MISSION_NAME_CLAIM, missionName)
.claim(MISSION_GUID_CLAIM, missionGuid.toString())
```

Three `TokenType`s; `createMissionSubscription` accepts any of `{INVITATION, SUBSCRIPTION, ACCESS}`.

**Password-protected missions** (`BCrypt` hashes):

```java
if (mission.isPasswordProtected()) {
    if (!Strings.isNullOrEmpty(password)) {
        if (!BCrypt.checkpw(password, mission.getPasswordHash()))
            throw new ForbiddenException("… Password did not match.");
    } else if (subRole == null) {
        throw new ForbiddenException("… No token role provided.");
    }
} else if (!Strings.isNullOrEmpty(password)) {
    throw new ForbiddenException("… No password provided.");   // password on a non-protected mission is an error
}
```

Note the last branch — supplying a password to an *unprotected* mission is rejected.

**Tokens are stripped from list output**: comments at lines 2852/2872 read *"ensure tokens are removed from the output"*. Do the same.

### C.7 Mission CoT notifications over the stream

Template and construction from `DistributedSubscriptionManager`:

```xml
<event how='h-g-i-g-o' type='t-x-m-c' version='2.0' uid='<generated>'
       time='<now>' start='<now>' stale='<now+20s>'>
  <point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/>
  <detail>
    <mission type="CHANGE" tool="" name="…" guid="…" authorUid="…" uid="…" token="…">
      <MissionChanges>
        <MissionChange>…</MissionChange>
      </MissionChanges>
    </mission>
  </detail>
</event>
```

`createMissionMessage` sets, conditionally (omitted when null/empty): `name` (always), `guid`, `type` (the *message* type: `CHANGE` or `INVITE`), `authorUid`, `tool`, `uid`, `token`. Changes XML is grafted under `<mission>` via JAXB `MissionChanges` → `<MissionChanges><MissionChange/>…</MissionChanges>` ([`MissionChanges.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/sync/model/MissionChanges.java)).

**CoT type varies by change kind** — a detail your spec should capture:

```java
case LOG:              cotType = "t-x-m-c-l";
case KEYWORD:          cotType = "t-x-m-c-k";
case UID_KEYWORD:      cotType = "t-x-m-c-k-u";
case RESOURCE_KEYWORD: cotType = "t-x-m-c-k-c";
case METADATA:         cotType = "t-x-m-c-m";
case EXTERNAL_DATA:    cotType = "t-x-m-c-e";
case MISSION_LAYER:    cotType = "t-x-m-c-h";
case CONTENT:          cotType = "t-x-m-c";
```

Invitations and role changes reuse the same builder:

```java
createMissionInviteMessage(...)     → cotType "t-x-m-i", msgType "INVITE", carries token + roleXml
createMissionRoleChangeMessage(...) → cotType "t-x-m-r", msgType "INVITE", carries roleXml
```

So the **invite carries the JWT inline** as `<mission token="…">`, and the client uses it to subscribe. Invitation REST:

```
POST   /Marti/api/missions/{name}/invite
PUT    /Marti/api/missions/{name}/invite/{type}/{invitee}
DELETE /Marti/api/missions/{name}/invite/{type}/{invitee}
GET    /Marti/api/missions/all/invitations
GET    /Marti/api/missions/invitations
GET    /Marti/api/missions/{missionName}/invitations
```

### C.8 Version, contacts, CoT query

```
GET /Marti/api/version              → plain string (versionBean.getVer())
GET /Marti/api/version/info         → VersionInfo object
GET /Marti/api/version/config       → ApiResponse<ServerConfig>
GET /Marti/api/node/id              → server node id string
```

[`VersionApi.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/util/VersionApi.java) — `ServerConfig` has exactly three fields: `version`, `api`, `hostname`. `api` = `Constants.API_VERSION` = `"3"`. `hostname` is derived from the request URL. The version string is normalised: `"TAK Server"` stripped, then `tokens[0] + "." + tokens[2] + "-" + tokens[1]`.

ATAK's consumer, [`ServerVersion.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/http/rest/ServerVersion.java), documents the expected JSON in its class comment:

```
"version": "2",
"version": "1.3.12.156-DEV",
"api": "2",
"hostname": "localhost"
```

i.e. the envelope `version` plus `data.{version,api,hostname}`. ATAK keeps only `apiVersion` (int) and `version` (string), and gates features on it: `MPT_TOOL_PARAM_MIN_VERSION = 2`. [`GetServerVersionOperation.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/http/rest/operation/GetServerVersionOperation.java) picks `api/version/config` or `api/version` based on `isGetConfig()`. **This is the capability-detection handshake — implement `version/config` correctly and early.**

```
GET /Marti/api/clientEndPoints?secAgo=&showCurrentlyConnectedClients=&showMostRecentOnly=&group=
```
[`ContactManagerApi.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/network/ContactManagerApi.java) → `ApiResponse<List<ClientEndpoint>>`. ATAK calls it from `GetClientListOperation.java` (confirmed by code search).

```
GET /Marti/api/cot/xml/{uid}
GET /Marti/api/cot/xml/{uid}/all?secago=&start=&end=
GET /Marti/api/cot/sa?start=&end=&left=&bottom=&right=&top=&isFiltered=true
GET /Marti/api/cot/matchUid?search=
GET|POST /Marti/api/cot
```
[`CotApi.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/sync/api/CotApi.java).

**`/Marti/api/contacts/all` — I could not verify.** It is not in `ContactManagerApi`, and my search was cut short by rate limiting. It is widely used by third-party tooling; treat as likely-present but unconfirmed in this codebase.

---

## D. Data Packages / Enterprise Sync

### D.1 Manifest format

From [`MissionPackageBuilder.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/file/MissionPackageBuilder.java):

```java
public static final String MANIFEST_PATH = "MANIFEST";
static final String MANIFEST_XML = MANIFEST_PATH + File.separator + "manifest.xml";
```

So the zip entry is `MANIFEST/manifest.xml`. Root `@Root(name = "MissionPackageManifest")` with `@Attribute(name="version", required=true)` ([`MissionPackageManifest.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/file/MissionPackageManifest.java)).

Configuration parameters ([`MissionPackageConfiguration.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/file/MissionPackageConfiguration.java)):

```java
PARAMETER_NAME              = "name"               // required
PARAMETER_UID               = "uid"                // required (UUID)
PARAMETER_REMARKS           = "remarks"
PARAMETER_OnReceiveDelete   = "onReceiveDelete"
PARAMETER_OnReceiveImport   = "onReceiveImport"
PARAMETER_DeleteWithPackage = "deleteWithPackage"
PARAMETER_OnReceiveAction   = "onReceiveAction"    // Intent action broadcast after extract/delete
```

`isValid()` requires **both** `name` and `uid`. Two parameters you didn't list — `deleteWithPackage` and `onReceiveAction` — exist and are honoured.

Content element ([`MissionPackageContent.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/file/MissionPackageContent.java)):

```java
@Attribute(name = "zipEntry", required = true)   // "Labelled 'zipEntry' in the Mission Package XML v2"
@Attribute(name = "ignore", required = false)    // default false
```

Child parameters include `PARAMETER_LOCALPATH`, `PARAMETER_UID`, `PARAMETER_CONTENT_TYPE`.

Resulting shape:

```xml
<MissionPackageManifest version="2">
  <Configuration>
    <Parameter name="uid" value="…"/>
    <Parameter name="name" value="…"/>
    <Parameter name="onReceiveDelete" value="true"/>
    <Parameter name="onReceiveImport" value="true"/>
  </Configuration>
  <Contents>
    <Content ignore="false" zipEntry="path/in/zip.ext">
      <Parameter name="uid" value="…"/>
      <Parameter name="name" value="…"/>
    </Content>
  </Contents>
</MissionPackageManifest>
```

### D.2 Upload flow — verified three-step

From [`missionpackagemanager.cpp`](https://github.com/TAK-Product-Center/atak-civ/blob/main/commoncommo/core/impl/missionpackagemanager.cpp), states `CHECK → UPLOAD → TOOLSET`:

**1. Dedup check**
```
GET https://{host}:{httpsPort}/Marti/sync/missionquery?hash={sha256}
```

**2. Upload** (only if the check 404s)
```
POST https://{host}:{httpsPort}/Marti/sync/missionupload?hash={sha256}&filename={name}&creatorUid={uid}
Content-Type: multipart/form-data
  part name:     "assetfile"
  part filename: {name}
  part type:     "application/x-zip-compressed"
```

**3. Tool tagging**
```
PUT https://{host}:{httpsPort}/Marti/api/sync/metadata/{sha256}/tool
Content-Type: text/plain
body: "public"   (server-only upload)  |  "private"  (direct send to contacts)
```

The `public`/`private` choice is literally `upCtx->contacts ? "private" : "public"` — a package sent to specific contacts is `private`; one uploaded to the server is `public`. This drives `GET /Marti/sync/search?keywords=missionpackage&tool=…` visibility.

Also note `CURLOPT_SSL_VERIFYHOST, 0L` on this path — **hostname verification is disabled for data-package transfer**.

### D.3 Server responses — plain text, as you suspected

[`MissionPackageQueryServlet.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/sync/MissionPackageQueryServlet.java):

```java
String responseStr = String.format("%s/Marti/sync/content?hash=%s", getBaseUrl(request), uid);
response.setStatus(HttpServletResponse.SC_OK);
PrintWriter writer = response.getWriter();
writer.print(responseStr);
// not found → response.sendError(SC_NOT_FOUND, "File not found")
```

[`MissionPackageUploadServlet.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/sync/MissionPackageUploadServlet.java) returns the **same** plain-text URL. Two notable comments in that servlet:

> `// clients may send a locally computed hash value that is ignored by TAK server`

The `hash` query param is **optional and ignored** — the server computes its own hash and returns *that* as the content UID. Your implementation should do the same rather than trusting the client.

> `"Data package upload must use multipart/form-data POST. … Part name should be named 'assetfile'"`
> `// ATAK sends a part called "assetfile"`

Required param: `filename`. Optional: `hash`, `mimetype`, `keywords`, `tool`, `creatorUid`, `groups`.

Other sync routes: `ContentServlet`, `DeleteServlet`, `MetadataServlet`, `SearchServlet`, `UploadServlet`, `MissionPackageCreatorServlet`.

`GET /Marti/sync/search?keywords=missionpackage` — ATAK's URL is hardcoded in [`QueryMissionPackageOperation.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/http/rest/QueryMissionPackageOperation.java): `.getUrl("/sync/search?keywords=missionpackage")`.

Response JSON per [`MissionPackageQueryResult.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/http/datamodel/MissionPackageQueryResult.java) — **note the capitalised keys**:

```json
{ "results": [ {
    "UID": "...", "Name": "...", "Hash": "...", "PrimaryKey": 1,
    "SubmissionDateTime": "...", "SubmissionUser": "...",
    "CreatorUid": "...", "Keywords": "...", "MIMEType": "...", "Size": 1814
} ] }
```

`UID`, `Name`, `Hash`, `PrimaryKey`, `SubmissionDateTime` are mandatory (`getString` without guard → `JSONException`); the rest are optional. `SubmissionUser` is the SSL cert identity; `CreatorUid` the ATAK device UID.

Download: `GET /Marti/sync/content?hash={hash}`, with `Content-Disposition` carrying `filename` (the test client parses it). ATAK appends `&receiver={ourCallsign}` to download URLs.

### D.4 FileShare CoT

`b-f-t-r` (request) — through-server capture, showing the rewritten `senderUrl`:

```xml
<event version="2.0" uid="0377f926-…" type="b-f-t-r" time="…" start="…" stale="…" how="h-e">
  <point lat="36.72459169511786" lon="-86.70106407887" hae="173.888" ce="9999999" le="9999999"/>
  <detail>
    <fileshare filename="MP-GEMINI.zip"
               senderUrl="http://192.168.135.160:8080/Marti/sync/content?uid=72fbcc94…"
               sizeInBytes="1814"
               sha256="72fbcc94207812f67580f2fb246dc3f0c2b3777249c5ccd6ca4af04a0e8671ed"
               senderUid="ANDROID-48:5A:3F:49:93:24" senderCallsign="GEMINI"
               name="MP-GEMINI"/>
    <ackrequest uid="a5fc4772-…" ackrequested="true" tag="MP-GEMINI"
                endpoint="192.168.167.167:4242:tcp"/>
    <precisionlocation geopointsrc="User" altsrc="DTED0"/>
    <_flow-tags_ takServer1="2016-01-14T19:17:14.970Z"/>
  </detail>
</event>
```

Peer-to-peer form uses `senderUrl="http://<ip>:8080/getfile?file=9&sender=GEMINI"` and may add `md5=`. `<ackrequest>` carries an optional `endpoint=` in the P2P case.

`b-f-t-a` (ack) — note the **event uid equals the `ackrequest` uid**, and the payload element is `<ackresponse>`:

```xml
<event version='2.0' uid='2d35c256-8740-4c39-a8c6-211cbb18fc55' type='b-f-t-a'
       time='…' start='…' stale='…' how='m-g'>
  <point lat='38.562' lon='-83.890' hae='235.3' ce='9999999' le='9999999'/>
  <detail>
    <contact callsign='ANDROID-48:5A:3F:49:93:24'/>
    <ackresponse uid='2d35c256-8740-4c39-a8c6-211cbb18fc55' sizeInBytes='1814'
                 sha256='72fbcc94…' senderUid='ANDROID-48:5A:3F:49:93:24'
                 reason='File already exists' success='true' tag='test.zip'/>
    <precisionlocation geopointsrc='User' altsrc='DTED0'/>
  </detail>
</event>
```

ATAK silently consumes acks: `MISSION_PACKAGE_ACK_TYPE = "b-f-t-a"` → *"quietly consume the mission package ack - no reason to process it"* (`CotMapComponent.java`).

**Your server's job is just to relay `b-f-t-r`/`b-f-t-a` and rewrite `senderUrl` to point at your own `/Marti/sync/content?hash=…`.** The capture shows TAK Server doing exactly that (P2P `getfile` URL → server `Marti/sync/content` URL).

### D.5 Tools and keywords

`tool` values seen: `public`, `private`, `ExCheck` (server has a full `com.bbn.marti.excheck` package), `vbm`. `GET /Marti/api/missions?tool=` filters on it and **defaults to `public`**. `PUT /Marti/api/sync/metadata/{hash}/{key}` sets arbitrary metadata; ATAK only ever sets `tool`.

---

## E. Groups / Channels

### E.1 Model

[`Group.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-common/src/main/java/com/bbn/marti/remote/groups/Group.java):

```java
String  name;
Direction direction;             // IN | OUT
Date    created;                 // @JsonFormat(pattern = "yyyy-MM-dd")
Type    type;                    // default Type.SYSTEM
Integer bitpos;                  // null until assigned
boolean active = true;
String  description;             // nullable
String  distinguishedName;       // LDAP DN, nullable
```

**Identity is `(name, direction)`** — `equals`/`hashCode` use both, so `"Blue/IN"` and `"Blue/OUT"` are distinct objects. `created` serialises as **date-only `yyyy-MM-dd`**, unlike mission timestamps.

`Constants.ANON_GROUP = "__ANON__"`, `ANONYMOUS_ROLE = "ROLE_ANONYMOUS"` ([`Constants.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-plugins/src/main/java/tak/server/Constants.java)).

Routing is a **bit-vector intersection**: `groupVector` / `GROUPS_BIT_VECTOR_KEY`, with `bitpos` as each group's index. A message from sender S reaches recipient R iff S's **OUT** vector intersects R's **IN** vector. `CommonGroupDirectedReachability` implements this. Caches: `ACTIVE_GROUPS_CACHE`, `IGNITE_USER_OUTBOUND_GROUP_CACHE`, `IGNITE_USER_INBOUND_GROUP_CACHE`.

### E.2 Endpoints

[`GroupsApi.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/groups/GroupsApi.java):

```
GET /Marti/api/groups/all?useCache=false&sendLatestSA=false
GET /Marti/api/groups/{name}/{direction}
GET /Marti/api/groups/user?username=
GET /Marti/api/groups/groupCacheEnabled
GET /Marti/api/users/all
GET /Marti/api/users/{connectionId}
```

[`SubscriptionApi.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/sync/api/SubscriptionApi.java):

```
PUT  /Marti/api/groups/active?clientUid=        body: Group[]
PUT  /Marti/api/groups/activebits?clientUid=    body: Integer[]
PUT  /Marti/api/groups/activeForce?username=    body: Group[]
POST /Marti/api/groups/update                   body: Set<String> usernames
GET  /Marti/api/groups/update/{username}
GET  /Marti/api/subscriptions/all?sortBy=CALLSIGN&direction=ASCENDING&page=-1&limit=-1
GET  /Marti/api/subscription/{uid}
POST /Marti/api/subscriptions/add
DELETE /Marti/api/subscriptions/delete/{uid}
POST /Marti/api/subscriptions/incognito/{uid}
GET|PUT /Marti/api/subscriptions/{clientUid}/filter
```

**Correction to your premise: `/Marti/api/groups/update/{clientUid}` is actually `/{username}`** (`@PathVariable(value = "username")`).

**`sendLatestSA=true` on `/groups/all` triggers the SA replay** described in §A.8 — that's the explicit hook.

### E.3 What ATAK actually calls

ATAK's Channels UI lives in [`com/atakmap/android/channels/`](https://github.com/TAK-Product-Center/atak-civ/tree/main/atak/ATAK/app/src/main/java/com/atakmap/android/channels) (`ChannelsMapComponent`, `ChannelsReceiver`, `ServerGroupsClient`, `ChannelsOverlay`, …). Two HTTP calls, verified:

**Fetch** — [`GetAllServerGroupsOperation.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/channels/net/GetAllServerGroupsOperation.java):
```java
String url = "/api/groups/all?useCache=true";
```
**`useCache=true` is hardcoded** — ATAK always asks for the cached view.

**Set active** — [`SetActiveServerGroupsOperation.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/channels/net/SetActiveServerGroupsOperation.java):
```java
HttpPut httpPut = new HttpPut(httpClient.getUrl("/api/groups/active?clientUid=" + MapView.getDeviceUid()));
httpPut.addHeader("content-type", "application/json");
httpPut.setEntity(new StringEntity(activeGroups, UTF8));
```

ATAK's group JSON ([`ServerGroup.java`](https://github.com/TAK-Product-Center/atak-civ/blob/main/atak/ATAK/app/src/main/java/com/atakmap/android/http/rest/ServerGroup.java)):

```java
public static final String GROUP_LIST_MATCHER = "com.bbn.marti.remote.groups.Group";
public static final String PATH_ALL_GROUPS    = "api/groups/all";
```

Serialises `name`, `distinguishedName`, `direction`, `created`, `type`, `bitpos`, `active` — **`description` is deliberately not sent back**. Parsing rules, which constrain your response:

- **`created` is parsed as a *string* with `KMLUtil.KMLDateFormatter`**, and a parse failure throws. Even though the Java server annotates it `yyyy-MM-dd`, ATAK needs a KML-style datetime. **This is a real interop hazard — emit an ISO-8601 datetime, and verify empirically.**
- `active`, `bitpos`, `description`, `distinguishedName` are each individually guarded — safe to omit.
- `name`, `direction`, `type` are mandatory (`getString`, unguarded).
- `isValid()` requires non-empty `name`, `direction`, `type`, plus `created >= 0` **and `bitpos >= 0`** — so **a group with no `bitpos` is silently dropped from the UI.** Always assign one.
- The envelope's `type` field must be exactly `com.bbn.marti.remote.groups.Group`.

### E.4 Cert → user → groups

[`UserAuthenticationFile.xsd`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-common/src/main/xsd/UserAuthenticationFile.xsd), namespace `http://bbn.com/marti/xml/bindings`:

```xml
<User identifier="…"            (required)
      fingerprint="…"           (optional — cert fingerprint)
      password="…"              (optional)
      passwordHashed="true|false"
      role="ROLE_ANONYMOUS">    (default)
  <groupList>…</groupList>      (0..n — both directions)
  <groupListIN>…</groupListIN>  (0..n — IN only)
  <groupListOUT>…</groupListOUT>(0..n — OUT only)
</User>
```

**Three group list elements, not one.** `<groupList>` grants both directions; `IN`/`OUT` variants grant one. Your premise mentioned only `<groupList>`.

Roles: `ROLE_NONEXISTENT`, `ROLE_ADMIN`, `ROLE_READONLY`, `ROLE_ANONYMOUS`, `ROLE_NON_ADMIN_UI`, `ROLE_WEBTAK`. (`ROLE_NON_ADMIN_UI` is listed twice in the XSD — a harmless upstream bug.)

Mapping: the cert **CN** is matched to `@identifier`, or the cert **fingerprint** to `@fingerprint`. Since enrollment sets `CN=<username>` (§B.1), the loop closes: enroll as `alice` → cert `CN=alice` → `<User identifier="alice">` → group list → bit vector. Authenticators: `FileAuthCodec`, `LdapAuthCodec`, `X509AuthCodec`, `AnonymousAuthCodec`. Config selects via `<auth><File location="UserAuthenticationFile.xml"/></auth>`.

### E.5 Group-change notification

```xml
<event how='h-g-i-g-o' type='t-x-g-c' version='2.0'
       uid='<generatedUid>.<clientUid>' time='<now>' start='<now>' stale='<now+20s>'>
  <point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/>
  <detail><link relation='p-p'/></detail>
</event>
```

Note the composite uid: `MessageConversionUtil.generateUid() + "." + clientUid`. On receipt ATAK re-fetches `/api/groups/all`. After a group change the server also runs `sendLatestReachableSA` + `sendUpdatedGroupsLatestSA`.

---

## F. Other requirements

**Port/auth matrix** (recap, `SslNetCotPort.java` + `CoreConfig.example.xml`):

| Port | Transport | Auth | Used for |
|---|---|---|---|
| 8080 | HTTP | none | plain Marti; legacy `getfile` |
| 8443 | HTTPS | **client cert** | Marti API, missions, device profile (connection/tool) |
| 8446 | HTTPS, `clientAuth="false"` | **HTTP Basic** | `/Marti/api/tls/*` — enrollment, enrollment profile, tool files |
| 8087 | TCP / UDP | anonymous | CoT (disabled by default) |
| 8088 | stcp | anonymous | streaming CoT |
| 8089 | TLS | client cert | streaming CoT (**default enabled**) |
| 8090 | QUIC | client cert + ALPN `takstream` | streaming CoT |

**`<takv>` fields** (`takv.proto`): `device`, `platform`, `os`, `version`. Typical: `platform="ATAK-CIV"`, `os="29"`, `version="4.10.0.0"`, `device="Google Pixel 6"`. All four required for protobuf conversion — a `<takv>` missing any one stays in `xmlDetail`.

**Roles endpoint** — [`HomeApi.java`](https://github.com/TAK-Product-Center/Server/blob/main/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/network/HomeApi.java):
```
GET /Marti/api/util/user/roles   → Collection<String>  (bare JSON array, no ApiResponse envelope)
GET /Marti/api/util/isAdmin      → boolean
GET /Marti/api/home
GET /Marti/api/ver
```

**CORS/security headers** set by `MissionRoleAssignmentRequestHolderFilterBean`:
```java
resp.setHeader("Strict-Transport-Security", "max-age=63072000; includeSubDomains");
resp.setHeader("Access-Control-Allow-Origin", "*");
resp.setHeader("Access-Control-Allow-Headers", "*");
resp.setHeader("Access-Control-Allow-Methods", "*");
```

**ATAK HTTPS client posture:**
- Enrollment port: `CertificateManager.getSockFactory(false, baseUrl, allowAllHostnames)` — hostname verification **optional**.
- Secure port: hostname verification **hardcoded true**.
- Data-package transfer (commoncommo): `CURLOPT_SSL_VERIFYHOST, 0L` — **off**.
- Streaming: custom in-memory chain validation against the enrollment truststore; OpenSSL's own verify disabled.

Net: **certificate-chain validation is always enforced; hostname verification is inconsistent.** Do not rely on hostname mismatch being tolerated on 8443.

**Mesh defaults** (from the capture file header, "From Shawn"): SA multicast `239.2.3.1:6969`, GeoChat `224.10.10.1:17012`, P2P TCP `:4242`. Out of scope for a server but relevant if you ever bridge mesh.

---

## Licensing — read before vendoring

**Both repositories are GPL-3.0.**

- `atak-civ/LICENSE.md` → GNU GPL v3 in full, with the standard "how to apply" appendix and no linking exception. GitHub reports the license as `NOASSERTION` / "Other" because of the appendix text, but the body is unmodified GPL-3.0.
- `Server/LICENSE.md` and `Server/LICENSE.txt` → GPL-3.0.

**The `.proto` files carry no per-file copyright or license header.** I read all ten in `commoncommo/core/impl/protobuf/` — each begins directly with `syntax = "proto3";`. There is no `LICENSE` inside `commoncommo/` or `takproto/`; `atak/.../license/commoncommo.txt` is a *third-party dependency* notice (libcurl, OpenSSL), **not** a grant covering commoncommo itself. So the only license attaching to those files is the repo-level GPL-3.0.

**Implications for a Rust TAK server:**

1. **Copying the `.proto` files into your repo makes them GPL-3.0 licensed files in your tree.** Whether the *generated* Rust code and your server are thereby derivative is the classic unsettled question — plausibly yes for a wire-format schema that is largely functional, but "largely functional" is an argument, not a safe harbour. **This is a legal question, not a technical one. Get advice before shipping non-GPL.**
2. **Lower-risk alternative: reimplement from the specification.** `protocol.txt` documents the framing, negotiation and varint encoding completely, and the field numbers and types are the observable wire format. Writing your own `.proto` (or hand-rolled prost structs) from the documented field numbers is a much weaker derivation claim than copying the files.
3. **Do not copy comment blocks.** The normative `detail.proto` sender/receiver rules are genuinely expressive prose and the most clearly copyrightable part of those files. Paraphrase them in your own docs; cite the source.
4. **Do not copy from TAK Server.** Endpoint paths, JSON field names and HTTP semantics are facts/interfaces and safe to implement; Java source is not.
5. The existing Rust crates below are MIT / Apache-2.0 and already embed protobuf definitions. If their maintainers derived them from the GPL originals, **that licensing question propagates to you** — check provenance before depending on one for the schema. This is my flag, not a claim about any specific crate.

---

## Rust crates

All metadata from the crates.io API, September 2026.

| Crate | Version | Updated | Downloads (total / recent) | License | Repository |
|---|---|---|---|---|---|
| [`cot-proto`](https://crates.io/crates/cot-proto) | 0.5.1 | 2025-01-24 | 9,596 / 630 | Apache-2.0 | [ajfabbri/cot-proto](https://github.com/ajfabbri/cot-proto) |
| [`cot_publisher`](https://crates.io/crates/cot_publisher) | 2.0.0 | 2025-11-04 | 3,480 / 88 | MIT | [martynp/cot_publisher](https://github.com/martynp/cot_publisher) |
| [`rustak`](https://crates.io/crates/rustak) | 0.1.1 | 2025-05-20 | 1,082 / 13 | MIT | [tesorrells/RusTAK](https://github.com/tesorrells/RusTAK) |
| [`cottak`](https://crates.io/crates/cottak) | 0.1.1 | 2025-07-08 | 955 / 21 | non-standard | none declared |
| [`takproto`](https://crates.io/crates/takproto) | 0.4.2 | 2025-11-11 | 365 / 88 | MIT OR Apache-2.0 | `rabarar/takproto` (**404 — private or renamed**) |

Assessment:

- **`cot-proto`** — the most-used and most mature. CoT XML (de)serialisation with serde. **XML only, no protobuf, no networking.** Last touched Jan 2025. Useful as a reference for CoT struct modelling; not a transport.
- **`takproto`** — closest to your needs on paper: TAK Protocol v1, protobuf via prost, mTLS, negotiation handshake, tokio. But it is **brand new** (created 2025-11-10, last release the next day), has **365 total downloads**, and its **repository URL 404s**, so I could not verify its source, provenance of the `.proto` files, or whether the framing is correct. Also note docs.rs shows a **2026-08-30** build date against a 2025-11 release. **Treat as unvetted.**
- **`rustak`** — name collision with your project. v0.1.1, 13 recent downloads, and its own README states **TAK protobuf support is planned, not implemented** (XML CoT only, plus UDP/TCP/TLS connection helpers). Client-oriented. **You may want to pick a different crate name, or contact the author, before publishing.**
- **`cot_publisher`** — publisher-only (multicast UDP + TCP to TAK servers), async and blocking. Not a server library. `non-standard`-adjacent maturity but actively maintained (Nov 2025).
- **`cottak`** — the crates.io description ("A built in test application for Linux using dynamic libraries in Rust") does **not** match the docs.rs/vendor pages, which describe XML + protobuf CoT support ([astute-systems.github.io/cot-tak](https://astute-systems.github.io/cot-tak/cottak/)). **License is `non-standard` and no repository is declared.** Avoid until clarified.
- `cotton` and `hedge` are **unrelated** — a CLI prelude crate and a half-edge mesh library respectively. `tak-rs` and `cursor-on-target` do not exist.

**Recommendation:** none of these is a viable foundation for a wire-compatible *server*. Every one is client/publisher-shaped, and the only one claiming full v1 protobuf + negotiation is unverifiable. Build the protocol layer yourself on `prost` + `tokio-rustls` + `quick-xml`, and use `cot-proto` at most as a modelling reference.

---

## Summary of things I could not verify

Stated explicitly so you don't mistake inference for fact:

1. **ATAK's actual Data Sync HTTP call sequence** — client code absent from the public repo. §C is server-derived.
2. **Whether `/Marti/api/missions/{name}/cot` returns a root `<events>` element** — endpoint confirmed, `Content-Type: application/xml` confirmed, body shape not traced.
3. **`/Marti/api/contacts/all`** — not found in `ContactManagerApi`; search cut short by rate limiting.
4. **Browser-based OIDC during ATAK 5.x enrollment** — no supporting code found in atak-civ; evidence points to bearer-token enrollment only.
5. **GeoChat `groupOwner=`, `senderCallsign=`, `parent=`** — present in modern ATAK per your description, but absent from the 2015-era capture that is the only primary sample in the repo.
6. **`__serverdestination` server-side writer** — format confirmed from capture; generating code not located.
7. **Issued-certificate extension requirements (EKU, SAN, validity)** — ATAK performs no policy validation; my guidance in §B.2 is a recommendation, not an observed requirement.
8. **The `!ECDH` cipher-list consequence** — the code line is verified verbatim; the conclusion that it excludes ECDHE under TLS 1.2 follows from OpenSSL's documented alias semantics and should be confirmed empirically.
9. **The `com.atakmap.app.civ_preferences` group name** — ATAK derives it from the package name at runtime and accepts three legacy aliases, none of which is that string. Verify against a real profile export.
10. **`ServerGroup.created` date format** — server annotates `yyyy-MM-dd`, ATAK parses with a KML datetime formatter and throws on failure. These appear inconsistent; test before relying on either.
