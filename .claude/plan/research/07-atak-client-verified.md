Research complete. Here is the full report.

---

# ATAK-CIV wire-behaviour report

All paths below are under the checkout root:

```
/private/tmp/claude-501/-Users-bpannell-dev-gh-SierraSoftworks-rustak/9ec7cece-906f-4639-9129-d03da7a814b7/scratchpad/refs/atak-civ
```

I abbreviate it as `<R>` below (e.g. `<R>/commoncommo/core/impl/enrollmentmanager.cpp:434`).

**Files the task assumed present that are NOT in this sparse checkout** (so anything about them is unverified here):
- `com/atakmap/comms/NetConnectString.java` — absent (only *used*, never defined here).
- `com/atakmap/net/AtakCertificateDatabase*.java`, `AtakAuthenticationDatabase.java`, `CertificateManager.java` — absent (`<R>/atak/ATAK/app/src/main/java/com/atakmap/net/` contains only `CertificateConfigRequest`, `CertificateEnrollmentClient`, `DeviceProfile{Callback,Client,Operation,Request}`, `certconfig/`).
- `com/atakmap/spatial/kml/KMLUtil.java` — absent (so the exact `KMLDateFormatter` / `KMLDateTimeFormatterMillis` patterns are **unverified**).
- `res/values/strings.xml` — absent.

---

## 1. Enrollment end-to-end

### 1.1 Who does the HTTP

The Java `CertificateEnrollmentClient` does **no** HTTP itself — it delegates to native commoncommo via `CommsMapComponent.enroll(...)` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/net/CertificateEnrollmentClient.java:552`). The comment at `:510` is explicit: *"Prior implementation of this used TakHttpClient which ignores all system certs"*. All wire behaviour is in `<R>/commoncommo/core/impl/enrollmentmanager.cpp`.

### 1.2 Exact HTTP sequence

Three steps, enum `ENROLL_STEP_KEYGEN → ENROLL_STEP_CSR → ENROLL_STEP_SIGN` (`enrollmentmanager.cpp:436-452`, status strings at `:816-835`).

**Step 0 — keygen (local, no HTTP).** RSA, `e = RSA_F4 (65537)`, bits = the `keyLen` argument. `keyLen` is hard-coded **4096** at `<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/CommsMapComponent.java:2794`. Private key serialised to encrypted PEM with a random 64-char password (`CertificateEnrollmentClient.java:489`, `ATAKUtilities.getRandomString(64)`). Key gen code: `<R>/commoncommo/core/impl/cryptoutil.cpp:269-314`.

> Note a genuine bug in `cryptoutil.cpp:288-293`: the `OSSL_PARAM` array sets `"bits"` twice (second time with the exponent value) and never sets `"e"`. Treat "4096" as nominal.

**Step 1 — CSR config.**

```
GET https://<host>:<enrollPort>/Marti/api/tls/config
```
URL built at `enrollmentmanager.cpp:434` + `:441`.
- No `Accept`/`Content-Type` header is set for this step (headers are only added when `step == ENROLL_STEP_SIGN`, `:968`).
- Auth: HTTP Basic via `CURLOPT_USERNAME`/`CURLOPT_PASSWORD` (`:962-965`), **unless** `useTokenAuth`, in which case `Authorization: Bearer <password>` (`:956-958`). ATAK always passes `passIsToken = false` (`CertificateEnrollmentClient.java:559`), so **ATAK-CIV always uses Basic** even for the QR-code `token=` flow (the token is sent as the Basic password).
- No `User-Agent` is set anywhere in commoncommo's curl setup — default libcurl UA.
- Response body must be XML with root `<certificateConfig>` containing a `<nameEntries>` child (`:634`, `:637`). Children named `nameEntry` with attributes `name` and `value` (`:652-654`).
- Max response 10 MiB (`:107`).

**CSR subject:** `CN` is inserted first, with the *enrollment username* as its value (`:648`: `entries.push_back(... ("CN", user))`), then every `<nameEntry name= value=>` from the server is appended in document order. Each name is resolved with `OBJ_txt2nid`, so it must be a recognised OpenSSL short/long name (`cryptoutil.cpp:365-372`) — an unknown name aborts CSR generation. Signature: **SHA-256** (`cryptoutil.cpp:407`, `EVP_sha256()`). No extensions/attributes are added to the CSR.

**Step 2 — sign.**

```
POST https://<host>:<enrollPort>/Marti/api/tls/signClient/v2?clientUid=<our uid>[&version=<urlencoded ATAK version>]
Accept: application/xml
Content-Type: application/octet-stream
(+ Basic auth or Bearer, as above)

body = base64 CSR body with the PEM BEGIN/END CERTIFICATE REQUEST banner lines removed
```
URL at `enrollmentmanager.cpp:444-451`; headers at `:969-970`; banner stripping at `:690-697`. `version` comes from `URLEncoder.encode(ATAKConstants.getVersionName())` (`CommsMapComponent.java:2783`). `clientUid` is the device UID.

**Response parsing** (`:715-767`): root element **must** be `<enrollment>`. Every child element is read as a PEM-ish certificate body (`xmlNodeListGetString` of the child's text). The element named **`signedCert`** is the client cert (exactly one allowed, `:749-755`); **every other child element name is treated as a CA** and pushed onto the CA stack (`:756-762`) — so the element name for CAs (`<ca>`) is not actually checked. The code tolerates certs with or without PEM header/footer and with or without a trailing newline (`:728-743`).

Outputs: a PKCS#12 client keystore (client cert + private key + CA chain, friendly name `"TAK Client Cert"`, `:788-791`) and, only if at least one CA was returned, a PKCS#12 truststore (`:794-799`). PKCS#12 is written with the legacy algorithms `NID_pbe_WithSHA1And3_Key_TripleDES_CBC` (key) / `NID_pbe_WithSHA1And40BitRC2_CBC` (cert) (`cryptoutil.cpp:503-505`), and the OpenSSL *legacy* provider is loaded for reading server-produced P12s (`cryptoutil.cpp:260-265`).

**HTTP status mapping** (`enrollmentmanager.cpp:1008-1030`): `200 → SUCCESS`, `401 → AUTH_ERROR`, `403 → ACCESS_DENIED`, `404|410 → URL_NO_RESOURCE`, everything else `OTHER_ERROR`. So **a `201` on `signClient/v2` is treated as a failure.**

**Connection tuning for enrollment** (shared curl setup, `<R>/commoncommo/core/impl/urlrequestmanager.cpp:699-727`): `CURLOPT_CONNECTTIMEOUT 90`, `LOW_SPEED_LIMIT 10` / `LOW_SPEED_TIME 120` (stall timeout), `FORBID_REUSE 1`, `NOSIGNAL 1`. No `CURLOPT_SSLVERSION`, no cipher list — libcurl/OpenSSL defaults.

### 1.3 Ports

`<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/SslNetCotPort.java:18-20`:
```java
private static int SERVER_API_PORT_UNSECURE = 8080;
private static int SERVER_API_PORT_SECURE = 8443;
private static int SERVER_API_PORT_CERT_ENROLLMENT = 8446;
```
`getServerApiPath(type)` returns `":<port>/Marti"` (`:73-76`). All three are overridable from preferences `apiUnsecureServerPort`, `apiSecureServerPort`, `apiCertEnrollmentPort` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/cot/CotMapComponent.java:1276-1313`; key constants at `:130-131`). Enrollment always uses `Type.CERT_ENROLLMENT` (8446) — `CertificateEnrollmentClient.java:554-555`.

### 1.4 TLS trust during enrollment — and the quick-connect split

`CertificateEnrollmentClient.execute()` (`:509-563`) builds an in-memory PKCS#12 truststore from:
- **non-quick-connect**: the already-stored `TYPE_TRUST_STORE_CA` for that server (`:515-528`). If none, `trustedIssuers` stays null.
- **quick-connect**: `CertificateManager.getInstance().getLocalTrustManager(false).getAcceptedIssuers()` — i.e. **public/system CAs** (`:530-533`).

The `verifyHost` argument passed to native is literally `request.getQuickConnect()` (`:556`), so:
- **quick connect → hostname verification ON** (`CURLOPT_SSL_VERIFYHOST 2`) + public CA trust;
- **normal enrollment → hostname verification OFF** and peer verification against the configured server truststore.

And critically (`urlrequestmanager.cpp:640-642`): **if `caCerts` is NULL, `CURLOPT_SSL_VERIFYPEER` is set to 0** — a non-quick-connect enrollment against a server with no stored truststore accepts *any* server certificate.

### 1.5 What ATAK does after success

`EnrollCompletionListener.onEnrollmentCompleted` (`CertificateEnrollmentClient.java:348-461`):
1. Save the raw private key blob under `TYPE_PRIVATE_KEY` (`:388-390`).
2. Save the random client-keystore password as credential type `TYPE_clientPassword` keyed by **host** (`:393-398`).
3. Save the client P12 via `saveCertificateForServerAndPort(TYPE_CLIENT_CERTIFICATE, host, ncs.getPort(), clientCertStore)` — **keyed by host *and* the streaming port** (`:403-407`).
4. Only if quick-connect: save the CA P12 password (`TYPE_caPassword`, by host) and the CA P12 (`TYPE_TRUST_STORE_CA`, host + port) (`:410-425`). **Non-quick-connect enrollment never stores the returned CA chain** — the pre-existing truststore is kept.
5. `CertificateManager.invalidate(server)` to drop cached socket factories (`:428`).
6. If `getProfile` → fetch the enrollment device profile (see §2) and *only then* reconnect; otherwise `cs.reconnectStreams()` immediately (`:430-460`).

**How the stream is configured.** The `TAKServer` bundle is built in `onEnrollmentOk` (`:794-811`):
```java
String connectString = host + ":" + port + ":" + protocol;   // :795
bundle.putString(TAKServer.CONNECT_STRING_KEY, connectString);
bundle.putString(TAKServer.DESCRIPTION_KEY, description);
bundle.putBoolean(TAKServer.ENROLL_FOR_CERT_KEY, true);
bundle.putBoolean(TAKServer.ENROLL_USE_TRUST_KEY, false);    // :803
```
Key names (`<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/TAKServer.java:21-39`):
`connectString`, `description`, `compress`, `enabled`, `connected`, `error`, `serverVersion`, `serverAPI`, `useAuth`, `username`, `password`, `enrollForCertificateWithTrust`, `enrollUseTrust`, `expiration`, `cacheCreds`, `isStream`, `isChat`.
Defaults: `enrollForCert()` false, `enrollUseTrust()` **true**, `isEnabled()` **true**, `isUsingAuth()` false (`TAKServer.java:127-190`).
Note `useAuth` is **not** set by the enrollment path — enrolled streams authenticate with the client cert, not the `<auth>` document.

