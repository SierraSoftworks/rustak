I've read the relevant Java. Here is the verified report.

**Repo root** (all paths below are absolute):
`/private/tmp/claude-501/-Users-bpannell-dev-gh-SierraSoftworks-rustak/9ec7cece-906f-4639-9129-d03da7a814b7/scratchpad/refs/takserver` — abbreviated below as `$R`.

---

# 0. Correction up front: two pipelines exist, only one is live

Your premise "SslCodec → X509AuthCodec → StreamingProtoBufOrCoTProtocol" describes the **legacy `coreVersion="1"` NIO pipeline**, which in this source revision is **only reachable for UDP/multicast inputs**. All TCP/TLS/STCP/QUIC/gRPC inputs go through **Netty**, regardless of `coreVersion`:

`$R/src/takserver-core/src/main/java/com/bbn/marti/service/SubmissionService.java:1786-1836`

```java
if (input.getProtocol().equals("grpc"))      { nettyBuilder.buildGrpcServer(input); }
else if (input.getProtocol().equals("quic")) { input.setCoreVersion2TlsVersions("TLSv1.3"); nettyBuilder.buildQuicServer(input); }
else if (!isUdp) {
    if (isTls)                                    nettyBuilder.buildTlsServer(input);
    else if (transport == TransportCotEvent.TCP)  nettyBuilder.buildTcpServer(input);
    else if (transport == TransportCotEvent.STCP) nettyBuilder.buildStcpServer(input);
} else { /* only here is input.getCoreVersion()==2 consulted */ }
```

`getCodecSources()` (`SubmissionService.java:2327-2393`), which builds `SslCodec`/`X509AuthCodec`/`FileAuthCodec` **CodecSource** chains, is only called on the `else` (UDP, coreVersion==1) branch at line 1828. The auth codec *classes* are still used by the Netty handlers, but instantiated directly, not via `CodecSource`. Implement the Netty behaviour.

---

# 1. Inputs and auth modes

## 1.1 `<input>` — `$R/src/takserver-common/src/main/xsd/CoreConfig.xsd:1472-1513`

| attribute | type | default | notes |
|---|---|---|---|
| `_name` | string | **required** | unique input name |
| `protocol` | **string, NOT an enum** | **required** | see 1.2 |
| `port` | int | **required** | |
| `auth` | `authType` | **`x509`** | enum: `ldap`, `file`, `anonymous`, `x509` (xsd:263-270) |
| `authRequired` | bool | `false` | only consulted in `X509Authenticator.auth()`'s `IllegalStateException` fallback (`X509Authenticator.java:400`) — suppresses the "fall back to anon group" behaviour |
| `group` | string | — | multicast group address |
| `iface` | string | — | multicast interface |
| `archive` | bool | `true` | false ⇒ `onNoArchiveDataReceivedCallback` added |
| `anongroup` | bool | *absent* | tri-state; see 1.3 |
| `archiveOnly` | bool | `false` | adds `onArchiveOnlyDataReceivedCallback` |
| `federateOnly` | bool | `false` | sets `Constants.FEDERATE_ONLY_KEY` ⇒ only `FederateSubscription` receives it |
| `coreVersion` | int | **`2`** | only gates the UDP/mcast path (Netty vs legacy NIO) |
| `syncCacheRetentionSeconds` | int | `3600` | |
| `maxMessageReadSizeBytes` | int | **`2048`** | **this is a socket read-chunk size, not a message limit** — `SO_RCVBUF` + `FixedRecvByteBufAllocator` (`NioNettyBuilder.java:241-242`, `:120-121`) |
| `coreVersion2TlsVersions` | string | `TLSv1.2,TLSv1.3` | passed verbatim to `sslHandler.engine().setEnabledProtocols(split(","))` (`NioNettyInitializer.java:230-231`) |
| `federated` | bool | `true` | |
| `binaryPayloadWebsocketOnly` | bool | `false` | |
| `quicConnectionTimeoutSeconds` | long | `30` | |
| `takServerHost` | string | — | used for GeoChat mission-chat fixup (§8) |
| `<filtergroup>` | string* | — | static group names (child elements) |
| `<filter>` | element | — | per-input; only `geospatialFilter` is honoured, everything else logs "Invalid filter assigned for Input" (`SubmissionService.java:1432-1447`) |

**`<datafeed>` extends `<input>`** with child elements `uuid`, `type`, `tag*`, `sync` (xsd:1516-1530).

## 1.2 `protocol` values — `$R/.../com/bbn/marti/service/TransportCotEvent.java:40-66`

The enum constant name and the config string differ. The config strings are:

| config string | enum | handler |
|---|---|---|
| `tcp` | `TCP` | `NioNettyTcpServerHandler` — **receive-only**, `writer = (data) -> {}` |
| `stcp` | `STCP` | `NioNettyStcpServerHandler` — streaming XML both ways, **no protobuf negotiation ever** |
| `tls` | `COTPROTOTLS` | `NioNettyTlsServerHandler` — **XML, with `t-x-takp-v` negotiation** ← what ATAK/CloudTAK use |
| `prototls` | `PROTOTLS` | `NioNettyTlsServerHandler` with `protobufSupported = true` from t=0, **no negotiation** |
| `cottls` | `TLS` | `NioNettyTlsServerHandler`, **no negotiation, XML only** |
| `ssl` | `SSL` | same as `cottls` |
| `udp` | `UDP` | single-datagram, protobuf-or-CoT auto-detect |
| `mcast` | `COTPROTOMUDP` | multicast, protobuf-or-CoT auto-detect |
| `cotmcast` | `MUDP` | multicast, XML only |
| `grpc` | *(string-compared, not in enum)* | `GrpcStreamingServer` |
| `quic` | *(string-compared, not in enum)* | `QuicStreamingServer`, ALPN `takstream` (`NioNettyInitializer.java:355`) |

`isTls()` = {`ssl`,`cottls`,`prototls`,`tls`} (`TransportCotEvent.java:123-132`).
`isStreaming()` = `isTls()` ∪ {`stcp`} (`:134-146`).

## 1.3 What each `auth` means for a connecting client

Dispatch: `NioNettyTlsServerHandler.createAuthenticationCodecs()` (`:437-455`) and `NioNettyStcpServerHandler.createAuthenticationCodecs()` (`:135-149`). **STCP/TCP has no `X_509` branch** — on a non-TLS input `auth="x509"` results in *no auth codec at all*, hence no user, hence no subscription user, hence no message delivery.

- **`x509`** (TLS only). `X509AuthCodec.onConnect()` → `doTlsAuth()` runs **immediately after handshake**, synchronously, before the subscription is created. Client sends no auth message. Re-auth every `auth/ldap/@updateinterval * 1000` ms, default **300000 ms** (`X509AuthCodec.java:181-199`).
- **`file`** (TLS and STCP). Client **must send `<auth>` as the first bytes** (see §2.3). `FileAuthCodec` → `FileAuthenticator`, which checks `UserAuthenticationFile.xml`. Periodic re-auth with a cancellation map.
- **`ldap`** (TLS and STCP). Same `<auth>` message, `LdapAuthCodec` → `LdapAuthenticator` bind.
- **`anonymous`** (TLS and STCP). `AnonymousAuthCodec.onConnect()` → `init()` assigns groups immediately and sets `AuthStatus.SUCCESS`; no client message. Group assignment (`AnonymousAuthCodec.java:191-237`):
  - `anonGroup` = `input.isAnongroup()` if set, else `!hasFiltergroups`.
  - if `anonGroup`: add `__ANON__` IN **and** OUT.
  - for each `<filtergroup>`: add that name IN **and** OUT.
  - Username on a **streaming** input is `"Anonymous_" + connectionId` (a distinct user per connection); on non-streaming inputs it's the single shared `Anonymous_auto` user (`:173-184`, `GroupFederationUtil.java:86-88,138-140`).

## 1.4 `<auth>` — `CoreConfig.xsd:279-353`

Attributes on `<auth>`:
`default` (string, **`"file"`**), `DNUsernameExtractorRegex` (`CN=(.*?)(?:,|$)`), `x509groups` (true), `x509groupsDefaultRDN` (false), `x509addAnonymous` (false), `x509useGroupCache` (false), `x509useGroupCacheDefaultActive` (false), `x509useGroupCacheDefaultUpdatesActive` (false), `x509useGroupCacheRequiresActiveGroup` (false), `x509useGroupCacheRequiresExtKeyUsage` (**true**), `x509checkRevocation` (false), `x509tokenAuth` (false), `x509assignAdminAllGroups` (**true**).

Children: `<ldap .../>` (huge attribute set incl. its **own** `x509groups`=true and `x509addAnonymous`=false — **both** the `<auth>` and `<ldap>` flags must be true, see `X509Authenticator.java:267` and `:351-352`), `<File location="UserAuthenticationFile.xml"/>`, `<oauth>`.

`<auth default>` is switched on in `X509Authenticator.auth()` (`:188`): only `"file"` and `"ldap"` are accepted; anything else throws `UnsupportedOperationException`.

## 1.5 `<connector>` — `CoreConfig.xsd:54-92`

`port` (int, required), `tls` (bool, **true**), `useFederationTruststore` (false), `clientAuth` (**string**, default `"true"`; mapped at `ServerConfiguration.java:767` — `"true"` → Tomcat `NEED`, otherwise passed through, so `"false"`/`"want"` work), `allowBasicAuth` (false), `crlFile`, `_name`, keystore/truststore overrides, `enableAdminUI` (true), `enableWebtak` (true), `enableNonAdminUI` (true), `allowOrigins` (""), `allowMethods`, `allowHeaders` (`Accept, Access-Control-Allow-Headers, Authorization, Content-Type, Cookie, Origin, missionauthorization, X-Requested-With`), `allowCredentials` (false), `maxHttpHeaderSize` (8192). Child `<header key= value=/>`.

Role gating by port: `AuthenticatorUtil.setUserRolesBasedOnRequestPort` (`$R/src/takserver-core/src/main/java/com/bbn/marti/groups/AuthenticatorUtil.java:30-90`) — a request on a port with **no matching `<connector>` throws `SecurityException("Connection is not allowed")`**; `ROLE_ADMIN` is stripped unless `enableAdminUI`; `ROLE_WEBTAK` added if `enableWebtak` and the user has webtak access; `ROLE_NON_ADMIN_UI` added if `enableNonAdminUI`; empty authorities ⇒ `ROLE_ANONYMOUS`.

## 1.6 `UserAuthenticationFile.xsd` — `$R/src/takserver-common/src/main/xsd/UserAuthenticationFile.xsd`

```
<UserAuthenticationFile>  (ns http://bbn.com/marti/xml/bindings)
  <User identifier="..."(req) fingerprint="..." password="..." passwordHashed="bool" role="ROLE_ANONYMOUS">
    <groupList>*  <groupListIN>*  <groupListOUT>*
```
Roles: `ROLE_NONEXISTENT, ROLE_ADMIN, ROLE_READONLY, ROLE_ANONYMOUS, ROLE_NON_ADMIN_UI, ROLE_WEBTAK`.
`FileAuthenticator.getGroups()` (`:287-310`): `groupList` ⇒ IN **and** OUT; `groupListIN` ⇒ IN only; `groupListOUT` ⇒ OUT only.

---

# 2. Netty pipelines and authentication

## 2.1 Netty channel pipelines — `$R/.../nio/netty/initializers/NioNettyInitializer.java`