`connectString` format is strictly `host:port:proto` — `CotService.addStreamingImpl` throws unless `split(":").length == 3` and `proto ∈ {ssl, quic, tcp}` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/CotService.java:405-440`).

### 1.6 `SERVER_NOT_TRUSTED`

`CertificateEnrollmentClient.java:238-272`. There is **no** "trust this CA anyway" prompt.
- quick connect: the just-added streaming entry is removed (`cs.removeStreaming(connectString, false)`) and a **retry** dialog is shown (`showAlertDialog(..., QUICK_CONNECT_ERROR, request)` → positive button re-opens the enrollment dialog, `:693-707`, `:738-740`).
- normal: a plain OK-only alert `"The TAK Server's identity could not be verified"` (`:239`, `:255-268`), then `return` — **hard fail**.

The only way to add a CA is manually: the `caLocation` preference opens a file picker that imports a `.p12` truststore (`<R>/atak/ATAK/app/src/main/java/com/atakmap/app/preferences/NetworkConnectionPreferenceFragment.java:124-132`), or a `.p12` arrives in a data package / `.pref` (see §2.3, §6.7).

**"Quick connect"** = the `EnrollmentDialog` flow, titled `R.string.tak_server_quick_connect` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/app/EnrollmentDialog.java:104`). User types address + username + password; `onEnrollmentOk` creates the `TAKServer` with `enrollUseTrust=false`, adds the stream, then calls `enroll(..., getProfile=true, isQuickConnect=true)` (`CertificateEnrollmentClient.java:804-816`). Semantics: trust the server via **public CAs + hostname verification**, and **store the CA chain the server returns** as the stream truststore.

### 1.7 QR handling

**`tak://com.atakmap.app/enroll?host=&username=&token=`** — `<R>/atak/ATAK/app/src/main/java/com/atakmap/android/cot/CotMapComponent.java:1625-1671`. Matched by `u.getHost() + u.getPath()` equal to `"com.atakmap.app/enroll"` (`:1631-1632`). All three params required (`:1636-1637`). Shows a yes/no confirm, then:
```java
.onEnrollmentOk(_mapView.getContext(), host, "", host, username, token, -1L);   // :1651-1655
```
i.e. `cacheCreds=""`, `description = host`, `password = token`, `expiration = -1`.

**Does `host` accept `host:port`? Yes.** `onEnrollmentOk` (`CertificateEnrollmentClient.java:759-795`):
- strips a leading `scheme://` if present (`:767-769`);
- `split(":")`; `[0]` = host, `[1]` = port (default **8089**, `:765`, `:779-781`);
- `[2]` is only honoured if it equals `"quic"` (case-insensitive); otherwise protocol is forced to **`ssl`** (`:783-787`).

So `host=takserver.example.com:8089` works; `host=…:8089:tcp` yields `…:8089:ssl`.

**`tak://com.atakmap.app/import?url=…`** — `<R>/atak/ATAK/app/src/main/java/com/atakmap/android/importexport/ImportExportMapComponent.java:1162-1209`. Matched on `getHost()+getPath() == "com.atakmap.app/import"`, reads query param `url`, shows a yes/no confirm, then `beginImport(uri)` which builds a `RemoteResource` with `type = "INTERNAL_TRANSIENT"` and name = last path segment, and hands it to the downloader (`:1211-1219`). No scheme restriction is applied here.

### 1.8 Certificate extension requirements

I grepped `ExtendedKeyUsage`, `getExtendedKeyUsage`, `checkClientTrusted`, `getKeyUsage`, `X509TrustManager`, `checkServerTrusted`, `HostnameVerifier` across `comms/`, `net/`, `android/network/`. **There is no EKU/KU inspection anywhere in this checkout.** The only trust-manager code present is `<R>/atak/ATAK/app/src/main/java/com/atakmap/android/network/AtakWebProtocolHandlerCallbacks.java:194-240`, an `AggregateTrustManagerImpl` that just tries each delegate in turn.

On the native side, server-cert validation is a bare chain build: `X509_verify_cert` with no purpose set (`<R>/commoncommo/core/impl/internalutils.cpp:521-534`), and the SSL_CTX is created with `SSL_VERIFY_NONE` so OpenSSL's own checks are bypassed (`<R>/commoncommo/core/impl/streamingsocketmanagement.cpp:80-85`).

**Conclusion (fact, from this source): ATAK-CIV imposes no EKU/KU requirement on the issued client certificate or on the server certificate.** Chain-to-trust-anchor is the only requirement. (Whether real TAK Server requires `clientAuth` is a server-side question, out of scope here.)

---

## 2. Device profiles

### 2.1 Endpoints (`<R>/atak/ATAK/app/src/main/java/com/atakmap/net/DeviceProfileOperation.java:105-168`)

| Trigger | Path (appended to `https://<host>:<port>/Marti`) | Port type |
|---|---|---|
| `onEnrollment` | `/api/tls/profile/enrollment?clientUid=<uid>` | `CERT_ENROLLMENT` (8446) |
| `onConnect` | `/api/device/profile/connection?syncSecago=<n>&clientUid=<uid>` | `SECURE` (8443) |
| tool + filepaths | `/api/tls/profile/tool/<tool>/file?relativePath=/<p>[&relativePath=/<p>…]&clientUid=<uid>[&syncSecago=<n>]` | `CERT_ENROLLMENT` (8446) |
| tool only | `/api/device/profile/tool/<tool>?clientUid=<uid>[&syncSecago=<n>]` | `SECURE` (8443) |

Leading `/` is forced on each `relativePath` (`:137-141`); spaces are replaced with `%20` and nothing else is URL-encoded (`:151`). `clientUid = MapView.getDeviceUid()` (`:103`).

### 2.2 Auth / TLS per port

`:182-195`:
- `CERT_ENROLLMENT` (8446): a `TakHttpClient(baseUrl, sslSocketFactory)` where the factory's `allowAllHostnames` = `profileRequest.isAllowAllHostnames()`. Because the URL is `https`, `useBasicAuth()` is false, so Basic auth is added **only** via the explicit `execute(httpget, credentials)` overload when the request carries username/password (`:211-215`). The enrollment-profile request is the one that carries them.
- `SECURE` (8443): `TakHttpClient.GetHttpClient(baseUrl, NetConnectString.fromString(connectString))` → client certificate from the stream's connect string, hostname verification **on**.

Enrollment-profile requests pass `allowAllHostnames = !quickConnect` (`CertificateEnrollmentClient.java:583`) — same polarity as §1.4.

### 2.3 Response handling (`:220-244`, `:265-486`)

- `200` → `processResults(...)`.
- `204 SC_NO_CONTENT` → logged, not an error.
- `304 SC_NOT_MODIFIED` → logged, not an error.
- anything else → `ConnectionException`.
- Status is always returned in `PARAM_PROFILE_OUTPUT_HTTP_STATUS`.

`Last-Modified` on the response is captured verbatim into `PARAM_PROFILE_OUTPUT_LAST_MODIFIED` (`:80`, `:380-387`); on a later request the caller may send it back verbatim as `If-Modified-Since` (`:81`, `:198-206`).

Body handling: streamed to `<root>/…/missionpackage/incoming/<random UUID>`. If `autoImportProfile` (true for enrollment and connection profiles), it is passed to `MissionPackageExtractorFactory.Extract(..., true)` and deleted (`:401-413`). Otherwise, if `Content-Type == application/zip` it is unzipped (`:419-448`).

### 2.4 When `onConnect` fires

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/cot/CotMapServerListener.java:100-163` `connected()`:
- **every** connect: `getServerVersion(connectString, -1, false, this)`;
- **only if we don't already have a contact list for that connect string**: `getClientList(...)` **and** `getProfileUpdates(host, ncs.toString())`;
- **every** connect: `getToolProfileUpdates(...)`.

Both profile paths are gated on the boolean pref **`deviceProfileEnableOnConnect`, default `false`** (`CotMapServerListener.java:703-705`, `:727-729`, and again in `DeviceProfileClient.getProfile` at `<R>/atak/ATAK/app/src/main/java/com/atakmap/net/DeviceProfileClient.java:180-185`). The enrollment profile is **not** gated (`DeviceProfileClient.java:154-155` comment).

`syncSecago` for the connection profile is `now_seconds - prefs["deviceProfileOnConnectSyncTime"+server]`, and that pref is only updated on success (`CotMapServerListener.java:707-730`). First run → `lastSyncTime = 0` → `syncSecago` ≈ current epoch seconds, i.e. "everything".

### 2.5 `.pref` import — `<R>/atak/ATAK/app/src/main/java/com/atakmap/app/preferences/PreferenceControl.java`

Document is `<preferences>` with `<preference name="…">` children (`:486-519`).

**Dispatched group names** (`:496-517`): exactly `cot_inputs`, `cot_outputs`, `cot_streams` → connection loader; **everything else** → generic `SharedPreferences` loader keyed by the `name` attribute (`:636-724`).

**Legacy aliases** (`:638-646`): `com.atakmap.app_preferences`, `com.atakmap.civ_preferences`, `com.atakmap.fvey_preferences` are all rewritten to `DEFAULT_PREFERENCES_NAME = <packageName> + "_preferences"` (`:117`).

**Entry `class` attribute strings** (`:681-720`) — exact, including the `class ` prefix:
```
"class java.lang.String"   "class java.lang.Boolean"   "class java.lang.Integer"
"class java.lang.Float"    "class java.lang.Long"
```
plus a set class (checked by `isSetClass(className)`) whose `<entry>` contains `<element>` children (`:704-717`). The `class` attribute is dereferenced without a null check (`:677-679`) — a missing `class` attribute NPEs the whole import.

**`cot_streams` indexed keys actually read** (`:531-599`) — only these:
```
count
description<i>        connectString<i>     enabled<i>
useAuth<i>            compress<i>          cacheCreds<i>
caPassword<i>         clientPassword<i>
caLocation<i>         certificateLocation<i>
enrollForCertificateWithTrust<i>
enrollUseTrust<i>     (optional — only applied if present)
expiration<i>
```
**`username<i>` / `password<i>` are NOT read here.** Credentials for a stream come from the credential DB (`AtakAuthenticationCredentials.TYPE_COT_SERVICE`, keyed by host) at `CotService.java:860-904` / `:980-1010`, driven by `cacheCreds`.

`cacheCreds` is normalised back to the **US-English** string form (`:551-562`) — so the wire values that matter are the en-US `cache_creds_both` / `cache_creds_username` strings.

Resulting `Bundle` keys are the unindexed names (`description`, `enabled`, `useAuth`, `compress`, `cacheCreds`, `caPassword`, `clientPassword`, `caLocation`, `certificateLocation`, `enrollForCertificateWithTrust`, `enrollUseTrust`, `expiration`) and are handed to `addInput`/`addOutput`/`addStream` (`:601-617`). If `CotMapComponent` isn't up yet, only `enabled`/`description`/`compress` survive into temp storage (`saveInputOutput`, `:741-763`).

`<preferences>` import ends by broadcasting `"com.atakmap.app.PREFERENCES_LOADED"` (`:630-632`).

### 2.6 Relative cert paths (`cert/xxx.p12`)

`.p12` files are sorted by `ImportCertResolver`, whose destination directory is `FileSystemUtils.getItem("cert")` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/importfiles/sort/ImportCertResolver.java:50`), with `SortFlags.IMPORT_COPY` forced (`:68-72`). So a package's `cert/foo.p12` lands in `<atak root>/cert/foo.p12`, and the `caLocation`/`certificateLocation` values in the `.pref` resolve against that.

`finalizeImport()` (`:209-339`) then:
1. reloads `cot_streams` properties and diffs against the pre-import snapshot (`:217-230`);
2. imports the **default** `caLocation`/`caPassword`/`certificateLocation`/`clientPassword` prefs and **deletes those four prefs afterwards** (`:251-255`, `:117-120`, `:140-143`);
3. per connection, imports `caLocation`/`certificateLocation` from that connection's `.properties`, stores the passwords as `TYPE_caPassword`/`TYPE_clientPassword` keyed by connect string (`:258-275`);
4. if `enrollForCertificateWithTrust != "0"` for a connection, kicks off `CertificateEnrollmentClient.enroll(..., getProfile=true, isQuickConnect = !enrollUseTrust)` (`:277-285`) — note `enrollUseTrust` defaults to `"1"` (`:265-266`);
5. also imports `updateServerCaLocation` / `updateServerCaPassword` (`:288-305`);
6. strips `caLocation`/`caPassword`/`certificateLocation`/`clientPassword` from the persisted connection config and rewrites it (`:316-337`);
7. `reconnectStreams()` if anything was imported (`:307-314`).

`.pref` files are sorted by extension `.pref` into `PreferenceControl.DIRNAME` with content sniffing requiring `<preferences` plus one of `<preference key` / `<entry key` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/importfiles/sort/ImportPrefSort.java:59-61`, `:77`, `:114-116`). It also deletes sensitive entries after import: `clientPassword`, `caPassword`, `certificateLocation`, `caLocation`, `networkMeshKey` (`:66-71`).

---

## 3. Streaming client

### 3.1 Connect string parsing

Java: `CotService.addStreamingImpl` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/CotService.java:405-440`) requires exactly `host:port:proto`, proto ∈ {`ssl`,`quic`,`tcp`}. `CotServiceRemote.Proto` enum is `{ ssl, quic, tcp, udp }` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/CotServiceRemote.java:42-47`). `stcp` is not a stream proto — it only appears as the broadcast sentinel (below).

Native endpoint key: `"<ssl|tcp|quic>:<addr>:<port>"` (`<R>/commoncommo/core/impl/streamingsocketmanagement.cpp:338-357`).

### 3.2 TLS setup

`<R>/commoncommo/core/impl/streamingsocketmanagement.cpp`:
- SSL_CTX: `SSL_CTX_new(SSLv23_client_method())` with `SSL_CTX_set_verify(ctx, SSL_VERIFY_NONE, NULL)` (`:80-85`). No TLS version floor is set — whatever the linked OpenSSL negotiates.
- No cipher list on the streaming CTX. A cipher list **is** set on the CTX handed to curl for mission-package downloads: `SSL_CTX_set_cipher_list(sslCtx, "DEFAULT:!ECDH")` with the comment about `demo.atakserver.com` (`:418-421`).
- Client cert + key from the connection's P12 are installed per-SSL-object (`SSL_use_certificate` / `SSL_use_PrivateKey`, `:1953-1954`).
- After `SSL_connect` succeeds, the peer cert is fetched and verified manually against the connection truststore (`:1971-1984`). The comment at `:1968-1970` is explicit:
  > `NOTE: we *intentionally* do not check authenticity of peer certificate by CN v. hostname comparison or the like (per ATAK sources at time of writing)`
  No peer cert at all → `ERR_CONN_SSL_NO_PEER_CERT`; verification failure → `ERR_CONN_SSL_PEER_CERT_NOT_TRUSTED`.
- **No ALPN on TLS streams.** `ALPN_STREAMING = {0x09,'t','a','k','s','t','r','e','a','m'}` (`:36-38`) is used only by the QUIC path (`:2383-2384`, `:2570-2571`).

Which certs are loaded (Java, `CotService.java:442-580`): per-connection `TYPE_TRUST_STORE_CA` / `TYPE_CLIENT_CERTIFICATE` **for host+port** first; falls back to the global truststore, and to the global client cert *unless* `enrollForCertificateWithTrust` is set. Missing cert or missing cert password sets `hadError` and the interface is not created at all (`CommsMapComponent.java:1803`).

### 3.3 `<auth>` credentials message

Generated only when `username && password && username[0] != '\0'` (`streamingsocketmanagement.cpp:1730`, and the duplicated inline copy at `:1895`). Exact document (libxml, unformatted, XML declaration included, **trailing newline stripped**, `:1745-1755`):

```xml
<?xml version="1.0"?>
<auth><cot username="..." password="..." uid="<our device uid>"/></auth>
```

- **SSL**: the auth message is `push_front`ed onto the tx queue the instant `SSL_connect` returns 1 — i.e. it is the **first** application bytes on the wire (`:1989-1992`).
- **QUIC**: same, on handshake completion; if there is *no* auth message a ping is sent instead, because "tak server's quic support has an issue where it won't send data to us until we send something to it" (`:2274-2282`).
- **plain TCP**: `TcpConnectionContext` takes no username/password (`:193-197`) and never builds an auth doc. **`useAuth` on a `:tcp` stream is a no-op in commoncommo.**

**No response is expected or parsed.** There is no success/failure handling for auth — the client just keeps going. Per `protocol.txt:210-217`, a server that rejects auth is expected to close the connection.

### 3.4 TAK Protocol v1 negotiation

State machine `PROTO_XML_NEGOTIATE → PROTO_WAITRESPONSE → PROTO_HDR_MAGIC/HDR_LEN/DATA`, or `→ PROTO_XML_ONLY` (`streamingsocketmanagement.cpp:1664`, `:1170-1263`).

- Types: `t-x-takp-v` (server support offer), `t-x-takp-q` (client request), `t-x-takp-r` (server response) — `<R>/commoncommo/core/impl/cotmessage.cpp:136-141`. `how="m-g"`, stale 60 s (`:142-143`).
- On receiving `t-x-takp-v` while in `PROTO_XML_NEGOTIATE`: parse `<detail><TakControl><TakProtocolSupport version="N">` (repeatable) plus optional `<DetailExt id="…"/>` or `<DetailExt supportsAll="true"/>` children (`cotmessage.cpp:1370-1427`). If the set contains **1**, the client queues a request and moves to `PROTO_WAITRESPONSE` (`streamingsocketmanagement.cpp:1200-1231`).
- The client request re-uses the **server's event UID** (`new CoTMessage(logger, msg->getEventUid(), 1, ourExtensions)`, `:1211-1213`) and emits `<detail><TakControl><TakRequest version="1"><DetailExt id="…"/>…` (`cotmessage.cpp:761-771`).
- After that request is written, `protoBlockedForResponse = true` — **the client sends nothing further until it gets `t-x-takp-r`** (`:931-935`, `ioWantsWrite()` at `:1685`).
- `t-x-takp-r` with `<TakResponse status="true">` → switch to protobuf framing immediately and re-serialise the queued tx items to v1 (`:1237-1245`, `convertTxToProtoVersion`). `status="false"` (or missing/anything else, `cotmessage.cpp:1441`) → `PROTO_XML_ONLY`.
- **Timeouts** (`:22`, `:1151-1168`): `PROTO_TIMEOUT_SECONDS = 60.0`. Timer is armed at connect (`:843`) and re-armed when the request is sent (`:1228`).
  - timeout while still `PROTO_XML_NEGOTIATE` (server never announced) → **`PROTO_XML_ONLY`, stay connected, XML forever**;
  - timeout while `PROTO_WAITRESPONSE` → return false → interface error → **reconnect** (`:1079-1083`).
- The switch to protobuf reading happens mid-buffer, right after the `</event>` of the accepted response (`:1319-1328`).
- Received `t-x-takp-*` and pong messages are consumed internally and never surfaced to the app (`:1283-1290`).

### 3.5 Framing / resync

Streaming frame: `0xBF` + unsigned varint payload length + payload (`protocol.txt:99-107`; writer at `<R>/commoncommo/core/impl/takmessage.cpp:207-214`, `HEADER_LENGTH` mode; reader at `streamingsocketmanagement.cpp:1338-1412`).

Mesh (UDP/TCP-direct) frame is different: `0xBF` + varint **version** + `0xBF` (`takmessage.cpp:198-206`, `isTAKProtoHeader` at `:66-89`).

- rx buffer / max frame: `rxBufSize = maxUDPMessageSize = 65536` (`streamingsocketmanagement.h:183`, `<R>/commoncommo/core/impl/netsocket.h:19`). A varint length `> 65536` is rejected and the scanner resyncs to the next `0xBF` (`:1367-1371`).
- Resync: any non-`0xBF` byte in `PROTO_HDR_MAGIC` is skipped with a single warning + skip counter (`:1349-1355`).
- If the buffer fills with no frame start, half of it is discarded and proto scanning restarts (`:1416-1427`).
- `CoTMessage` outbound max length is also 65536 (`<R>/commoncommo/core/impl/commo.cpp:999-1005` etc.).

XML mode delimiting is a raw scan for the literal token `"</event>"` (`:32-33`, `:1312-1334`). Outbound XML has its trailing newline stripped, "needed by at least streaming to TAK server" (`<R>/commoncommo/core/impl/cotmessage.cpp:1034-1036`).

### 3.6 Ping/pong, timeouts, reconnect (`streamingsocketmanagement.cpp:22-34`, `:1056-1115`)

```
PROTO_TIMEOUT_SECONDS   = 60.0
DEFAULT_CONN_TIMEOUT_SECONDS = 20.0   (overridable via setConnTimeout)
CONN_RETRY_SECONDS      = 15.0
RESOLVE_RETRY_SECONDS   = 30.0
RX_STALE_SECONDS        = 15.0   // start pinging after this much rx silence
RX_STALE_PING_SECONDS   = 4.5    // repeat ping this often
RX_TIMEOUT_SECONDS      = 25.0   // drop + reconnect
PING_UID_SUFFIX         = "-ping"
```
Ping CoT: type `t-x-c-t`, `how="m-g"`, stale 10 s, point 0/0/0 with ce/le = "no value", **uid = `<device uid>-ping`** (`cotmessage.cpp:153-156`, `:735-745`; `streamingsocketmanagement.cpp:74`, `:1761-1772`). Pong is `t-x-c-t-r`; the client discards it (`cotmessage.cpp:1454-1463`). ATAK's Java layer also short-circuits both types (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/cot/CotMapComponent.java:213-214`, `:653-658`).