```
tls  (:218-245):  "ssl"(SslHandler, ClientAuth.REQUIRE, OPENSSL) → ByteArrayDecoder → ByteArrayEncoder → NioNettyTlsServerHandler(input)
stcp (:195-216):                                                    ByteArrayDecoder → ByteArrayEncoder → NioNettyStcpServerHandler(input)
tcp  (:172-193):                                                    ByteArrayDecoder → ByteArrayEncoder → NioNettyTcpServerHandler(input)
```
There is **no length-framing codec in the Netty pipeline** — the handler gets raw `byte[]` chunks and does its own framing. TLS **always requires a client certificate** (`ClientAuth.REQUIRE`, `:288`), even for `auth="anonymous"` / `auth="file"`.

All three refuse connections while Ignite is not connected (`:176-180`, `:199-203`, `:223-227`).

## 2.2 TLS handler order of operations — `$R/.../nio/netty/handlers/NioNettyTlsServerHandler.java:103-139`

On `handshakeFuture` success, in this exact order:
1. `createConnectionInfo()` — `connectionId = igniteId + MessageConversionUtil.getConnectionId(channel)`, `tls=true`, `cert = chain[0]`
2. `createAdaptedNettyProtocol()` / `createAdaptedNettyHandler()`
3. **`createAuthenticationCodecs()`** — for `x509` this *is* the authentication (synchronous)
4. `setReader()`, `setWriter()`, `setNegotiator()`
5. `buildCallbacks()` — see 2.4
6. `setupFlushHandler()`
7. **`createSubscription()`** — which, if a user exists, sends latest-SA **then** starts negotiation

`NioNettyStcpServerHandler.channelActive` (`:40-55`) is the same but with `buildCallbacks()` **before** `createAuthenticationCodecs()`, and `connectionInfo.setTls(false)`.

## 2.3 The `<auth>` message (`auth="file"` / `auth="ldap"`)

`$R/.../nio/codec/impls/AbstractAuthCodec.java:136-259`, model at `$R/src/takserver-core/src/main/java/com/bbn/cot/model/AuthMessage.java` / `AuthCot.java`.

Exact shape (JAXB: root element `auth`, child **element** `cot`, all-attributes):

```xml
<auth><cot username="user" password="pass" uid="ANDROID-xxxx" callsign="ALPHA"/></auth>
```

- `callsign` is parsed but **not used** by `doAuth` (only `username`, `password`, `uid`).
- **Must be first.** `decode()` buffers into a **1024-byte** `authBuffer`. Rules:
  - overflow of 1024 bytes ⇒ `AuthStatus.EXCEPTION` + `forceClose()` (`:152-168`)
  - if `</auth>` (case-insensitive) is not found **and** the accumulated string does not contain the literal `<auth>` ⇒ `AuthStatus.EXCEPTION` + `forceClose()` (`:196-216`). So **the very first bytes must begin an `<auth>` element**.
  - everything after `</auth>` in the same read is captured as `remainder` and **then discarded** — `decode()` unconditionally returns `ByteUtils.getEmptyReadBuffer()` and advances the buffer past everything (`:249-253`). `postAuthMessage` is threaded into the callback but never replayed. **Do not pipeline a CoT event in the same TCP segment as the auth message.**
- On `AuthStatus.SUCCESS` (`:340-390`): if the user has **zero groups** ⇒ `forceClose()` + `AuthenticationFailedException`. Otherwise `authStatus=SUCCESS`, `setUserForSubscription`, optional `sendLatestReachableSA`, then `startProtocolNegotiation`.
- On `FAILURE` / anything else ⇒ `forceClose()` and `cleanup()`. **No response is ever written to the client on success or failure.** The only observable signal of failure is TCP close.
- Once `authStatus == SUCCESS`, `decode()` becomes a pass-through (`:141-145`).

`AnonymousAuthCodec.decode` is a pure pass-through (`:58-69`); `X509AuthCodec.decode`/`encode` call `doTlsAuth()` (rate-limited by `lastAuthTime`) and pass the buffer through unchanged.

## 2.4 `buildCallbacks()` — listener order — `$R/.../nio/netty/handlers/NioNettyHandlerBase.java:364-394`

```
1. negotiationListener          (only if connectionInfo.isTls() && !protobufSupported)
2. callsignExtractorCallback    (only if transport != TCP)
3. onArchiveOnlyDataReceivedCallback   (if input.archiveOnly)
4. onNoArchiveDataReceivedCallback     (if !input.archive)
5. onFederateOnlyDataReceivedCallback  (if input.federateOnly)
6. onDataReceivedCallback       (the real submitter)
```

## 2.5 X509 → user → groups — `$R/.../groups/X509Authenticator.java:124-421`

1. `certFingerprint = RemoteUtil.getCertSHA256Fingerprint(cert)` (lowercase hex SHA-256 of the DER cert).
2. If `x509checkRevocation` or `x509tokenAuth`: look up `tak_cert` by hash; a non-null `revocation_date` ⇒ `RevokedException`.
3. If `x509tokenAuth` and the cert row has a `token`: set token, `groupManager.authenticate("oauth", user)` and return.
4. **Username = CN from the Subject DN**, via `X509UsernameExtractor` → `CommonNameExtractor` with regex `auth/@DNUsernameExtractorRegex`, default `CN=(.*?)(?:,|$)` applied to `cert.getSubjectDN().getName()` (`X509UsernameExtractor.java:20-23`, `X509AuthCodec.java:47-48,234-241`). Empty CN ⇒ abort, no user. Expired cert (`getNotAfter().before(now)`) ⇒ `TakException` ⇒ `forceClose()`.
5. Group resolution, in order:
   - **File**: iterate **all** `UserAuthenticationFile` users; match if `fileUser.fingerprint == certFingerprint` **OR** `fileUser.identifier == username` (`:212-215`). Note: this is an OR, and it does **not** break — every matching row contributes. Role is added to authorities. If role is `ROLE_ADMIN` and `x509assignAdminAllGroups` ⇒ **all** IN+OUT groups on the server. Otherwise `FileAuthenticator.getGroups(fileUser)`.
   - **LDAP** (`:266-359`): only if `auth/@x509groups && auth/ldap/@x509groups`. CN is re-extracted and used as the search key; `input.getFiltergroup()` acts as a **substring whitelist** on returned LDAP group DNs (`nextGroup.contains(filterGroup)`, `:295-316`). If `ldap/@x509addAnonymous && auth/@x509addAnonymous` ⇒ also add `__ANON__`.
   - **`x509useGroupCache`**: enabled iff `auth/@x509useGroupCache` **and** (the cert has extended key usage OID `1.2.840.113549.1.9.7` **or** `x509useGroupCacheRequiresExtKeyUsage=false`) (`:198-206`). Groups then go through `ActiveGroupCacheHelper.assignGroupsCheckCache`, and a change fires `sendGroupsUpdatedMessage` (`t-x-g-c`, §7.4).
   - **Fallbacks** (only when *not* using the group cache, `:364-386`): if still no groups and `x509groupsDefaultRDN` ⇒ `doRDNAssignment` (concatenate all non-CN RDN **values** with `-` separators and trailing `-`, e.g. `US-MyOrg-`, as IN+OUT) (`:423-445`); if still none ⇒ `doAnonAssignment` (`__ANON__`).
6. `X509Authenticator.authenticate` **can never fail** — it always returns `AuthStatus.SUCCESS` (`:117-122`). Access control is entirely group-based downstream.

Group registration side-effect: `updateGroups` → `addUserToGroup` → `addUser` → `groupStore().getConnectionIdUserMap().putIfAbsent(connectionId, user)` (`DistributedPersistentGroupManager.java:149-177`, `:254-270`, `:540-565`). This is why `createSubscriptionFromConnection` finds the user immediately for x509 but not for file/ldap.

---

# 3. Protocol negotiation

## 3.1 When the announcement is sent

`negotiate()` is only ever invoked from `DistributedSubscriptionManager.startProtocolNegotiation` (`:2479-2485`), which is called from exactly two places:
- `SubmissionService.createSubscriptionFromConnection:1149` — right after handshake, **only if a user was already registered by connectionId** (i.e. `auth="x509"` or `auth="anonymous"`);
- `AbstractAuthCodec$CodecAuthCallback.authenticationReturned:383` — **after successful `<auth>`** (`auth="file"`/`"ldap"`).

And the negotiator itself short-circuits (`NioNettyTlsServerHandler.java:286-302`):

```java
negotiator = () -> {
    if (transport == TransportCotEvent.COTPROTOTLS) {
        if (authCodec != null && authCodec.getAuthStatus().get() != AuthStatus.SUCCESS) return;
        CotEventContainer announcement = StreamingProtoBufOrCoTProtocol
                .buildProtocolAnnouncement(negotiationUuid = UUID.randomUUID().toString());
        nettyContext.writeAndFlush(announcement.getOrInstantiateEncoding());
    }
};
```

**Therefore: the announcement is sent only on `protocol="tls"`, only after auth succeeds, and it is preceded on the wire by any latest-SA / data-feed replay messages** (because `sendLatestReachableSA` runs before `startProtocolNegotiation` at `SubmissionService.java:1141-1149`). Those replayed messages are XML.

`protocol="stcp"` / `"tcp"`: `negotiator = () -> {}` (`NioNettyStcpServerHandler.java:131-133`, `NioNettyTcpServerHandler.java:38-41`). `protocol="prototls"`: `protobufSupported=true` in the constructor, no announcement (`NioNettyTlsServerHandler.java:89-92`). QUIC: announcement always sent, no `COTPROTOTLS` check (`NioNettyQuicServerHandler.java:79-93`).

## 3.2 Announcement template — `$R/.../nio/protocol/connections/StreamingProtoBufOrCoTProtocol.java:169-194`

Source string (TIMEOUT_MILLIS = 60000, so `stale = time + 60s`):

```
<?xml version='1.0' encoding='UTF-8' standalone='yes'?>
<event version='2.0' uid='{uuid}' type='t-x-takp-v' time='{t}' start='{t}' stale='{t+60s}' how='m-g'>
<point lat='0.0' lon='0.0' hae='0.0' ce='999999' le='999999'/>
<detail><TakControl><TakProtocolSupport version='1'/>
<TakServerVersionInfo serverVersion='{ver}' apiVersion='3'/>
</TakControl></detail>
</event>
```

`apiVersion` is `Constants.API_VERSION = "3"` (`$R/src/takserver-plugins/src/main/java/tak/server/Constants.java:45`). `serverVersion` comes from the version bean.

**Important:** this string is parsed by `SAXReader` into a dom4j `Document` and then re-serialised by `getOrInstantiateEncoding()` → `asXml()` → `doc.asXML()`. So the **actual bytes on the wire use dom4j's output format**, not the literal above: `<?xml version="1.0" encoding="UTF-8"?>` + `\n`, **double-quoted** attributes, **no `standalone`**, self-closing empty elements, no indentation, **no trailing newline**. *(This last part is dom4j `OutputFormat`/`XMLWriter` default behaviour — `newLineAfterDeclaration=true`, `lineSeparator="\n"`, `suppressDeclaration=false` — not visible in this checkout; verify against a live server if you want byte-exactness.)*

Parse it, don't string-match it.

## 3.3 Request detection

`NioNettyHandlerBase.negotiationListenerCallback` (`:409-422`) matches on `data.getType().compareTo("t-x-takp-q") == 0`, then `processProtocolRequest` (`:424-443`):

```java
String versionRequested = protocolRequest.getDocument()
        .selectSingleNode("/event/detail/TakControl/TakRequest/@version").getText();
boolean supported = versionRequested.compareTo("1") == 0;
((AbstractBroadcastingProtocol) protocol).removeProtocolListener(negotiationListener);
CotEventContainer response = StreamingProtoBufOrCoTProtocol.buildProtocolResponse(supported, negotiationUuid);
nettyContext.writeAndFlush(response.getOrInstantiateEncoding());
protobufSupported.set(supported);
```

- Any `uid`/`time` on the request is ignored; only the XPath `/event/detail/TakControl/TakRequest/@version` matters, and only the literal string `"1"` is accepted.
- Missing that node ⇒ exception, logged, **no response, no switch** — client hangs.
- `t-x-takp-q` is also in `controlMsgTypes`, so `processControlMessage` sees it and does nothing (`SubmissionService.java:2106-2108`) — it is **not** relayed to other clients.

## 3.4 Response template — `StreamingProtoBufOrCoTProtocol.java:241-262`

```
<?xml version='1.0' encoding='UTF-8' standalone='yes'?>
<event version='2.0' uid='{same negotiationUuid as the announcement}' type='t-x-takp-r' time='{t}' start='{t}' stale='{t+60s}' how='m-g'>
<point lat='0.0' lon='0.0' hae='0.0' ce='999999' le='999999'/>
<detail><TakControl><TakResponse status='true'/></TakControl></detail>
</event>
```

`status` is `Boolean.toString(supported)` — literally `true` or `false`. Same dom4j re-serialisation caveat. The reference client checks `status == "true"` (`create_cot.py:180-186`).

## 3.5 Switchover timing

`nettyContext.writeAndFlush(responseXml)` happens **before** `protobufSupported.set(true)` (lines 437-438). Both run on `Resources.negotiationProcessor`, while reads run on a separate `readParseProcessor` thread — so there is a genuine (tiny) race. In practice clients wait for `t-x-takp-r` before sending their first framed message.

After the flag flips:
- **Reader**: every subsequent inbound byte is parsed as `0xbf`-framed protobuf (`setReader`, `:211-215`).
- **Writer**: every outbound message is protobuf (`setWriter`, `:230-243`) — it reuses `data.getProtoBufBytes()` if `BrokerService` already pre-converted it, else `StreamingProtoBufProtocol.convertCotToProtoBufBytes(data)`.

**The server never sends XML after the switch.** The `t-x-takp-r` is the last XML frame.

## 3.6 Streaming frame decoder

`NioNettyTlsServerHandler.convertAndSubmitProtoBufBytesAsCot` (`:340-435`):

```
frame := 0xBF <varint32 payloadLen> <payloadLen bytes of TakMessage>
```

- `MAGIC = (byte) 0xbf` (`:64`; also `StreamingProtoBufHelper.MAGIC`).
- Varint decode at `:423-435` — standard LEB128, **no length cap, no varint-length cap** (`StreamingProtoBufProtocol.java:77` has a `TODO check for varint max size`).
- **No resync.** A wrong magic byte does `log.error(...); break;` — the loop exits and `leftovers` is left as-is/null, so the connection silently stops making progress. Any exception ⇒ `channelHandler.forceClose()`.
- Partial frames go into `leftovers` and are concatenated on the next read.
- **Outbound only** has a size limit: `StreamingProtoBufProtocol.MAX_SIZE = 65536` (`:35`). If a serialised `TakMessage` exceeds 64 KiB the server **replaces the message with a `b-f-t-r` file-transfer request** pointing at `https://{network/@takServerHost}:{connector[0]/@port}/Marti/api/cot/xml/{uid}` (`StreamingProtoBufProtocol.java:158-196, 254-296`). Inbound has no size check.
- **Mesh/UDP framing is different**: `0xBF <varint version> 0xBF <TakMessage>` (`SingleProtobufOrCotProtocol.java:112-136`). Don't confuse the two.

## 3.7 Client that never negotiates

The negotiation listener is removed after 60 s in the legacy path (`StreamingProtoBufOrCoTProtocol.java:264-274`), but the **Netty** path (`NioNettyHandlerBase`) has **no timeout** — the listener stays forever and `protobufSupported` stays `false`. The connection keeps working as plain streaming XML indefinitely. That is the correct fallback to implement.

## 3.8 `coreVersion="2"`

In this revision it *only* selects Netty vs legacy-NIO for UDP/multicast inputs (`SubmissionService.java:1820-1836`). It does **not** mean "protobuf" and does **not** affect TCP/TLS. `coreVersion2TlsVersions` is consulted for all TLS/gRPC/QUIC inputs regardless of `coreVersion`.

---

# 4. XML framing

`$R/.../nio/protocol/connections/StreamingCotProtocol.java:211-276` (`add()` — used by both the TLS and STCP Netty handlers).

- **Token scan on `</event>`**, not SAX streaming:
  - `START_OF_COT_MSG_STR = "<event"`, `END_OF_COT_MSG_STR = "</event>"` (`:56-57`).
  - Scan resumes from `max(0, bufferLen - 8)` so a `</event>` split across reads is caught (`:221`).
  - For each `</event>` found: `openIndex = indexOf("<event", 0)`, `closeIndex = indexOfEnd + 8`, substring, parse with `CotParser`, then `delete(0, closeIndex)`.
  - **Anything before the first `<event` is silently skipped** — this is explicitly how stray `<auth>` bytes on anonymous ports are tolerated (comment at `:227`).
  - Edge case: if the buffer contains `</event>` but no `<event`, `substring(-1, closeIndex)` throws out of `add()` (line 232 is outside the try).
- **Size limits**:
  - `INDIVIDUAL_COT_MSG_SIZE_LIMIT = 8388608` (8 MiB): a single message longer than this is discarded (but still consumed); a buffer that exceeds it with no complete message is cleared (`:234-268`).
  - `BUFFER_TRIM_SIZE = 1000000`: `trimToSize()` when capacity exceeds 1 MB.
  - There is **no** `maxMessageSize` config knob. `maxMessageReadSizeBytes` is a read-buffer size, not a framing limit.
- **XML declaration**: not special-cased. A leading `<?xml ...?>` simply falls into the "before the first `<event`" region and is discarded by the `openIndex` logic. Parsing is done by `CotParser` (`$R/src/takserver-plugins/src/main/java/tak/server/cot/CotParser.java`), a dom4j `SAXReader` with DOCTYPE/external entities disabled and `FEATURE_SECURE_PROCESSING` on. Validation against `Event_modified.xsd` only if `submission/@validateXml` and the file is readable.
- **Outgoing serialisation**: `CotEventContainer.getOrInstantiateEncoding()` → `asXml().getBytes()` → `doc.asXML()` (`$R/src/takserver-plugins/src/main/java/tak/server/cot/CotEventContainer.java:343-349`, `XmlContainer.java:34-36`). So: `<?xml version="1.0" encoding="UTF-8"?>` + `\n` + `<event …>…</event>`, **no trailing newline, no whitespace between messages** — messages are laid head-to-tail. As a reader you must not assume a delimiter other than `</event>`.
- **Server-generated timestamps are second-resolution**: `DateUtil.toCotTime` uses `yyyy-MM-dd'T'HH:mm:ss'Z'` (`$R/src/takserver-common/src/main/java/com/bbn/marti/remote/util/DateUtil.java:15-20, 51-53`). Parsing accepts both (`.` present ⇒ ISO8601 with millis). **`proto2cot` therefore loses sub-second precision** (`StreamingProtoBufHelper.java:453-455`).

---

# 5. Control messages

`$R/.../com/bbn/marti/service/SubmissionService.java`

## 5.1 The set — `:256-269`

```java
"t-b", "t-b-a", "t-b-c", "t-b-q",
"t-x-c-f", "t-x-c-t", "t-x-c-t-r",
"t-x-takp-q", "t-x-c-m", "t-x-c-i-e", "t-x-c-i-d"
```
`isControlMessage()` lowercases the incoming type before the set lookup (`:2077-2086`), but `processControlMessage` switches on the **original-case** `c.getType()` (`:2093`). So `T-X-C-T` is classified as control (and thus consumed, never relayed) but falls into `default:`.

## 5.2 Preconditions before a control message is even seen

`processNextEvent` (`:1962-2018`) runs, in order, **before** control dispatch:
1. `if (c.getLat() == null || c.getLat().isEmpty()) return;` — **a control message with no `<point lat=…>` is silently dropped.** Your ping must carry a point.
2. server-level `geospatialEventFilter`, `StrictUidMissionMemebershipFilter`, `DisableBroadcastMapItemFilter`, `dropFilters`.
3. `isControlMessage` ⇒ `processControlMessage(c); return;` — **control messages are consumed and never brokered**, and the flow-tag is never applied to them.

## 5.3 Dispatch — `:2088-2121`

| type | action |
|---|---|
| `t-b` | `processSubscriptionMessage` — reads `/event/detail/subscription/tests/@xpath` and `/event/detail/subscription/@publish` as `proto:host:port`. For `stcp` it **sets `subscription.xpath` on the caller's own connection**; for anything else it opens an outbound client connection. Stub-able, but note it is an unauthenticated XPath-filter setter and outbound-connection primitive. |
| `t-x-c-f` | `processFilterMessage` — parses `/event/detail/subscription/geospatialFilter` with `@filterTAKClients` and N `<boundingBox minLongitude minLatitude maxLongitude maxLatitude/>`; sets `subscription.geospatialEventFilter` |
| `t-b-q` | logged and ignored |
| `t-x-c-t` | `sendPong(c)` — see 5.4 |
| `t-x-takp-q` | explicit no-op (handled by the codec) |
| `t-x-c-m` | `processMetricsMessage` — `/event/detail/stats/@{app_framerate, battery, battery_status, battery_temp, deviceDataRx, deviceDataTx, heap_current_size, heap_free_size, heap_max_size, ip_address, storage_available, storage_total}`; **every one is read with `.getValue()` without a null check**, so a missing attribute aborts the whole method |
| `t-x-c-i-e` | `setIncognito(c, true)` |
| `t-x-c-i-d` | `setIncognito(c, false)` |
| **`default:`** | **`subMgr.deleteSubscription(c.getUid())`** |

The `default:` branch is reached by `t-b-a`, `t-b-c`, `t-x-c-t-r`, and any case-variant. `deleteSubscription(uid)` here takes the **subscription uid** (e.g. `tls:37`), so a client sending `t-x-c-t-r` with a random uid is harmless in practice — but a client that guesses/knows a subscription uid can delete someone else's subscription. Implement `default:` as a no-op.

## 5.4 `sendPong` — `:2274-2301`

```java
"<event version='2.0' uid='takPong' type='t-x-c-t-r' how='h-g-i-g-o' time='" + now + "' start='" + now +
"' stale='" + (now + 20_000) + "'> <point ce='9999999' le='9999999' hae='0' lat='0' lon='0' /></event>"
```

- uid is the literal string **`takPong`**; `type='t-x-c-t-r'`; `how='h-g-i-g-o'`; stale = now + 20 s; **no `<detail>` element at all**.
- `dest.lastPingTime` is updated.
- Delivered via `dest.submit(pong)` — i.e. **directly to the pinging subscription only**, bypassing the broker (so no flow-tag, no group check). If the subscription has negotiated protobuf, the pong is emitted as protobuf with an absent `detail` field.
- Again re-serialised by dom4j, so the literal single quotes and the stray space after `'>` do not survive.