Reconnect: on any rx/io/proto error the context is `reset(now + 15s, clearIo=true)`, which clears the tx queue, resets `txQueueProtoVersion = 0`, clears the extension set, and puts `protoState` back to `PROTO_XML_NEGOTIATE` (`:1100`, `:1695-1721`). Hostnames are re-resolved on each retry (`:1701-1704`). Name resolution retries forever at 30 s (`:64`). Connection/handshake timeouts also use 15 s backoff (`:712`, `:732`, `:795`, `:818`).

### 3.7 What the client sends on connect, and what it never requests

Order on an SSL stream: **(1)** the `<auth>` doc if configured; **(2)** whatever SA/PLI/chat the app queues. Nothing else. **The client never sends a query, subscribe, or "hello" of any kind** — no version request, no group request, no contact request over the CoT stream. Everything else (server version, client list, groups, profiles) is fetched over HTTPS on the API port (§2, §5, §8).

Only two CoT classes are registered per stream: `CoTMessageType.CHAT` and `CoTMessageType.SITUATIONAL_AWARENESS` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/CommsMapComponent.java:1806-1809`).

On connect, `CotMapComponent.connected` forces an immediate SA report: `_reportingRate.setReportAsap("Server connected: " + connectString)` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/cot/CotMapComponent.java:1604-1609`). SA cadence is `locationReportingStrategy` = `Dynamic` (default) or `Constant`, gated by `dispatchLocationCotExternal` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/ReportingRate.java:85`, `:117`, `:183-186`, `:350`, `:401-476`).

Incoming stream messages get their contact endpoint rewritten to the streaming sentinel before dispatch (`streamingsocketmanagement.cpp:1461-1466`), and the Java side tags the bundle with `serverFrom = <connect string>` (`CommsMapComponent.java:2202-2210`).

### 3.8 `t-x-*` handling in the app

| Type | Handler | Behaviour |
|---|---|---|
| `t-x-c-t`, `t-x-c-t-r` | `CotMapComponent.java:653-658` | consumed, no-op (native already bumped last-rx) |
| `t-x-takp-v/q/r` | native only (`streamingsocketmanagement.cpp:1170-1263`) | never reaches Java |
| `t-x-d-d` | `<R>/atak/ATAK/app/src/main/java/com/atakmap/android/cotdelete/CotDeleteEventMarshal.java:15` (`COT_TASK_DISPLAY_DELETE_TYPE`) | delete map item(s) named by `<detail><link uid=… relation="none" type="none"/>`; optional `<__forcedelete/>` (documented at `CotDeleteImporter.java:21-22`) |
| `t-x-g-c` | `<R>/atak/ATAK/app/src/main/java/com/atakmap/android/channels/ui/overlay/ChannelsOverlay.java:659-700` | groups-changed: requires `serverFrom` extra; ignored if `event.getUID()` contains our device UID; clears all streamed map items from that server, re-fetches groups with `sendLatestSA=true`, posts a notification, broadcasts `CHANNELS_UPDATED` |
| `t-x-a-m-Geofence` | `<R>/atak/ATAK/app/src/main/java/com/atakmap/android/cot/CotMapAdapter.java:243` | permanently ignored |
| `t-x-c-m` | `<R>/atak/ATAK/app/src/main/java/com/atakmap/android/metricreport/MetricReportMapComponent.java:116` | realtime metrics |
| `t-x-m-*` | — | **absent from this checkout** (see §9) |
| `b-f-t-a` | `CotMapComponent.java:216`, `:661-664` | "quietly consume… no reason to process it" (native handles it) |

### 3.9 Endpoint sentinels (`<R>/commoncommo/core/impl/cotmessage.cpp:729-732`, `:1646-1698`)

```cpp
STREAMING_ENDPOINT   = "*:-1:stcp"
TCP_USESRC_ENDPOINT  = "tcpsrcreply"    // rendered "tcpsrcreply:4242:srctcp"
UDP_USESRC_ENDPOINT  = "udpsrcreply"    // "udpsrcreply:6969:srcudp"
QUIC_USESRC_ENDPOINT = "quicsrcreply"   // "quicsrcreply:4243:srcquic"
```
`*:-1:stcp` is emitted verbatim with **no** port/proto suffix appended (`:1691`). Default mesh ports: udp 6969, tcp 4242, quic 4243 (`:1669-1683`; corroborated by the captures header, `<R>/commoncommo/core/atakcotcaptures.txt:2-4` — non-chat 6969, chat 17012). Parser accepts `udp|quic|tcp|stcp|srctcp|srcudp` (`:1620-1633`).

---

## 4. Protobuf spec

### 4.1 `takproto/` vs `commoncommo/core/impl/protobuf/`

`diff -u` over all ten protos: **nine are byte-identical**. The single difference is `contact.proto`, where the commoncommo copy has one extra field that `takproto/` lacks:
```proto
// altendpoints is optional; if missing/empty do not populat.
string altendpoints = 3;
```
`takproto/` additionally has `README.txt`, which is **byte-identical to** `commoncommo/core/impl/protobuf/protocol.txt` (verified with `diff -q`). `commoncommo/.../protobuf/` has `protocol.txt`; `takproto/` has no `protocol.txt`.

**License headers: none.** Every `.proto` begins with `syntax = "proto3";`. `protocol.txt`/`README.txt` have no header either. A case-insensitive grep for `copyright|license|GPL` across all of them returns nothing. (Repo-level `LICENSE.md` applies, but these files carry no per-file notice.)

### 4.2 Message definitions (facts)

All files: `syntax = "proto3"`, `option optimize_for = LITE_RUNTIME`, `package atakmap.commoncommo.protobuf.v1`. Being proto3 with no `optional` keyword, **nothing is wire-level required**; "required" in the comments is an application-level contract enforced by the encoder (it falls back to opaque XML if it can't satisfy it).

**`takmessage.proto` — `TakMessage`**
| # | type | name | app-level |
|---|---|---|---|
| 1 | `TakControl` | `takControl` | optional — "if omitted, continue using last reported control information" |
| 2 | `CotEvent` | `cotEvent` | optional — "if omitted, no event data in this message" |

**`takcontrol.proto` — `TakControl`**
| # | type | name | app-level |
|---|---|---|---|
| 1 | `uint32` | `minProtoVersion` | optional; 0 ⇒ assume 1 |
| 2 | `uint32` | `maxProtoVersion` | optional; 0 ⇒ assume 1 |
| 3 | `string` | `contactUid` | may be omitted if paired with a `CotEvent` that carries it |
| 4 | `repeated uint32` | `extensionIds` | decode-support advertisement; absent ⇒ supports none |

**`cotevent.proto` — `CotEvent`** ("All items are required unless otherwise noted")
| # | type | name | app-level |
|---|---|---|---|
| 1 | string | `type` | required |
| 2 | string | `access` | optional-by-legacy; empty ⇒ CoT `"Undefined"` |
| 3 | string | `qos` | optional |
| 4 | string | `opex` | optional |
| 5 | string | `uid` | required |
| 6 | uint64 | `sendTime` | required, ms since epoch |
| 7 | uint64 | `startTime` | required, ms |
| 8 | uint64 | `staleTime` | required, ms |
| 9 | string | `how` | required |
| 10 | double | `lat` | required |
| 11 | double | `lon` | required |
| 12 | double | `hae` | required; 999999 = unknown |
| 13 | double | `ce` | required; 999999 = unknown |
| 14 | double | `le` | required; 999999 = unknown |
| 15 | `Detail` | `detail` | optional |
| 16 | string | `caveat` | optional |
| 17 | string | `releasableTo` | optional |

(Note the out-of-order tags: 16/17 were appended later.)

**`detail.proto` — `Detail`**
| # | type | name |
|---|---|---|
| 1 | string | `xmlDetail` |
| 2 | `Contact` | `contact` (`<contact>`) |
| 3 | `Group` | `group` (`<__group>`) |
| 4 | `PrecisionLocation` | `precisionLocation` (`<precisionlocation>`) |
| 5 | `Status` | `status` (`<status>`) |
| 6 | `Takv` | `takv` (`<takv>`) |
| 7 | `Track` | `track` (`<track>`) |
| 8 | `repeated Detail.ExtensionEncodedDetail` | `extensionDetails` |

nested `ExtensionEncodedDetail`: `1 uint32 extensionId`, `2 bytes data`.

**Leaves** (all "required unless noted"):
- `Contact`: `1 string endpoint` (optional), `2 string callsign` (required), `3 string altendpoints` (optional; **commoncommo copy only**)
- `Group`: `1 string name`, `2 string role`
- `PrecisionLocation`: `1 string geopointsrc`, `2 string altsrc`
- `Status`: `1 uint32 battery`
- `Takv`: `1 string device`, `2 string platform`, `3 string os`, `4 string version`
- `Track`: `1 double speed`, `2 double course`

### 4.3 `protocol.txt` in my own words

- **Protocol 0 (legacy).** Mesh SA = one XML `<event>` per UDP datagram to a well-known multicast group. Directed mesh = open TCP, send one XML event, close. Streaming = back-to-back XML events over one TCP socket, split immediately after `</event>`. Outbound events must each start with an XML declaration followed by a newline, and there must be **no** whitespace between one `</event>` and the next `<?xml`.
- **Ground rules.** Anyone who *sends* version V can *receive* V; everyone must be able to decode version 0.
- **Mesh framing (v≥1).** Payload is prefixed by `0xBF`, a varint version, `0xBF`.
- **Streaming framing (v≥1).** Payload is prefixed by `0xBF` and a varint *byte length*; the version is not repeated per message because it was fixed by negotiation.
- **Payload v1.** Exactly one serialised `TakMessage` (protobuf 3). Fields may only be appended to existing messages when ignoring them is semantically harmless for every TAK app; anything else requires a new protocol version. v1 defines no extra negotiation attributes.
- **Detail extensions.** An opt-in mechanism that swaps a whole `<detail>` child element (element plus all descendants — partial handling is forbidden) for an integer-tagged binary blob. IDs are centrally registered; unregistered IDs must not be used in public deployments. Senders may only use an extension the recipient advertised. Receivers should expect to see extensions they didn't advertise (servers relay opaquely) and should flag partially-decoded messages to the operator. The doc warns implementers about absent-vs-default values, out-of-range values, endianness and string encoding.
- **Streaming negotiation.** Connect → exchange XML; if auth is required the auth XML must be the client's very first message, and a rejected auth closes the connection. The server *may* send `t-x-takp-v` once (and at most once) per connection advertising one or more `<TakProtocolSupport version="N">`, each optionally carrying `<DetailExt id="…"/>` entries or a single `<DetailExt supportsAll="true"/>`. The client may then send exactly one `t-x-takp-q` carrying one `<TakRequest version="N">` (with its own `<DetailExt id>` list; `id="*"` is not legal here) — and must not have sent it without first seeing the offer. After sending the request the client must stop sending CoT and wait at least a minute; the server must answer with `t-x-takp-r` / `<TakResponse status="true|false"/>` as soon as it notices the request, and must keep looking for requests for at least a minute after its offer (and for a minute after any `false`). On `true`, both directions switch to framed protobuf at the negotiated version immediately — the server must not emit more XML. On `false`, both sides carry on in XML and the client may retry. No response before the client's timeout ⇒ the client disconnects and starts over. The negotiation UID is minted by the server in the offer and echoed by both sides in the request and response, and must not collide with any other UID.
- **Mesh negotiation.** Every device supporting v>0 broadcasts a `TakControl` at least once a minute (alone or alongside a CotEvent) stating its min/max decodable version and its decodable extension IDs. Each device tracks per-peer min/max: a newly seen peer is assumed to speak only the version its discovering message arrived in; a `TakControl` updates it; a peer silent for two minutes reverts to the version of its most recent message. Broadcasts go out at the highest version *every* known peer supports (falling back to 0/XML if there's no overlap), using only the extension set common to all peers (a sender may use fewer). Any change to the computed version forces an immediate `TakControl` broadcast.
- **Varints.** Standard protobuf unsigned varint: 7 bits at a time, LSB group first, continuation bit set on all but the last group. Values are non-negative, limited to 64 bits — effectively `[0, 2^63-1]`, at most 10 bytes.

### 4.4 Encoder/decoder facts worth mirroring

Encoding (`cotmessage.cpp:1045-1342`): `access`, `caveat`, `releasableTo`, `qos`, `opex` are each best-effort. For each of the six strong-typed details, the encoder requires **all** the message's fields to be present as attributes, **no** leftover attributes or child nodes on that element, and **exactly one** occurrence — any violation clears that submessage and leaves the element in `xmlDetail` (`:1132-1146`, `:1155-1161`, etc.). `contact.endpoint` and `contact.altendpoints` are individually optional; `callsign` is mandatory. Remaining `<detail>` children are serialised (no `<detail>` wrapper, no XML header) into `xmlDetail`; if nothing at all survives, `detail` is cleared (`:1276-1332`).

Decoding (`:839-1025`): `xmlDetail` is re-wrapped as `<?xml version="1.0" encoding="UTF-8"?><detail>` + blob + `</detail>` (`:865-867`). Extensions are decoded and appended as children; a decode failure is **non-fatal**, just logged and skipped (`:941-946`). Each strong-typed submessage is only materialised if an element of that name isn't already in `xmlDetail` (`:954`, `:962`, …) — "xmlDetail wins", exactly as `detail.proto:53-56` specifies. `TakControl.minProtoVersion`/`maxProtoVersion` of 0 are coerced to 1 (`takmessage.cpp:294-297`).

---

## 5. Channels

### 5.1 URLs and payloads

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/channels/net/GetAllServerGroupsOperation.java:51-61`:
```java
final String baseUrl = "https://" + groupsRequestRequest.getServer();  // bare host
httpClient = new TakHttpClient(baseUrl, connectString);                // ⇒ + ":8443/Marti"
String url = "/api/groups/all?useCache=true";
if (sendLatestSA) url += "&sendLatestSA=true";
```
→ `GET https://<host>:8443/Marti/api/groups/all?useCache=true[&sendLatestSA=true]`, client-cert auth, `response.verifyOk()` (2xx OK/Created only).

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/channels/net/SetActiveServerGroupsOperation.java:53-59`:
```java
HttpPut httpPut = new HttpPut(httpClient.getUrl("/api/groups/active?clientUid=" + MapView.getDeviceUid()));
httpPut.addHeader("content-type", "application/json");
httpPut.setEntity(new StringEntity(activeGroups, UTF-8));
```
→ `PUT https://<host>:8443/Marti/api/groups/active?clientUid=<device uid>`, header literally lower-case `content-type: application/json`.
**Yes, `clientUid` is sent on `PUT /groups/active`.**
Body is `ServerGroup.toResultJSON(list).toString()` — a **bare JSON array**, not a `{type,data}` envelope (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/channels/net/ServerGroupsClient.java:86-87`; `ServerGroup.java:125-134`). Each element is (`ServerGroup.java:106-123`):
```json
{"name":…, "distinguishedName":…, "direction":…, "created":<long ms>, "type":…, "bitpos":<int>, "active":<bool>}
```
Note the asymmetry: **`created` is serialised as epoch milliseconds on PUT, but parsed as a formatted string on GET.** `description` is not serialised. Invalid groups are silently dropped from the array.

**`/groups/groupCacheEnabled` is never called** — grep for `groupCacheEnabled` over the whole checkout returns nothing.

### 5.2 Parsing (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/http/rest/ServerGroup.java`)