## 5.5 Incognito — `:2123-2131` + `:1376-1380`

`subscription.incognito = true/false`. Effect (in `onDataReceivedCallback`):
```java
if (sub.incognito && !isControlMessage(data.getType())
    && data.getDocument().selectNodes("/event/detail/marti/dest[@callsign]").size() == 0) return;
```
i.e. an incognito client's messages are dropped at ingest unless they are control messages or carry at least one `<dest callsign=…>`. Also, `sendLatestReachableSA` skips incognito subscriptions (`MessagingUtilImpl.java:203-205`).

## 5.6 `t-x-d-d`

**Not** a control type. Client-originated `t-x-d-d` is brokered like any normal CoT. The server *generates* it on disconnect (§6.5) and federate subscriptions special-case it (`FederateSubscription.java:95-104`).

## 5.7 `<contact>` / `<takv>` / `<__group>` → subscription

Two paths, both driven by the SA message:

**a) `callsignExtractorCallback`** (`SubmissionService.java:1666-1737`) — fires on the **first** message from the connection that has a `<contact endpoint>`, then **removes itself** (`protocol.removeProtocolListener(this)`). It calls:
```java
subMgr.setClientForSubscription(data.getUid(), callsign, handler, /*overwriteSub=*/true);
```
which sets `sub.clientUid = event/@uid`, `sub.callsign = detail/contact/@callsign`, and indexes the subscription by both (`DistributedSubscriptionManager.java:1563-1604`). Then `trackLatestSA(sub, data, force=true)`, optional store-and-forward chat replay, and a `CONNECTED` audit row.

**b) `Subscription.setLatestSA`** (`$R/.../service/Subscription.java:69-94`) — every time the latest SA is stored:
```java
this.team  = /event/detail/__group/@name
this.role  = /event/detail/__group/@role
this.takv  = /event/detail/takv/@platform + ":" + /event/detail/takv/@version
```
On **any** parse failure all three become the literal string `"unknown"`. Changes re-publish the `RemoteSubscription` to the Ignite cache.

**`latestSA` caching** — `GroupFederationUtil.trackLatestSA` (`:175-196`): stores `cot.copy()` on the subscription when `cot.getUid().equals(subscription.clientUid)` (or `force`). `isSituationalAwarenessMessage()` = non-empty callsign **and** non-empty `contact/@endpoint` **and** non-empty uid (`CotEventContainer.java:554-557`). Data-feed subscriptions never store latest SA (`Subscription.java:71`).

Gated by `buffer/latestSA/@enable` (default **false** in the XSD but **true** in `CoreConfig.example.xml:110`). `buffer/latestSA/@validateClientUid` enables `clientUidMatchesCert` (`SubmissionService.java:1227-1273`) which, for SA messages from a cert that has a `tak_cert` enrollment row, **kills all subscriptions using that certificate** if `event/@uid != cert.clientUid`.

## 5.8 Flow tags — `$R/src/takserver-core/src/main/java/com/bbn/cot/filter/FlowTagFilter.java`

- Tag name: `"TAK-Server-" + serverInfo.getServerId()` (`:28`, `:126-128`).
- Insertion (`:40-58`): requires an existing `<detail>`; creates/reuses `/event/detail/_flow-tags_` and adds an **attribute** named after the flow tag whose value is `DateUtil.toCotTime(now)`:
  ```xml
  <_flow-tags_ TAK-Server-myserver="2026-09-17T12:00:00Z"/>
  ```
- Loop detection (`SubmissionService.java:2026-2031`): before tagging, `c.matchXPath("/event/detail/_flow-tags_[@TAK-Server-" + serverId + "]")` ⇒ **drop**, logged as "Duplicate message - already processed by this takserver". So the server ID must be a valid XML NCName.
- `unfilter(c)` removes only this server's attribute; `unfilterAll` removes every attribute under `_flow-tags_`. Used before re-injecting stored messages (store-and-forward, delivery-failure notifications, group-update SA re-send).
- Empty `serverId` ⇒ tagging is skipped entirely with a one-time error.
- Ordering: `processContactMessage` runs *after* flow-tagging, and only then is the message copied to each consumer service (`:2038-2069`).

## 5.9 `<__serverdestination>`

**Not found as a writer anywhere in this checkout.** It appears only in comments documenting ATAK-generated chat messages (`RepositoryService.java:536`, `:564`). The server never produces it, and the parsing code in `RepositoryService` ignores it. Treat it as client-generated, pass-through.

---

# 6. Routing

## 6.1 `<marti><dest>` — `$R/src/takserver-core/src/main/java/com/bbn/cot/filter/StreamingEndpointRewriteFilter.java:68-78`

Selection XPath:
```
/event/detail/marti/dest[@callsign or @publish or @uid or @mission or @path or @after or @mission-guid or @group]
```

Each `<dest>` is `detach()`ed and matched against attributes in **strict if/else-if order** (`:175-232`) — only the **first** matching attribute on a given `<dest>` is honoured:

1. `callsign` → `EXPLICIT_CALLSIGN_KEY` list. `<dest callsign="X"/>` resolves via the callsign index (`getExplicitCallsignMatches`, `:654-686`). **If any callsign in the list is the literal `"All Streaming"`, the whole callsign list is discarded** (`:243`) — i.e. it degrades to implicit (group-wide) broadcast.
2. `publish` → `EXPLICIT_PUBLISH_KEY`. **Not implemented** — `getExplicitPubMatches` logs a warning and returns empty (`DistributedSubscriptionManager.java:640-648`). It still makes `doExplicitBrokering` true, so a message with only `<dest publish=…>` goes nowhere.
3. `uid` → `EXPLICIT_UID_KEY` set → `getSubscriptionByClientUid` (`:520-540`).
4. `mission` (name) → plus optional `path`, and `after` **only if `path` is also present** (`:185-190`).
5. `mission-guid` (UUID string; unparseable ⇒ warning, dest ignored) → plus optional `path`/`after`.
6. `group` → `groupManager.hydrateGroup(new Group(name, Direction.IN))`; **if the sender's `GROUPS_KEY` set does not contain it, throws `ForbiddenException`**; otherwise the message's group set is *replaced* by the dest groups (`:221-237`).

`path` / `after` are used for layer placement: `MissionContent.getOrCreatePaths().put(path, [content])` and `content.setAfter(after)` (`:444-454`, `:659-669`).

## 6.2 Is `<marti>` stripped?

**Yes, unconditionally**, at the end of `filter()` (`:297-301`):
```java
Element martiElem = (Element) cot.getDocument().selectSingleNode("/event/detail/marti");
if (martiElem != null) martiElem.detach();
```
The individual `<dest>` elements were already detached in the loop; this removes the now-empty (or non-matching-attribute) `<marti>` too. The GeoChat mission-chat branch does the same on its copies (`:326-330`). So **receivers never see `<marti>`**.

Caveat: `filter()` only enters the dest-processing block at all if `filter/streamingbroker/@enable` is true (`:116`; default `false` in the XSD, `true` in the example config). The `<marti>` strip at `:298` happens either way.

## 6.3 Reachability — the exact rule

`$R/src/takserver-core/src/main/java/com/bbn/marti/groups/CommonGroupDirectedReachability.java:102-150`

```java
for (Group inGroup : groups) {                       // "groups" = the SENDER's groups, IN-filtered upstream
    if (inGroup.getDirection().equals(Direction.IN)) {
        Group outGroup = groupManager.getGroup(inGroup.getName(), Direction.OUT);
        if (outGroup == null) continue;
        if (outGroup.getNeighbors().contains(dest)) return true;
    }
}
return false;
```