Envelope (`:190-215`): top level must have `"type"` exactly equal to
```java
GROUP_LIST_MATCHER = "com.bbn.marti.remote.groups.Group"   // :22
```
otherwise `JSONException`. Then `json.getJSONArray("data")`. An empty/missing array yields an empty list; **any element that fails `isValid()` aborts the entire parse** (`:209-211`).

Per element (`:136-188`):
- `created` — `obj.getString("created")` (must be a **JSON string**, `getString` on a number would coerce, but on a missing key it throws), parsed with `KMLUtil.KMLDateFormatter.get().parse(...)`; a `ParseException` throws `JSONException("Unable to parse created time")` and kills the whole response.
- mandatory (`getString`/throw): `name`, `direction`, `type`, `created`.
- optional/defaulted: `active` (default **true**), `bitpos` (default **-1**), `description` (null), `distinguishedName` (null).

`isValid()` (`:98-104`): `name`, `direction`, `type` all non-empty **and** `created >= 0` **and** `bitpos >= 0`. So **a group with no `bitpos` is rejected**, and the default of -1 makes `bitpos` effectively mandatory.

**`KMLUtil.KMLDateFormatter` is not in this checkout** — I cannot state its pattern as fact. What I *can* state:
- It is a `SimpleDateFormat`-family thread-local (`.get().parse(String)` returning `Date`), so `parse()` is lenient about **trailing** garbage but not about a prefix mismatch.
- Whether a bare `yyyy-MM-dd` parses, and whether a full ISO datetime parses, depends entirely on that missing pattern. **Unverified — do not rely on my guess.** The safest server behaviour is to emit whatever real TAK Server emits for this field.
- For contrast, a format that *is* visible: `MissionPackageQueryResult.TIMESTAMP_FORMAT = "yyyy-MM-dd'T'HH:mm:ss.SSS'Z'"` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/http/datamodel/MissionPackageQueryResult.java:29-31`) and `ServerContact` uses `KMLUtil.KMLDateTimeFormatterMillis` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/http/rest/ServerContact.java:225`). Given `created` uses the *other* formatter (`KMLDateFormatter`, not `…DateTimeFormatterMillis`), a date-only or second-precision form is plausible — but that is inference, not verified.

### 5.3 When ATAK fetches groups

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/channels/ui/overlay/ChannelsOverlay.java`:
- **construction of the overlay** (which happens when the Channels UI is enabled): `getGroups(null, false)` for every connected server (`:143`, `:165-188`);
- **on stream connect**: `CotStreamListener.connected(port, true)` → `getAllGroups(ctx, port.getConnectString(), sendLatestSA=false, …)` (`:95-101`);
- **on stream output update** when both enabled and connected (`:104-118`);
- **on `t-x-g-c`**: `getGroups(host, true)` — i.e. `sendLatestSA=true` (`:691`).

`ServerGroupsClient.getAllGroups` drops a request if one is already in flight (`ServerGroupsClient.java:62-65`). `setActiveGroups` is rate-limited by a `LimitingThread` with `SET_GROUPS_TIMEOUT` (`ChannelsOverlay.java:120-141`). After a successful PUT, ATAK broadcasts `ChannelsReceiver.CHANNELS_UPDATED` with extra `"server" = <host>` (`ServerGroupsClient.java:145-150`).

### 5.4 Preferences gating

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/channels/ChannelsMapComponent.java:22-24`:
```java
PREFERENCE_ENABLE_CHANNELS_UI_KEY            = "prefs_enable_channels"
PREFERENCE_ENABLE_CHANNELS_HOST_KEY          = "prefs_enable_channels_host"
PREFERENCE_ENABLE_CHANNELS_HIERARCHY_HOST_KEY = "prefs_enable_channels_hierarchy_host"
```
- `prefs_enable_channels` — **boolean**, default false; toggles creation/destruction of the whole overlay + nav button (`:74-95`).
- `prefs_enable_channels_host-<host>` — read as a **String** compared to `"true"`, default `"false"`; per-host gate for showing that server in the Channels list (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/channels/ui/overlay/ChannelsOverlayListModel.java:107-113`; also `ChannelsOverlay.java:624-630`). Note the `"-"` separator.
- `prefs_enable_channels_hierarchy_host-<host>` — same String/`"true"` convention; enables the DN-hierarchy grouping using `distinguishedName` (`ChannelsOverlay.java:449-460`).
- `ChannelsPrefs.ASSOCIATION_KEY = "channelsPreference"` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/channels/prefs/ChannelsPrefs.java:5`).

---

## 6. Mission packages / file share

### 6.1 Server search

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/http/rest/QueryMissionPackageOperation.java:63-69`:
```
GET https://<host>:8443/Marti/sync/search?keywords=missionpackage[&tool=<tool>]
```
Response is checked with `client.get(queryUrl, "resultCount")` — the raw body **must contain the literal substring `resultCount`** or `TakHttpResponse.getStringEntity(verify)` fails.

Parsing (`<R>/…/http/datamodel/MissionPackageQueryResult.java:255-273`): top level key `"results"` (a JSON array). Per element (`:176-201`), **case-sensitive**:
- mandatory: `UID` (string), `Name` (string), `Hash` (string), `PrimaryKey` (**int**), `SubmissionDateTime` (string)
- optional: `SubmissionUser`, `CreatorUid`, `Keywords`, `MIMEType` (strings), `Size` (long)

`isValid()` (`:67-73`): `UID`, `Name`, `Hash`, `SubmissionDateTime` non-empty and `PrimaryKey >= 0`. **Any invalid element throws and kills the whole response** (`:269-271`).

`SubmissionDateTime` is parsed with `new SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'", LocaleUtil.getCurrent())` (`:29-31`). A parse failure is **non-fatal** — it just leaves `SubmissionDateTimeLong = -1` and falls back to lexicographic comparison in `isNewerThan` (`:112-118`, `:132-146`). Note: the `'Z'` is a *literal*, and no timezone is set on the formatter, so it is parsed in device-local time.

### 6.2 Upload (`<R>/commoncommo/core/impl/missionpackagemanager.cpp:1086-1220`)

Base URL is built from the **streaming interface's resolved IP** (not hostname) plus the configured http/https port, `https` iff the stream is SSL, `+ "/Marti"` (`:1092-1119`). `CURLOPT_SSL_VERIFYHOST 0` (`:1113`). Client cert comes from the stream's SSL config via `configSSLForConnection`.

**CHECK:**
```
GET  https://<ip>:8443/Marti/sync/missionquery?hash=<sha256>
```
**UPLOAD:**
```
POST https://<ip>:8443/Marti/sync/missionupload?hash=<sha256>&filename=<name>&creatorUid=<our uid>
Content-Type: multipart/form-data
  part name     : "assetfile"
  part filename : <filename>
  part type     : "application/x-zip-compressed"
```
(`:1140-1168`; part name at `:1155`.) **Expected response: the body is the URL of the stored package** — it is captured into `urlFromServer` by the write callback and then used verbatim as `senderUrl` in the outgoing `b-f-t-r` (`:1276`, `:1293`, `:1299`).

**TOOLSET (third step):**
```
PUT https://<ip>:8443/Marti/api/sync/metadata/<sha256>/tool
Content-Type: text/plain
body: "private"   (when sending to contacts)  |  "public"  (server-only upload)
```
(`:1182-1195`.) Failure here is deliberately tolerated — "we expect it to fail on older server versions and won't consider it an overall failure anyway" (`:1208-1209`).

`PostMissionPackageOperation` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/http/rest/PostMissionPackageOperation.java:58-100`) is only a wrapper: it temporarily enables/connects the stream (waiting up to 10× 1 s), calls `CopyAndSendTask.postPackage(...)` → `CommsMapComponent.sendMissionPackage(...)` → the native path above, and returns the posted URL.

### 6.3 Download

Two distinct paths.

**(a) Peer/server-pushed `b-f-t-r` → native** (`missionpackagemanager.cpp:1600-1776`): `GET <senderUrl>` with `&receiver=<our callsign>` appended (`:1733-1737`). **`http://` is accepted as-is**; a client certificate is installed only when the URL is `https:` and `peerHosted` is false (`:1759-1769`) — in which case the cert comes from the *sender's* streaming endpoint, falling back to the endpoint the CoT arrived on (`:2328-2365`). For `peerHosted` https, `SSL_VERIFYPEER` is set to 0. **Hostname verification is off in every case** (`:1768`). If `peerHosted` and `httpsPort` is set, an `http://` senderUrl is rewritten to `https://` on that port (`:2311-2318`). A senderUrl containing `/Marti/` marks the transfer as server-hosted (`:2319-2320`); otherwise the host may be rewritten to the sender's known endpoint IP (`:1650-1670`), and QUIC peers get the URL rewritten from `/getfile` to a local `/qprox` proxy (`:1678-1706`).