**Semantics (from the user's perspective):**
- `Direction.IN` on group *G* = the user may **publish into** *G* (traffic flows **in** to the server on *G*).
- `Direction.OUT` on group *G* = the user may **receive from** *G* (traffic flows **out** of the server to the user on *G*).
- **Delivery is allowed iff ∃G : sender has IN on G **and** receiver has OUT on G.** It is *not* symmetric and it is *not* "intersect the group sets".

This is corroborated by the repeated comment at the call sites, e.g. `SubmissionService.java:1553-1554`:
> `// Only put IN groups in the message - out groups do not matter here`
> `data.setContext(Constants.GROUPS_KEY, gfu.filterGroupDirection(Direction.IN, groups));`

(also `MessagingUtilImpl.java:288-289, 432-433`).

The inverse, `getAllReachableFrom(src)` (`:152-216`), walks the **src's OUT** groups and collects members of the matching **IN** groups — i.e. "everyone whose traffic `src` is entitled to hear". That is what latest-SA replay uses.

`Direction` is a 2-value enum with `IN(1)`, `OUT(2)` (`$R/src/takserver-common/src/main/java/com/bbn/marti/remote/groups/Direction.java`).

**Bit vectors** (`$R/src/takserver-common/src/main/java/com/bbn/marti/remote/util/RemoteUtil.java:104-190`): a fixed-length boolean array indexed by `Group.bitpos`, rendered as a **big-endian bit string** (`bitVectorToString` sets `bits[i-1]` from `groupsBitVector[len-i]`; `getGroupsForBitVectorString` reverses the string and indexes by `bitpos`). `getBitVectorForGroups(groups)` with no direction ORs IN|OUT. This is the DB-level ACL (`groups` column); the in-memory streaming path uses the graph walk above, not the bit vector.

## 6.4 Match selection — `DistributedSubscriptionManager.getMatches` `:416-518`

```
doExplicitBrokering(c) ?  explicit  :  implicit
```
`doExplicitBrokering` (`:561-586`) is true if any of `EXPLICIT_CALLSIGN_KEY`, `EXPLICIT_PUBLISH_KEY`, `EXPLICIT_UID_KEY`, `EXPLICIT_MISSION_KEY`, `EXPLICIT_MISSION_KEY_GUID` is non-empty, or `EXPLICIT_FEED_UID_KEY` is merely non-null.

**Explicit path** (`:424-512`): collect raw matches by publish/callsign/uid/feed-uid, plus **all** `FigFederateSubscription`s if any mission key is set. Then for each candidate:
- skip null handler / null user
- optional classification filter
- `reachability().isReachable(srcGroups, receiver)` using the message's `GROUPS_KEY`
- **bug worth reproducing-or-not**: the fallback at `:484-492` tests `if (reachableMatches.isEmpty())` — i.e. once *any* candidate has matched, the `isReachable(sender, receiver)` fallback is never evaluated for later candidates. The fallback `isReachable(sender, receiver)` also uses the *sender's* IN groups vs receiver OUT.
- **No self-echo suppression on the explicit path.** A client can address itself by uid or callsign.
- If the result is empty **and** `type.startsWith("b-t-f")` **and** `type != "b-t-f-s"` ⇒ `sendDeliveryFailure(senderClientUid, c)` (§8).

**Implicit path** (`:692-879`): iterate **all** subscriptions.
- source groups = `GROUPS_KEY` if present and non-empty, else `groupManager.getGroups(sender)`; if still empty ⇒ **drop the message entirely**.
- skip null handler / null user
- `REPEATER_KEY` messages are not federated to federates with `shareAlerts == false`
- `FEDERATE_ONLY_KEY` ⇒ only `FederateSubscription`s
- `isReachable(srcGroups, receiver)` (the IN/OUT rule)
- `xpath` match, `geospatialEventFilter`, `dropFilters`
- **self-echo suppression** (`:838-842`): `destSubscription.getHandler().identityHash() != cot.getContext(Constants.SOURCE_HASH_KEY)`. `identityHash()` is `String.valueOf(System.identityHashCode(handler))` (`AbstractBroadcastingChannelHandler.java:313-315`), stamped onto the message at ingest (`SubmissionService.java:1342`). It is **per-connection**, not per-uid — the same client on two connections will echo to itself.

`BrokerService` then converts once to protobuf for all recipients (`BrokerService.java:170`) and fans out via `subscription.submit(cot, hitTime)` (`:196-209`).

## 6.5 What a newly connected client receives

Order on the wire, for a `protocol="tls"` + `auth="x509"` input:

1. **Data-feed replay** — `sendLatestFeedEventsToSub` (`MessagingUtilImpl.java:295-318`): skipped if `vbm/@enabled`; uses the subscriber's **OUT** groups → group vector → `DataFeedService.getDataFeedsByGroup` → for each feed with `sync=true`, replay `DistributedDataFeedCotService.getCachedDataFeedEvents(uuid)` straight to `sub.submit(event)`.
2. **Latest SA** — `sendLatestReachableSA(user)` (`:128-259`), only if `buffer/latestSA/@enable`:
   - `reachableUsers = CommonGroupDirectedReachability.getAllReachableFrom(destUser)` (destUser's OUT ∩ others' IN)
   - plus federated group-mapping subscriptions
   - skip `incognito` subs
   - for each, `sendLatestSA(sub.getLatestSA(), destSubscription)` — which **copies** the CoT, sets `SUBSCRIBER_HITS_KEY` to just this one connection, refreshes `submissionTime`/`creationTime`, and calls `broker.processMessage(sa)` directly (bypassing the filter chain and `getMatches`).
   - federate subs are handled separately via `getLatestSAForHandler` with a group intersection check.
3. **`t-x-takp-v`** announcement (§3.1) — so steps 1 and 2 are **always XML**.
4. …negotiation…, then normal traffic.

**No mission notifications are sent on connect.** The mission↔uid map (`SubscriptionStore.missionUidMap` / `uidMissionMap`, `:693-705`) is populated only by the REST `PUT /Marti/api/missions/{name}/subscription` path (`MissionServiceDefaultImpl.java:1631-1637` → `DistributedSubscriptionManager.missionSubscribe`). It is an in-memory multimap that is **not** cleared on disconnect and **not** rehydrated from the DB on connect. ATAK re-subscribes over REST after reconnecting.

## 6.6 Disconnect

`SubmissionService.handleChannelDisconnect(handler)` (`:1174-1225`), called from `channelUnregistered` and from `onInboundClose`/`onOutboundClose`:

1. Only if **both** `subscription.callsign` and `subscription.clientUid` are non-empty:
   - if `latestSA` enabled: `messagingUtil.sendDisconnect(subscription.getLatestSA(), subscription)`
   - `auditCallsignUIDEventAsync(..., DISCONNECTED, groupVector)`
2. `federationManager.removeLocalContact(clientUid)`
3. async `subMgr.removeSubscription(handler)`

`MessagingUtilImpl.sendDisconnect` (`:421-445`):
- recipients = `groupFederationUtil.getReachableSubscriptions(subscription)` = `getAllReachableFrom(user)` = everyone the disconnecting user could hear. (Note: this is the *inverse* direction from delivery, so it's a superset/subset mismatch in asymmetric setups.)
- message = `makeDeleteMessage(lastSA.getUid(), lastSA.getType())` from the seed at `DistributedSubscriptionManager.java:1722`:

```xml
<event how='h-g-i-g-o' type='t-x-d-d' version='2.0' uid='{random}' start='{t}' time='{t}' stale='{t+20s}'>
  <point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/>
  <detail><link relation='p-p' uid='{clientUid}' type='{last SA event type}'/></detail>
</event>
```
(`makeDeleteMessage`, `:1730-1751`: adds `uid`/`start`/`time`/`stale` to `<event>` and `uid`/`type` to the pre-existing `<link relation='p-p'>`.)
- `SUBSCRIBER_HITS_KEY` pre-set to the reachable set, `USER_KEY` = the departing user, `GROUPS_KEY` = that user's **IN** groups, `SOURCE_TRANSPORT_KEY` = the dying handler (which makes self-echo suppression drop it for the departing connection). Sent via `cotMessenger().send()`.

`sendReachableDisconnectMessage(username)` (`DistributedSubscriptionManager.java:2617-2637`) does the same for all of a username's users (used on account changes).

## 6.7 `<dest mission=…>` — publish CoT into a mission

`processTracksByMissionName` (`StreamingEndpointRewriteFilter.java:344-558`); the GUID twin is `processTracksByMissionGuid` (`:560-776`). Per mission:

1. **Permission check** (skipped for `FederateUser`): look up the `MissionSubscription` by (missionName, clientUid, username); if not found and the user has a cert, retry with the **CN** from the cert Subject DN (needed for `auth=ldap`/`auth=file` inputs). If still not found ⇒ `logger.error("unable to find mission subscription for client …")` and **skip this mission**. If the subscription's role lacks `MISSION_WRITE` ⇒ skip.
2. `missionUuid = missionService.getMissionGuidByNameCheckGroups(missionName, groupVector)` where `groupVector` is derived from the message's `GROUPS_KEY`.
3. **Recipients**: `subscriptionManager.getMissionSubscriptions(missionUuid, connectedOnly=true)` → `SubscriptionStore.getLocalUidsByMission` → every connected client uid subscribed to that mission, **minus the submitter** (`if (clientUid.equals(missionClientUid)) continue;`). These uids are added to the same `uids` set that feeds `EXPLICIT_UID_KEY`. So **mission CoT is relayed over the mission subscribers' normal streaming connections**, as an explicit-uid broker hit — still subject to the IN/OUT group check in `getMatches`.
4. **Persistence** (skipped if `NATS_MESSAGE_KEY` is set, i.e. cluster replica):
   ```java
   MissionContent mc = new MissionContent(); mc.getUids().add(cot.getUid());
   if (path present) { if (after present) mc.setAfter(after);
                       MissionContent p = new MissionContent();
                       p.getOrCreatePaths().put(path, List.of(mc)); mc = p; }
   missionService.addMissionContentAtTime(missionUuid, mc, clientUid, groupVector, new Date(), null);
   ```
   `addMissionContent*` in turn writes a `MissionChange(ADD_CONTENT)` row and fires `announceMissionChange` (§7), which is what actually notifies the subscribers of the *change* (separately from the raw CoT relay).
5. **Federation** if `federation/@enableFederation && @allowMissionFederation`, subject to `federateOnlyPublicMissions` (tool must be `"public"`, or `network/@missionCopTool` with `vbm` enabled).
6. If `cot.getType().startsWith("b-t-f")` ⇒ `fixupStreamingMissionChat` (§8) and **return early**, replacing the single message with one per-recipient copy.

**Group-side effects at ingest** (`SubmissionService.java:1508-1551`): if `<marti>` exists, `marti/@archive` overrides `ARCHIVE_EVENT_KEY`; if `dest[@mission]` exists then
- `MissionUseGroupsForContents` ⇒ the message's groups are **replaced** by the mission's groups (after checking the user's vector is allowed),
- `postMissionEventsAsPublic` (an `<ldap>` attribute) ⇒ `__ANON__` IN is added,
- `alwaysArchiveMissionCot` ⇒ force archive.

---

# 7. Mission notifications over the stream

All templates come from three seeds in `$R/.../service/DistributedSubscriptionManager.java:1720-1725`, cloned per message. XPaths: `detailXPath="/event/detail"`, `linkXPath=detail+"/link"`, `missionXPath=detail+"/mission"`, `missionContentXPath=detail+"/mission/MissionChanges/MissionChange/content"` (`:1710-1713`).

## 7.1 `createMissionMessage` — `:1798-1856`

Seed:
```xml
<?xml version='1.0' encoding='UTF-8' standalone='yes'?>
<event how='h-g-i-g-o' type='t-x-m-c' version='2.0'>
  <point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/>
  <detail><mission type="CHANGE" tool=""/></detail>
</event>
```
Then, in order:
- `<event>`: `uid` = fresh random UUID, `type` = **overwritten** with the real `cotType`, `start`=`time`=now, `stale`=now+**20 s**.
- `<mission>`: `name={missionName}`; `guid={uuid}` if non-null; `type={msgType}` (**overwrites the seed's `CHANGE`**); `authorUid`, `tool`, `uid`, `token` each added **only if non-null and non-empty**. Note the seed leaves `tool=""` present when `tool` is empty.
- if `changes != null`: parse the `changes` XML and append its **root element** under `<mission>` (`addMissionXml`, `:1776-1785`); then, if `xmlContentForNotification != null`, `DocumentHelper.makeElement(doc, "/event/detail/mission/MissionChanges/MissionChange/content")` creates the missing `<content>` node and appends that XML under it (`:1787-1796`).
- if `roleXml != null`: append its root (`<role …>`) under `<mission>`.

So `<point>` is always `lat=0 lon=0 hae=0 ce=9999999 le=9999999` and `stale` is **20 seconds**.

## 7.2 Type map

`createMissionChangeMessage` — `:1858-1874`, `msgType = "CHANGE"`:

| `ChangeType` | `event/@type` |
|---|---|
| `CONTENT` (and `default:`) | `t-x-m-c` |
| `LOG` | `t-x-m-c-l` |
| `KEYWORD` | `t-x-m-c-k` |
| `UID_KEYWORD` | `t-x-m-c-k-u` |
| `RESOURCE_KEYWORD` | `t-x-m-c-k-c` |
| `METADATA` | `t-x-m-c-m` |
| `EXTERNAL_DATA` | `t-x-m-c-e` |
| `MISSION_LAYER` | `t-x-m-c-h` |

Note `ChangeType` also has `DATA_FEED` and `MAP_LAYER` (`$R/src/takserver-common/src/main/java/com/bbn/marti/remote/SubscriptionManagerLite.java:21`), both of which fall through `default:` to **`t-x-m-c`**.

Other messages (`:1876-1890`):

| factory | `event/@type` | `mission/@type` | extra `<mission>` attrs |
|---|---|---|---|
| `createMissionCreateMessage` | `t-x-m-n` | `CREATE` | — |
| `createMissionDeleteMessage` | `t-x-m-d` | `DELETE` | — |
| `createMissionInviteMessage` | `t-x-m-i` | `INVITE` | `token=`, plus a `<role>` child |
| `createMissionRoleChangeMessage` | `t-x-m-r` | **`INVITE`** (yes, `INVITE`, not `ROLE`) | `<role>` child |

`t-x-m-r` is `mission/@type="INVITE"` — that is not a typo in my reading; line 1889 passes `"INVITE"`.

The `uid` parameter of `createMissionMessage` is never non-null from any of these five factories in this file, so `<mission uid=…>` does not appear in practice from the streaming path.

## 7.3 Who receives what

| message | recipients | code |
|---|---|---|
| `t-x-m-c*` (change) | every **locally connected** uid in `SubscriptionStore.getLocalUidsByMission(missionGuid)`, resolved by `getSubscriptionByClientUid(uid)`; uids beginning `topic:` are diverted into `Constants.TOPICS_KEY` instead. Then `sendToPlugins`. | `submitAnnounceMissionChangeCot(missionName, guid, msg)` `:2029-2049` and `(uid, msg)` `:1989-2026` |
| `t-x-m-n` / `t-x-m-d` / broadcast `t-x-m-c-k`/`-m` | **all** subscriptions except the creator (`creatorUid.equalsIgnoreCase(sub.clientUid)` ⇒ skip), gated by `missionGroupVector & (subGroupVector ∪ __ANON__ IN) != 0` — note `__ANON__` IN is force-added to every subscriber's group set so LDAP users can see public missions | `broadcastMissionAnnouncement` `:2052-2078`, `submitBroadcastMissionAnnouncementCot` `:2081-2140` |
| `t-x-m-i` | exactly the `uids[]` passed in, via `getSubscriptionByClientUid` | `submitSendMissionInviteCot` `:2157-2190` |
| `t-x-m-r` | the single `clientUid` | `submitSendMissionRoleChangeCot` `:2205-2232` |

In every case delivery is `sub.submit(message)` directly — **not** through `BrokerService`, so **no group reachability check and no flow tag** on mission notifications (except the group-vector check explicitly coded into `submitBroadcastMissionAnnouncementCot`). Websocket subscriptions are collected into `websocketHits` and routed via `WebsocketMessagingBroker` instead.

## 7.4 `t-x-g-c` (group change) — `:1723`, `:1753-1774`, `:2685-2732`

Seed:
```xml
<event how='h-g-i-g-o' type='t-x-g-c' version='2.0' uid='{randomUUID}[.{clientUid}]' start='{t}' time='{t}' stale='{t+20s}'>
  <point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/>
  <detail><link relation='p-p'/></detail>
</event>
```
`uid` = `generateUid()` and, if `clientUid != null`, `+ "." + clientUid`. `<link>` is left with only `relation='p-p'`. Sent to every subscription of the named user **except** the one whose `clientUid` equals the initiating `clientUid`. Triggered by `ActiveGroupCacheHelper.assignGroupsCheckCache` returning true during x509 auth (`X509Authenticator.java:247-256, 330-339`).

## 7.5 `<MissionChanges><MissionChange>` layout

`MissionChanges`: `$R/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/sync/model/MissionChanges.java`
```java
@XmlRootElement(name = "MissionChanges")
@XmlAccessorType(XmlAccessType.FIELD)
@XmlElement(name="MissionChange") private List<MissionChange> missionChanges;
```

`MissionChange`: `$R/src/takserver-plugins/src/main/java/com/bbn/marti/sync/model/MissionChange.java` — `@XmlRootElement(name="MissionChange")`, default `XmlAccessType.PUBLIC_MEMBER` over **getters**, **no `@XmlAttribute` anywhere**. Therefore **every serialised field is a child element**, in JAXB's default (alphabetical-ish, implementation-defined) order:

| XML element | getter | line | notes |
|---|---|---|---|
| `<type>` | `getType()` | `:146` | enum name: `CREATE_MISSION`, `DELETE_MISSION`, `ADD_CONTENT`, `REMOVE_CONTENT`, `CREATE_DATA_FEED`, `DELETE_DATA_FEED` (`MissionChangeType`, common copy; the plugins copy still says `CREATE_MISSION_FEED`/`DELETE_MISSION_FEED` — the **common** one is authoritative for the wire) |
| `<isFederatedChange>` | `getIsFederatedChange()` | `:217` | boolean |
| `<missionName>` | `getMissionName()` | `:199` | |
| `<missionGuid>` | `getMissionGuid()` | `:208` | UUID |
| `<timestamp>` | `getTimestamp()` | `:168` | `@XmlJavaTypeAdapter(DateAdapter)`, JSON pattern `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'` |
| `<creatorUid>` | `getCreatorUid()` | `:332` | |
| `<contentUid>` | `getContentUid()` | `:226` | for uid content |
| `<details>` | `getUidDetails()` | `:252-257` | `@XmlElement(name="details")` on a `UidDetails` |
| `<contentResource>` | `getContentResource()` | `:299-303` | a `Resource`; the reference client reads `contentResource/hash` and `contentResource/filename` (`utils.py:143-147`) |
| `<logEntry>` | `getTempLogEntry()` | `:321-325` | `@JsonProperty("logEntry")`; no `@XmlElement` rename, so the element name is `tempLogEntry` in XML |
| `<content>` | — | — | **not a JAXB field**; injected by `addMissionChangeContentXml` under the **first** `MissionChange` |

Explicitly `@XmlTransient`: `id`, `contentHash`, `servertime`, `mission`, `externalDataUid/Name/Tool/Token/Notes`, `missionFeedUid`, `tempResource`.

Marshalling: `CommonUtil.toXml(obj)` (`$R/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/util/CommonUtil.java:668-684`) marshals then **strips everything up to and including the first `>`** — i.e. removes the XML declaration. The result is embedded as a child of `<mission>`.

`roleXml` = `commonUtil.toXml(missionRole)` producing `<role type="..."><permissions>…</permissions></role>` (`MissionRole.java:34` `@XmlRootElement(name="role")`, `:83-85` `@XmlAttribute(name="type")`, `:104-105` `@XmlElement(name="permissions")`).

---

# 8. GeoChat (`b-t-f`)

There is **no dedicated chat router** — `b-t-f` is brokered by the same `<marti><dest>` mechanism as everything else. The server-side special-casing is:

1. **Delivery-failure notification** — `DistributedSubscriptionManager.getMatches:500-504`: if explicit brokering produced **zero** reachable matches and `type.startsWith("b-t-f") && type != "b-t-f-s"`, call `messagingUtil().sendDeliveryFailure(senderSub.clientUid, c)`. That builds a copy of the original message with `ARCHIVE_EVENT_KEY`, `EXPLICIT_CALLSIGN_KEY`, `EXPLICIT_UID_KEY` stripped, **removes this server's flow tag**, sets **`type = "b-t-f-s"`** ("stored"), disables archiving, and addresses it back to the sender's uid (`MessagingUtilImpl.java:447-469`).

2. **Mission chat fixup** — `StreamingEndpointRewriteFilter.fixupStreamingMissionChat:306-342`, entered when a `b-t-f` carries `<dest mission=…>` (`:546-551`, `:764-769`). For each recipient uid whose **input** has a non-empty `takServerHost`, it makes a per-recipient copy, strips `<marti>`, and rewrites (`CommonUtil.fixupMissionChat:747-766`):
   - `/event/detail/__chat/@id`
   - `/event/detail/__chat/chatgrp/@uid1`
   - `/event/detail/__chat/chatgrp/@id`
   
   each to `"{takServerHost}-8443-ssl-{missionName}"` (`fixupMissionChatAttr:737-745`). Recipients on inputs without `takServerHost` get **nothing** (`continue`). This is how ATAK's mission-chat room id is made server-local.

3. **Chat persistence parsing** — `RepositoryService.java:571-673` parses `__chat` / `__chatreceipt`:
   - `chatgrp` with exactly **3 attributes** ⇒ P2P, `destUid = @uid1`
   - `chatgrp` with **>3** attributes ⇒ group chat, every `uid*` attribute except `uid0` becomes a destination row
   - `<hierarchy>` ⇒ `extractGroupContacts` builds a callsign→uid map, used to resolve `<dest callsign=…>` to a uid
   - `__chat/@senderCallsign`, `__chat/@chatroom`, `remarks` text are stored
   - if no destUid can be derived ⇒ the row is dropped with an error

4. **Store-and-forward** — `buffer/queue/@enableStoreForwardChat`. On callsign assignment, `forwardMessages(sub, groupVector)` (`SubmissionService.java:1628-1663`) queries `getChatMessagesForUidSinceLastDisconnect` and replays them one at a time (`storeForwardSendBufferMs` apart) with the flow tag removed, archiving off, `STORE_FORWARD_KEY` set (forces single-threaded ordered executors in `BrokerService`), and `EXPLICIT_UID_KEY = [destUid]`.

**"All Chat Rooms" / "All Streaming"** — `$R/src/takserver-core/takserver-war/src/main/java/com/bbn/marti/util/SpecialChatrooms.java`:
```java
ALL_STREAMING("All Streaming"), ALL_CHAT("All Chat Rooms");
```
These are **only** used on the REST chat-injection path (`CommonUtil.getSpecialChatroom`, `isAllStreamingChat`, `chatToCot` `:430-480`), which synthesises:
```xml
<event uid='{uid}.{chatroom}.{randomUUID}' type='b-t-f' …>
  <detail>
    <__chat parent='RootContactGroup' groupOwner='false' chatroom='{chatroom}' id='{chatroom}' senderCallsign='{cs}'>
      <chatgrp uid0='{from}' uid1='{chatroom}' id='{chatroom}'/></__chat>
    [<marti>…</marti> only if addresses were resolved]
    <remarks time='{t}' source='{uid}'>{body}</remarks>
  </detail></event>
```
On the **streaming** path the only handling is `StreamingEndpointRewriteFilter:243` — a `<dest callsign="All Streaming"/>` causes the **entire callsign list to be discarded**, so the message falls back to implicit group broadcast. `"All Chat Rooms"` appears in streaming only as a literal `dest_uid` value in the store-and-forward SQL (`RepositoryService.java:706`).

---

# 9. File share

- **The server does not rewrite `<fileshare senderUrl=…>` on the client-to-client streaming path.** `b-f-t-r` is brokered like any other CoT; the URL points at the sending ATAK's own HTTP server.
- The **only** `senderUrl` rewrite is on the **federation** path: `FederateSubscription.sendMissionPackage` (`$R/src/takserver-core/src/main/java/tak/server/federation/FederateSubscription.java:89-94, 246-336`) — the server downloads the package locally, pushes it to the remote federate over HTTP, gets back the federate-local URL, and does `fileshare.addAttribute("senderUrl", missionPackageUrl)` (`:313`) before protobuf-encoding the event. If `/event/detail/fileshare` is missing it logs `"mission package announce CoT invalid - detail/fileshare element not present"` and drops it. `FigFederateSubscription.java:316` and `FigServerFederateSubscription.java:165` have the same `b-f-t-r` branch.
- **Server-generated** `b-f-t-r` (`CommonUtil.getFileTransferCotMessage`, `:634-654`; near-identical copy at `GroupFederationUtil.java:478-486`):
```xml
<?xml version='1.0' encoding='UTF-8' standalone='yes'?>
<event version='2.0' uid='{uid}' type='b-f-t-r' time='{t}' start='{t}' stale='{t+100s}' how='h-e'>
  <point lat='0.0' lon='0.0' hae='9999999.0' ce='9999999' le='9999999'/>
  <detail>
    <fileshare sha256='{hash}' senderUid='{uid}' name='{filename}' filename='{filename}'
               senderUrl='{url}' sizeInBytes='{n}' senderCallsign='{callsign}'/>
    <marti><dest uid='{contact}'/>…</marti>
  </detail></event>
```
  This is also the substitute emitted when an outbound protobuf message exceeds 64 KiB (§3.6), with `senderUrl = https://{takServerHost}:{connector0.port}/Marti/api/cot/xml/{uid}` and `senderCallsign` = the authenticated user's name or `"takserver"`.
- `MissionPackageExtractor` (`$R/.../service/MissionPackageExtractor.java:48, 89`) parses `//event/detail/fileshare/@senderUrl` for `b-f-t-r`.
- **`b-f-t-a` has no server-side handling at all** in this checkout. It appears only in two SQL exclusion lists (`MissionKMLServlet.java:351`, `JDBCCachingKMLDao.java:224`) that exclude `b-t-f`, `b-f-t-r` and `b-f-t-a` from KML output. It is relayed as ordinary CoT (normally addressed with `<dest uid=…>`).

---

# 10. Repeater / injector / QoS / federation

**Repeater** (`$R/.../service/RepeaterService.java`, `$R/.../repeater/RepeaterApi.java`). A `BaseService` consumer that sees every brokered message and, if it matches a configured `initiate-test` XPath, stores it and re-broadcasts it every `repeater/@periodMillis` (example config: 3000 ms) until a `cancel-test` XPath matches or `staleDelayMillis` (15000) passes. Built-in types plus `<repeatableType initiate-test cancel-test _name>` from config (the example ships 911/RingTheBell/GeoFenceBreach/TroopsInContact keyed on `/event/detail/emergency[@type=…]`, cancel `@cancel='true'`). Repeated copies get `Constants.REPEATER_KEY` set, which suppresses federation to federates with `shareAlerts=false` (`DistributedSubscriptionManager.java:800-811`), and the sender's user+groups are *replicated* so the repeat keeps its ACL. REST: `GET /Marti/api/repeater/list` → `ApiResponse<List<Repeatable>>` (version string `1.0.0`); `GET|POST /Marti/api/repeater/period` (int millis, POST body is a bare JSON integer); `GET /Marti/api/repeater/remove/{uid:.+}` → `ApiResponse<Boolean>`. **Safe to stub** — pure server-side re-emission, no client protocol impact.

**Injector** (`$R/.../injector/UidCotTagInjector.java`, `StringCotTagInjector.java`, `ClusterUidCotTagInjector.java`, `$R/.../injector/InjectionApi.java`). A per-CoT-uid rule that splices a fixed XML fragment into `<detail>` of every matching message at ingest (`injectionManager.process(sub, data)` at `SubmissionService.java:1566-1573`, inside the group-attachment task). Config either from `CoreConfig` `<injectionfilter>`/`<uidInject>` or at runtime via REST. Endpoints, all under `/Marti` + `BASE_PATH = "/injectors/cot/uid"`: `GET /Marti/injectors/cot/uid` (all), `GET /Marti/injectors/cot/uid/{uid}`, `POST /Marti/injectors/cot/uid` with body `{"uid":"…","toInject":"<xml/>"}` (upsert; rejects uid matching `.*[<>'"].*` and toInject matching `.*[<>].*` — note the second regex rejects *any* `<`, so only text injection actually works through the API), `DELETE /Marti/injectors/cot/uid?uid=…&toInject=…`. **Safe to stub.**

**QoS** (`$R/src/takserver-core/src/main/java/tak/server/qos/`, `CoreConfig.xsd:934-996`). Three independent rate limiters keyed off the number of connected clients: `deliveryRateLimiter` (enabled by default) throttles per-**connectionId** outbound delivery, `readRateLimiter` (default off) throttles per-connectionId inbound, `dosRateLimiter` (default off, `intervalSeconds=60`) throttles per source **IP address**. Delivery/read use `<rateLimitRule clientThresholdCount reportingRateLimitSeconds/>`; DoS uses `<dosLimitRule clientThresholdCount messageLimitPerInterval/>`. Each rule means "once ≥ `clientThresholdCount` clients are connected, allow at most one message per `reportingRateLimitSeconds` (resp. `messageLimitPerInterval` per interval)". Hook points: `NioNettyHandlerBase.isNotDOSLimited` / `isNotReadLimited` (`:206-214`, applied in the readers) and `isNotDeliveryLimited` (`:216-219`, applied in `protocol.write`). **Safe to stub as always-allow.**

**Federation.** Two generations. V1 = raw TLS on `federation/federation-server/@port`, Netty pipeline `SslHandler → ByteArrayDecoder/Encoder → NioNettyFederationServerHandler`, wire format is length-prefixed `FederatedEvent` protobufs (`fig.proto`), with `ProtoBufHelper.cot2protoBuf` mapping CoT→`GeoEvent` (a *different*, flatter schema than `cotevent.proto`). V2 ("ROGER FIG") = gRPC service `FederatedChannel` in `fig.proto` with bidirectional `ClientEventStream`/`ServerEventStream`, `ClientROLStream`/`ServerROLStream` (ROL = the mission-change DSL), `ServerFederateGroupsStream`, `HealthCheck`, `GetAuthTokenByX509`. Federates appear as `FederateSubscription`/`FigFederateSubscription` in the same subscription store and participate in normal brokering, with extra rules: federate→federate is never reachable (`CommonGroupDirectedReachability.java:76-84`), `b-f-t-r` triggers the mission-package relay with `senderUrl` rewrite (§9), `t-x-d-d` is converted to a proto contact-delete, group names are mapped through inbound/outbound group lists, and hop limits/provenance prevent loops. **Stub entirely** — nothing here affects the ATAK/CloudTAK client protocol.

---

# 11. Reference client (`src/testing/load_test/*.py`)

**These files are not in your sparse checkout.** They exist in the git object store; I read them with `git show HEAD:src/testing/load_test/<file>`. Do the same if you want to re-read them.

## 11.1 Files

`pyTak.py` (plain XML over TLS, asyncio.Protocol), `pyTakStreamingCot.py` (XML, reader/writer tasks), **`pyTakStreamingProto.py`** (the important one), `pyTakWebsocket.py`, `create_cot.py`, `create_proto.py`, `mission_api.py`, `mission_handlers.py`, `utils.py`, `stats.py`, `tak_tester.py`, `cloud_watch.py`, vendored `protobuf_msg/*_pb2.py`, `base_config*.yml`, `requirements.txt`.

## 11.2 Negotiation — `pyTakStreamingProto.py:161-189`

```python
async def negotiate_protocol(self, reader, writer):
    while not self.negotiated:
        try:
            data = await reader.readuntil(b'</event>')
        except asyncio.LimitOverrunError as e:
            if sent_request:
                data = await reader.read(reader._limit)
                if data[0].to_bytes(1,'big') == b'\xbf':
                    self.buffer = data; self.negotiated = True; return
            raise e
        msg = CotMessage(msg=data)
        if msg.server_protocol_version_support() == '1':      # detail/TakControl/TakProtocolSupport/@version
            response = CotMessage(uid=msg.uid)                # <-- reuses the SERVER's uid
            response.protocol_response_message(version='1')
            writer.write(response.to_string().encode('utf-8')); await writer.drain()
            sent_request = True
        elif msg.server_protocol_negotiation_handshake():      # detail/TakControl/TakResponse/@status == "true"
            self.negotiated = True
```

Notes worth copying into your spec:
- It reads by scanning for `</event>` — confirming the server sends no other delimiter.
- The request **echoes the server's negotiation uid**. The server ignores the uid, but real ATAK does the same.
- `protocol_response_message` (`create_cot.py:188-196`) mutates a normal `a-f-G-U-C-I` event into: `type="t-x-takp-q"`, `how="m-g"`, point `lat/lon=0.0 hae=0.0 ce=999999 le=999999`, and appends `<TakControl><TakRequest version="1"/></TakControl>` under `<detail>`.
- The `LimitOverrunError` branch handles the case where the server's `t-x-takp-r` and the first `0xbf` protobuf frame arrive in the same read.

## 11.3 Framing — `create_proto.py`

```python
MAGIC_BYTE = b'\xbf'
def serialize(self):                      # :118-121
    msg = self.message.SerializeToString()
    return MAGIC_BYTE + get_size_bytes(msg) + msg
def get_size_bytes(msg):                  # :166-176  LEB128 of len(msg)
def get_msg_size(msg):                    # :150-164  returns payload_len + hdr_size, where hdr_size starts at 2
```
`get_msg_size` returns **length including the magic byte and the varint**, which is why the read loop at `pyTakStreamingProto.py:205-220` slices `fullbuffer[:msg_size]` from the *start of the frame* and then re-parses with `deserialize_message`, which re-checks the magic and re-reads the varint. Your decoder should return the payload length only; just be aware of this when comparing.

## 11.4 Ping/pong — `pyTakStreamingProto.py:302-307, 224-228`

```python
ping_message = CotProtoMessage(uid=self.uid, lat=..., lon=..., type="t-x-c-t")
writer.write(ping_message.serialize())
```
- The ping is a **full CoT** with `uid` = the client uid, `how="h-g-i-g-o"`, a valid `<point>` (required — see §5.2), `staleTime`/`startTime`/`sendTime` set, **no detail**. Default interval 1000 ms.
- Pong detection is `self.message.cotEvent.type == "t-x-c-t-r"` — nothing else.

## 11.5 Self-SA — `create_proto.py:49-74`, `pyTakStreamingProto.py:246-254`

Uses the **strongly typed** detail fields, not `xmlDetail`:
```python
detail.contact.callsign = uid;  detail.contact.endpoint = "*:-1:stcp"
detail.group.name = "Red";      detail.group.role = "Team Member"
detail.takv.platform = "PyTAKStreamingProto"; detail.takv.version = "0.0.2"
```
`endpoint="*:-1:stcp"` is the canonical streaming-client endpoint string. Note it sets **no `takv.device`/`takv.os`** — which means the server's *reverse* conversion (`cot2protoBuf`) would not have accepted that `<takv>` (it requires all four attributes **and** `attributeCount()==4`), but `proto2cot` emits it happily. Type defaults to `a-f-G-U-C-I`; SA cadence default 1 s.

Mission CoT: `add_sub_detail("marti", "dest", {"mission": mission})` → serialised into `detail.xmlDetail` as `<marti><dest mission="X"/></marti>` (`create_proto.py:76-103`), sent **once per mission** unless `send_only_new_tracks`.

## 11.6 Mission / Enterprise Sync REST calls — `mission_api.py`

Base: `https://{host}:{port}/Marti/`, `base_mission_api = base + "api/missions/"`, `base_sync_api = base + "sync/"`. Transport: `requests.Session` with mutual TLS — the **p12 is converted to a temporary PEM** (`utils.p12_to_pem`) and used as **both** `session.cert` and `session.verify` (`:49-53`), with a custom `HTTPAdapter` that sets `assert_hostname=False`. Default timeout 60 s, 10 SSL retries.

| call | method | URL | params / body / headers |
|---|---|---|---|
| list missions | GET | `/Marti/api/missions/` | — → `response.json()['data']` |
| get mission | GET | `/Marti/api/missions/{name}` | → `['data']` |
| get mission CoT | GET | `/Marti/api/missions/{name}/cot` | — |
| client endpoints | GET | `/Marti/api/clientEndPoints` | — |
| create mission | **PUT** | `/Marti/api/missions/{name}` | query `creatorUid`, `group` (default `__ANON__`), optional `tool`, `description`; **201** = created, 2xx = already existed |
| delete mission | DELETE | `/Marti/api/missions/{name}` | — |
| add content | **PUT** | `/Marti/api/missions/{name}/contents` | `Content-Type: application/json`, body `{"hashes":[…],"uids":[…]}` |
| subscribe | **PUT** | `/Marti/api/missions/{name}/subscription` | query `uid={clientUid}` |
| unsubscribe | DELETE | `/Marti/api/missions/{name}/subscription` | query `uid={clientUid}` |
| add data feed | POST | `/Marti/api/missions/{name}/feed` | query `creatorUid`, `dataFeedUid` |
| upload file | POST | `/Marti/sync/upload` | query `name`, `creatorUid`, `uid`, `latitude`, `longitude`, `altitude`, `keywords[]` (always includes `pyTakLoadTest`); multipart `files=`; `Content-Type` header |
| download | GET | `/Marti/sync/content` | query `hash`; reads `Content-Disposition` filename |
| delete file | **GET** | `/Marti/sync/delete` | query `hash` (yes, GET) |
| search | GET | `/Marti/sync/search` | query `keywords` |

No `Authorization` header anywhere — **auth is purely the client certificate**. No `missionauthorization` header is sent (though the CORS config allows it).

## 11.7 Reacting to change messages — `utils.py:117-155`

```python
xml_details = ET.fromstring("<detail>" + detail.xmlDetail + "</detail>")   # proto path
for elem in xml_details:
    if elem.tag == "_flow-tags_": continue
    if elem.tag == "mission":
        change_type = elem.attrib.get("type"); name = elem.attrib.get("name")
        for change in elem.find("MissionChanges").findall("MissionChange"):
            for res in change.findall("contentResource"):
                hash = res.find("hash").text   ...
        if file_hashes: return "download_mission_files", file_hashes
        if change_type in {"CHANGE","CREATE","INVITE","DELETE"}: return change_type, name
```
This is the authoritative confirmation of §7's element-vs-attribute layout: `<mission type= name=>` are **attributes**; `MissionChanges/MissionChange/contentResource/hash` and `.../filename` are **nested elements**. Also note the client reaches the mission element through `detail.xmlDetail` — the mission notification survives the protobuf round-trip as opaque XML, since `<mission>` is not one of the six typed detail fields.

`extract_fileshare` reads `fileshare/@sha256` and `fileshare/@filename` from `xmlDetail`.

---

# 12. Protobuf

Directory: `$R/src/takserver-protobuf/src/main/proto/`. **All 18 files are `syntax = "proto3"` and none carries any licence header** — the only comments are technical. (The repo-level licence is `$R/LICENSE.md`.)

| file | package | java options |
|---|---|---|
| `takmessage.proto` | `atakmap.commoncommo.protobuf.v1` | none |
| `cotevent.proto` | `atakmap.commoncommo.protobuf.v1` | none |
| `detail.proto` | `atakmap.commoncommo.protobuf.v1` | none |
| `contact.proto` | `atakmap.commoncommo.protobuf.v1` | none |
| `group.proto` | `atakmap.commoncommo.protobuf.v1` | none |
| `precisionlocation.proto` | `atakmap.commoncommo.protobuf.v1` | none |
| `status.proto` | `atakmap.commoncommo.protobuf.v1` | none |
| `takv.proto` | `atakmap.commoncommo.protobuf.v1` | none |
| `track.proto` | `atakmap.commoncommo.protobuf.v1` | none |
| `takcontrol.proto` | `atakmap.commoncommo.protobuf.v1` | none |
| `message.proto` | `atakmap.commoncommo.protobuf.v1` | none — TAK-Server-internal envelope |
| `missionannouncement.proto` | `atakmap.commoncommo.protobuf.v1` | none — TAK-Server-internal |
| `binarypayload.proto` | `gov.tak.cop.proto.v1` | none |
| `streaminginput.proto` | `com.atakmap` | `java_package=com.atakmap.Tak`, `outer=StreamingInputProto`, `objc_prefix=TAK` |
| `fig.proto` | `com.atakmap` | `java_package=com.atakmap.Tak`, `outer=FigProto`, `objc_prefix=TAK` |

## 12.1 Field numbers — the ATAK-CIV-compatible set

```proto
message TakMessage {              // takmessage.proto
  TakControl takControl   = 1;
  CotEvent   cotEvent     = 2;
  uint64     submissionTime = 3;  // TAK-Server extension, not in atak-civ
  uint64     creationTime   = 4;  // TAK-Server extension, not in atak-civ
}

message CotEvent {                // cotevent.proto
  string type           = 1;
  string access         = 2;
  string qos            = 3;
  string opex           = 4;
  string uid            = 5;
  uint64 sendTime       = 6;      // ms since epoch
  uint64 startTime      = 7;
  uint64 staleTime      = 8;
  string how            = 9;
  double lat            = 10;
  double lon            = 11;
  double hae            = 12;     // 999999 = unknown
  double ce             = 13;     // 999999 = unknown
  double le             = 14;     // 999999 = unknown
  Detail detail         = 15;
  string caveat         = 16;
  string releaseableTo  = 17;     // note spelling; maps to XML attribute "releasableTo"
}

message Detail {                  // detail.proto
  string            xmlDetail         = 1;
  Contact           contact           = 2;   // <contact>
  Group             group             = 3;   // <__group>
  PrecisionLocation precisionLocation = 4;   // <precisionlocation>
  Status            status            = 5;   // <status>
  Takv              takv              = 6;   // <takv>
  Track             track             = 7;   // <track>
}

message Contact           { string endpoint = 1; string callsign = 2; }
message Group             { string name = 1; string role = 2; }
message PrecisionLocation { string geopointsrc = 1; string altsrc = 2; }
message Status            { uint32 battery = 1; }
message Takv              { string device = 1; string platform = 2; string os = 3; string version = 4; }
message Track             { double speed = 1; double course = 2; }
message TakControl        { uint32 minProtoVersion = 1; uint32 maxProtoVersion = 2; }
```

**Match against atak-civ:** `CotEvent` 1–15, `Detail` 1–7, `Contact`, `Group`, `PrecisionLocation`, `Status`, `Takv`, `Track`, `TakControl` are **identical** to the atak-civ / libcommo definitions in both numbers and types. Two deltas to flag:
- `CotEvent.caveat = 16` / `releaseableTo = 17` are TAK-Server/MIL-STD-6090 additions; older ATAK builds don't populate them but proto3 ignores unknown fields, so it's wire-safe.
- `TakMessage.submissionTime = 3` / `creationTime = 4` are **server-side only**. `cot2protoBuf` sets them (`StreamingProtoBufHelper.java:412-421`) and `proto2cot` reads them back (`:607-612`). ATAK ignores them. **Do not rely on them and do not require them.** If you emit them you are still interoperable.
- `TakControl` is **never populated by TAK Server** in the streaming path — `cot2protoBuf` only ever sets `cotEvent`. Version negotiation is done entirely in XML (§3).

## 12.2 XML↔proto conversion rules you must replicate exactly

`$R/src/takserver-plugins/src/main/java/tak/server/proto/StreamingProtoBufHelper.java`

**XML → proto (`cot2protoBuf`, `:49-431`)** — a typed sub-message is used **only if the element has exactly the expected attributes and no others**:
- `<contact>`: requires `@callsign`; accepted iff (`endpoint` absent **and** `attributeCount()==1`) or (`endpoint` present **and** `attributeCount()==2`). `endpoint` is only set if non-empty.
- `<__group>`: requires `@name` and `@role` **and** `attributeCount()==2`.
- `<precisionlocation>`: `@geopointsrc` + `@altsrc` **and** `attributeCount()==2`.
- `<status>`: `@battery` **and** `attributeCount()==1`.
- `<takv>`: all four of `@device @platform @os @version` **and** `attributeCount()==4`.
- `<track>`: `@speed` + `@course` **and** `attributeCount()==2`.
- If accepted, the element is **removed** from the detail copy. Whatever remains is concatenated (`Element.asXML()` per child, no wrapper, no XML header) into `Detail.xmlDetail`; if nothing remains, `xmlDetail` is left unset (`:395-401`).
- Missing `type`/`uid`/`how`/`time`/`start`/`stale`/`point` are **logged as errors but not fatal** — the message is still built with defaults.
- `ce`/`le` default to **999999** when the attribute is missing; `lat`/`lon`/`hae` default to 0.
- XML attribute `releasableTo` ↔ proto field `releaseableTo`.

**proto → XML (`proto2cot`, `:440-628`)**:
- Always emits `<event version="2.0" uid type how time start stale>` (times via `DateUtil.toCotTime` ⇒ **second resolution, sub-second precision is lost**), then `caveat`/`releasableTo`/`opex`/`qos`/`access` only when non-empty.
- Always emits `<point lat lon hae ce le>` with `Double.toString(...)` — so you get Java double formatting (`0.0`, `1.0E-5`, etc.).
- Emits the typed elements for whichever `has*()` is true, then, if `xmlDetail` is non-empty, wraps it as `<?xml version="1.0" encoding="UTF-8"?><detail>{xmlDetail}</detail>`, parses it, and for each child: **if the child's name collides with one of the six typed names, the typed element is removed and the xmlDetail one wins** (`:582-598`).
- `<detail>` is emitted whenever `cotEvent.getDetail()` is non-null — which in proto3 Java is **always** (it returns the default instance), so proto→XML always yields an `<event>` with a `<detail>` element, possibly empty.

## 12.3 The other protos (server-internal — you can ignore them for client compat)

- `message.proto` `Message{ TakMessage payload=1; string source=2; string clientId=3; repeated string groups=4; repeated string destClientUids=5; repeated string destCallsigns=6; repeated string provenance=7; bool archive=8; string feedUuid=9; string connectionId=10; repeated BinaryPayload bloads=11; }` — the Ignite/plugin bus envelope.
- `missionannouncement.proto` `MissionAnnouncement{ payload=1; missionName=2; missionAnnouncementType=3; creatorUid=4; groupVector=5; clientUid=6; repeated uids=7; missionGuid=8; }`.
- `binarypayload.proto` `BinaryPayload{ type=1; version=2; bytes data=3; int64 timestamp=4; uuid=5; }`.
- `streaminginput.proto` — gRPC `InputChannel` service (`protocol="grpc"` inputs): `ClientTakMessageStream(ClientSubscription) → stream TakMessage`, `ServerTakMessageStream(stream TakMessage) → ClientSubscription`, `VersionCheck(ClientVersion{int32 version=1}) → ServerVersion{int32 version=1; bool compatible=2}`.
- `fig.proto` — federation only.

---

# Things I could not find / could not confirm

1. **`src/testing/load_test/*.py` are not checked out** — I read them from git objects (`git show HEAD:…`). If your sparse-checkout rules change, they may disappear.
2. **Byte-exact dom4j output format** (declaration quoting, the newline after the declaration, absence of a trailing newline). dom4j is not vendored here; my statement is based on dom4j's documented `OutputFormat`/`XMLWriter` defaults (`newLineAfterDeclaration=true`, `lineSeparator="\n"`, `omitEncoding=false`, `suppressDeclaration=false`). Worth confirming with a packet capture against a real server before you hard-code it.
3. **No `<__serverdestination>` writer exists** anywhere in this source. It appears only in explanatory comments.
4. **No `maxMessageSize` config** exists. The only limits are `INDIVIDUAL_COT_MSG_SIZE_LIMIT = 8 MiB` (XML, inbound) and `MAX_SIZE = 64 KiB` (protobuf, outbound only).
5. **No per-server "All Chat Rooms" routing** on the streaming path — it is a client-side room name plus a REST-injection special case plus a store-and-forward SQL literal. Nothing in the streaming broker treats it specially.
6. **`<dest publish=…>` is not implemented** — it makes the message explicit-brokered and then matches nothing.
7. JAXB **element ordering** inside `<MissionChange>` is implementation-defined (no `@XmlType(propOrder=…)`), so parse by name, never by position.