**(b) User picks a result from `/Marti/sync/search` → Java** (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/http/rest/GetFileTransferOperation.java:145-171`): it **ignores `senderUrl`** and constructs
```
GET https://<host>:8443/Marti/sync/content?hash=<sha256>[&offset=<n>]
```
from the `FileTransfer`'s connect string, `http`+8080 if the stream is not SSL (`:150-155`). Resume via `offset` on retry (`:105-120`). Spaces → `%20` (`:171`). It first tries with **no** auth, and on `401` retries with preemptive Basic (`:187-194`).

### 6.4 `b-f-t-r` construction (`<R>/commoncommo/core/impl/cotmessage.cpp:328-400`, `:145-151`)

type `b-f-t-r`, `how="h-e"`, stale **10 s**, event uid = a fresh UUID (`missionpackagemanager.cpp:741`, `:762`).
```xml
<detail>
  <fileshare filename=… senderUrl=… sizeInBytes=… sha256=… senderUid=… senderCallsign=… name=…
             [peerHosted="true"] [httpsPort="…"]/>
  <ackrequest uid=<ackUuid> ackrequested="true" tag=<name>/>
</detail>
```
`<ackrequest>` is emitted only when `ackuid` is non-empty (`:389-397`).

**Inbound parsing** (`:544-611`) requires, via `checkedGetProp` (throws if absent): `name`, `sha256`, `filename`, `senderUrl`, `sizeInBytes`, `senderUid`, `senderCallsign`. Optional: `peerHosted` (`=="true"`), `httpsPort`. `<ackrequest>` is honoured only when `ackrequested` is present, and then `uid` is mandatory (`:600-605`).

### 6.5 `b-f-t-a` ack

Construction (`cotmessage.cpp:266-326`, `:149-151`): type `b-f-t-a`, `how="m-g"`, stale 10 s, **event uid = the receiver's own contact UID** (`missionpackagemanager.cpp:1519-1520`).
```xml
<detail>
  <ackresponse uid=<ackrequest uid> senderUid=<receiver uid> success="true|false"
               tag=<name> reason=<message> sha256=… sizeInBytes=…/>
</detail>
```
Sent directly to `req->senderuid` via the contact manager (`missionpackagemanager.cpp:1526-1528`), i.e. over whichever endpoint that contact is currently reachable on — including via the TAK server with a `<marti><dest callsign>` (see §7.5).

> The real ATAK capture at `<R>/commoncommo/core/atakcotcaptures.txt:32` uses the **ackrequest uid** as the event uid, and includes `<contact callsign=…>` and `<precisionlocation>` in the detail. Implementations differ here; don't key off the event uid.

Inbound handling (`missionpackagemanager.cpp:233-248`): if `getFileTransferAckUid()` is non-empty the message is consumed as an ack and **returns early** — it never falls through to file-transfer-request handling. ATAK's Java layer separately swallows `b-f-t-a` (`CotMapComponent.java:216`, `:661-664`).

### 6.6 Manifest schema

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/file/`, Simple-XML annotated.

`MissionPackageManifest.java:46-84`:
```java
@Root(name = "MissionPackageManifest")
@Attribute(name = "version", required = true) private int VERSION = 2;
@Element(name = "Configuration", required = true) MissionPackageConfiguration _configuration;
@Element(name = "Contents",      required = true) MissionPackageContents _contents;
```
`MissionPackageConfiguration.java:24-85`: `@ElementList(entry = "Parameter", inline = true, required = false)`.
`MissionPackageContents.java:21-27`: `@ElementList(entry = "Content", inline = true, required = false)`.
`MissionPackageContent.java:26-52`: extends Configuration, adds `@Attribute(name="zipEntry", required=true)` and `@Attribute(name="ignore", required=false)`.
`NameValuePair.java:22-26`: `@Attribute(name="name", required=true)`, `@Attribute(name="value", required=true)`.

So the document is:
```xml
<MissionPackageManifest version="2">
  <Configuration>
    <Parameter name="uid" value="…"/>
    <Parameter name="name" value="…"/>
    …
  </Configuration>
  <Contents>
    <Content zipEntry="…" [ignore="false"]>
      <Parameter name="localpath" value="…"/>
      …
    </Content>
  </Contents>
</MissionPackageManifest>
```

**Configuration parameter names** (`MissionPackageConfiguration.java:29-81`):
`name`, `uid`, `remarks`, `onReceiveDelete`, `onReceiveImport`, `deleteWithPackage`, `onReceiveAction`.
`isValid()` = has `name` **and** `uid` (`:98-100`).

**Content parameter names** (`MissionPackageContent.java:31-40`):
`name`, `localpath`, `isCoT`, `contentType`, `visible`, `refContent` (plus `uid`, inherited/used at `MissionPackageManifest.java:529`).
`MissionPackageContent.isValid()` = `zipEntry` non-empty (`:79-82`). `MissionPackageContents.isValid()` always true (`:38-40`). `MissionPackageManifest.isValid()` = both children valid (`:136-138`).

Manifest location: `MANIFEST/manifest.xml` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/missionpackage/file/MissionPackageBuilder.java:33-35`). Every content path is **relative to the directory containing `MANIFEST/`**, so a package may be nested one level (the "Windows right-click compress" case) — documented at `MissionPackageManifest.java:73-78`.

### 6.7 `.pref` and `.p12` inside a package

Extraction is driven by `MissionPackageExtractorFactory.Extract(context, file, root, importFlag)` (called from `DeviceProfileOperation.java:406-408`). The extractor itself contains no `.pref`/`.p12` special-casing — files are written out and then picked up by the `ImportResolver` sorters:
- `.pref` → `ImportPrefSort` / `ImportPrefResolver`, destination `PreferenceControl.DIRNAME`, sniffed for `<preferences` + `<preference key`/`<entry key` (§2.5).
- `.p12` → `ImportCertResolver`, destination `<root>/cert`, `IMPORT_COPY` forced; `finalizeImport()` then wires certs/passwords into the credential DB and may auto-trigger enrollment (§2.6).

---

## 7. Contacts & GeoChat

### 7.1 Contacts from the server

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/http/rest/operation/GetClientListOperation.java:99-101`:
```java
String queryUrl = client.getUrl("api/clientEndPoints");
String responseBody = client.getGZip(queryUrl, queryRequest.getMatcher());
```
→ `GET https://<host>:8443/Marti/api/clientEndPoints` with `Accept-Encoding: gzip`. **No query parameters** (there's a TODO at `:98` noting the endpoint supports some it doesn't use). Body must contain the matcher string.

Parsing (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/http/rest/ServerContact.java:258-284`): top-level `"type"` must equal
```java
CLIENT_LIST_MATCHER = "com.bbn.marti.remote.ClientEndpoint"   // GetClientListRequest.java:23
```
then `"data"` array. Per element (`:219-250`), mandatory:
- `uid` (string)
- `callsign` (string)
- `lastEventTime` (string, parsed by `KMLUtil.KMLDateTimeFormatterMillis`; parse failure ⇒ `JSONException` ⇒ **whole response rejected**)
- `lastStatus` (string, must be **exactly** `"Connected"` or `"Disconnected"` — `LastStatus.valueOf`, `:233-242`; anything else rejects the whole response)

`isValid()` (`:105-111`): `uid` and `callsign` non-empty, `lastEventTime > 0`, `syncTime > 0`, `server != null`. Any invalid element aborts the parse (`:278-279`).

**There is no `username` field** in `ServerContact` — it is neither read nor stored. (Requested item: not found.)

The same response body is *also* fed to `ServerVersion.fromJSON` to pick up the legacy API version (`GetClientListOperation.java:110-113`) — so the JSON must carry a top-level `"version"` integer.

Fetched **once per connect string per ATAK run**, only when no contact list is cached (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/cot/CotMapServerListener.java:124-150`; explicit TODO at `:141-143` about it never refreshing). Results are stashed in `serverContactMap` keyed by connect string (`:55`, `:458-480`) and used for uid→callsign lookup (`:306-346`).

The live contact list itself is built from **SA** (`a-f-G-U-C…` events) via `CotMapAdapter`/`Contacts`; `ServerContact` only supplements it with server-known/offline users.

### 7.2 GeoChat `b-t-f` construction

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/chat/GeoChatService.java:508-582` (`bundleToCot`) + `:390-481` (`addSendDetails`).

Event: `type="b-t-f"`, `version="2.0"`, `how="h-g-i-g-o"`, `time=start=now`, `stale = now + 1 day` (`:513-521`). `access`/`caveat`/`releasableTo` from prefs (`:523-534`).

**UID** (`:549`):
```java
cotEvent.setUID("GeoChat." + from + "." + id + "." + messageId);
```
where `from` = sender uid (default `"Android"`), `id` = conversationId (random UUID if absent), `messageId` = random UUID if absent.

Point: self position, or `CotPoint.ZERO` if `dispatchLocationCotExternal` is false or `dispatchLocationHidden` is true (`:416-426`).

**`<__chat>` attributes** (`:431-460`):
| attr | value |
|---|---|
| `id` | conversationId |
| `messageId` | messageId |
| `senderCallsign` | sender callsign (falls back to sender uid) |
| `chatroom` | conversationName, default `"All Chat Rooms"` |
| `parent` | `chatMessage["parent"]` (may be null) |
| `groupOwner` | `"true"`/`"false"`, default false |
| `deleteChild` | only if set |
| `tadilj` | `"true"` only if set |

Child `<chatgrp>` (`:442-448`): attribute `id` = conversationId, then `uid0` = **our own uid**, `uid1..uidN` = each destination in order.
Optional child `<hierarchy>` built from the `paths` bundle (`:463-468`).

**Sibling `<link>`** (`:472-476`): `uid` = our uid, `type` = our CoT type (default `"a-f"`), `relation="p-p"`.

**Sibling `<__serverdestination>`** (`:478-480`): `destinations = "<our IP>:4242:tcp:<sender uid>"` (built at `:405`). Comma-separated when there are several.

**`<remarks>`** (`:554-571`):
```java
remarks.setAttribute("source", "BAO.F.ATAK." + from);
// "to" is set only when every non-self destination equals the conversation id:
if (dest.equals(id)) remarks.setAttribute("to", dest); else { removeAttribute("to"); break; }
remarks.setAttribute("time", time.toString());
remarks.setInnerText(message);
```
So `to=` appears on **direct** messages only, never on group/room chat.

### 7.3 Routing, `<marti><dest>` and "All Chat Rooms"

`DEFAULT_CHATROOM_NAME = "All Chat Rooms"`, legacy alias `DEFAULT_CHATROOM_NAME_LEGACY = "All Streaming"` (`GeoChatService.java:52-53`). Both are normalised to the modern name on receive (`ChatMessageParser.java:83-84`, `:94-95`).

Send routing (`GeoChatService.java:700-727`): destinations flagged `fakeGroup` (the All-Chat-Rooms pseudo-contact) or `TadilJContact` cause `dispatchToBroadcast(cotEvent)`; everything else goes to `dispatchToContacts(cotEvent, recipients)`.

- **Broadcast** → `commo.broadcastCoT(...)` → over a stream this is sent with **no `<marti>` element at all**, which means "everyone" (`<R>/commoncommo/core/impl/commo.cpp:1011-1012` passes an empty recipient vector; `cotmessage.cpp:1792-1800` removes any existing `<marti>` for the empty case).
- **Directed** → `ContactManager::sendCoT` (`<R>/commoncommo/core/impl/contactmanager.cpp:602-754`). Contacts are bucketed by endpoint type. For STREAMING contacts it groups by stream endpoint and, per stream, sends a **copy** with:
  ```cpp
  sMsg.setEndpoints(ENDPOINT_STREAMING, "", NULL);   // contact endpoint → "*:-1:stcp"
  sMsg.setTAKServerRecipients(&v);                   // v = the contacts' CALLSIGNS
  ```
  (`:736-741`). `setTAKServerRecipients` emits (`cotmessage.cpp:1786-1814`):
  ```xml
  <marti><dest callsign="CALLSIGN1"/><dest callsign="CALLSIGN2"/>…</marti>
  ```
  — **callsign, not uid**, and any pre-existing `<marti>` children are removed first.
- For **peer** (udp/tcp/quic) sends, `<__dest uid="…">` is inserted instead (`contactmanager.cpp:646-648`, `cotmessage.cpp:1763-1784`), and it is **explicitly stripped before any streaming send** (`contactmanager.cpp:723-725`, comment: "not desired for streaming traffic"). ATAK also strips `__dest` from internally-looped events (`CommsMapComponent.java:2238-2244`).
- Mission-destined sends produce `<marti><dest mission="…"/></marti>` (`cotmessage.cpp:1816-1829`).

### 7.4 Receiving / matching to conversations

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/chat/ChatMessageParser.java`:
- A message is GeoChat iff `cotEvent.getType().startsWith("b-t-f")` — **`<__chat>` is not required** (`GeoChatService.java:587-595`, comment: "non-ATAK devices don't necessarily specify this").
- Version sniff (`ChatMessageParser.java:255-263`): presence of a `<chatgrp>` child ⇒ `CHAT3`, else `GEO_CHAT`.
- Conversation UID (`:232-254`): `CHAT3` ⇒ `__chat@id` (with `"Streaming"` mapped to the legacy all-rooms name); `GEO_CHAT` ⇒ look up a contact whose **callsign** equals `__chat@chatroom`.
- Fallback when that yields nothing (`:377-390`): split `event@uid` on `.` — index 1 if the body contains `<origin uid=`, else index 2; final fallback `"All Chat Rooms"`.
- Sender uid (`:396+`): `uid.split(".")[1]`.
- `messageId` (`:355-375`): `__chat@messageId` if present, else the **last** dot-separated component of the event uid.
- `conversationName` = `__chat@chatroom`; if `convId == our device uid`, it is rewritten to the *sender's* contact name/uid so the conversation shows as a DM from them (`:96-104`).
- Destinations (`:300-341`): every `<chatgrp>` attribute whose name starts with `uid` (dedup'd, order preserved). If `<chatgrp>` is absent, fall back to `<__serverdestination destinations="…">`, split on `,`; entries starting with `udp` map to the all-rooms contact or are re-ordered `host:port:proto`; four-colon entries yield the trailing uid; and if there is **no** `__serverdestination` at all the destination defaults to `"All Chat Rooms"` (`:341`).
- Message text = the first `<remarks>` child's inner text (`:265-274`).

### 7.5 Receipts

`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/chat/ChatLine.java:56-66`:
```java
public enum Status { ..., DELIVERED("b-t-f-d"), READ("b-t-f-r"), PENDING("b-t-f-p"); }
```
All three exist. Construction (`GeoChatService.java:322-382`):
- event `type = receipt.cotType`, **`uid = messageId`** (the message being acked), `version="2.0"`, `how="m-g"`, stale = +1 day.
- sender/recipient are swapped; `parent` = the contacts root group uid.
- details are built with `addSendDetails("__chatreceipt", …)` — i.e. the *same* structure as a chat, but the main element is named **`<__chatreceipt>`** instead of `<__chat>` (`:378`, `:431`), still with `<chatgrp>`, `<link>` and `<__serverdestination>` siblings. No `<remarks>`.
- Sent point-to-point via `dispatchToContact(cotEvent, sender)` (`:380-381`).

READ receipts are suppressed for self-chat, for group chats, and for messages with no `messageId`/`senderUid`; the rule is `line.senderUid.equals(line.conversationId)`, i.e. **direct messages only** (`:299-315`). Receipts are never sent to ourselves (`:330-332`).

---

## 8. HTTP client conventions

`<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/http/TakHttpClient.java` + `HttpUtil.java`.

**Base URL** (`TakHttpClient.java:61-106`): given `url`, if it starts with `https` (case-insensitive) → `_baseUrl = url + SslNetCotPort.getServerApiPath(SECURE)` = `url + ":8443/Marti"`, and the socket factory carries the client certificate for that connect string. Otherwise → `url + ":8080/Marti"` and a plain client with **preemptive Basic auth**. `getUrl(path)` inserts a `/` only when neither side has one (`:206-216`).

So: **https ⇒ client cert, no Basic; http ⇒ Basic, no cert.** `useBasicAuth()` is literally `!_baseUrl.startsWith("https")` (`:223-226`). Basic can still be forced per request via `execute(request, credentials)` (`:413-420`) or `execute(request, true)`.

**Timeouts** (`HttpUtil.java:59-60`): `DEFAULT_CONN_TIMEOUT_MS = 10000`, `DEFAULT_SO_TIMEOUT_MS = 15000`. HTTP/1.1 forced (`:183-184`).

**Basic auth header** (`HttpUtil.java:228-247`): `Authorization: Basic ` + `Base64(user:pass, NO_WRAP)`, UTF-8. Credentials come from `TLSUtils.getCredentials(host, /*useDefault*/ true)` → `AtakAuthenticationCredentials.TYPE_COT_SERVICE` for that host, falling back to the default entry (`HttpUtil.java:200-201`; `<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/app/TLSUtils.java:183-240`).

**`User-Agent`: none.** No `CURLOPT_USERAGENT` in commoncommo and no `User-Agent` header anywhere in the Java HTTP layer — Apache HttpClient's default is used. Do not key server behaviour off UA.

**`Accept`**: set only where explicitly requested — `HttpUtil.MIME_XML`/`MIME_JSON`/`MIME_ZIP` (`HttpUtil.java:55-57`), applied via `TakHttpClient.get(url, verify, accept)` (`:324-331`). Enrollment's sign step sends `Accept: application/xml` (§1.2). `getGZip(...)` adds `Accept-Encoding: gzip` (`:311-314`) and the response is transparently inflated (`TakHttpResponse.checkGZip`, `:130-136`).

**Scheme registry** (`HttpUtil.java:102-109`): only `https` is registered by default; plain `http` is registered **only** when `setGlobalHttpPermissiveMode(true)` has been called (`:104-106`, `:280-282`).

**Hostname verification**: chosen by the caller when building the socket factory — `CertificateManager.getSockFactory(bUseDefault, url|ncs, allowAllHostnames)`. Device profiles pass `allowAllHostnames` only for the 8446 path (`DeviceProfileOperation.java:182-187`). `CertificateManager` itself isn't in this checkout, so the default is unverified.

**`ServerVersion`** (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/http/rest/ServerVersion.java`):
```
GET https://<host>:8443/Marti/api/version/config   (getConfig=true)
GET https://<host>:8443/Marti/api/version          (getConfig=false)
```
with gzip and a body-substring check: `"ServerConfig"` for the config form, `"TAK Server"` for the plain form (`<R>/…/request/GetServerVersionRequest.java:23-24`, `:32-36`; `<R>/…/operation/GetServerVersionOperation.java:75-82`).

`fromJSON` (`ServerVersion.java:94-128`): reads top-level `"version"` as an **int** → that becomes `apiVersion`; then requires `"type" == "ServerConfig"` and `data.version` (a **string**) → that becomes the display version. A missing/invalid `type` or `data` yields a `ServerVersion` with just the api number. Documented example at `:17-25`:
```json
{"version":"2","type":"ServerConfig","data":{"version":"1.3.12.156-DEV","api":"2","hostname":"localhost"}}
```
`MPT_TOOL_PARAM_MIN_VERSION = 2` gates whether the `tool` query param is sent on sync search (`:31`, `:136-152`). `TAKServer.setServerVersion(ServerVersion)` stores both under `serverVersion`/`serverAPI` (`TAKServer.java:89-104`). The version fetch happens on **every** connect (`CotMapServerListener.java:117`).

**`GetCotEventOperation`** (`<R>/…/operation/GetCotEventOperation.java:66-70`): `GET <base>/api/cot/xml/<uid>` (note: **no** leading slash — relies on `getUrl`'s separator logic). Response is scanned for `&#x…;` numeric entities and manually decoded (`:74-93`), then `CotEvent.parse`.

**`GetCotHistoryOperation`** (`<R>/…/operation/GetCotHistoryOperation.java:75-97`): `GET <base>/api/cot/xml/<uid>/all[?start=<t>][&end=<t>]` with times formatted by `KMLUtil.KMLDateTimeFormatterMillis`, URL sanitised, gzip, `Accept: text/xml`. Optionally parses the concatenated events.

**`GetFilesOperation`** (`<R>/…/operation/GetFilesOperation.java:53-55`): just loops requests through `TakHttpClient.GetHttpClient(request.getUrl(), request.getServerConnectString())` and `GetFileOperation.GetFile(...)`.

**`TakServerHttpsProtocolHandler`** (`<R>/atak/ATAK/app/src/main/java/com/atakmap/android/network/TakServerHttpsProtocolHandler.java:22-53`): registers for scheme `https`; builds a `TakHttpClient` from `scheme://host` **only** (so `_baseUrl` gets `:8443/Marti` appended, but the request itself uses the caller's full URL), GETs it, returns the stream on 2xx.

---

## 9. Data Sync / Mission API

**Not present in this checkout.** Verified by grepping the whole tree:
- `api/missions` — **0 hits**
- `t-x-m-` — **0 hits**
- `MissionAuthorization` — 0 hits
- `MissionApi` — 0 hits
- `datasync` / `DataSync` — 0 hits in `com/atakmap/**` (only unrelated `CommsProvider`/plugin-registry matches for the broader grep pattern)

There is **no handling of any `t-x-m-*` CoT type in `CotMapComponent` or anywhere else** in this checkout.

What *does* exist is the send-side plumbing for mission-addressed CoT:
- `CommsMapComponent.sendCoTToServersByMission(uniqueIfaceKey, …)` → `commo.sendCoTToServerMissionDest(id, mission, event)` (`<R>/atak/ATAK/app/src/main/java/com/atakmap/comms/CommsMapComponent.java:2066`, `:2092`; `DefaultCommsProvider.java:344-346`; base `CommsProvider.java:362-364` logs "not implemented").
- Native: `Commo::sendCoTToServerMissionDest` → `msg.setEndpoints(ENDPOINT_STREAMING,…)` + `msg.setTAKServerMissionRecipient(mission)` (`<R>/commoncommo/core/impl/commo.cpp:1106-1131`), which emits `<marti><dest mission="…"/></marti>` (`<R>/commoncommo/core/impl/cotmessage.cpp:1816-1829`).
- `sendCoTToServersOnly` → `sendCoTServerControl` also exists (`CommsMapComponent.java:2119-2142`).

Conclusion: a wire-compatible server must accept `<marti><dest mission="…"/>` routing, but this checkout gives no evidence about the Mission/Data-Sync REST API or its CoT notifications.

---

## 10. Captures (`<R>/commoncommo/core/atakcotcaptures.txt`)

### 10.1 Distinct message types present

| type | count | lines |
|---|---|---|
| `a-f-G-U-C` | 8 | 54, 99, 167, 184, 203, 241, 286 (+ 1 in `<link type>` at 367) |
| `a-f-G-U-C-I` | 6 | 148, 222, 269, 304, 322, 339 |
| `b-f-t-r` | 4 | 18, 70, 117 |
| `b-f-t-a` | 1 | 32 |
| `b-t-f` | 1 | 358 |

Also noted in the header (`:2-4`): ATAK's mesh listen ports are **6969** (non-chat) and **17012** (chat); multicast group `239.2.3.1:6969` for SA, `224.10.10.1:17012` for chat (`:284`, `:356`). And `:6-8` states that the **event `uid` identifies the message, not the originator** — only treat a uid as a contact identity when `<contact>` is present.

### 10.2 Server-side rewrites visible (paraphrased, not copied)

**`_flow-tags_`.** Two events that traversed a TAK server carry an extra, otherwise-absent child of `<detail>` named `_flow-tags_`. It has a single attribute whose *name* is the server's identifier (here a name ending in `1`) and whose *value* is an ISO-8601 UTC timestamp with millisecond precision — the moment that server handled the event. The timestamps sit a few seconds to a minute *before* the event's own `time` attribute in these samples, and the events are otherwise byte-for-byte the same detail set as the direct-mesh versions of the same messages. Seen on both an SA event and a file-share request (`:113`, `:134`). Practical reading: the server appends one attribute per hop, keyed by its own name, used for loop suppression; a client-side server must not choke on unknown `<detail>` children, and a server implementation should add its own tag rather than rewrite an existing one.

**`senderUrl` rewriting.** The same logical file-transfer offer appears in two forms. Sent peer-to-peer over the mesh, the `<fileshare>` element's `senderUrl` points at the *sending device's* embedded web server — an `http` URL on the device's LAN address, port 8080, with a `getfile` path taking a numeric file index and the sender's callsign as query parameters (`:23`, `:81`). The same offer as observed after relaying through a TAK server instead points at the *server's* address (a different subnet entirely) on the same port, with the path changed to the server's sync-content endpoint keyed by the package hash (`:125`). Everything else — `sha256`, `sizeInBytes`, `senderUid`, `senderCallsign`, `name` — is unchanged. So the uploading client posts the package to the server first, then advertises the server's URL; recipients on the server side never reach the originating device. This matches `missionpackagemanager.cpp:1276`/`:1299`, where `urlFromServer` (the body of the `missionupload` response) becomes the `senderUrl` of the outgoing `b-f-t-r`, and `:2319-2320`, where a `senderUrl` containing `/Marti/` flips the transfer to "server-hosted" mode.

Also visible in the mesh form only: `<ackrequest>` carries an extra `endpoint` attribute pointing back at the sender's TCP contact endpoint (`:87`, `:132`) — commoncommo neither emits nor reads that attribute.

**`__serverdestination`.** Present on the GeoChat sample (`:370`), which was captured on the *mesh* multicast chat group, not off a server — so it is **client-authored, not a server rewrite**. Its `destinations` attribute is a single 4-field string: the sender's LAN IP, port 4242, the literal `tcp`, and the sender's device uid. This matches `GeoChatService.java:405` exactly. Its purpose is to let a receiver that has no `<chatgrp>` reconstruct a reply path (`ChatMessageParser.java:329-341`).

**Other observations worth encoding in a server.** The all-rooms chat sample uses conversation id and `chatroom` both equal to `All Chat Rooms`, and `<chatgrp>` lists `uid0` = sender and `uid1` = the literal string `All Chat Rooms` (`:364-365`) — i.e. the room name is used as a pseudo-uid. The chat event's `uid` follows the `GeoChat.<senderUid>.<conversationId>.<messageId>` shape (`:358`). SA contact endpoints in the wild appear in both mesh form (`ip:4242:tcp`, `:60`, `:174`) and server-relayed form (`*:-1:stcp`, `:155`), confirming the sentinel. Several samples come from non-ATAK clients: a `utf-16` XML declaration (`:268`, `:321`), a non-4242 contact port (`8087`), `precisionlocation` sources of `???` (`:297`, `:315`) or `User`/`DTED0`, an empty `phone=''` attribute (`:292`), and a `<detail>` with no `<status>` at all (`:275-280`). A tolerant parser is required.

---

## Gaps / explicitly not verified

1. `NetConnectString` semantics (`toString()` round-trip, `getPort()` default, `setCallsign`) — class absent.
2. `KMLUtil.KMLDateFormatter` and `KMLDateTimeFormatterMillis` patterns — class absent. **This is the one item in §5 you asked about that I could not settle.** Both `ServerGroup.created` and `ServerContact.lastEventTime`/CoT-history `start`/`end` depend on it.
3. `AtakCertificateDatabase.importCertificate(location, connectString, type, boolean)` — how a relative vs absolute `caLocation` string is resolved, and what `"(built-in)"` means as a sentinel — class absent.
4. `CertificateManager.getSockFactory(...)` default hostname-verification behaviour — class absent.
5. Any Data Sync / Mission API client — absent (§9).
6. `MissionPackageExtractor` internals: I confirmed it has no `.pref`/`.p12` special-casing and that the sorters handle them, but I did not read its zip-walking logic end to end.
7. `res/values/strings.xml` — absent, so the exact en-US values of `cache_creds_both` / `cache_creds_username` (which are compared as literals during `.pref` import) are unverified.
