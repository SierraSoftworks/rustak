All source read. Here is the verified report.

---

# TAK Server HTTP API — verified against source

**Checkout:** `TAK-Product-Center/Server` @ `5187abd`, `git describe` = **`5.7-RELEASE-14`**. All line numbers below are from that tree. Base path for all Spring controllers is `/Marti/api` (from `BaseRestController`), and `/Marti` for the legacy servlets.

Root: `…/scratchpad/refs/takserver/` — I abbreviate as `$B` below; `WAR` = `$B/src/takserver-core/takserver-war/src/main/java`, `CORE` = `$B/src/takserver-core/src/main/java`, `PLUG` = `$B/src/takserver-plugins/src/main/java`, `COMMON` = `$B/src/takserver-common/src/main/java`.

---

## 1. Envelope & headers

### 1.1 `ApiResponse<T>`

`WAR/com/bbn/marti/cot/search/model/ApiResponse.java:18-34`

```java
@JsonInclude(Include.NON_NULL)
public class ApiResponse<T> {
    private String version;  private String type;  private T data;
    private List<String> messages;  private final String nodeId;
    public ApiResponse() { this.nodeId = ApiDependencyProxy.getInstance().serverInfo().getServerId(); }
```

Wire shape:

```json
{ "version": "3", "type": "<see below>", "data": <T>, "nodeId": "<serverId>" }
```

* `NON_NULL` at class level ⇒ **`version`, `type`, `data`, `messages` are all omitted when null.** `messages` is normally absent (only the 4-arg ctor sets it; grep shows no caller in the API layer).
* `nodeId` is `final`, set in the no-arg ctor from `ServerInfo.getServerId()`, and is **always emitted** (unless serverId itself is null). There is a second ctor `ApiResponse(String nodeId, String version, String type, T data)` used only by `FileManagerApi` (`WAR/tak/server/filemanager/FileManagerApi.java:167,186`).
* No `@JsonPropertyOrder`; property order is not contractual.

### 1.2 `type` string — it is **inconsistent** across endpoints. Verified values:

| Endpoint | `type` value | Source |
|---|---|---|
| `/groups/all`, `/groups/user`, `/groups/{name}/{direction}` | `"com.bbn.marti.remote.groups.Group"` (FQCN, `Group.class.getName()`) | `WAR/com/bbn/marti/groups/GroupsApi.java:183,202,319,341` |
| `/users/all`, `/users/{connectionId}` | `"com.bbn.marti.remote.groups.User"` (FQCN) | `GroupsApi.java:91,146,169` |
| `/groups/groupCacheEnabled` | `"java.lang.Boolean"` | `GroupsApi.java:360` |
| `/clientEndPoints` | `"com.bbn.marti.remote.ClientEndpoint"` (FQCN) | `WAR/com/bbn/marti/network/ContactManagerApi.java:112` |
| `/missions*` (all Mission payloads) | `"Mission"` (**simple** name) | `WAR/com/bbn/marti/sync/api/MissionApi.java:234,308,…` |
| `/missions/**/changes`, `/contents/missionpackage` | `"MissionChange"` (simple) | `MissionApi.java:1780,1817,1668` |
| `/missions/**/layers*` | `"MissionLayer"` (simple) | `MissionApi.java:4620,…` |
| `/missions/**/subscription` (PUT/GET) | `"com.bbn.marti.sync.model.MissionSubscription"` (FQCN) | `MissionApi.java:2368,2510` |
| `/missions/**/subscriptions`, `/subscriptions/roles`, `/all/subscriptions` | `"MissionSubscription"` (**hardcoded literal**) | `MissionApi.java:2789,2800,2818,2880` |
| `/missions/**/invitations`, `/missions/invitations`, `/missions/all/invitations` | `"MissionInvitation"` (hardcoded literal) | `MissionApi.java:3025,3052,3068,3085` |
| `/missions/**/role` | `"com.bbn.marti.sync.model.MissionRole"` (FQCN) | `MissionApi.java:2896` |
| `/missions/**/log`, `/missions/logs/entries*`, `/missions/all/logs` | `"com.bbn.marti.sync.model.LogEntry"` (FQCN) | `MissionApi.java:3368,3381,…` |
| `/missions/{name}/token` | `"java.lang.String"` (FQCN) | `MissionApi.java:2340` |
| `/sync/search`, `/resources/{hash}` | `"Resource"` (simple) | `MissionApi.java:2235`, `MissionApi.java:3551` |
| `/missions/**/maplayers` | `"MapLayer"` (hardcoded literal) | `MissionApi.java:4470,…` |
| `/subscriptions/all`, `/subscription/{uid}`, `/subscriptions/add` | `"SubscriptionInfo"` (simple) | `WAR/com/bbn/marti/sync/api/SubscriptionApi.java:192,208,264` |
| `/version/config` | `"ServerConfig"` (simple) | `WAR/com/bbn/marti/util/VersionApi.java:85` |
| `/files/metadata` | `"Files"` / `/files/metadata/count` → `"Count"` / `/files/{hash}` HEAD → `"data"` | `FileManagerApi.java:167,186,…` |
| `/authentication/config`, `/security/config` | FQCN of the config class | `SecurityAuthenticationApi.java:56,…` |

**Endpoints that do NOT use `ApiResponse` at all** (bare JSON): `/contacts/all`, `/contacts/all/lite`, `/contacts/all/full`, `/missions/**/contacts`, `/cot/matchUid`, `/util/user/roles`, `/util/isAdmin`, `/home`, `/ver`, `/version`, `/version/info`, `/node/id`, `/files/api/config`, `/login/.well-known/openid-configuration`.

### 1.3 `API_VERSION` header

`PLUG/tak/server/Constants.java:44-45`

```java
public static final String API_VERSION_HEADER = "API_VERSION";
public static final String API_VERSION = "3";
```

`MissionServiceDefaultImpl.getApiVersionNumberFromRequest` (`WAR/com/bbn/marti/sync/service/MissionServiceDefaultImpl.java:4351-4363`) reads the request header `API_VERSION`, parses it as an int, and **defaults to `2`** if absent or unparseable.

Behaviour changes (the only 9 call sites, all in `MissionApi`):

| Line | Condition | Effect |
|---|---|---|
| `MissionApi.java:298`, `:386` | `<= 2` in `GET /missions/{name}` and `GET /missions/guid/{guid}` | On `ForbiddenException` (no MISSION_READ), rethrow (→ 403). For `>= 3`, instead calls `mission.clear()` and returns a **stripped** mission (contents/uids/externalData/mapLayers/feeds emptied) with **200**. |
| `:2424`, `:2543` | `>= 4` in `PUT …/subscription` | Uses `getMission(...)` instead of `getMissionByNameCheckGroups` + `validateMission`. |
| `:2497`, `:2616` | `>= 3` in `PUT …/subscription` | Sets `missionSubscription.setMission(mission)` and attaches `missionChanges` + `logs` to it. For `<= 2` it sets `setMission(null)` — **no mission in the subscription response**. |
| `:3692`, `:3814` | `> 2` in `POST …/invite` | Different invite processing branch. |

Note the response header is *not* echoed generally; only `ContentServlet` adds one: `response.addHeader("api-version", Constants.API_VERSION)` (`WAR/com/bbn/marti/sync/ContentServlet.java:129`).

### 1.4 Security / CORS headers filter

There is **no** `X-Frame-Options` / `X-Content-Type-Options` / CSP anywhere in the tree. The only security header is HSTS, set in `MissionRoleAssignmentRequestHolderFilterBean.doFilter` (`WAR/com/bbn/marti/util/spring/MissionRoleAssignmentRequestHolderFilterBean.java:49-62`):

```java
if (servletRequest.getLocalPort() != 8080 && …getNetwork().isEnableHSTS()) {
    resp.setHeader("Strict-Transport-Security", "max-age=63072000; includeSubDomains");
}
if (…getNetwork().isAllowAllOrigins()) {
    resp.setHeader("Access-Control-Allow-Origin", "*");
    resp.setHeader("Access-Control-Allow-Headers", "*");
    resp.setHeader("Access-Control-Allow-Methods", "*");
} else { CorsHeaders.checkAndApplyCorsForConnector(req, resp); }
CustomHeaders.checkAndApplyCustomHeadersForConnector(req, resp);
```

Cache headers: `BaseRestController.setCacheHeaders` (`WAR/com/bbn/marti/network/BaseRestController.java:11-15`) sets `Cache-Control: must-revalidate, max-age=0, no-cache, no-store` and `Expires: 0` — called explicitly only by `/clientEndPoints`.

### 1.5 Error responses

Two distinct paths:

**(a) Spring controller exceptions → JSON `ErrorResponse`** via `@ControllerAdvice CustomExceptionHandler` (`WAR/com/bbn/marti/groups/CustomExceptionHandler.java`). Shape (`WAR/com/bbn/marti/groups/ErrorResponse.java`), no `@JsonInclude`, so all three fields always present:

```json
{ "status": "NOT_FOUND", "code": 1, "message": "Not Found: …" }
```

`status` serialises as the **enum name** (`HttpStatus`), not the numeric code.

| Exception | HTTP | `code` | `message` prefix |
|---|---|---|---|
| `RemoteLookupFailureException` | 404 | 0 | `"RMI Service Not Found"` |
| `NotFoundException`, `EmptyResultDataAccessException` | 404 | 1 | `"Not Found"` (+ `": " + msg`) |
| `DuplicateFederateException`, `IllegalArgumentException`, `NumberFormatException` | 400 | 2 | `"Invalid Request"` (+ `": " + msg`) |
| `HttpMessageNotWritableException` | 501 | 3 | `"JSON serialization error"` |
| `HttpMessageNotReadableException` | 400 | 2 | `"Invalid Request Body"` |
| `DuplicateException` | **409** | 4 | `"Duplicate Exception "` (+ `": " + msg`) |
| `ValidationException` (TAK) | 400 | 5 | `": " + msg` |
| `TakException`, `NullPointerException` | 500 | 6 | `""` |
| `RetryableException` | **304** | 7 | `": " + msg` |
| `MissionDeletedException` | **410 Gone** | 8 | `": " + msg` |
| `UnauthorizedException` | 401 | 9 | `": " + msg` |
| `ForbiddenException` | 403 | 10 | `": " + msg` |

**(b) Container-level errors (`response.sendError`, 404 on unmapped path, auth failures) → HTML** via `tak.server.util.ErrorController` (`WAR/tak/server/util/ErrorController.java:19-62`), mapped to `/error`. Two fixed HTML documents:
* 404 → `<title>404 TAK Server resource not found</title>` / `<h3>404 TAK Server resource not found</h3>`
* everything else → `<title>TAK Server resource unavailable or not allowed.</title>`

This is a TAK-specific page, **not** the stock Tomcat page. All the legacy `/Marti/sync/*` servlets use `response.sendError(...)` so they produce this HTML.

### 1.6 Date format constants

`PLUG/tak/server/Constants.java:22-27`

```java
public static final String COT_DATE_FORMAT            = "yyyy-MM-dd'T'HH:mm:ss.S'Z'";
public static final String COT_DATE_FORMAT_PAD_MILLIS = "yyyy-MM-dd'T'HH:mm:ss.SSS'Z'";
public static final String ISO_DATE_FORMAT_NO_MILLIS  = "yyyy-MM-dd'T'HH:mm:ssXXX";
public static final String SQL_DATE_FORMAT            = "yyyy-MM-dd HH:mm:ss";
public static final String XML_HEADER = "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>";
```

⚠️ `COT_DATE_FORMAT` uses a single `S` — Java `SimpleDateFormat` emits the **millisecond value without zero-padding** (e.g. `2024-01-01T00:00:00.7Z`, or `.123Z`). `COT_DATE_FORMAT_PAD_MILLIS` always pads to 3 digits. The `'Z'` is a **literal**, not a timezone offset; Jackson `@JsonFormat` without an explicit `timezone` uses the mapper's default TZ. Which constant each model uses is listed per-model below.

---

## 2. Version endpoints

`WAR/com/bbn/marti/util/VersionApi.java` (`@RestController extends BaseRestController`).

### `GET /Marti/api/version` — line 35
Returns a bare `String` → Spring `StringHttpMessageConverter` → `Content-Type: text/plain;charset=UTF-8`. Body = the raw contents of classpath resource `/shortver.txt` (`VersionBean.getVer()`, `WAR/com/bbn/marti/util/VersionBean.java:39-52`).

`shortver.txt` is generated at build time (`$B/src/takserver-core/build.gradle:91`):
```groovy
new File("$projectDir/src/main/resources/shortver.txt").text = "$takversion-$takrelease$branch"
```
with `takversion` = `5.7`, `takrelease` = `RELEASE-14`, `branch` = `""` on master/maintenance (`$B/src/build.gradle:24-49`). So the body is exactly **`5.7-RELEASE-14`** — no trailing newline, no "TAK Server" prefix in this build. (`getVersionConfig` still strips a `"TAK Server"` prefix and `\n` defensively, so older builds did emit that.)

⚠️ The resource file is **not present in this sparse checkout** (generated at build time) — I could not read an actual sample.

### `GET /Marti/api/version/config` — line 70
```java
ServerConfig serverConfig = new ServerConfig();
serverConfig.api = Constants.API_VERSION;                                   // "3"
serverConfig.hostname = new URI(request.getRequestURL().toString()).getHost();
String version = versionBean.getVer().replace("\n","").replace("TAK Server","").trim();
String[] tokens = version.split("-");
if (tokens != null && tokens.length >= 3) serverConfig.version = tokens[0] + "." + tokens[2] + "-" + tokens[1];
return new ApiResponse<ServerConfig>(Constants.API_VERSION, ServerConfig.class.getSimpleName(), serverConfig);
```
`ServerConfig` is a private inner class with 3 public fields (`version`, `api`, `hostname`) — lines 61-68. So:
```json
{"version":"5.7.14-RELEASE","type":"ServerConfig","data":{"version":"5.7.14-RELEASE","api":"3","hostname":"tak.example.com"},"nodeId":"…"}
```
Note the top-level `version` is `"3"` (API version), while `data.version` is the reordered product version `major.minor.patch-RELEASE`. If `shortver.txt` has fewer than 3 dash-tokens, `data.version` is **absent** (NON_NULL is class-level on `ApiResponse` only, but the field is null → Jackson default `ALWAYS` for `ServerConfig`, so it would serialise as `"version": null`).

### `GET /Marti/api/version/info` — line 45
Returns `tak.server.util.VersionInfo` directly (no envelope). `COMMON/tak/server/util/VersionInfo.java`:
```json
{ "major": 5, "minor": 7, "patch": 14, "branch": "<git branch>", "variant": "DIRECT" }
```
`major/minor/patch` are `long`. `variant` is forced to `Constants.FEDERATION_VARIANT` = `"DIRECT"` (`VersionBean.java:74`). Source is classpath `/ver.json` parsed with Gson.

### `GET /Marti/api/node/id` — line 55
Bare `String` (`text/plain`) = `ApiDependencyProxy.getInstance().serverInfo().getServerId()`.

Also: `GET /Marti/api/ver` (`HomeApi.java:65`) returns the contents of classpath `ver.txt` as plain text (different file; deleted by the build at `takserver-core/build.gradle:285` — likely empty/absent).

Auth: `security-context.xml` grants `/Marti/api/version/**` and `/Marti/api/node/id/**` to `ROLE_ANONYMOUS`.

---

## 3. Enrollment — `CertManagerApi`

`WAR/com/bbn/tak/tls/CertManagerApi.java`. Class is `@RestController @Profile({api, monolith})`, extends `BaseRestController`.

### 3.1 Full mapping list under `/Marti/api/tls/**`

| Verb | Path | Line | Notes |
|---|---|---|---|
| GET | `/Marti/api/tls/makeClientKeyStore` | 110 | params `cn` (opt), `clientUid` (opt), `password` (opt, default `"atakatak"`). Returns PKCS#12 bytes, `Content-Type: application/octet-stream`. **`cn` must equal the authenticated username** (`verifyCN`, line 164-174) else `403` via `sendError`. |
| GET | `/Marti/api/tls/makeClient` | 202 | **commented out** (inside a `/* … */` block spanning 201-251). Not mapped. |
| POST | `/Marti/api/tls/signServer` | 222 | **commented out**. Not mapped. |
| GET | `/Marti/api/tls/config` | 272 | XML, see §3.4 |
| POST | `/Marti/api/tls/signClient` | 301 | v1 — returns PKCS#12 bytes |
| POST | `/Marti/api/tls/signClient/v2` | 344 | v2 — returns JSON or XML |

Also under `/Marti/api/tls/` but in `ProfileAPI` (§10): `GET /Marti/api/tls/profile/enrollment`, `GET /Marti/api/tls/profile/tool/{toolName}/file`.

`CertManagerAdminApi` (`WAR/com/bbn/tak/tls/CertManagerAdminApi.java`) owns `/Marti/api/certadmin/cert*` — see §3.6.

### 3.2 `POST /Marti/api/tls/signClient/v2` — exact contract

```java
@RequestMapping(value = "/tls/signClient/v2", method = RequestMethod.POST)
ResponseEntity<String> signClientCertV2(
    @RequestParam(value = "clientUid", defaultValue = "") String clientUid,
    @RequestParam(value = "version", required = false) String version,
    @RequestBody String base64CSR,
    HttpServletRequest request, HttpServletResponse response)
```
(`CertManagerApi.java:344-351`)

* **Body** is the raw request body as a `String` (any `Content-Type` that Spring's `StringHttpMessageConverter` accepts — ATAK sends `text/plain`).
* **PEM armour is optional.** `CertManagerService.signClient` (`WAR/com/bbn/tak/tls/Service/CertManagerService.java:128-133`):
  ```java
  String tempCsr = new String(base64CSR);
  tempCsr = tempCsr.replace("-----BEGIN CERTIFICATE REQUEST-----", "");
  tempCsr = tempCsr.replace("-----END CERTIFICATE REQUEST-----", "");
  byte[] bytes = Base64.decodeBase64(tempCsr.getBytes("UTF-8"));
  PKCS10 csr = new PKCS10(bytes);
  ```
  Commons-Codec `decodeBase64` ignores embedded newlines/whitespace, so both armoured and bare base64 work. Note only `CERTIFICATE REQUEST` headers are stripped — `NEW CERTIFICATE REQUEST` would fail.
* **`version` param:** `certManagerService.signClient(clientUid, version != null, base64CSR)` — its only effect is `addChannelsExtUsage = (version != null)`. The *value* is never inspected. Comment at line 309: *"TAK 4.4 clients that support Channels will pass in the version parameter"*.
* **Username derivation:** `getHttpUser()` = `SecurityContextHolder.getContext().getAuthentication().getName()`.
* **CN must match:** `validateCSR` (`CertManagerService.java:62-115`):
  ```java
  String cn = csr.getSubjectName().getCommonName();
  if (cn == null) { … return false; }
  if (getHttpUser().compareToIgnoreCase(cn) != 0) { logger.error("HttpUser didn't equal CN!"); return false; }
  ```
  Case-**insensitive** compare. Additionally the CSR subject must have **exactly** `nameEntries.size() + 1` RDNs, and each configured `<nameEntry name= value=>` must appear (case-insensitive on both type and value). Failure → `TakException("CSR validation failed!")` → caught → `response.sendError(500)`.
* **CN is not rewritten** on the TAK_SERVER path: `info.set(X509CertInfo.SUBJECT, new X500Name(request.getSubjectName().getName()))` (`WAR/com/bbn/tak/tls/CertManager.java:105-106`) — the CSR's subject is used verbatim. (The unrelated `makeClientKeyStore` path *does* synthesise a DN: `"CN=" + cn + issuerDn.substring(issuerDn.indexOf(','))`, `CertManagerApi.java:170`.)
* **Auth:** `/Marti/api/tls/**` requires `ROLE_NO_CLIENT_CERT` (the 8446/8447 connector role, `security-context.xml:336-337, 182-187` region) **and** is in `httpsBasicPaths` (`security-context.xml:55-58`), which makes `RolePortUserServiceWrapper.loadUserDetails` throw `AuthenticationCredentialsNotFoundException` unless an `Authorization: Basic …` header is present (`WAR/com/bbn/marti/util/spring/RolePortUserServiceWrapper.java:63-82`). So **Basic auth is effectively mandatory on `/Marti/api/tls/**`**; a Bearer-only request is rejected at that point *unless* the bearer filter has already populated the SecurityContext (it runs **after** `martiPreAuthenticationFilter` in the chain, `security-context.xml:38-48`). In practice: **Basic works; Bearer-only token enrollment goes through `OAuthAuthenticator` and is what `CertManagerService` reads via `((MartiSocketUserDetailsImpl)…getPrincipal()).getToken()` (line 153-154)** — that token is `null` for Basic auth.

### 3.3 v2 response by `Accept`

`CertManagerApi.java:361-406`:

```java
String accept = request.getHeader("Accept");
if (accept != null) accept = accept.toLowerCase();
if (accept == null || (accept.contains("*/*") || accept.contains("application/json") || accept.length() == 0)) {
    … rootNode.put("signedCert", clientCertPem);
    int ndx = 0; for (X509Certificate ca : cert.getX509CertificateChain()) rootNode.put("ca" + ndx++, caPem);
    result = mapper.writerWithDefaultPrettyPrinter().writeValueAsString(rootNode);
    response.addHeader("Content-Type", "application/json");
} else if (accept.contains("application/xml")) {
    StringBuilder xml = new StringBuilder("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    xml.append("<enrollment>");
    xml.append("<signedCert>"); … xml.append("</signedCert>");
    for (X509Certificate ca : cert.getX509CertificateChain()) { xml.append("<ca>"); … xml.append("</ca>"); }
    xml.append("</enrollment>");
    response.addHeader("Content-Type", "application/xml");
} else return new ResponseEntity<>(HttpStatus.BAD_REQUEST);
```

* **JSON** (default, and for `*/*`): pretty-printed, keys `signedCert`, `ca0`, `ca1`, … (one per chain cert, index appended).
* **XML**: declaration `<?xml version="1.0" encoding="UTF-8"?>` (double quotes, no `standalone`), root `<enrollment>`, one `<signedCert>`, then `<ca>` repeated. **No namespace.** There is no `<validityPeriod>` or any other element.
* PEM bodies come from `Util.certToPEM(cert, /*includeHeader=*/ false)` — **no `-----BEGIN/END CERTIFICATE-----` lines**, base64 wrapped at 64 chars with `\n` (`WAR/com/bbn/tak/tls/Util.java:17-38`):
  ```java
  String encoded = DatatypeConverter.printBase64Binary(bytes);
  if (includeHeader) sw.write(header + "\n");
  sw.write(encoded.replaceAll("(.{64})", "$1\n"));
  ```
  Note: when `includeHeader` is false there is **no trailing newline**.
* Status is `200 OK` in both cases. Failures → `sendError(500)` (HTML).
* `Content-Type` is set via `addHeader` **after** the body string was built, and `ResponseEntity<String>` will also cause Spring to negotiate; in practice both headers can appear. Be liberal when parsing.

### 3.4 `GET /Marti/api/tls/config` — exact XML

`CertManagerApi.java:272-299`:
```java
jaxbContext = JAXBContext.newInstance(CertificateConfig.class);   // static init, line 104
…
QName qName = new QName("com.bbn.marti.config", "certificateConfig");
JAXBElement<CertificateConfig> root = new JAXBElement<>(qName, CertificateConfig.class, config);
marshaller.marshal(root, writer);
return ResponseEntity.ok().body(xml);
```

* JAXB class: `com.bbn.marti.config.CertificateConfig`, generated from `$B/src/takserver-common/src/main/xsd/CoreConfig.xsd:576-583` (`<xs:complexType name="certificateConfig">` containing one optional `<nameEntries>` of `<nameEntry name= value=/>`). Binding package from `XmlBindings.xjb` = `com.bbn.marti.config`; XSD `targetNamespace` = `http://bbn.com/marti/xml/config`, `elementFormDefault="qualified"`.
* The root `QName` namespace is the **literal string `com.bbn.marti.config`** (not a URI, and *not* the schema namespace) — a quirk you must reproduce byte-for-byte if clients validate. Child elements are in `http://bbn.com/marti/xml/config`. Marshalled output therefore looks like:
  ```xml
  <?xml version="1.0" encoding="UTF-8" standalone="yes"?>
  <ns2:certificateConfig xmlns="http://bbn.com/marti/xml/config" xmlns:ns2="com.bbn.marti.config">
      <nameEntries>
          <nameEntry name="O" value="Test Org Name"/>
          <nameEntry name="OU" value="Test Org Unit Name"/>
      </nameEntries>
  </ns2:certificateConfig>
  ```
  (prefix choice is JAXB-implementation-dependent). Not pretty-printed (`JAXB_FORMATTED_OUTPUT` is **not** set here).
* **No `validityPeriod`, no `<nameEntries>` when absent** — `certificateConfig` has exactly one optional child.
* `Content-Type`: `ResponseEntity.ok().body(String)` → `text/plain;charset=UTF-8` unless the client sends `Accept: application/xml`. ⚠️ Verify against a live server; ATAK does not appear to care.
* Config source: `CoreConfig → <certificateSigning><certificateConfig>` (`CertManagerApi.getCertificateConfig`, lines 252-270). If absent → `sendError(500)`.

### 3.5 Certificate issuance parameters

`CertManagerService.signClient` TAK_SERVER branch (`CertManagerService.java:156-195`):

```java
long validityDays  = subMgr.getSigningValidity();
Integer validityHours = takServerCAConfig.getValidityHours();
long validityNotBeforeOffsetMinutes = takServerCAConfig.getValidityNotBeforeOffsetMinutes();
long validMs = validityHours != null ? validityHours*1000*60*60 : validityDays*1000*60*60*24;
Date notBefore = new Date(now - validityNotBeforeOffsetMinutes*60*1000);
Date notAfter  = new Date(now + validMs);
if (token != null && takServerCAConfig.isUseTokenExpiration()) {
    Claims claims = JwtUtils.getInstance().parseClaims(token, SignatureAlgorithm.RS256, false);
    Integer exp = (Integer)claims.get("exp");
    if (exp != null) notAfter = new Date(exp.longValue() * 1000);
}
signedCert = certManager.signCertificate(csr, signingCert, CERTTYPE.CLIENT, notBefore, notAfter,
        takServerCAConfig.getSignatureAlg(), addChannelsExtUsage, responderUrl);
```

`TAKServerCAConfig` attributes (`CoreConfig.xsd:585-597`):
```xml
<xs:attribute name="keystore"    type="xs:string" use="required"/>
<xs:attribute name="keystoreFile" type="xs:string" use="required"/>
<xs:attribute name="keystorePass" type="xs:string" use="required"/>
<xs:attribute name="validityDays"  type="xs:int" default="365"/>
<xs:attribute name="validityHours" type="xs:int"/>            <!-- overrides validityDays -->
<xs:attribute name="validityNotBeforeOffsetMinutes" type="xs:int" default="720"/>
<xs:attribute name="signatureAlg" type="xs:string" use="required"/>   <!-- e.g. SHA256WithRSA -->
<xs:attribute name="CAkey" .../> <xs:attribute name="CAcertificate" .../>
<xs:attribute name="useTokenExpiration" type="xs:boolean" default="false"/>
```
Example (`$B/src/takserver-core/example/CoreConfig.example.xml:159-164`) uses `validityDays="30" signatureAlg="SHA256WithRSA"`.

`CertManager.signCertificate` (`WAR/com/bbn/tak/tls/CertManager.java:89-155`):
* `VERSION` = V3; `SERIAL_NUMBER` = **`new java.util.Random().nextInt() & 0x7fffffff`** — a non-cryptographic 31-bit random, regenerated per cert, **no uniqueness check**.
* `ALGORITHM_ID` from `signatureAlg`; but the actual signing uses the **issuer's** sig alg: `outCert.sign(issuerCertificate.key, issuerCertificate.cert.getSigAlgName())` (line 153).
* `ISSUER` = issuer cert's subject; `SUBJECT` = CSR subject verbatim; `KEY` = CSR's `SubjectPublicKeyInfo`.
* Extensions for `CERTTYPE.CLIENT` (lines 139-140, 175-202):
  * **KeyUsage**: `digitalSignature`, `keyAgreement`, `nonRepudiation`
  * **ExtendedKeyUsage**: `clientAuth` (1.3.6.1.5.5.7.3.2); **plus** `KnownOIDs.ChallengePassword` (**1.2.840.113549.1.9.7**) when `addChannelsExtUsage` — i.e. when the `version` query param was present. This is the "Channels enabled" marker ATAK looks for.
  * For CA: `BasicConstraints(true,true,-1)` + KU `digitalSignature,keyCertSign,cRLSign,nonRepudiation`.
  * For SERVER: KU `digitalSignature,keyEncipherment,keyAgreement,nonRepudiation`; EKU `serverAuth,clientAuth`.
  * If `<security><tls enableOCSP="true" responderUrl="…">`, an `AuthorityInfoAccess` OCSP entry is added (lines 144-149).
  * **No SubjectKeyIdentifier / AuthorityKeyIdentifier / CRLDistributionPoints.**
* Key/cert constants: `WAR/com/bbn/tak/tls/Constants.java` — `KEY_TYPE="RSA"`, `CERTBITS=2048`, `CERT_TYPE="X.509"`.

MICROSOFT_CA branch (lines 197-230) posts the bare base64 CSR to WSTEP and picks the returned cert whose EKU contains `1.3.6.1.5.5.7.3.2` as the leaf; the rest become the chain.

Persistence (`CertManagerService.java:236-240`, `CertManagerApi.java:315/359`):
```java
String username = usernameExtractor.extractUsername(signedCert);   // regex from auth.DNUsernameExtractorRegex
return new TakCert(signedCert, caChain, getHttpUser(), username, new Date(), clientUid, token);
…
takCertRepository.save(cert);
```

### 3.6 `POST /Marti/api/tls/signClient` (v1)

`CertManagerApi.java:301-342`. Same params/body/validation. Builds a PKCS#12 in memory:
```java
keyStore.setCertificateEntry("signedCert", cert.getX509Certificate());
int ndx = 0; for (X509Certificate ca : cert.getX509CertificateChain()) keyStore.setCertificateEntry("ca" + ndx++, ca);
keyStore.store(bos, DEFAULT_PASSWORD.toCharArray());   // "atakatak"
return new ResponseEntity<byte[]>(p12, HttpStatus.OK);
```
⚠️ The `HttpHeaders headers` with `APPLICATION_OCTET_STREAM` is built at line 333-334 but **not passed** to the `ResponseEntity` (line 335) — so Spring content-negotiates the `byte[]`, typically yielding `application/octet-stream` anyway. Contains **only certificates, no private key** (the client keeps its own key).

### 3.7 `/Marti/api/certadmin/cert*` (`CertManagerAdminApi`, `ROLE_ADMIN`)

| Verb | Path | Line |
|---|---|---|
| GET | `/certadmin/cert` | 61 |
| GET | `/certadmin/cert/active` | 80 |
| GET | `/certadmin/cert/replaced` | 92 |
| GET | `/certadmin/cert/expired` | 104 |
| GET | `/certadmin/cert/revoked` | 116 |
| GET | `/certadmin/cert/download/{ids}` | 128 |
| DELETE | `/certadmin/cert/delete/{ids}` | 168 |
| DELETE | `/certadmin/cert/revoke/{ids}` | 187 |
| GET | `/certadmin/cert/{hash}` | 212 |
| GET | `/certadmin/cert/{hash}/download` | 230 |
| DELETE | `/certadmin/cert/{hash}` | 320 |

These read from `TakCertRepository` (table of `TakCert` rows saved at issuance). Not ATAK-critical.

---

## 4. OAuth / tokens

### 4.1 Wiring

All in `$B/src/takserver-core/src/main/resources/security-context.xml`. Spring Authorization Server **1.4.2**, Spring Security **6.5.9** (`$B/src/gradle.properties`).

Filter chain (lines 37-49) — single chain for `/**`:
```
X509AuthenticationFilter, BasicAuthenticationFilter, BasicAuthenticationExceptionTranslationFilter,
martiPreAuthenticationFilter, AnonymousAuthenticationFilter, CorsProcessingFilter,
oAuth2TokenEndpointFilter, oAuth2BearerTokenAuthenticationFilter, FilterSecurityInterceptor
```

Token endpoint (lines 454-459):
```xml
<bean id="oAuth2TokenEndpointFilter" class="org.springframework.security.oauth2.server.authorization.web.OAuth2TokenEndpointFilter">
    <constructor-arg ref="passwordGrantAuthenticationManager" />
    <constructor-arg value="/oauth/token"/>
    <property name="authenticationConverter" ref="passwordGrantAuthenticationConverter" />
    <property name="authenticationSuccessHandler" ref="passwordGrantAuthenticationSuccessHandler" />
</bean>
```
No custom `authenticationFailureHandler` ⇒ Spring's default `OAuth2ErrorAuthenticationFailureHandler`.

### 4.2 `POST /oauth/token` — password grant

Converter: `WAR/com/bbn/marti/oauth/PasswordGrantAuthenticationConverter.java:31-72`
```java
String grantType = request.getParameter(OAuth2ParameterNames.GRANT_TYPE);
if (!OAuth2ParameterNames.PASSWORD.equals(grantType)) return null;
String username = parameters.getFirst(OAuth2ParameterNames.USERNAME);
if (Strings.isNullOrEmpty(username)) throw new OAuth2AuthenticationException(OAuth2ErrorCodes.INVALID_REQUEST);
String password = parameters.getFirst(OAuth2ParameterNames.PASSWORD);
if (Strings.isNullOrEmpty(password)) throw new OAuth2AuthenticationException(OAuth2ErrorCodes.INVALID_REQUEST);
```

**Accepted form fields (`application/x-www-form-urlencoded`):** exactly `grant_type=password`, `username`, `password`. `client_id`/`client_secret`/`scope` are read from the map but ignored — a throwaway `RegisteredClient` is synthesised per request with `clientId = username`, a random secret, `ClientAuthenticationMethod.NONE`, `AuthorizationGrantType.PASSWORD`, scopes `openid` + `profile`, and:
```java
TokenSettings.builder()
  .accessTokenFormat(OAuth2TokenFormat.SELF_CONTAINED)
  .accessTokenTimeToLive(Duration.ofMinutes(CoreConfigFacade…getNetwork().getHttpSessionTimeoutMinutes()))
```
So **token lifetime = `CoreConfig network/@httpSessionTimeoutMinutes`**.

Provider: `PasswordGrantAuthenticationProvider.authenticate` (`…/PasswordGrantAuthenticationProvider.java:42-99`) delegates credential checking to `martiAuthenticationProvider` (`com.bbn.marti.util.spring.TakAuthenticationProvider`), persists the client via `JdbcRegisteredClientRepository`, generates the token with `JwtGenerator`, and builds:
```java
OAuth2AccessToken accessToken = new OAuth2AccessToken(BEARER, generatedAccessToken.getTokenValue(),
        generatedAccessToken.getIssuedAt(), generatedAccessToken.getExpiresAt(), null);   // ← scopes null
…
return new OAuth2AccessTokenAuthenticationToken(registeredClient, clientPrincipal, accessToken);  // ← no refresh token
```

Success handler: `PasswordGrantAuthenticationSuccessHandler.onAuthenticationSuccess` (lines 33-74) builds an `OAuth2AccessTokenResponse` and writes it with the stock `OAuth2AccessTokenResponseHttpMessageConverter`.

**Exact success JSON** (HTTP 200, `Content-Type: application/json`):
```json
{ "access_token": "<JWT>", "token_type": "Bearer", "expires_in": 1800 }
```
* **No `refresh_token`** — the provider never creates one.
* **No `scope`** — `accessToken.getScopes()` is empty (null passed to the ctor), and the converter omits empty scope.
* **No `jti`** in the response body (it *is* a JWT claim).
* `expires_in` = `ChronoUnit.SECONDS.between(issuedAt, expiresAt)`.

**Side effect:** the handler also sets chunked cookies:
```java
for (ResponseCookie cookie : AuthCookieUtils.createCookiesWithMaxSize(
        OAuth2TokenType.ACCESS_TOKEN.getValue(), accessTokenResponse.getAccessToken().getTokenValue(),
        -1, sameSite == SameSite.Strict, request.isSecure()))
    response.addHeader(HttpHeaders.SET_COOKIE, AuthCookieUtils.createCookiePartitioned(cookie, partitioned));
```
`AuthCookieUtils.createCookiesWithMaxSize` (`WAR/com/bbn/marti/oauth/AuthCookieUtils.java:99-109`) splits the JWT into 4000-char chunks and emits cookies named **`access_token_0`, `access_token_1`, …** — `HttpOnly`, `Path=/`, `Secure` when the request is secure, `SameSite=Strict` (or `None` + `Partitioned` for CORS-with-credentials), `Max-Age=-1` (session). Optional `Domain` from `<cookie customDomainEnabled="true" customDomain="…">`.

### 4.3 JWT header & claims

Signing key: `jwkSource` bean (`CORE/tak/server/ServerConfiguration.java:1336-1345`):
```java
RSAPublicKey publicKey = (RSAPublicKey) JwtUtils.getInstance().getPublicKey();
RSAPrivateKey privateKey = (RSAPrivateKey) JwtUtils.getInstance().getPrivateKey();
RSAKey rsaKey = new RSAKey.Builder(publicKey).privateKey(privateKey).keyID(UUID.randomUUID().toString()).build();
```
`JwtUtils.loadKeys()` (`WAR/com/bbn/marti/jwt/JwtUtils.java:95-133`) loads the keypair from **`CoreConfig security/tls/@keystore,@keystoreFile,@keystorePass`** — i.e. the *server TLS keystore*, first alias with a private key. So: **alg `RS256`, key = the server's TLS RSA key**, `kid` = a fresh random UUID generated at each server start.

* **JOSE header:** `{"alg":"RS256","kid":"<random-uuid>"}` (JwtGenerator/NimbusJwtEncoder default).
* **Claims** (Spring `JwtGenerator` for grant type PASSWORD with `OAuth2ClientAuthenticationToken` principal):
  * `sub` = `clientPrincipal.getName()` = `RegisteredClient.getClientId()` = **the username**
  * `aud` = `["<clientId>"]` = `["<username>"]`
  * `iat`, `exp` (exp − iat = `httpSessionTimeoutMinutes` × 60), `nbf` (= iat)
  * `jti` = random UUID
  * `iss` — **absent**: `DefaultOAuth2TokenContext.builder()` in `PasswordGrantAuthenticationProvider` has `.authorizationServerContext(...)` **commented out** (line 65), so `JwtGenerator` has no issuer to stamp.
  * `scope` — **absent**: `authorizedScopes` is never set on the token context, and `JwtGenerator` only emits `scope` when non-empty.
  * **No `user_name`, no `authorities`, no `client_id`** — those are legacy `spring-security-oauth2` (pre-2021) claims and are *not* produced here.
* There is **no `/oauth/token_key`, no `/oauth/jwks`, no `/.well-known/jwks.json`** mapping anywhere in the tree. The only "well-known" endpoint is `GET /login/.well-known/openid-configuration` (§4.6), which returns only the *upstream* IdP's endpoints.
* `/oauth/check_token` is granted `ROLE_ANONYMOUS` in `security-context.xml` but **no controller implements it** → 404 HTML.

### 4.4 Error JSON on bad credentials

No custom failure handler → Spring Authorization Server's default writes an `OAuth2Error` as JSON:
```json
{ "error": "invalid_grant" }
```
(possibly with `error_description` / `error_uri` when set). Status codes: `invalid_client` → **401**, everything else (`invalid_request`, `invalid_grant`, `unsupported_grant_type`) → **400**. `server_error` → 500. ⚠️ This is framework default behaviour inferred from the absence of an override, not a TAK-authored code path — worth confirming against a live 5.7 server.

If the `grant_type` is not `password`, the converter returns `null` (line 35) ⇒ the filter doesn't handle the request at all ⇒ it falls through the chain (typically 403/404). **Only `grant_type=password` is supported at `/oauth/token`.**

### 4.5 `/Marti/api/token` (`TokenApi`)

`WAR/com/bbn/marti/oauth/TokenApi.java`, `ROLE_ADMIN` (except `/token/access`).

| Verb | Path | Line | Params | Response |
|---|---|---|---|---|
| GET | `/Marti/api/token` | 82 | `expired` (bool, default `false`) | `ApiResponse<List<TokenResult>>`, `type = "TokenResult"` |
| DELETE | `/Marti/api/token/{token}` | 108 | — | `void` (200, empty body) |
| DELETE | `/Marti/api/token/revoke/{tokens}` | 121 | `{tokens}` = comma-separated | `void` |

`TokenResult` (lines 38-56):
```json
{ "clientId": "...", "token": "<JWT>", "username": "<sub claim>", "expires": "2024-05-01T12:00:00.0Z" }
```
`expires` is `@JsonFormat(shape = STRING, pattern = Constants.COT_DATE_FORMAT)` → single-`S` millis.
`username` is read from the persisted access-token claims: `authorization.getAccessToken().getClaims().get("sub")` (line 98) — confirming `sub` carries the username.

`GET /Marti/api/token/access` is in `OAuthApi` (not `TokenApi`), path `/token/access` on a **plain `@RestController` with no `@RequestMapping` prefix** ⇒ actual URL is **`/token/access`**, not `/Marti/api/token/access`. ⚠️ `security-context.xml` grants `/Marti/api/token/access` to `ROLE_ANONYMOUS` — an apparent mismatch. `OAuthApi.java:358-374`: requires `<oauth allowAccessTokenRetrieval="true">`, else 403; returns `ApiResponse<String>` with `type="String"` and the caller's own bearer token as `data`.

### 4.6 `/login/*` and external OIDC (`OAuthApi`)

`WAR/com/bbn/marti/oauth/OAuthApi.java` — `@RestController` with **no class-level `@RequestMapping`**, so the paths are absolute (no `/Marti/api` prefix). All `/login/**` requires `ROLE_NO_CLIENT_CERT` (8446/8447).

| Verb | Path | Line | Behaviour |
|---|---|---|---|
| GET | `/login/auth` | 77 | Generates 32 random bytes → base64url-nopad `state`; sets cookie `state` (`Max-Age=-1`, `SameSite=Lax`, not secure-forced); 302 to `authServer.authEndpoint?response_type=code&client_id=…&redirect_uri=…&state=<sha256(state) base64url-nopad>[&scope=…]` |
| GET | `/login/redirect` | 207 | Params `code` (req), `state` (req), cookie `state` (req). Validates `sha256(stateCookie).equals(state)`; expires the cookie; POSTs `grant_type=authorization_code&code&client_id&client_secret&redirect_uri` form to `authServer.tokenEndpoint`; extracts `authServer.accessTokenName` (default `access_token`) from the JSON, stores it in chunked `access_token_N` cookies; stores `authServer.refreshTokenName` (default `refresh_token`) in the **HTTP session**. Forwards to `/Marti/login/redirect.html` (or `/Marti/login/webtak-role-error.html` if `webtakScope` is configured and missing from the token). |
| GET | `/login/refresh` | 281 | Uses session refresh token; POSTs `grant_type=refresh_token&refresh_token&client_id&client_secret`; forwards to `/Marti/login/redirect.html`. On failure 401. |
| GET | `/login/authserver` | 316 | `ApiResponse<String>` `type="java.lang.String"`, `data` = first configured authServer's `name`; **404** if none configured. |
| GET | `/login/.well-known/openid-configuration` | 340 | Bare JSON `{"authorization_endpoint":"…","token_endpoint":"…"}` (fields of `OAuthApi.OpenIdConfiguration`, lines 334-337). |
| GET/POST | `/logout` | 353 | Expires all cookies whose name starts with `access_token`, invalidates session, sets `Location: /webtak/index.html`, status **301**. |
| GET | `/token/access` | 358 | see §4.5 |

`authServer` config (`CoreConfig.xsd:729-753`), under `<auth><oauth>`:
```xml
<authServer name= issuer= clientId= secret= redirectUri= scope=
            authEndpoint= tokenEndpoint=
            accessTokenName="access_token" refreshTokenName="refresh_token" trustAllCerts="false">
  <key>…base64 SPKI…</key>*     <!-- optional, repeatable -->
</authServer>
```
Alternative auto-discovery: `<openIdDiscoveryConfiguration name= clientId= secret= redirectUri= configurationUri= .../>` (lines 755-770), resolved by `OAuthUtils.processTrustedAuthServerConfig` into an equivalent `AuthServer`.

**Verifier key resolution** (`JwtUtils.getExternalVerifiers` / `resolveAndCachePublicKeyDetails`, `JwtUtils.java:209-311`): if `<key>` elements are present, each is base64-decoded as an X.509 SPKI RSA public key. Otherwise the **`issuer` attribute is treated as a file path** and must end in `.pem` (splits on `-----BEGIN PUBLIC KEY-----`), `.der`, or `.crt`. Cached for `buffer/queue/@oAuthPublicKeyCacheSeconds`.

### 4.7 Bearer → user + groups

`AccessTokenResolver` (`WAR/com/bbn/marti/oauth/AccessTokenResolver.java:30-68`) — **this is the port gate**:
```java
// TAK - only look for tokens on the oauth ports (skip mission tokens on 8443)
if (request.getLocalPort() != 8446 && request.getLocalPort() != 8447) {
    return null;
}
```
So **bearer tokens are only honoured on ports 8446 and 8447 — never on the 8443 mTLS connector.** (Comment explicitly says this exists so `Authorization: Bearer <mission-token>` on 8443 isn't mistaken for an OAuth token.) Resolution order on 8446/8447: `Authorization: Bearer <token>` (regex `^Bearer (?<token>[a-zA-Z0-9-._~+/]+=*)$`, case-insensitive) → `access_token` request parameter (only if `<oauth allowUriQueryParameter="true">`) → reassembled `access_token_N` cookies.

`OAuthAuthenticator.auth` (`CORE/com/bbn/marti/groups/OAuthAuthenticator.java:123-249`):
1. `JwtUtils.getInstance().parseClaims(token, RS256, ignoreExpired = user.getCert()!=null)` — tries every external verifier first, then the local key.
2. Username: `oauth/@usernameClaim` if set → else `email` claim ("For jwt's from keycloak") → else `sub` (and then the token **must** be findable and unexpired in `OAuth2AuthorizationService`, i.e. a TAK-issued token) → else a random UUID.
3. Optional classification from claims `country`, `classification`, `accms`, `sciControls`.
4. Admin: if `oauth/@adminTokenClaimName` value matches `@adminTokenClaimValue` (string equality or list containment) → `user.getAuthorities().add("ROLE_ADMIN")` (lines 166-182, 206-208).
5. Groups: `ArrayList<String> groupNames = (ArrayList<String>) claims.get(groupsClaim)` where `groupsClaim` = `oauth/@groupsClaim` (**default `"groups"`**, `CoreConfig.xsd:777`).
   * `LdapAuthenticator.applyGroupPrefixFilter(names, oauth/@groupprefix)` (default prefix `""`).
   * Optional `oauth/@groupNameExtractorRegex` applied per name.
   * `LdapAuthenticator.groupNamesToGroups(...)` — **the `_READ`/`_WRITE` mapping** (`CORE/com/bbn/marti/groups/LdapAuthenticator.java:746-789`):
     ```java
     if (readOnlyGroupName != null && … groupName.compareTo(readOnlyGroupName)==0) { remove; readOnly = true; }
     …
     boolean grantReadAccess = true, grantWriteAccess = true;
     if (groupName.endsWith(readSuffix))  { grantWriteAccess = false; groupName = groupName.substring(0, groupName.indexOf(readSuffix)); }
     else if (groupName.endsWith(writeSuffix)) { grantReadAccess = false; groupName = groupName.substring(0, groupName.indexOf(writeSuffix)); }
     if (grantWriteAccess && !readOnly) groups.add(hydrateGroup(new Group(groupName, Direction.IN)));
     if (grantReadAccess)               groups.add(hydrateGroup(new Group(groupName, Direction.OUT)));
     ```
     Suffixes default to **`_READ`** and **`_WRITE`** (`CoreConfig.xsd:775-776`). Semantics: a bare group name → both `IN` and `OUT`; `FOO_READ` → **`OUT` only** (client may receive); `FOO_WRITE` → **`IN` only** (client may send). `<oauth readOnlyGroup="X">` membership strips all `IN` grants. Note `substring(0, indexOf(suffix))` uses the **first** occurrence, so `A_READ_B_READ` truncates at the first `_READ`.
6. If no `groupsClaim` present, falls back to `auth/@default` = `file` (match `UserAuthenticationFile` identifier) or `ldap` (search by username / email).
7. `<oauth oauthUseGroupCache="true">` routes through `ActiveGroupCacheHelper.assignGroupsCheckCache` instead of `groupManager.updateGroups`, **except** that the cache is force-disabled for exactly one URI (lines 198-203):
   ```java
   if (request.getRequestURI().equals("/Marti/api/tls/profile/enrollment")) useGroupCache = false;
   ```
8. `<oauth oauthAddAnonymous="true">` or an empty group set → `doAnonAssignment` (adds `__ANON__`).
9. `AuthCookieUtils.userHasWebtakAccess(oauthConf, claims)` checks `oauth/@scopeClaim` (default `"scope"`) contains `oauth/@webtakScope`; then `AuthenticatorUtil.setUserRolesBasedOnRequestPort`.

Failures: `InvalidBearerTokenException` and `JwtException` are rethrown; anything else → `AuthStatus.FAILURE`. `BearerTokenAuthenticationFailureHandler` (`WAR/com/bbn/marti/oauth/BearerTokenAuthenticationFailureHandler.java:21-45`) turns an `ExpiredJwtException` into **302 → `Location: /login/refresh`** (and expires the `access_token_N` cookies); other auth exceptions clear the context, run `logout`, and rethrow.

Connector→role map (`security-context.xml:549-556`): `8444 → ROLE_FEDERATE`, `8445 → ROLE_XMPP`, `8446 → ROLE_NO_CLIENT_CERT`, `8447 → ROLE_NO_CLIENT_CERT`. Role hierarchy (lines 538-547):
```
ROLE_ADMIN > ROLE_READONLY ; ROLE_READONLY > ROLE_ANONYMOUS ;
ROLE_ADMIN > ROLE_NON_ADMIN_UI ; ROLE_WEBTAK > ROLE_ANONYMOUS ; ROLE_NON_ADMIN_UI > ROLE_ANONYMOUS
```

---

## 5. Groups

### 5.1 `GroupsApi` (`WAR/com/bbn/marti/groups/GroupsApi.java`)

| Verb | Path | Line | Params | Response |
|---|---|---|---|---|
| GET | `/Marti/api/users/all` | 88 | — | `ApiResponse<SortedSet<User>>`, type `com.bbn.marti.remote.groups.User`. Sorted by `created` **descending** (ties → `id` desc). 200 always (empty set if groupManager null). |
| GET | `/Marti/api/users/{connectionId:.+}` | 143 | — | `ApiResponse<UserGroups>` where `UserGroups = {user, groups}` (public fields, line 344-347). type `…groups.User`. **404** when user not found (with an empty `UserGroups` as data). |
| GET | `/Marti/api/groups/{name}/{direction:.+}` | 180 | `{direction}` ∈ `IN`\|`OUT` (enum) | `ApiResponse<Group>`, type `…groups.Group`. **404** with `data` omitted when not found. |
| GET | `/Marti/api/groups/all` | 228 | `useCache` (bool, default **`false`**), `sendLatestSA` (bool, default **`false`**) | `ApiResponse<Collection<Group>>`, type `…groups.Group`, always 200 |
| GET | `/Marti/api/groups/user` | 328 | `username` (**required**) | `ApiResponse<Collection<Group>>`, `ROLE_ADMIN` |
| GET | `/Marti/api/groups/groupCacheEnabled` | 349 | — | `ApiResponse<Boolean>`, type `java.lang.Boolean` |

`/groups/all` logic (lines 229-320):
* **Admin** (`martiUtil.isAdmin(request)`): returns `groupManager.getAllGroups()` — every group loaded on the server — and **skips** all the normalisation below.
* Non-admin:
  * `useCache=true` → `activeGroupCacheHelper.getActiveGroupsForUser(username)`; if `sendLatestSA=true` also calls `subscriptionManager.sendLatestReachableSA(username)` (pushes every reachable peer's latest SA CoT down the caller's streaming connection).
  * On cache miss (or `useCache=false`) → `martiUtil.getGroupsFromRequest(request)`; and **when `useCache=false`**, it then removes any group whose direction isn't `OUT` (lines 255-264) — so `GET /groups/all` without `useCache` returns **OUT groups only**, while `useCache=true` returns whatever is in the active-group cache (which does include `IN`).
  * If `auth/@default == "ldap"`, fills `description` and `distinguishedName` from an LDAP search (cached `buffer/queue/@groupDescriptionCacheSeconds`).
  * **ATAK compatibility normalisation** (lines 298-314) — reproduce this, ATAK's `ServerGroup.isValid()` depends on it:
    ```java
    if (g.getBitpos() == null || g.getBitpos() < 0) g.setBitpos(0);
    if (g.getCreated() == null) g.setCreated(new Date());
    if (g.getType() == null)    g.setType(Group.Type.SYSTEM);
    if (g.getDirection() == null) g.setDirection(Direction.OUT);
    ```

### 5.2 `Group` JSON

`COMMON/com/bbn/marti/remote/groups/Group.java`, `@JsonInclude(NON_NULL)` (line 27).

| JSON field | Java | Notes |
|---|---|---|
| `name` | `getName()` | |
| `distinguishedName` | `getDistinguishedName()` | LDAP only; omitted when null |
| `direction` | `getDirection()` | `"IN"` \| `"OUT"` (enum name; `Direction.IN(1)`, `OUT(2)`) |
| `created` | `getCreated()` | **`@JsonFormat(shape = STRING, pattern = "yyyy-MM-dd")`** (line 93) — date only, e.g. `"2024-03-11"` |
| `type` | `getType()` | `"LDAP"` \| `"SYSTEM"` (enum, line 194-198; ordinals `LDAP=0`, `SYSTEM=1`) |
| `bitpos` | `getBitpos()` | `Integer`, omitted when null |
| `active` | `getActive()` | `boolean`, **always present** (primitive) |
| `description` | `getDescription()` | omitted when null |
| `leaf` | — | **excluded**: `isLeaf()` is `@JsonIgnore` (line 154) |
| `neighbors` | — | **excluded**: `@JsonIgnore` (line 183) |

Ctor defaults (lines 34-39): `bitpos = null`, `created = new Date()`, `type = SYSTEM`, `active = true`.

### 5.3 `bitpos` assignment

`CORE/com/bbn/marti/groups/PersistentGroupDao.java:60-118`. Inside a transaction:
```sql
lock table group_bitpos_sequence;
-- if group not already in `groups`:
update group_bitpos_sequence set bitpos = bitpos + 1 returning bitpos + 1
insert into groups (name, bitpos, create_ts, type) values (?, ?, now(), ?)
```
⚠️ Note the returned value is `bitpos + 1` **after** the update already incremented — i.e. it skips by 2 per allocation relative to the stored counter. Bit positions are therefore monotonically increasing and never reused; a group's bitpos is **global and direction-independent** (direction is *not* persisted — `load()`/`fetchAll()` always set `Direction.OUT`, lines 149, 182). `type` is stored as the enum ordinal but read back as `rs.getInt(4) == 0 ? Type.LDAP : Type.SYSTEM`.

The "group vector" used everywhere in the API is a Postgres `bit varying` string, one bit per bitpos.

### 5.4 `SubscriptionApi` group mappings

`WAR/com/bbn/marti/sync/api/SubscriptionApi.java`

| Verb | Path | Line | Body / params |
|---|---|---|---|
| PUT | `/Marti/api/groups/active` | 1013 | body `Group[]` (JSON array of Group objects, `active` field meaningful); param `clientUid` (**optional**) |
| PUT | `/Marti/api/groups/activebits` | 1041 | body `Integer[]` of bitpos values; param `clientUid` (optional) |
| PUT | `/Marti/api/groups/activeForce` | 1097 | body `Group[]`; param `username` (**required**); `ROLE_ADMIN` |
| POST | `/Marti/api/groups/update` | 1127 | body `Set<String>` usernames; `ROLE_ADMIN` |
| GET | `/Marti/api/groups/update/{username:.+}` | 1132 | `ROLE_ADMIN` |

`PUT /groups/active` (lines 1013-1039):
```java
public ResponseEntity setActiveGroups(@RequestBody Group[] activeGroups,
        @RequestParam(value = "clientUid", required = false) String clientUid) {
    String username = SecurityContextHolder.getContext().getAuthentication().getName();
    if (activeGroups == null) return new ResponseEntity(HttpStatus.BAD_REQUEST);
    doSetActiveGroups(username, Arrays.asList(activeGroups), clientUid, false);
    return new ResponseEntity(HttpStatus.OK);
}
```
Status: **200** on success, **400** on null body or `ValidationException`, **500** otherwise. Empty body (200, no content).

`doSetActiveGroups(username, groups, clientUid, isForced)` (lines 1023-1011 → 973-1011):
1. If `auth/@x509UseGroupCacheRequiresActiveGroup` and **no** group has `active == true` → send a groups-updated message and throw `ValidationException(username + " must have at least 1 active group")` → 400.
2. `subscriptionManager.sendReachableDisconnectMessage(username)` — broadcasts a disconnect SA to everyone currently reachable, *before* the change.
3. `activeGroupCacheHelper.setActiveGroupsForUser(username, activeGroups)`.
4. `groupManager.authenticateCoreUsers(username)` — re-authenticates the user's live streaming connections.
5. `subscriptionManager.sendUpdatedGroupsLatestReachableSA(username)` — pushes all newly-reachable peers' latest SA to this user and rebroadcasts this user's SA.
6. **`if (isForced || clientUid != null)`** → `subscriptionManager.sendGroupsUpdatedMessage(username, clientUid)`.

**⇒ When `clientUid` is absent (and not forced), step 6 is skipped: no `t-x-g-c` is emitted.** The rationale in the source comment is *"notify all devices if forced, or other devices, with same username of the group update, so they can update"* — the `clientUid` identifies the device that *made* the change so it can be excluded/targeted.

### 5.5 `t-x-g-c` emission

`CORE/com/bbn/marti/service/DistributedSubscriptionManager.java:1723` (seed) and `makeGroupChangeMessage` (lines 1754-1775):
```xml
<?xml version='1.0' encoding='UTF-8' standalone='yes'?>
<event how='h-g-i-g-o' type='t-x-g-c' version='2.0'>
  <point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/>
  <detail><link relation='p-p'/></detail>
</event>
```
At send time the root gets `uid`, `start`, `time`, `stale` attributes. `uid = <generated uid>` and, when a `clientUid` is supplied, **`uid = <generated uid> + "." + clientUid`**. `stale = time + 20 s`. (The sibling `t-x-d-d` delete message uses the same seed shape with `type='t-x-d-d'`.)

### 5.6 `ROLE_*` values and `UserAuthenticationFile.xsd`

`PLUG/tak/server/Constants.java:86-94`:
```java
public static final String ANON_GROUP     = "__ANON__";
public static final String FEDERATE_ROLE  = "ROLE_FEDERATE";
public static final String ANONYMOUS_ROLE = "ROLE_ANONYMOUS";
public static final String READONLY_ROLE  = "ROLE_READONLY";
```
Additional roles used in `security-context.xml`: `ROLE_ADMIN`, `ROLE_NON_ADMIN_UI`, `ROLE_WEBTAK`, `ROLE_XMPP`, `ROLE_NO_CLIENT_CERT`, `ROLE_NONEXISTENT`, `ROLE_ALLOW_LOGIN`.

`$B/src/takserver-common/src/main/xsd/UserAuthenticationFile.xsd`, `targetNamespace="http://bbn.com/marti/xml/bindings"`, `elementFormDefault="qualified"`:
```xml
<UserAuthenticationFile>
  <User identifier="…"            <!-- required -->
        fingerprint="…"           <!-- optional -->
        password="…"              <!-- optional -->
        passwordHashed="true|false"
        role="ROLE_ANONYMOUS">    <!-- default ROLE_ANONYMOUS -->
    <groupList>…</groupList>*      <!-- both IN and OUT -->
    <groupListIN>…</groupListIN>*
    <groupListOUT>…</groupListOUT>*
  </User>*
</UserAuthenticationFile>
```
`Role` enumeration: `ROLE_NONEXISTENT`, `ROLE_ADMIN`, `ROLE_READONLY`, `ROLE_ANONYMOUS`, `ROLE_NON_ADMIN_UI`, `ROLE_WEBTAK` (`ROLE_NON_ADMIN_UI` is listed twice — a schema bug). Passwords are BCrypt when `passwordHashed="true"` (`security-context.xml:496` uses `BCryptPasswordEncoder`). Empty-file example: `$B/src/takserver-core/example/UserAuthenticationFile.cluster.xml`.

### 5.7 `Direction IN/OUT` gating

`CORE/com/bbn/marti/groups/CommonGroupDirectedReachability.java:103-150`:
```java
for (Group inGroup : groups) {
    if (inGroup.getDirection().equals(Direction.IN)) {
        Group outGroup = groupManager.getGroup(inGroup.getName(), Direction.OUT);
        if (outGroup == null) continue;
        if (outGroup.getNeighbors().contains(dest)) return true;
    }
}
return false;
```
**Semantics: `src` can send to `dest` iff `src` has group *G* with `Direction.IN` and `dest` is a member of the *same-named* group with `Direction.OUT`.** So `IN` = "this user may write into this channel", `OUT` = "this user may read from this channel". Federate→federate is always unreachable when `<federation>` is configured (lines 77-84). Null src/dest, or a null/empty group set, → `false`.

This is also what `HomeApi.getUserRoles` uses to decide read-only (`WAR/com/bbn/marti/network/HomeApi.java:91-102`): if the caller has **no** `IN` group, `ROLE_READONLY` is appended to the returned role list.

---

## 6. Contacts / endpoints / subscriptions

### 6.1 `GET /Marti/api/contacts/all` (`WAR/com/bbn/marti/sync/api/ContactsApi.java:67`)

```java
ResponseEntity<List<RemoteSubscriptionLite>> getAllContactsLite(
    @RequestParam(value = "sortBy",      defaultValue = "CALLSIGN")  SubscriptionSortField sortBy,
    @RequestParam(value = "direction",   defaultValue = "ASCENDING") SubscriptionSortOrder direction,
    @RequestParam(value = "noFederates", defaultValue = "false")     boolean noFederates)
```
**Returns a bare JSON array — no `ApiResponse` envelope.** `ResponseEntity<List<…>>(subscriptions, new HttpHeaders(), HttpStatus.OK)`.

⚠️ `sortBy` and `direction` are **accepted but ignored** on `/contacts/all` and `/contacts/all/lite` — no sorting code runs (compare `/contacts/all/full`, lines 230-234, which does sort). Passing an invalid enum value still 400s.

`RemoteSubscriptionLite` (`COMMON/com/bbn/marti/remote/RemoteSubscriptionLite.java`) — no Jackson annotations, plain bean, **all fields always present** (`null` when unset):
```json
[ { "filterGroups": ["..."] , "notes": "...", "callsign": "...",
    "team": "...", "role": "...", "takv": "...", "uid": "..." } ]
```
Field mapping (ctor, lines 15-23): **`uid` ← `remoteSubscription.clientUid`** (not `sub.uid`). `takv` is the raw `takv` string (`"<client>:<version>"`-ish). `notes` carries appended user info (`DistributedSubscriptionManager.java:1701`: `subscription.notes += " " + user.getId()`).

Group filtering: `martiUtil.getGroupBitVector(request, Direction.OUT)` plus `<filter><contactApi groupName=… writeOnly="true"/>` entries add write-only `IN` groups to the visibility set (lines 79-98).

Variants:
* `GET /Marti/api/contacts/all/lite` (line 114) → `List<RemoteSubscriptionLiteWithUser>` — same fields plus a nested **`user`** object (`com.bbn.marti.remote.groups.User`).
* `GET /Marti/api/contacts/all/full` (line 162) → `List<RemoteSubscription>` (the full subscription object), **does** honour `sortBy`/`direction`, and appends federated contacts as synthetic `RemoteSubscription`s with only `callsign`, `clientUid`, `uid` set (lines 212-229).

### 6.2 `GET /Marti/api/clientEndPoints` (`WAR/com/bbn/marti/network/ContactManagerApi.java:58`)

```java
Callable<ResponseEntity<ApiResponse<List<ClientEndpoint>>>> getClientEndpoints(
    HttpServletRequest request, HttpServletResponse response,
    @RequestParam(value="secAgo",  required=false, defaultValue="0")     long   secAgo,
    @RequestParam(value="showCurrentlyConnectedClients", required=false, defaultValue="false") String showCurrentlyConnectedClients,
    @RequestParam(value="showMostRecentOnly",            required=false, defaultValue="false") String showMostRecentOnly,
    @RequestParam(value="group",   required=false) String[] queryGroupNames)
```
Note the two booleans are declared as **`String`** and parsed with `Boolean.valueOf(...)` — any non-`"true"` value is false, and they never 400.

* `group` is **repeatable** (`String[]`). When present, the caller's OUT vector must be a superset of the requested groups' vector, else `ForbiddenException` → **403 JSON** `{"status":"FORBIDDEN","code":10,…}` (lines 75-96). Also 403 if either vector is zero.
* `secAgo < 0` → `IllegalArgumentException` → 400.
* Calls `setCacheHeaders(response)` (§1.4).
* Response: `ApiResponse<List<ClientEndpoint>>`, `type = "com.bbn.marti.remote.ClientEndpoint"`.

`ClientEndpoint` (`COMMON/com/bbn/marti/remote/ClientEndpoint.java`), `@JsonInclude(NON_NULL)`:

| JSON | Notes |
|---|---|
| `callsign`, `uid`, `username`, `team`, `role`, `lastStatus` | strings |
| `lastEventTime` | `@JsonFormat(shape = STRING, pattern = "yyyy-MM-dd'T'HH:mm:ss.S'Z'")` — hardcoded literal at line 68, equal to `COT_DATE_FORMAT` |
| `groups` | **`@JsonIgnore` (line 85) — never serialised** |

`lastStatus` is set from the ctor param named `lastEventName` (line 24).

### 6.3 `GET /Marti/api/subscriptions/all` (`SubscriptionApi.java:99`)

```java
ResponseEntity<ApiResponse<Set<SubscriptionInfo>>> getAllSubscriptions(
    @RequestParam("sortBy",    defaultValue="CALLSIGN")  SubscriptionSortField sortBy,
    @RequestParam("direction", defaultValue="ASCENDING") SubscriptionSortOrder direction,
    @RequestParam("page",      defaultValue="-1") int page,
    @RequestParam("limit",     defaultValue="-1") int limit)
```
`type = "SubscriptionInfo"` (simple name). Admins get **all** subscriptions (and `page`/`limit` are honoured via `getCachedSubscriptionList`); non-admins get group-filtered results with in-memory sorting and `page`/`limit` **ignored**.

`SubscriptionInfo` (`SubscriptionApi.java:334-…`) — plain bean, **no `@JsonInclude`**, so every property is emitted including nulls. Getters (line numbers in `SubscriptionApi.java`):

`dn`(572), `callsign`(580), `clientUid`(588), `lastReportMilliseconds`(596, `long`), `lastReportDiffMilliseconds`(600, `long`), `takClient`(608), `takVersion`(616), `username`(624), `groups`(632, `NavigableSet<Group>`), `role`(640), `ipAddress`(648), `port`(656, String), `pendingWrites`(664, `long`), `team`(672), `protocol`(680), `xpath`(688), `subscriptionUid`(696), `numProcessed`(704, `long`), `appFramerate`(712), `battery`(720), `batteryStatus`(728), `batteryTemp`(736), `deviceDataRx`(744), `deviceDataTx`(752), `heapCurrentSize`(760), `heapFreeSize`(768), `heapMaxSize`(776), `deviceIPAddress`(784), `storageAvailable`(792), `storageTotal`(800), `incognito`(808, `boolean`, default `false` at line 569), `handlerType`(816). Plus `metrics` (`RemoteSubscriptionMetrics`) via `setMetrics` at line 188.

Construction notes (lines 411-…): `callsign` falls back to `sub.uid` when `sub.callsign` is empty; `takClient`/`takVersion` come from splitting `sub.takv` on `":"` (first two tokens); `subscriptionUid = sub.uid`; `groups` is the union of the OUT-groups and IN-groups decoded from the connection's cached bit vectors (lines 158-181).

### 6.4 `GET /Marti/api/subscription/{uid}` (`SubscriptionApi.java:195`)

```java
Set<SubscriptionInfo> subscriptions = getAllSubscriptions(CALLSIGN, ASCENDING, -1, -1).getBody().getData();
SubscriptionInfo result = subscriptions.stream().filter(s -> s.clientUid.equals(uid)).findFirst().orElse(null);
return new ResponseEntity<>(new ApiResponse<>(API_VERSION, "SubscriptionInfo", result), new HttpHeaders(),
        result != null ? HttpStatus.OK : HttpStatus.NOT_FOUND);
```
`{uid}` is matched against **`clientUid`**, not `subscriptionUid`. **404** with `data` omitted when not found.

### 6.5 Other `SubscriptionApi` mappings

| Verb | Path | Line | Auth | Notes |
|---|---|---|---|---|
| POST | `/Marti/api/subscriptions/add` | 212 | `ROLE_ADMIN` | body `tmpStaticSub {uid, protocol, subaddr, subport, xpath, filterGroups(CSV string), iface}`; **201** on success, **400** on bad port/validation |
| DELETE | `/Marti/api/subscriptions/delete/{uid}` | 273 | `ROLE_ADMIN` | 200 / 400 / 500, `ApiResponse<String>` with a human message, `type = "String"` |
| POST | `/Marti/api/subscriptions/incognito/{uid}` | 294 | — | **toggles** `incognito`; returns bare **200** (no body) or 500 |
| PUT | `/Marti/api/subscriptions/{clientUid}/filter` | 309 | `ROLE_ANONYMOUS` | `consumes = application/xml`, body is a `com.bbn.marti.config.Filter`; 400 if `getGeospatialFilter()` is null; 200/500 |
| DELETE | `/Marti/api/subscriptions/{clientUid}/filter` | 325 | `ROLE_ANONYMOUS` | clears the geospatial filter; 200/500 |

**Incognito** is a single boolean on the subscription that the operator toggles from the UI; it is exposed read-only as `SubscriptionInfo.incognito` and is not settable per-request.

---

## 7. Missions — `MissionApi`

`WAR/com/bbn/marti/sync/api/MissionApi.java` (4952 lines). Every response listed as "Mission" is `ApiResponse<Set<Mission>>` (a **set**, i.e. a JSON array, even for single-mission results) with `type = "Mission"`, except where noted.

### 7.1 Endpoint table

Guid variants exist for most endpoints; where both exist I list them together. `{name}` path segments use the regex `:.+` (so dots are allowed) and are passed through `missionService.trimName(name)`.

| Verb | Path | Line | Params (default / required) | Success status | Payload |
|---|---|---|---|---|---|
| GET | `/missions` | 191 | `passwordProtected`=`false`, `defaultRole`=`false`, `tool` (opt) | 200 | `ApiResponse<List<Mission>>` |
| GET | `/missions/{name:.+}` | 244 | `password`=`""`, `changes`=`false`, `logs`=`false`, `secago` (opt), `start` (opt ISO), `end` (opt ISO) | 200 | `ApiResponse<Set<Mission>>` |
| GET | `/missions/guid/{guid:.+}` | 331 | same | 200 | same |
| PUT | `/missions/{name:.+}` | 414 | see §7.2 | **201** create / **200** update | `ApiResponse<Set<Mission>>` |
| POST | `/missions/{name:.+}` | 516 | see §7.2 (adds `allowDupe`, drops `allowGroupChange`) | **201** / **200** | same |
| PUT | `/missions/{missionName:.+}/copy` | 1117 | `creatorUid` (**req**), `copyName` (**req**), `copyPath` (opt), `defaultRole` (opt), `password` (opt) | 200/201 | `ApiResponse<Set<Mission>>` |
| DELETE | `/missions/{name:.+}` | 1213 | `creatorUid`=`""`, `deepDelete`=`false` | 200 | deleted `Mission` |
| DELETE | `/missions` | 1310 | **`guid` (required query param)**, `creatorUid`=`""`, `deepDelete`=`false` | 200 | deleted `Mission` |
| GET | `/missions/{name:.+}/archive` | 1404 | — | 200 | `byte[]` zip + `Content-Disposition` |
| POST | `/missions/{name:.+}/send`, `/missions/guid/{guid}/send` | 1433, 1492 | `contacts` (repeatable, **required non-empty**) | 200 | `ApiResponse<Set<Mission>>` |
| PUT | `/missions/{name:.+}/contents`, `/missions/guid/{guid}/contents` | 1551, 1601 | body `MissionContent`; `creatorUid`=`""` | 200 | `ApiResponse<Set<Mission>>` |
| PUT | `/missions/{name:.+}/contents/missionpackage` | 1643 | body `byte[]` (zip); `creatorUid` (**req**) | 200, or **409** on conflicts, **500** otherwise | `ApiResponse<List<MissionChange>>` (`type="MissionChange"`) — the conflict list |
| DELETE | `/missions/{name:.+}/contents`, `/missions/guid/{guid}/contents` | 1679, 1709 | `hash` (opt), `uid` (opt), `creatorUid`=`""` | 200 | `ApiResponse<Set<Mission>>` |
| GET | `/missions/{name:.+}/changes`, `/missions/guid/{missionGuid}/changes` | 1750, 1787 | `secago`, `start`, `end`, **`squashed`=`true`** | 200 | `ApiResponse<Set<MissionChange>>` |
| DELETE / PUT | `/missions/{name:.+}/keywords` | 1828 / 1861 | `creatorUid`=`""`; PUT body = `List<String>` | 200 | `ApiResponse<Set<Mission>>` |
| DELETE | `/missions/{name:.+}/keywords/{keyword}` | 1920 | | 200 | |
| PUT/DELETE | `/missions/{name}/uid/{uid}/keywords` | 1960 / 2004 | | 200 | |
| PUT/DELETE | `/missions/{name}/content/{hash}/keywords` | 2030 / 2075 | | 200 | |
| GET | `/sync/search` | 2105 | see §9.7 | 200 | `ApiResponse<NavigableSet<Resource>>`, `type="Resource"` |
| GET | `/missions/{missionName:.+}/token` | 2322 | `password`=`""` | **`@ResponseStatus(CREATED)` → 201** | `ApiResponse<String>` (`type="java.lang.String"`), data = ACCESS JWT |
| GET | `/missions/{missionName}/subscription`, `/missions/guid/{guid}/subscription` | 2346, 2373 | `uid`=`""` | 200 | `ApiResponse<MissionSubscription>` (`type` = FQCN). **404** (`NotFoundException`) if no subscription |
| PUT | `/missions/{missionName}/subscription`, `/missions/guid/{guid}/subscription` | 2401, 2521 | `uid`=`""`, `topic`=`""`, `password`=`""`, `secago`, `start`, `end` | **201** | `ApiResponse<MissionSubscription>` — **includes `token`** |
| POST | `/missions/{missionName}/subscription`, guid | 2638, 2672 | body `List<MissionSubscription>`; `creatorUid` (**req**) | 200 / 500 | `void` (empty body) |
| DELETE | `/missions/{missionName}/subscription`, guid | 2710, 2745 | `uid`=`""`, `topic`=`""`, **`disconnectOnly`=`true`** | 200 | empty |
| GET | `/missions/all/subscriptions` | 2780 | `ROLE_ADMIN` | 200 | `ApiResponse<List<Map.Entry<String,String>>>`, `type="MissionSubscription"` |
| GET | `/missions/all/subscriptions/guid` | 2791 | `ROLE_ADMIN` | 200 | nested `Map.Entry` |
| GET | `/missions/{missionName}/subscriptions`, guid | 2802, 2823 | | 200 | `ApiResponse<List<String>>` — bare client UIDs |
| GET | `/missions/{missionName}/subscriptions/roles`, guid | 2864, 2842 | | 200 | `ApiResponse<List<MissionSubscription>>`; **tokens are explicitly nulled out** (`setToken(null)`) |
| GET | `/missions/{missionName}/role`, guid | 2883, 2900 | | 200 | `ApiResponse<MissionRole>` — the role the *filter* assigned to this request |
| PUT | `/missions/{missionName}/role`, guid | 2955, 2917 | `clientUid`=`""`, `username`=`""`, `role` (opt enum) | 200 / 500 | `void` |
| GET | `/missions/all/invitations` | 2999 | `clientUid`=`""` | 200 | `ApiResponse<Set<String>>` — **mission *names* only** |
| GET | `/missions/invitations` | 3029 | `clientUid` (**req**) | 200 | `ApiResponse<Set<MissionInvitation>>` — full objects incl. `token` |
| GET | `/missions/{missionName}/invitations`, guid | 3055, 3073 | | 200 | `ApiResponse<List<MissionInvitation>>` |
| PUT | `/missions/{name}/invite/{type}/{invitee}`, guid | 3088, 3152 | `creatorUid` (**req**), `role`=`""` (enum) | 200 | `void` |
| DELETE | `/missions/{name}/invite/{type}/{invitee}`, guid | 3271, 3211 | | 200 | `void` |
| POST | `/missions/{name}/invite`, guid | 3647, 3773 | `creatorUid` (opt) | 200 | `void` |
| POST | `/missions/logs/entries` | 3340 | body `LogEntry` (**`id` must be absent**) | **201** | `ApiResponse<LogEntry>` |
| PUT | `/missions/logs/entries` | 3423 | body `LogEntry` (**`id` required, `servertime` must be null**) | **201** | `ApiResponse<LogEntry>` |
| GET | `/missions/logs/entries/{id}` | 3387 | | 200 | `ApiResponse<List<LogEntry>>` (single-element list) |
| DELETE | `/missions/logs/entries/{id}` | 3472 | | 200 | `void` |
| GET | `/missions/all/logs` | 3376 | `ROLE_ADMIN` | 200 | `ApiResponse<List<LogEntry>>` |
| GET | `/missions/{missionName}/log` | 3513 | `secago`, `start`, `end` | 200 | `ApiResponse<List<LogEntry>>` |
| GET | `/resources/{hash}` | 3533 | `ROLE_ADMIN` | 200 | `ApiResponse<List<Resource>>` |
| GET | `/missions/{name}/cot`, `/missions/guid/{guid}/cot` | 3553, 3585 | `path` (opt) | 200 | **`ResponseEntity<String>`, `Content-Type: application/xml`** — raw XML, no envelope |
| GET | `/missions/{name}/contacts`, guid | 3628, 3612 | | 200 | bare `List<RemoteSubscription>` JSON array |
| PUT | `/missions/{child}/parent/{parent}`, guid form | 3897, 3917 | | 200 | |
| DELETE | `/missions/{child}/parent`, guid | 3936, 3949 | | 200 | |
| GET | `/missions/{name}/children`, guid | 3962, 3977 | | 200 | |
| GET | `/missions/{name}/parent` | 3992 | | 200, **404** if no parent | `ApiResponse<Mission>` (single object, not a set) |
| GET | `/missions/{name}/kml`, guid | 4007, 4036 | `download`=`false` | 200 | `Content-Type: application/vnd.google-earth.kml+xml`; when `download=true` adds `Content-Disposition` form-data `kml` / `<name>.kml` |
| POST | `/missions/{name}/externaldata`, guid | 4067, 4091 | | **201** | `ApiResponse<ExternalMissionData>` |
| DELETE | `/missions/{name}/externaldata/{id}`, guid | 4117, 4133 | | 200 | |
| POST | `/missions/{name}/externaldata/{id}/change`, guid | 4151, 4168 | | 200 | |
| PUT | `/missions/{name}/password`, guid | 4187, 4213 | `password`=`""`, `creatorUid`=`""` | 200 | `void` |
| DELETE | `/missions/{name}/password`, guid | 4240, 4265 | `creatorUid`=`""` | 200 | `void` |
| PUT | `/missions/{name}/expiration`, guid | 4288, 4314 | `expiration` (opt `Long`) | 200 / 500 | `void` |
| POST | `/missions/{missionName}/feed` | 4342 | `creatorUid`(req), `dataFeedUid`(req), `filterPolygon` (repeatable), `filterCotTypes` (JSON array string), `filterCallsign` | 200 | `void` |
| POST | `/missions/{missionGuid}/feed` | 4386 | same — **note: no `/guid/` segment**, path is `/missions/{missionGuid:.+}/feed` | 200 | |
| DELETE | `/missions/{missionName}/feed/{uid}` | 4431 | | 200 | |
| POST | `/missions/{missionName}/maplayers`, `/missions/guid/{guid}/maplayers` | 4454, 4474 | `creatorUid` (req); body `MapLayer` | 200 | `ApiResponse<MapLayer>`, `type="MapLayer"` |
| PUT | `/missions/{missionName}/maplayers`, guid | 4527, 4547 | same | 200 | |
| DELETE | `/missions/{missionName}/maplayers/{uid}` | 4495 and `/missions/{missionGuid}/maplayers/{uid}` 4511 | | 200 | |
| GET | `/missions/{missionName}/layers`, `/missions/guid/{guid}/layers` | 4602, 4625 | | 200, 500 on error | `ApiResponse<List<MissionLayer>>` |
| GET | `/missions/{missionName}/layers/{layerUid}`, guid | 4650, 4676 | | 200 | `ApiResponse<MissionLayer>` |
| PUT | `/missions/{missionName}/layers`, guid | 4704, 4728 | `name`(req), `type`(req enum), `uid`(opt), `parentUid`(opt), `afterUid`(opt), `creatorUid`(req) | 200 | `ApiResponse<MissionLayer>` |
| PUT | `…/layers/{layerUid}/name` | 4752, 4769 | `name`(req), `creatorUid`(req) | 200 | `void` |
| PUT | `…/layers/{layerUid}/position` | 4786 (`/missions/guid/{missionName}/…` ⚠️ misnamed var), 4803 | | 200 | |
| PUT | `…/layers/parent` | 4820, 4844 | | 200 | |
| DELETE | `/missions/{missionName}/layers`, guid | 4868, 4886 | | 200 | |
| GET | `/pagedmissions` | 4906 | `passwordProtected`=**`true`**, `defaultRole`=**`true`**, `page`=`0`, `pagesize`=`10`, `tool`(opt), `sort`=`""`, `nameFilter`=`""`, `uidFilter`=`""`, `ascending`=`true` | 200 | `ApiResponse<List<Mission>>` |
| GET | `/missioncount` | 4933 | `passwordProtected`=`true`, `defaultRole`=`true`, `tool`(opt) | 200 | `ApiResponse<Integer>`, `type="Mission"` |

### 7.2 Create / update — `PUT` vs `POST /missions/{name}`

`PUT` params (`MissionApi.java:414-431`):
```
creatorUid = ""        group = "__ANON__"   (String[] — REPEATABLE, ?group=a&group=b)
description = ""       chatRoom = ""        baseLayer = ""      bbox = ""
boundingPolygon = []   (List<String> — repeatable; each "lat,lon")
path = ""              classification = ""  tool = "public"
password (opt)         defaultRole (opt, MissionRole.Role enum)
expiration (opt Long)  inviteOnly = false   allowGroupChange = false
@RequestBody(required = false) byte[] requestBody
```
`POST` params (line 516-534) are identical **except**: no `allowGroupChange`, and it adds `allowDupe = false`.

⚠️ `group` is a `String[]` bound by Spring, so it accepts **both** `?group=a&group=b` **and** `?group=a,b` (Spring splits a single comma-containing value for array targets). `boundingPolygon` is a `List<String>`, same rule.

**Request body handling** (`doCreateMissionAllowDupe`, lines 610-641):
```java
String contentType = request.getHeader("content-type");
if (contentType != null && contentType.toLowerCase().contains("application/json")) {
    reqMission = objectMapper.readValue(new String(requestBody), Mission.class);   // FAIL_ON_UNKNOWN_PROPERTIES=false
    // body fields OVERRIDE the query params where non-null:
    description, chatRoom, baseLayer, bbox, boundingPolygon, path, classification,
    tool, defaultRole(.getRole()), expiration, groups(→groupNames), inviteOnly
} else {
    missionPackage = requestBody;      // treated as a Mission Package zip
}
```
So a non-JSON body is imported as a data package (`missionService.addMissionPackage`, line 966).

**Status codes.** Create path (`doInternalCreateMission`, line 979-980):
```java
response.setStatus(mission.getId() != 0 ? HttpServletResponse.SC_CREATED : HttpServletResponse.SC_INTERNAL_SERVER_ERROR);
```
→ **201 Created**. Update path (line 875): `response.setStatus(HttpServletResponse.SC_OK)` → **200 OK**. The javadoc mentions "409 Conflict and duplicate error message" but no `DuplicateException` is thrown here in 5.7 — duplicates are handled by `allowDupe`. (409 is still reachable generically via `CustomExceptionHandler`.)

**`token`:** only the create path sets it (line 962):
```java
MissionSubscription ownerSubscription = missionService.missionSubscribe(mission.getGuidAsUUID(), mission.getId(), creatorUid, username, ownerRole, groupVectorUser);
mission.setToken(ownerSubscription.getToken());
mission.setOwnerRole(ownerRole);
```
⇒ **`token` and `ownerRole` are present on 201 responses and absent on 200 updates.** The token is a SUBSCRIPTION-type JWT.

**Group-change guard** (PUT only, lines 470-483): if the requested group set differs from the mission's current groups, the caller must be admin **or** `MISSION_OWNER`, **and** either `allowGroupChange=true` or `<network missionAllowGroupChange="true">`; otherwise `ForbiddenException` → 403.

**`__ANON__` fallback** (lines 671-679): if the caller's vector doesn't cover the requested mission groups **and** the request asked for exactly `__ANON__`, the mission silently gets the **caller's own** group vector instead of failing.

Also enforced: `<network missionCreateGroupsRegex>` must match at least one of the caller's group names (`validateMissionCreateGroupsRegex`, `MissionServiceDefaultImpl.java:4119-4140`), else 403.

### 7.3 `GET /missions` — `tool` default

Lines 205-223: if `tool` is **absent**, it queries `tool = "public"` (hardcoded). With VBM enabled + `returnCopsWithPublicMissions`, it additionally appends `<network missionCopTool>` missions and the caller's invite-only COPs. So `?tool` has **no "all tools" mode** — you must name the tool.

`defaultRole` and `passwordProtected` are inclusion filters, defined by the repository SQL (`WAR/com/bbn/marti/sync/repository/MissionRepository.java:103-107`):
```sql
from mission where invite_only = false
and ((:passwordProtected = false and password_hash is null) or :passwordProtected = true)
and ((:defaultRole = false and (default_role_id is null or default_role_id = 2)) or :defaultRole = true)
```
with the inline comment *"return new missions with default role of MISSION_SUBSCRIBER to older clients"*. So `defaultRole=false` **excludes** missions whose default role is anything other than id 2 (`MISSION_SUBSCRIBER`), and `passwordProtected=false` excludes password-protected missions. Both default to `false` on `/missions` and `true` on `/pagedmissions`/`/missioncount`. Invite-only missions are **never** returned by this query.

### 7.4 `GET /missions/{name}` — `password` / `changes` / `logs` / `secago`

Lines 264-303:
```java
if (!Strings.isNullOrEmpty(password)) {
    missionService.validatePassword(mission, password);
    String token = missionService.generateToken(UUID.randomUUID().toString(), mission.getGuidAsUUID(),
            mission.getName(), MissionTokenUtils.TokenType.ACCESS, -1);
    mission.setToken(token);
} else if (!missionService.validatePermission(MISSION_READ, request)) {
    throw new ForbiddenException("Illegal attempt to access mission! Request did not have read access.");
}
if (changes) mission.setMissionChanges(missionService.getMissionChanges(name, groupVector, secago, start, end, /*squashed=*/false));
if (logs)    mission.setLogs(missionService.getLogEntriesForMission(mission, secago, start, end));
mission.setGroups(RemoteUtil.getInstance().getGroupNamesForBitVectorString(mission.getGroupVector(), userGroups));
```
Note `changes` here uses **`squashed = false`** (full history), unlike `/changes` which defaults to squashed. A correct password mints an **ACCESS** token with **no expiry** (`expirationMillis = -1`).

### 7.5 `DELETE /missions` (collection form, by guid)

Line 1310-1395. `@RequestParam("guid")` is **required**; a malformed UUID → `IllegalArgumentException("Invalid mission guid in request")` → 400. Before deleting, the server always archives the mission and stores the zip in enterprise sync:
```java
byte[] archive = missionService.archiveMission(mission.getGuidAsUUID(), groupVector, request.getServerName());
missionService.addMissionArchiveToEsync(mission.getName(), archive, groupVector, true);
mission = missionService.deleteMissionByGuid(guid, creatorUid, groupVector, deepDelete);
```
`deepDelete=true` additionally requires the caller's role to hold `MISSION_DELETE` (else 403). The name-based `DELETE /missions/{name}` (line 1213) also honours `<network missionDeleteRequiresOwner>` and, with VBM + COP tool, forces owner-only.

Note `MissionRoleAssignmentRequestHolderFilterBean` special-cases the collection DELETE (path exactly `/Marti/api/missions` + method DELETE, lines 83-102) to resolve the mission from `?guid=` before the controller runs.

### 7.6 `PUT /missions/{name}/subscription`

Lines 2401-2512 (guid twin 2521-2634). `@ResponseStatus(HttpStatus.CREATED)` → **201**.

```java
MissionRole subRole = missionService.getRoleFromToken(mission,
        new TokenType[]{ INVITATION, SUBSCRIPTION, ACCESS }, request);

if (mission.isPasswordProtected()) {
    if (!Strings.isNullOrEmpty(password)) {
        if (!BCrypt.checkpw(password, mission.getPasswordHash()))
            throw new ForbiddenException("Illegal attempt to subscribe to mission! Password did not match.");
    } else if (subRole == null)
        throw new ForbiddenException("Illegal attempt to subscribe to mission! No token role provided.");
} else if (!Strings.isNullOrEmpty(password)) {
    throw new ForbiddenException("Illegal attempt to subscribe to mission! No password provided.");
} else if (mission.isInviteOnly() && subRole == null) {
    subRole = missionService.getRoleFromTypeAndInvitee(mission.getGuidAsUUID(), userName, username);
    if (subRole == null) throw new ForbiddenException("Illegal attempt to subscribe to invite only mission!");
}
MissionRole role = (subRole != null) ? subRole : missionService.getDefaultRole(mission);
if (Strings.isNullOrEmpty(uid) && Strings.isNullOrEmpty(topic))
    throw new IllegalArgumentException("either 'uid' or 'topic' parameter must be specified");
MissionSubscription missionSubscription = missionService.missionSubscribe(mission.getGuidAsUUID(), mission.getId(),
        Strings.isNullOrEmpty(topic) ? uid : "topic:" + topic, username, role, groupVector);
```
⚠️ Note the middle branch: supplying a `password` for a **non**-password-protected mission is a 403 with the (misleading) message `"No password provided."`.

Subscribing also auto-clears matching `clientUid` and `callsign` invitations (lines 2484-2497).

Response body is `MissionSubscription` — **including `token`** (the SUBSCRIPTION JWT). With `API_VERSION >= 3` the nested `mission` is populated with `missionChanges` (squashed=false, filtered by `secago`/`start`/`end`) and `logs`; with `<= 2` `mission` is `null`.

### 7.7 `/changes` and squashing

`squashed` defaults to **`true`** (`MissionApi.java:1755`, `:1792`).

Implementation: `MissionServiceDefaultImpl.getMissionChanges` (line 3606-3617) picks between two native-SQL unions in `MissionChangeRepository`:
* `MISSION_CHANGES` (squashed) — for each of ADD/REMOVE by hash and by uid, the query does
  `select max(mc.id), …, max(mc.ts), max(mc.servertime) … group by hash/uid, creatoruid, mission_id, mission_name, mission_guid, remote_federated_change[, xml_content_for_notification]`,
  i.e. **at most one row per (content item, creatorUid)** carrying the newest timestamp; `HASH_ADDS`/`UID_ADDS` **inner-join** `mission_resource`/`mission_uid` so only items *still present* appear, and `HASH_REMOVES`/`UID_REMOVES` add `and mr.resource_hash is null` so only items *actually gone* appear. Result: a **current-state delta**, not a history.
* `MISSION_CHANGES_FULL_HISTORY` — one row per `mission_change` record, left-joined, no grouping.

Both are additionally filtered by `mc.ts >= m.create_time` and the `start`/`end` predicate. `TimeUtils.validateTimeInterval(secago, start, end)` resolves the window (`secago` wins if given).

The controller wraps results in a `ConcurrentSkipListSet<>` → sorted by `MissionChange.compareTo`. On any exception the method **returns `null`** (line 1783/1820) → Spring writes an empty 200 body. Watch out for that.

### 7.8 `/cot` root element — **verified `<events>`**

`MissionServiceDefaultImpl.getCachedCotImpl` (lines 653-683):
```java
StringBuilder result = new StringBuilder();
result.append(Constants.XML_HEADER);      // <?xml version='1.0' encoding='UTF-8' standalone='yes'?>
result.append("<events>");
…
    result.append(cotElement.toCotXml());
    result.append('\n');
…
result.append("</events>");
```
`path` filters on `/detail/marti/dest[@path="<path>"]`. Controller sets `Content-Type: application/xml` and 200 (`MissionApi.java:3577-3580`). Identical structure is used by `CotApi` `/cot/xml/{uid}/all`, `/cot`, `/cot/sa`.

### 7.9 `/contents` — `MissionContent` body

`PLUG/com/bbn/marti/remote/sync/MissionContent.java`, `@JsonInclude(NON_NULL)`:
```json
{ "hashes": ["..."], "uids": ["..."], "paths": { "<path>": [ <MissionContent>, … ] }, "after": "<uid>" }
```
`hashes` and `uids` are `final List<String>` initialised to empty (so always emitted as `[]`); `paths` is `Map<String, List<MissionContent>>` (recursive) and `after` is a plain string — both omitted when null. `getOrCreatePaths()` is `@JsonIgnore`.

Controller check (`MissionApi.java:1567-1570`): at least one of `hashes`, `uids`, `paths` must be non-empty, else `IllegalArgumentException` → 400.

`DELETE /contents` takes `?hash=` or `?uid=` **query params** (not a body).

### 7.10 `/archive` — zip layout

`MissionServiceDefaultImpl.archiveMission` (lines 2544-2630) + `WAR/com/bbn/marti/util/missionpackage/MissionPackage.java`.

**Yes, it is a Mission Package with `MANIFEST/manifest.xml`.** Layout, in write order:
```
cot/                       (directory entry)
contents/                  (directory entry)
cot/<uid>.cot              one per mission uid, body = CotElement.toCotXml()
contents/<n>_<name>        n = 0-based index; falls back to contents/<hash> if resource name is null
MANIFEST/                  (directory entry)
MANIFEST/manifest.xml
```
All `ZipEntry`s get `setTime(0)`.

`manifest.xml` is JAXB-marshalled `MissionPackageManifest` with `JAXB_FORMATTED_OUTPUT=true`, root element `<MissionPackageManifest version="2">`, children `<Configuration>`, `<Contents>`, `<Groups>`, `<Role>` (`MissionPackageManifest.java:42-62`). `<Configuration>` holds `<Parameter name= value=/>` (`ParameterType.java:41-43`); `<Contents>` holds `<Content zipEntry= ignore= keywords= mimeType= name= submitter= uid= creatorUid= size= tool= latitude= longitude= altitude= submissionTime= filename=>` with an optional nested `<Parameter>` (`ContentType.java:50-80`).

Parameters emitted (lines 2550-2564), in this order:
```
uid=<random UUID>            name=<mission name>       mission_guid=<guid>
password_hash=<hash>         creatorUid=<uid>          create_time=<epoch millis as string>
expiration=<long>            chatroom=<...>            description=<...>
tool=<...>                   onReceiveImport=true      onReceiveDelete=false
mission_name=<name>          mission_label=<name>
mission_uid=<serverName>-8443-ssl-<name>
mission_server=<serverName>:8443:ssl
```
Plus `<Groups><Group name="…"/>…` for the mission's OUT groups, and `<Role name="MISSION_…"><Permission name="MISSION_READ"/>…</Role>` from the default role.

Zip filename (used only internally): `<missionName>_<guid>.zip`. The `/archive` endpoint's header is (`MissionApi.java:1417-1419`):
```java
response.addHeader("Content-Disposition",
    "attachment; filename=\"" + URLEncoder.DEFAULT.encode(name, UTF_8) + "\".zip");
```
⚠️ Note the misplaced quote — the emitted value is literally `attachment; filename="MyMission".zip`.

### 7.11 `Mission` JSON — exact

`PLUG/com/bbn/marti/sync/model/Mission.java`, `@JsonIgnoreProperties(value={"hibernateLazyInitializer","handler"}, ignoreUnknown=true)` + `@JsonInclude(NON_NULL)` (lines 53-54).

| JSON field | Getter | Line | Notes |
|---|---|---|---|
| — | `getId()` | 190 | **`@JsonIgnore`** |
| `name` | 199 | | |
| `description` | 208 | | |
| `chatRoom` | 217 | | |
| `baseLayer` | 226 | | |
| `bbox` | 235 | | |
| `boundingPolygon` | 244 | | |
| `path` | 253 | | |
| `classification` | 262 | | |
| `tool` | 271 | | |
| `expiration` | 280 | `Long` | |
| — | `getContents()` (`Set<Resource>`) | 293 | **`@JsonIgnore`** |
| — | `getUids()` (`Set<String>`) | 305 | **`@JsonIgnore`** |
| `keywords` | 316 | `Set<String>` | |
| `creatorUid` | 325 | | |
| `createTime` | 335 | `@JsonFormat(STRING, COT_DATE_FORMAT_PAD_MILLIS)` → `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'` | |
| `lastEdited` | 345 | same format | |
| **`uids`** | `getUidAdds()` | 354 | `@JsonProperty("uids")` → `List<MissionAdd<String>>` — **overrides** the ignored raw set |
| **`contents`** | `getResourceAdds()` | 364 | `@JsonProperty("contents")` → `List<MissionAdd<Resource>>` |
| — | `getGroupVector()` | 375 | **`@JsonIgnore`** |
| `groups` | `getGroups()` | 384 | `@Transient NavigableSet<String>` — **group names**, populated only on some endpoints |
| — | `getParent()`, `getChildren()` | 396, 402 | **`@JsonIgnore`** |
| `externalData` | 407 | `Set<ExternalMissionData>` | |
| `mapLayers` | 412 | `Set<MapLayer>` | |
| `feeds` | 417 | `Set<MissionFeed>` | |
| — | `getPasswordHash()` | 423 | **`@JsonIgnore`** |
| `passwordProtected` | `isPasswordProtected()` | 433 | `@JsonProperty("passwordProtected")`, `boolean` = `!isNullOrEmpty(passwordHash)`. **Primitive ⇒ always present.** |
| `token` | `getToken()` | 438 | `@JsonProperty("token")`, `@Transient` — omitted when null |
| `inviteOnly` | `isInviteOnly()` | 443 | `Boolean` (boxed) — omitted when null |
| `ownerRole` | 452 | `@Transient MissionRole` — only set on create | |
| `defaultRole` | 457 | `MissionRole` object | |
| `missionChanges` | 461 | `Set<MissionChange>` — only when `?changes=true` | |
| `logs` | 465 | `List<LogEntry>` — only when `?logs=true` | |
| `guid` | 469 | `String` | |
| — | `getGuidAsUUID()` | 591 | **`@JsonIgnore`** |
| — | `pageCount`, `missionCount`, `pageSize` | 110-112 | fields exist but **have no getters** ⇒ never serialised |

**`defaultRole` / `ownerRole` object shape** — `MissionRole` (`PLUG/.../MissionRole.java`, `@JsonInclude(NON_NULL)`):
```json
{ "type": "MISSION_SUBSCRIBER", "permissions": ["MISSION_READ","MISSION_WRITE", …] }
```
* `type` ← `getRole()` with `@JsonProperty("type")` + `@XmlAttribute(name="type")` (line 82-85). Enum: **`MISSION_OWNER`, `MISSION_SUBSCRIBER`, `MISSION_READONLY_SUBSCRIBER`** (lines 41-45). `MissionRole.defaultRole = MISSION_SUBSCRIBER`.
* `permissions` ← `@Transient getPermissions()` (line 113-120) — a `Set<String>` of `MissionPermission.Permission` **names**, derived from `getMissionPermissions()` which is itself `@JsonIgnore`.
* `id` is `@JsonIgnore`; `isUsingMissionDefault()` is `@JsonIgnore` and `@Transient`.
* Permission enum (`MissionPermission.java:28-37`): `MISSION_READ`, `MISSION_WRITE`, `MISSION_DELETE`, `MISSION_SET_ROLE`, `MISSION_SET_PASSWORD`, `MISSION_UPDATE_GROUPS`, `MISSION_MANAGE_FEEDS`, `MISSION_MANAGE_LAYERS`.

**`MissionAdd<T>`** (lines 511-558, `@JsonInclude(NON_NULL)`):
```json
{ "data": <String | Resource>, "timestamp": "2024-01-01T00:00:00.000Z", "creatorUid": "...", "keywords": ["..."] }
```
`timestamp` uses `COT_DATE_FORMAT_PAD_MILLIS`. Subclass `MissionAddDetails<T>` adds **`details`** (`@JsonProperty("details")` on `getUidDetails()`, line 571-572).

`Mission.clear()` (lines 579-587) empties `contents`, `resourceAdds`, `uids`, `uidAdds`, `externalData`, `mapLayers`, `feeds` — used for the API≥3 "forbidden but still 200" case.

### 7.12 `MissionChange` JSON

`PLUG/com/bbn/marti/sync/model/MissionChange.java`, `@JsonInclude(NON_NULL)` + `@XmlRootElement(name="MissionChange")`.

| JSON | Getter | Line | Notes |
|---|---|---|---|
| `type` | `getType()` | 146 | **`MissionChangeType`** enum: `CREATE_MISSION`, `DELETE_MISSION`, `ADD_CONTENT`, `REMOVE_CONTENT`, `CREATE_MISSION_FEED`, `DELETE_MISSION_FEED` (`PLUG/com/bbn/marti/remote/sync/MissionChangeType.java`; **ordinals matter at DB level**: 0,1,2,3,4,5) |
| `timestamp` | 168 | `@JsonFormat(STRING, COT_DATE_FORMAT_PAD_MILLIS)` | |
| `serverTime` | `getServerTime()` | 179 | same format; field is `servertime` but the **JSON key is `serverTime`** |
| `missionName` | 199 | | |
| `missionGuid` | 208 | `UUID` → string | |
| `isFederatedChange` | `getIsFederatedChange()` | 217 | `boolean`, always present |
| `contentUid` | 226 | | |
| `creatorUid` | 332 | | |
| `details` | `getUidDetails()` | 255 | `@JsonProperty("details")` → `UidDetails` |
| `contentResource` | 301 | `@JsonProperty("contentResource")` → `Resource` |
| `logEntry` | `getTempLogEntry()` | 323 | `@JsonProperty("logEntry")` → `LogEntry` |
| `missionFeed` | 445 | `@JsonProperty("missionFeed")` → `MissionFeed` |
| `mapLayer` | 491 | `@JsonProperty("mapLayer")` → `MapLayer` |
| `externalData` | `getExternalMissionData()` | 528 | `@JsonProperty("externalData")` |
| — | `id`, `contentHash`, `mission`, `tempResource`, `externalDataUid/Name/Tool/Token/Notes`, `missionFeedUid`, `mapLayerUid`, `xmlContentForNotification`, `tempExternalData` | | **`@JsonIgnore`** |

**`contentHash` is `@JsonIgnore`** (line 154) — the hash is surfaced only inside `contentResource.hash`.

`UidDetails` (`PLUG/.../UidDetails.java`, `@JsonInclude(NON_NULL)`, public fields): `type`, `callsign`, `title`, `iconsetPath`, `color`, `attachments` (`List<String>`), `name`, `category`, `location` → `{"lat":…, "lon":…}` (`Location.java`, `Double`s).

### 7.13 `MissionSubscription` JSON

`WAR/com/bbn/marti/sync/model/MissionSubscription.java`, `@JsonInclude(NON_NULL)`:
```json
{ "token": "<JWT>", "mission": { … }, "clientUid": "...", "username": "...",
  "createTime": "2024-01-01T00:00:00.0Z", "role": { "type": "...", "permissions": [...] } }
```
* **`uid` is `@JsonIgnore`** (line 48) — the subscription's own primary key is never exposed.
* `createTime` uses `@JsonFormat(STRING, Constants.COT_DATE_FORMAT)` (line 86) — **single-`S` millis**, unlike Mission's padded format.
* `mission` is the full nested Mission (null ⇒ omitted).

### 7.14 `MissionInvitation` JSON

`WAR/com/bbn/marti/sync/model/MissionInvitation.java`, `@JsonInclude(NON_NULL)`:
```json
{ "missionName":"...", "invitee":"...", "type":"clientUid", "creatorUid":"...",
  "createTime":"2024-01-01T00:00:00.0Z", "token":"<JWT>", "role":{…}, "missionId":1, "missionGuid":"…" }
```
* `id` is `@JsonIgnore` (line 76).
* `createTime` → `COT_DATE_FORMAT` (line 117).
* `type` is stored/serialised as a **plain `String`** (line 101-107), but the valid values come from the enum `MissionInvitation.Type` (line 35-37): **`clientUid`, `callsign`, `userName`, `group`, `team`** — note the lowercase-first camelCase, and `userName` with a capital N. The `PUT …/invite/{type}/{invitee}` path variable **is** bound to the enum, so it must match exactly.

### 7.15 `MissionLayer` JSON — `mission_layers` **is** snake_case

`PLUG/com/bbn/marti/sync/model/MissionLayer.java`, **`@JsonInclude(NON_EMPTY)`** (line 39 — note: *NON_EMPTY*, not NON_NULL; empty lists/strings are dropped too).

| JSON | Getter | Line |
|---|---|---|
| `uid` | `@JsonProperty("uid")` | 86-88 |
| `name` | 96 | |
| `type` | 104 | enum `GROUP`, `UID`, `CONTENTS`, `MAPLAYER`, `ITEM` (lines 45-51) |
| — | `getAfter()` | 111-113 **`@JsonIgnore`** |
| `parentUid` | 122 | |
| — | `getParent()` | 130 **`@JsonIgnore`** |
| **`mission_layers`** | `getChildren()` | **133-136**: `@JsonProperty("mission_layers")` → `List<MissionLayer>` |
| `uids` | `getUidAdds()` | 140 `@JsonProperty("uids")` |
| `contents` | `getResourceAdds()` | 150 `@JsonProperty("contents")` |
| `maplayers` | `getMaplayerAdds()` | 160 `@JsonProperty("maplayers")` |
| — | `getAbsolutePath()` | 170 **`@JsonIgnore`** |

Confirmed: **`mission_layers`** is the literal key for nested layers — the only snake_case key in the mission model.

### 7.16 `LogEntry` JSON

`PLUG/com/bbn/marti/sync/model/LogEntry.java`, `@JsonInclude(NON_NULL)`:
```json
{ "id":"<uuid>", "content":"...", "creatorUid":"...", "entryUid":"...",
  "missionNames":["..."], "servertime":"…", "dtg":"…", "created":"…",
  "contentHashes":["..."], "keywords":["..."] }
```
`servertime` (114-115), `dtg` (132-133), `created` (141-142) all use `@JsonFormat(STRING, COT_DATE_FORMAT_PAD_MILLIS)`. Note the key is `servertime` (all lowercase) here, vs `serverTime` on `MissionChange`. `missionNames`, `contentHashes`, `keywords` are `Set<String>`.

### 7.17 `Resource` JSON

`PLUG/com/bbn/marti/sync/model/Resource.java`, `@JsonInclude(NON_NULL)` (line 52), `@XmlRootElement(name="Resource")`. **lowerCamelCase** keys:

`filename`, `keywords` (`List<String>`), `mimeType`, `contentType`, `name`, `submissionTime` (`@JsonFormat(STRING, COT_DATE_FORMAT)`, line 227), `submitter`, `uid`, `hash`, `size` (`Long`), `creatorUid`, `tool`, `latitude`, `longitude`, `altitude` (`Double`), `expiration` (`Long`), `groups` (`NavigableSet<String>`, `@Transient`).
`id` and `groupVector` are `@JsonIgnore` (lines 166, 338). Field defaults are non-null empty strings / `0` / `0L` for `id/filename/mimeType/name/submitter/uid/creatorUid/hash/size`, so those are always present.

---

## 8. Mission tokens

### 8.1 `MissionTokenUtils`

`WAR/com/bbn/marti/sync/service/MissionTokenUtils.java`:
```java
public static final String MISSION_NAME_CLAIM = "MISSION_NAME";
public static final String MISSION_GUID_CLAIM = "MISSION_GUID";
public enum TokenType { SUBSCRIPTION, INVITATION, ACCESS }

private void setPrivateKey(PrivateKey privateKey) {
    this.privateKey = privateKey;
    secretKeySpec = new SecretKeySpec(privateKey.getEncoded(), privateKey.getAlgorithm());
}

public String createMissionToken(String id, String missionName, TokenType tokenType,
                                 long expirationMillis, String issuer, UUID missionGuid) {
    JwtBuilder builder = Jwts.builder()
        .setId(id)                                   // jti
        .setIssuedAt(now)                            // iat
        .setSubject(tokenType.name())                // sub = "SUBSCRIPTION" | "INVITATION" | "ACCESS"
        .setIssuer(issuer)                           // iss
        .signWith(SignatureAlgorithm.HS256, secretKeySpec)
        .claim(tokenType.name(), id)                 // e.g. "SUBSCRIPTION": "<subscription uid>"
        .claim(MISSION_NAME_CLAIM, missionName)
        .claim(MISSION_GUID_CLAIM, missionGuid.toString());
    if (expirationMillis > 0) builder.setExpiration(new Date(now.getTime() + expirationMillis));
    return builder.compact();
}
```

* **Algorithm: `HS256`.** The HMAC secret is the **PKCS#8-encoded bytes of the server's RSA private key** (`privateKey.getEncoded()`), i.e. the same TLS keystore key used for the OAuth RS256 JWTs. (`JwtUtils.getPrivateKey()`, see §4.3.)
* Claims: `jti`, `iat`, `sub` (= the token type name), `iss`, optional `exp`, plus **`<TYPE>`** (the token type name as a claim key, value = the id), **`MISSION_NAME`**, **`MISSION_GUID`**.
* `expirationMillis <= 0` ⇒ **no `exp` claim** (never expires). `/missions/{name}/token` and the password path both pass `-1`.
* Verification: `JwtUtils.parseMissionTokenClaims` (`JwtUtils.java:357-373`) tries HS256 with the local private key first, then each `<security><missionTls>` keystore's private key — enabling token portability across a federation of servers sharing a mission TLS key.
* `MissionTokenUtils` is a lazy singleton keyed on the first `privateKey` passed in (lines 44-51).

### 8.2 `getRoleFromToken` — header precedence

`MissionServiceDefaultImpl.java:3993-4116`:
```java
if (commonUtil.isAdmin(request)) {
    return missionRoleRepository.findFirstByRole(MissionRole.Role.MISSION_OWNER);   // ADMIN BYPASS
}
String authorization = request.getHeader("MissionAuthorization") != null ?
        request.getHeader("MissionAuthorization") : request.getHeader("Authorization");
if (authorization == null) return null;
if (!authorization.startsWith("Bearer ")) { … return null; }
String token = authorization.substring(7);
Claims claims = MissionTokenUtils.getInstance(JwtUtils.getInstance().getPrivateKey()).decodeMissionToken(token);
TokenType tokenType = TokenType.valueOf(claims.getSubject());
if (!ArrayUtils.contains(validTokenTypes, tokenType)) return null;
String missionName = (String) claims.get(MISSION_NAME_CLAIM);
if (missionName == null) return null;
if (missionName.compareTo(mission.getName()) != 0) return null;   // case-SENSITIVE
```

* **Header precedence: `MissionAuthorization` wins over `Authorization`.** This is exactly what lets a client hold an OAuth bearer in `Authorization` (8446) *and* a mission token in `MissionAuthorization` simultaneously.
* `Bearer ` prefix is **case-sensitive** here (`startsWith("Bearer ")`), unlike the OAuth resolver.
* **Admin bypass is unconditional and first** — an admin always gets a real `MISSION_OWNER` role row.
* Per token type (lines 4069-4110):
  * `SUBSCRIPTION` → looks up `missionSubscriptionRepository.findByUidAndMissionNameNoMission(claims.get("SUBSCRIPTION"), mission.getName())`; returns that subscription's role. Missing subscription → `null`.
  * `INVITATION` → `missionInvitationRepository.findByToken(token)` (the **whole token string** is stored in the DB); the invitation's `missionName` must match **case-insensitively** (`compareToIgnoreCase`); returns the invitation's role.
  * `ACCESS` → returns `getDefaultRole(mission)`.
* Mission-name binding prevents token reuse across missions (`"illegal attempt to re-use token for different mission!"`).

### 8.3 `getRoleForRequest`

Lines 4143-4191:
```java
MissionRole role = getRoleFromToken(mission, new TokenType[]{ ACCESS, SUBSCRIPTION }, request);
if (role != null) return role;
if (isAdmin) return missionRoleRepository.getRoleByOrdinalId(MissionRole.Role.MISSION_OWNER.ordinal());
if (mission.isPasswordProtected() || mission.isInviteOnly()) return null;   // must have a token
return getDefaultRole(mission);
```
Note `INVITATION` tokens are **not** accepted here — only on the subscribe call.

`getDefaultRole(mission)` (4325-4348) returns `mission.getDefaultRole()` if set, else the row for `MISSION_SUBSCRIBER`, and sets `usingMissionDefault = true` on it (a `@JsonIgnore` transient).

### 8.4 `MissionRoleAssignmentRequestHolderFilterBean`

`WAR/com/bbn/marti/util/spring/MissionRoleAssignmentRequestHolderFilterBean.java` — a `GenericFilterBean` that runs before the controllers.

1. Sets HSTS / CORS / custom headers (§1.4).
2. Matches `/api/missions/` (`apiMissions`) or `/api/cops/` (`copMissions`) anywhere in the URI, or the exact DELETE-by-guid case.
3. Extracts the mission name (the segment after `/api/missions/`), `trimName`s and URL-decodes it, and **skips** the literals `all`, `logs`, `invitations`, `hierarchy` (lines 128-131).
4. If the name is literally `guid`, takes `path.split("/")[5]` as the UUID and resolves by guid; otherwise resolves by name.
5. `setMissionRole(...)` (lines 212-242) stores two request attributes:
   ```java
   request.setAttribute(Mission.class.getName(), mission);
   request.setAttribute(MissionRole.class.getName(), role);   // role = getRoleForRequest(mission, request, isAdmin)
   ```
   These are what `@PreAuthorize("hasPermission(#request, 'MISSION_READ')")` (via `MissionPermissionEvaluator`, `WAR/com/bbn/marti/sync/service/MissionPermissionEvaluator.java:23-52`) and `missionService.validatePermission(...)` read.
6. **Early exits without invoking the controller:**
   * `NotFoundException` and not a create (`PUT`/`POST` with no trailing path segment) → **`setStatus(404)` and `return`** (empty body).
   * `MissionDeletedException` and not a create → **`setStatus(410)` and `return`**.
   * Invalid UUID → rethrows `IllegalArgumentException("invalid mission UUID")` → 400 JSON.
   * VBM enabled and `validateAccess` fails → `setStatus(404)`.
   * `missionCreate` is also set true for `OPTIONS` when `allowAllOrigins` is on (line 113-116).

**Password-protected mission flow, end to end:**
1. `GET /Marti/api/missions/{name}?password=X` → `validatePassword` → mints an **ACCESS** token with no expiry, returned as `Mission.token`; **or** `GET /Marti/api/missions/{name}/token?password=X` → 201 with the token as `ApiResponse.data`.
2. Client sends `MissionAuthorization: Bearer <ACCESS token>` (or `Authorization:`) on subsequent calls → the filter resolves it to `getDefaultRole(mission)`.
3. `PUT …/subscription` accepts either the raw `password` (BCrypt-checked) or an `INVITATION`/`SUBSCRIPTION`/`ACCESS` token, and returns a **SUBSCRIPTION** token that carries the *subscriber's* role thereafter.

---

## 9. Enterprise Sync / files

### 9.1 Servlet registration

`CORE/tak/server/config/ApiConfiguration.java:250-310`, with `@Value("${takserver.compat.context-path}") compatServletPath` = **`/Marti`** (`$B/src/takserver-core/src/main/resources/application.properties:11`):

| Servlet | URL pattern |
|---|---|
| `SearchServlet` | `/Marti/sync/search/*` |
| `DeleteServlet` | `/Marti/sync/delete/*` |
| `ContentServlet` | `/Marti/sync/content/*` (async) |
| `UploadServlet` | `/Marti/sync/upload/*` (multipart, async) |
| `MissionPackageCreatorServlet` | `/Marti/sync/missioncreate/*` (multipart) |
| `MissionPackageQueryServlet` | `/Marti/sync/missionquery/*` |
| `MissionPackageUploadServlet` | `/Marti/sync/missionupload/*` (multipart) |
| `MissionKMLServlet` | `/Marti/ExportMissionKML/*` |
| `LatestKMLServlet` | `/Marti/LatestKML/*` |
| `KmlMasterSaServlet` | `/Marti/KmlMasterSA/*` |
| `TracksKMLServlet` | `/Marti/TracksKML/*` |
| `ResubscribeServlet` | `/Marti/ResubscribeServlet/*` |
| `GetServerTimeServlet` | `/Marti/GetTime/*` |
| `GetCotDataServlet` | `/Marti/GetCotData/*` |
| `CreateSubscriptionServlet` | `/Marti/CreateSubscriptionServlet/*` |
| `EditSubscriptionServlet` | `/Marti/EditSubscriptionServlet/*` |
| `DBAdminServlet` | `/Marti/DBAdmin/*` |
| `VideoConnectionManager` / `Uploader` / `Sender` | `/Marti/vcm/*`, `/Marti/vcu/*`, `/Marti/vcs/*` |
| `CotQueryServlet` | `/Marti/CotQueryServlet/*` |
| `LogServlet` | `/Marti/ErrorLog/*` |

⚠️ **`MetadataServlet` is NOT registered** — the class exists (`WAR/com/bbn/marti/sync/MetadataServlet.java`) but no `ServletRegistrationBean` references it. **`/Marti/sync/{hash}/metadata` does not exist in 5.7.** Use `PUT /Marti/api/sync/metadata/{hash}/{key}` (§9.5) or `PUT /Marti/api/files/{hash}/metadata` instead.

Multipart limits (lines 237-247): `maxFileSize` = `maxRequestSize` = `network/@enterpriseSyncSizeLimitMB × 1024 × 1024`.

### 9.2 `POST /Marti/sync/upload` (`UploadServlet`)

`WAR/com/bbn/marti/sync/UploadServlet.java`. `doGet` and `doPut` → **405**.

* **Parameters:** every non-machine-generated `Metadata.Field` name (case-insensitive match via `Field.fromString`), plus the aliases **`MIME`** (→ `MIMEType`) and **`name`** (static block, lines 72-82). An unrecognised parameter → **400** `"Unrecognized parameter <x>"`.
* Array-valued fields (`Keywords`, `Permissions`, `Contacts`, `Groups`) accept either repeated params or a **single comma-delimited value** (`values[0].split("\\s*,\\s*")`, lines 218-222).
* `Groups` param → `groupManager.validateAccess(fileGroupNames, groupVector)`; a group the caller lacks → `ForbiddenException` (403).
* `MissionName` + `<network missionUseGroupsForContents="true">` → the file inherits the mission's group vector when the caller's subscription role isn't `MISSION_READONLY_SUBSCRIBER` (lines 254-270).
* Body: if `Content-Type` is **not** `multipart/form-data`, the **raw request body** is the payload. If it is multipart, the part is `assetfile` (ATAK) or `resource` (browsers); the filename is parsed out of that part's `content-disposition` into `DownloadPath` and, if `Name` is empty, `Name` (lines 314-373).
* `UID` is auto-generated (`UUID.randomUUID()`) when not supplied (lines 295-297).
* Size: `Content-Length > enterpriseSyncSizeLimitMB × 1_000_000` → **400** with message `"Uploaded file exceeds server's size limit of N MB! (limit is set in CoreConfig.xml network.enterpriseSyncSizeLimitMB"`. `Content-Length < 1` → **400** `"HTTP request body has no content."`. Init-time guard: `enterpriseSyncSizeLimitMB > 550` → server fails to start (line 91).
* Async timeout = `network/@enterpriseSyncSizeUploadTimeoutMillis`.

**Response** (lines 456-470):
```java
PrintWriter writer = response.getWriter();
writer.print(uploadedMetadata.toJSONObject());
response.setHeader("Content-Type", "text/json");     // note: text/json, not application/json
response.setStatus(HttpServletResponse.SC_OK);
```
**200**, `Content-Type: text/json`, body = `Metadata.toJSONObject()` (`PLUG/com/bbn/marti/sync/Metadata.java:492-507`), which emits **the enum constant names verbatim as keys**, scalars as JSON strings and array fields as JSON arrays:

```
Altitude, DownloadPath, Keywords[], Latitude, Longitude, Hash, MIMEType, Name,
Permissions[], Size, Remarks, SubmissionUser, PrimaryKey, SubmissionDateTime, UID,
Contacts[], CreatorUid, Tool, EXPIRATION, PluginClassName, Groups[], MissionName
```
(`Metadata.Field`, lines 39-61.) **`Size` and `PrimaryKey` are JSON *strings*, not numbers** (everything goes through `String[]`). `EXPIRATION` is upper-case; all the rest are Title/Camel case. Keys with no value are **omitted**.

### 9.3 `GET /Marti/sync/search` (`SearchServlet`)

`WAR/com/bbn/marti/sync/SearchServlet.java`. `doPost` → 405.

Accepted query params (enum `RequestParameters`, lines 66-84) — **case-insensitive**:
`BBox`, `Circle`, `StartTime`, `StopTime`, `SubmissionDateTime` (alias for StartTime), `MinAltitude`, `MaxAltitude`, `PrimaryKey`, `Filename`, `Keywords`, `MIMEType`, `Name`, `Permissions`, `Remarks`, `UID`, `Tool`. Any other parameter → 400. Timestamps are parsed with `SimpleDateFormat(Constants.COT_DATE_FORMAT)` in UTC (lines 161-167). Both `BBox` and `Circle` → 400.

**Response** (lines 277-286):
```java
JSONObject results = new JSONObject();
results.put(SearchServletConstant.RESULT_COUNT_KEY, array.size());   // "resultCount"
results.put(SearchServletConstant.RESULT_KEY, array);                // "results"
response.setContentType("text/json");
```
**200**, `Content-Type: text/json`:
```json
{ "resultCount": 2, "results": [ { …Metadata.toJSONObject()… }, … ] }
```
* `resultCount` is a **JSON number** (`array.size()`, an `int`).
* Each element of `results` is exactly the `Metadata` object from §9.2 — **Title-case keys**, all values strings/arrays. So the fields you listed (`UID, Name, Hash, PrimaryKey, SubmissionDateTime, SubmissionUser, CreatorUid, Keywords, MIMEType, Size, EXPIRATION, Tool`) are correct, plus `Altitude, DownloadPath, Latitude, Longitude, Permissions, Remarks, PluginClassName, Groups, MissionName` when set. **`Size` and `PrimaryKey` are strings, not ints.** Missing fields are omitted, not null.
* Entries failing ESAPI validation are silently skipped (lines 265-275).

### 9.4 `GET /Marti/sync/content` (`ContentServlet`)

`WAR/com/bbn/marti/sync/ContentServlet.java`. Params (enum, lines 70-75): **`UID`**, **`Hash`**, **`offset`**, **`length`** — looked up **case-insensitively** (`SecurityUtils.getCaseInsensitiveParameter`). `Hash` wins if both given. `doPost` → 405; `doGet`/`doHead` supported (HEAD skips the body but sets all headers).

Headers set on success (lines 129-197):
| Header | Value |
|---|---|
| `api-version` | `"3"` (`Constants.API_VERSION`) — the only endpoint that emits this |
| `Content-Type` | the resource's `MIMEType` metadata value (after ESAPI validation) |
| `Content-Disposition` | `inline; filename="<urlencoded>"` where the name is `DownloadPath` → `Name` → `DEFAULT_FILENAME` |
| `Content-Encoding` | `gzip`, **only when the request has `Accept-Encoding: …gzip…`** (the servlet gzips the stream itself) |

Status: **200**; **206 Partial Content** when `length > 0 && offset + length < totalSize`; **404** `"File not found"` (HTML) if no metadata matches; **500** if metadata exists but the content stream is null. Async timeout = `network/@enterpriseSyncSizeDownloadTimeoutMillis`.

An `onComplete` listener (lines 397-422) fires `missionService.checkAndSendPendingNotifications(feedName, hash, 1, …)` when the request carried a `feedName` param and returned 200 — used for the concurrent-download limiter.

### 9.5 `MetadataApi` — `PUT /Marti/api/sync/metadata/**`

`WAR/com/bbn/marti/sync/MetadataApi.java`:

| Verb | Path | Line | Body / params | Allowed keys |
|---|---|---|---|---|
| PUT | `/Marti/api/sync/metadata/{hash}/{metadata}` | 50 | `@RequestBody(required=false) String` = the new value | **only `tool` and `mimetype`** (case-insensitive compare against `Metadata.Field.Tool.name()` / `MIMEType.name()`, lines 73-74). Anything else → **400**. The DB column is `metadataField.toLowerCase()`. |
| PUT | `/Marti/api/sync/metadata/{hash}/keywords` | 95 | `@RequestBody List<String> keywords` (JSON array) | dedicated route |
| PUT | `/Marti/api/sync/metadata/{hash}/expiration` | 127 | `@RequestParam("expiration") Long` (**query param**, required) | dedicated route |

All three return **200** on success, **404** when the update matched no row, **400** on ESAPI validation failure, **500** on exception; body is empty. The `{metadata}` and `keywords` routes both invalidate the mission cache for every mission containing that hash.

⚠️ So the *only* metadata keys mutable via `/Marti/api/sync/metadata/**` are **`tool`, `mimetype`, `keywords`, `expiration`**. Notably **not** `name`, `creatorUid`, or `groups`.

### 9.6 `POST /Marti/sync/missionupload` and `GET /Marti/sync/missionquery`

`MissionPackageUploadServlet` (`WAR/com/bbn/marti/sync/MissionPackageUploadServlet.java`):
* `doGet`/`doPut` → 405. **Must** be `multipart/form-data` (line 168-172: otherwise 400 with the message *"Data package upload must use multipart/form-data POST… Part name should be named 'assetfile'"*). Part = `assetfile` or `resource`.
* Params (enum lines 75-81): **`filename` (required)**, `mimetype` (default `application/x-zip-compressed`), `keyword` (default `MISSION_PACKAGE_KEYWORD`), `tool` (default `"public"`), `creatorUid` (**required** — `defaultValue ""` but `isRequired()` is `defaultValue == null`, so `""` means optional… actually `CREATORUID` has default `""` ⇒ optional), `groups`. Plus an ignored `hash` param ("clients may send a locally computed hash value that is ignored by TAK server", line 61).
* Duplicate detection compares `filename` + `creatorUid` (the `isComparableKey()` fields) against existing rows with the same UID → **403 Forbidden** with the message `"HTTP post attempting to overwrite existing database entry with same values. Request: {…}"`.
* `SubmissionUser` is forced to the authenticated name.
* **Response: 200, `Content-Type: text/plain`, body is a bare URL string**:
  ```java
  String responseStr = String.format("%s/Marti/sync/content?hash=%s",
          MissionPackageQueryServlet.getBaseUrl(request), metadataResult.getUid());
  ```
  ⚠️ Note it interpolates **`getUid()`**, not `getHash()` — but `insertResourceStreamUID` sets the UID to the DB-computed hash.
  `getBaseUrl` (`MissionPackageQueryServlet.java:207-221`) is `scheme://host:port`, **rewriting port 8444 → 8443**.
* On any exception → `sendError(400, e.toString())`.

`MissionPackageQueryServlet` (`WAR/com/bbn/marti/sync/MissionPackageQueryServlet.java`):
* `doPost` → 405. `doGet` only.
* Single required param **`hash`** (enum `PostParameter.HASH("hash", Metadata.Field.UID)` — the value is matched against the resource **UID** column).
* **404** `"File not found"` when absent; otherwise **200, `Content-Type: text/plain`**, body = the same `…/Marti/sync/content?hash=<uid>` URL.
* This is ATAK's "do you already have this package?" probe.

### 9.7 `GET /Marti/api/sync/search` (the JSON `Resource` variant)

`MissionApi.java:2105-2236` — *different* from the `/Marti/sync/search` servlet.

Params (all optional): `box` (Coordinates), `circle` (Coordinates), `startTime` (ISO), `endTime` (ISO), `minAltitude` (Double), `maxAltitude` (Double), `filename`, `keyword` (**repeatable → `List<String>`**), `mimetype`, `name`, `uid`, `hash`, `mission`, `tool`. `box` + `circle` together → `IllegalArgumentException` → 400.

Response: `ApiResponse<NavigableSet<Resource>>` with **`type = "Resource"`** (line 2235) — so the data array holds **lowerCamelCase `Resource` objects** (§7.17), not Title-case `Metadata`. This is the modern endpoint; `/Marti/sync/search` is the legacy one.

### 9.8 `DELETE /Marti/sync/delete` (`DeleteServlet`)

`WAR/com/bbn/marti/sync/DeleteServlet.java`. `doGet` and `doPost` both delegate to `doDelete` (lines 39-50), so all three verbs work.

Params: **`PrimaryKey`** (`ID_KEY`, line 29, repeatable, must be non-negative integers) and **`Hash`** (`HASH_KEY`, line 30, hexadecimal, only the **first** value used). If `Hash` is present it wins; otherwise the `PrimaryKey` list is deleted. Both matched case-insensitively.

⚠️ **Response is HTML, status 200** (lines 145-153):
```html
<html>
<head>
<title>Enterprise Sync Status</title>
</head>
<h1>Success</h1>
<p>Deleted N resource(s).</p>
</html>
```
Errors: 400 for non-numeric `PrimaryKey`, 500 on SQL/JNDI failure.

### 9.9 `FileManagerApi` — `/Marti/api/files/**`

`WAR/tak/server/filemanager/FileManagerApi.java` (`ROLE_ANONYMOUS`).

| Verb | Path | Line | Params |
|---|---|---|---|
| GET | `/Marti/api/files/metadata` | 85 | `page`=`-1`, `limit`=`-1`, `mission`=`""`, **`missionPackage`=`false`**, **`name`=`""`**, `sort`=`""`, `ascending`=`true` |
| GET | `/Marti/api/files/metadata/count` | 170 | `mission`=`""`, `missionPackage`=`false` |
| GET | `/Marti/api/files/{hash}` | 190 | — |
| DELETE | `/Marti/api/files/{hash}` | 232 | — |
| HEAD | `/Marti/api/files/{hash}` | 257 | — |
| PUT | `/Marti/api/files/{hash}/metadata` | 296 | `user`=`""`, `expiration`=`""`, `keywords`=`""` (repeatable `List<String>`) |

Dispatch (lines 113-160): `mission` blank && `!missionPackage` → all files (or `findByName` if `name` set); `mission` blank && `missionPackage` → mission-package resources only; `mission` non-blank → resources of that mission. `page`/`limit` = `-1` means unpaged.

`GET /files/metadata` → `ApiResponse<Collection<Map<String,String>>>` with `type = "Files"` and the **4-arg ctor** (so `nodeId` is `serverInfo.getServerId()`). Each element is a flat `Map<String,String>` from `buildResourceEntry` (lines 384-…) — **all values are strings**:
```json
{ "Name":"…", "User":"…", "Creator":"<HTML-escaped callsign>",
  "Size":"12kB", "Time":"<Date.toString()>", "MimeType":"…",
  "Keywords":"a,b,c", "Expiration":"2024-05-01T00:00:00" | "none",
  "Hash":"…", "Groups":"a,b" }
```
`Size` is humanised (`B`/`kB`/`MB`/`GB`, or `"Unknown"`). `Time` is `resource.getSubmissionTime().toString()` — **Java's default `Date.toString()`**, e.g. `"Wed May 01 12:00:00 UTC 2024"` — *not* ISO. `Expiration` is `Instant.ofEpochSecond(exp).toString()` with the trailing `Z` **chopped off**, or the literal `"none"`.

`GET /files/metadata/count` → `ApiResponse<Integer>` with `type = "Count"`.
`HEAD /files/{hash}` → `ApiResponse<Map<String,String>>` with `type = "data"`, built by `buildMetadataEntry` (lines 328-381) — same keys minus `Groups`, and `Time` is the raw `SubmissionDateTime` metadata string.
`GET /files/{hash}` → the bytes with `Content-Type` from the metadata MIMEType, `Content-Length`, and `Content-Disposition: attachment; filename=<urlencoded Name>`. On error → **500 with an empty `ByteArrayResource`** (the byte array is `null` → NPE risk).
`DELETE /files/{hash}` → `void`, **always 200** (exceptions are swallowed and logged).
`PUT /files/{hash}/metadata` → `void`, always 200; updates `SubmissionUser`, `EXPIRATION`, and keywords.

### 9.10 `/files/api/config` — found

`CORE/com/bbn/file/FileConfigurationApi.java`:
```java
@Validated @RestController
@RequestMapping(value = "/files/api")
public class FileConfigurationApi {
    @RequestMapping(value = "/config", method = RequestMethod.GET)
    public FileConfigurationModel getFileConfiguration() {
        int uploadSizeLimit = CoreConfigFacade.getInstance().getRemoteConfiguration().getNetwork().getEnterpriseSyncSizeLimitMB();
        FileConfigurationModel config = new FileConfigurationModel();
        config.setUploadSizeLimit(uploadSizeLimit);
        return config;
    }
    @RequestMapping(value = "/config", method = RequestMethod.POST)
    public void setFileConfiguration(@RequestBody FileConfigurationModel fileConfigurationModel) { … }
}
```

* **Absolute path `GET /files/api/config`** (no `/Marti` prefix — the class does **not** extend `BaseRestController`). It lives in `takserver-core`, not the war.
* Returns a bare `FileConfigurationModel` (`CORE/com/bbn/file/FileConfigurationModel.java`), no envelope:
  ```json
  { "uploadSizeLimit": 400 }
  ```
  `uploadSizeLimit` is an **`int` in megabytes**; the class's default is `400` (line 11) but the getter always overwrites it from config.
* **Config source: `CoreConfig` → `<network enterpriseSyncSizeLimitMB="…">`** — the same value that drives `UploadServlet`, `MissionPackageUploadServlet`, `MissionPackageCreatorServlet`, `ProfileService`, and the Spring `MultipartConfigElement`. Hard cap: `UploadServlet.init` throws if it exceeds **550**.
* `POST /files/api/config` writes it back (`setAndSaveEnterpriseSyncSizeLimit` + `saveChangesAndUpdateCache`), returns `void`/200.
* Auth: falls through to the catch-all `<sec:intercept-url pattern="/**" access="ROLE_ANONYMOUS" />`.

---

## 10. Device profiles

### 10.1 `ProfileAPI` (client-facing) — `WAR/com/bbn/marti/device/profile/api/ProfileAPI.java`

| Verb | Path | Line | Params |
|---|---|---|---|
| GET | `/Marti/api/tls/profile/enrollment` | 95 | **`clientUid` (required)** |
| GET | `/Marti/api/device/profile/connection` | 126 | **`syncSecago` (required `Long`)**, **`clientUid` (required)** |
| GET | `/Marti/api/device/profile/tool/{toolName}` | 158 | `syncSecago`=`-1`, **`clientUid` (required)** |
| GET | `/Marti/api/tls/profile/tool/{toolName}/file` | 188 | **`relativePath` (required, repeatable `String[]`)**, `syncSecago`=`-1`, **`clientUid` (required)** |
| GET | `/Marti/api/device/profile/tool/{toolName}/file` | 208 | same |
| HEAD | `/Marti/api/device/profile/{name}/missionpackage` | 228 | — (**no-op**, empty try block, always 200) |
| GET | `/Marti/api/device/profile/{name}/missionpackage` | 238 | — |

⚠️ **There is no `/Marti/api/device/profile/enrollment` mapping** even though `security-context.xml` grants it. The enrollment profile is only at **`/Marti/api/tls/profile/enrollment`** (which lives on the 8446 `ROLE_NO_CLIENT_CERT` connector and is Basic-auth-gated via `httpsBasicPaths`). The `/device/profile/*` endpoints are for the mTLS 8443 connector.

**Status codes for the three package endpoints** (lines 108-111, 140-143, 172-175):
```java
List<ProfileFile> files = profileService.getProfileFiles(host, groupVector, applyOnEnrollment, applyOnConnect, syncSecago);
if (files.size() == 0) { response.setStatus(HttpServletResponse.SC_NO_CONTENT); return null; }
response.addHeader("Content-Disposition", "attachment; filename=" + ProfileService.defaultFilename);
return profileService.createProfileMissionPackage(ProfileService.defaultFilename, "<Enrollment|Connection|toolName>", files);
```
⇒ **204 No Content with an empty body when there is nothing to send; otherwise 200 with the zip.** `defaultFilename = "profile.zip"` (`ProfileService.java:57`). The `Content-Disposition` value is unquoted. Content-Type is whatever Spring negotiates for `byte[]` (typically `application/octet-stream`).

Group vector selection (`ProfileAPI.getGroupVectorFromStreamingClient`, lines 75-93): when `<profile useStreamingGroup="true">`, the groups of the *streaming* subscription with this `clientUid` override the HTTP caller's groups.

`syncSecago` semantics (`ProfileService.getProfileFiles`, lines 81-93): `-1` → all matching profiles; otherwise only profiles whose `updated` timestamp is newer than `now - syncSecago` seconds.

### 10.2 Zip layout for `/enrollment`, `/connection`, `/tool/{name}`

`ProfileService.createProfileMissionPackage` (lines 188-209):
```java
MissionPackage mp = new MissionPackage(filename);
mp.addParameter("uid", uid);                 // random UUID, or "ProfileMissionPackage-<profileId>"
mp.addParameter("name", name);               // "Enrollment" | "Connection" | "<toolName>" | "<profile name>"
mp.addParameter("onReceiveImport", "true");
mp.addParameter("onReceiveDelete", "true");
int ndx = 0;
for (ProfileFile profileFile : files) {
    mp.addDirectory("file" + ndx + "/");
    mp.addFile("file" + ndx + "/" + profileFile.getName(), profileFile.getData());
    ndx++;
}
return mp.save();
```
⇒
```
file0/            file0/<name>
file1/            file1/<name>
…
MANIFEST/         MANIFEST/manifest.xml
```
Each profile file gets its **own numbered directory** — that's the directory-naming convention. (`onReceiveDelete=true` here, vs `false` for mission archives.)

### 10.3 `.pref` generation

`ProfileService` synthesises up to two `PreferenceFile`s and appends them to the enrollment/connection lists (lines 100-120):

* **`user-profile.pref`** (`getUserPreferences`, lines 125-148) — only when `auth/@default` LDAP config sets any of `callsignAttribute`, `colorAttribute`, `roleAttribute`. Keys:
  * `locationCallsign` ← `ldapUser.getCallsign()`
  * `locationTeam` ← `ldapUser.getColor()`
  * `atakRoleType` ← `ldapUser.getRole()`
* **`enable-channels.pref`** (`getX509GroupCachePreference`, lines 150-155) — only when `<auth x509useGroupCache="true">`. Keys:
  * `prefs_enable_channels` = `"true"`
  * `prefs_enable_channels_host-<takServerHost>` = `"true"` (host from the request URL)

`PreferenceFile.getData()` (`WAR/com/bbn/marti/device/profile/model/PreferenceFile.java:26-40`) — **exact** output, no whitespace between elements:
```java
String prefs =
    "<?xml version='1.0' standalone='yes'?>" +
        "<preferences>" +
            "<preference version=\"1\" name=\"com.atakmap.app.civ_preferences\">";
for (Map.Entry<String,String> preference : preferences.entrySet())
    prefs += "<entry key=\"" + preference.getKey() + "\" class=\"class java.lang.String\">" + preference.getValue() + "</entry>";
prefs += "</preference></preferences>";
```
So:
```xml
<?xml version='1.0' standalone='yes'?><preferences><preference version="1" name="com.atakmap.app.civ_preferences"><entry key="locationCallsign" class="class java.lang.String">Alpha</entry>…</preference></preferences>
```
Note: **XML declaration uses single quotes and has no `encoding`**; the preference group name is **`com.atakmap.app.civ_preferences`** with `version="1"`; the class attribute is the literal string **`class java.lang.String`** (with the redundant leading `class `). Entry order is `HashMap` iteration order (unstable). No XML escaping is applied to keys/values.

### 10.4 `…/tool/{toolName}/file` — `relativePath` and 304

`ProfileService.getProfileDirectoryContent` (lines 457-566):
1. For each `relativePath` (the controller first ESAPI-validates each against `PreventDirectoryTraversal`, `ProfileAPI.java:196-200`), resolves `profiles/<profileDirectory.path><relativePath>`; `checkFile` requires `canonicalPath == absolutePath` **and** canonical path starts with the `profiles` root. Directories are walked recursively.
2. If no profile directories match the tool → **404** (`setStatus(SC_NOT_FOUND)`, empty body).
3. If `getProfileDirectoryContent` returns null (traversal detected / file over `enterpriseSyncSizeLimitMB`) → **500**.
4. If nothing was found at all → **404**.
5. Always sets `Last-Modified` from the newest file, formatted **RFC 1123** (`DateTimeFormatter.RFC_1123_DATE_TIME`, UTC).
6. `If-Modified-Since` (parsed as RFC 1123) removes every file whose mtime is **not after** it.
7. Then:
   * **> 1 file remaining** → `Content-Disposition: attachment; filename=profile.zip`, `Content-Type: application/zip`, `Content-Length`, body = a Mission Package built by `createProfileFileMissionPackage` (lines 525-540) that **recreates the on-disk directory hierarchy inside the zip** (walking each file's parents up to the profile-directory root) plus `MANIFEST/manifest.xml` with params `uid=<random>`, `name=multiFile`, `onReceiveImport=true`, `onReceiveDelete=true`. Status **200**.
   * **exactly 1 file** → `Content-Disposition: attachment; filename=<file name>`, `Content-Type` = `URLConnection.guessContentTypeFromName(...)` (may be `null`!), `Content-Length`, body = the raw file. Status **200**.
   * **0 files left after If-Modified-Since** → **304 Not Modified**, no body.

Second-order timestamp truncation is used throughout (`Instant.ofEpochSecond(lastModified / 1000)`).

### 10.5 Admin API — `ProfileAdminAPI` (`ROLE_ADMIN` for DELETE; rest via `/Marti/**` → ROLE_ADMIN)

`WAR/com/bbn/marti/device/profile/api/ProfileAdminAPI.java`:

| Verb | Path | Line | Params / body |
|---|---|---|---|
| GET | `/Marti/api/device/profile` | 100 | → `ApiResponse<List<Profile>>` |
| GET | `/Marti/api/device/profile/{name}` | 124 | → `ApiResponse<Profile>` |
| POST | `/Marti/api/device/profile/{name}/send` | 149 | body `String[] selected` (client UIDs) |
| DELETE | `/Marti/api/device/profile/{id}` | 200 | by numeric id |
| POST | `/Marti/api/device/profile/{name}` | 214 | `group` (repeatable, default `""`) — create |
| PUT | `/Marti/api/device/profile/{name}` | 232 | body `Profile` — update |
| GET | `/Marti/api/device/profile/{name}/files` | 259 | → `ApiResponse<List<ProfileFile>>` |
| PUT | `/Marti/api/device/profile/{name}/file` | 272 | `filename` (req), body `byte[]` → `ApiResponse<ProfileFile>` |
| GET | `/Marti/api/device/profile/{name}/file/{id}` | 294 | → raw `byte[]` |
| DELETE | `/Marti/api/device/profile/{name}/file/{id}` | 312 | |
| GET | `/Marti/api/device/profile/directories` | 323 | valid sub-dirs of `./profiles` |
| PUT | `/Marti/api/device/profile/{name}/directories/{directories}` | 330 | |
| GET | `/Marti/api/device/profile/{name}/directories` | 359 | |
| DELETE | `/Marti/api/device/profile/{name}/directories` | 373 | |

`Profile` JSON (`WAR/com/bbn/marti/device/profile/model/Profile.java`): `id`, `name`, `active` (`boolean`), `applyOnEnrollment` (`boolean`), `applyOnConnect` (`boolean`), `type`, `updated` (`Date`), `tool`, and **`groups`** ← `@JsonProperty("groups") getGroupNames()` (line 141-142). `groupVector` is `@JsonIgnore` (line 113).

---

## 11. CoT query — `CotApi`

`WAR/com/bbn/marti/sync/api/CotApi.java`. `/Marti/api/cot/**` is `ROLE_ANONYMOUS`.

| Verb | Path | Line | Params | Response |
|---|---|---|---|---|
| GET | `/Marti/api/cot/xml/{uid:.+}` | 65 | — | `application/xml`, `Constants.XML_HEADER + cot.toCotXml()` — a **single `<event>`**, no `<events>` wrapper. **404 with an empty body** if not found; **500 with empty body** on validation failure. |
| GET | `/Marti/api/cot/xml/{uid:.+}/all` | 117 | `secago`, `start` (ISO), `end` (ISO) | `application/xml`, `XML_HEADER + "<events>" + …(each event + '\n')… + "</events>"`. **404 empty** when zero results. |
| GET **and** POST | `/Marti/api/cot` | 182 | `@RequestBody Set<String> uids` (JSON array of strings — yes, a body on GET) | `<events>…</events>`, `application/xml`. Empty/null set → `IllegalArgumentException` → 400. Any internal error → **500 with an empty body**. |
| GET | `/Marti/api/cot/sa` | 235 | **`start` (required ISO)**, **`end` (required ISO)**, `left`, `bottom`, `right`, `top` (all `Double`, opt — all four required together to form a bbox), `isFiltered`=`true` | `<events>…</events>`. `start.after(end)` or a window **> 24 hours** → `IllegalArgumentException` → 400. **404 empty** when zero results. |
| GET | `/Marti/api/cot/matchUid` | 295 | `search` (default `" "`) | **bare JSON array of strings** (`ResponseEntity<List<String>>`), no envelope, always 200. |

`/cot/xml/{uid}` post-processing (lines 96-105) — reproduce this if you want byte-identical ATAK file-transfer behaviour:
```java
if (cot.cottype != null && cot.cottype.startsWith("b-t-f")) cot = missionService.fixupMissionChat(cot, request.getServerName());
cot.setHae(Double.parseDouble(cot.hae));                       // normalise hae precision
cot.detailtext = cot.detailtext.replaceAll("<marti>.+<\\/marti>", "");   // strip <marti> detail
```
The comment says this must match `StreamingProtobufProtocol.createFileTransferRequest`.

`/cot/sa` bbox fields map to `GeospatialFilter.BoundingBox`: `left`→minLongitude, `bottom`→minLatitude, `right`→maxLongitude, `top`→maxLatitude.

Separately, `CotQueryApi` (`WAR/com/bbn/marti/cot/search/api/CotQueryApi.java`, `ROLE_ADMIN`) serves `/Marti/api/cot/search/date`, `/Marti/api/cot/search/{id}`, and a bare `GET /Marti/api` — these are the admin CoT-search endpoints, not ATAK-facing.

---

## 12. Complete `@RequestMapping` inventory, grouped by controller

All paths below are prefixed with **`/Marti/api`** unless marked ⚑ (absolute path — the controller does not extend `BaseRestController`).

**`VersionApi`** — `/version` GET, `/version/info` GET, `/version/config` GET, `/node/id` GET
**`HomeApi`** — `/home` GET, `/ver` GET, `/util/isAdmin` GET, `/util/user/roles` GET
**`GroupsApi`** — `/users/all` GET, `/users/{connectionId:.+}` GET, `/groups/{name}/{direction:.+}` GET, `/groups/all` GET, `/groups/user` GET, `/groups/groupCacheEnabled` GET
**`SubscriptionApi`** — `/subscriptions/all` GET, `/subscription/{uid}` GET, `/subscriptions/add` POST, `/subscriptions/delete/{uid}` DELETE, `/subscriptions/incognito/{uid}` POST, `/subscriptions/{clientUid}/filter` PUT+DELETE, `/groups/active` PUT, `/groups/activebits` PUT, `/groups/activeForce` PUT, `/groups/update` POST, `/groups/update/{username:.+}` GET
**`ContactsApi`** — `/contacts/all` GET, `/contacts/all/lite` GET, `/contacts/all/full` GET
**`ContactManagerApi`** — `/clientEndPoints` GET
**`CotApi`** — `/cot/xml/{uid:.+}` GET, `/cot/xml/{uid:.+}/all` GET, `/cot` GET+POST, `/cot/sa` GET, `/cot/matchUid` GET
**`CotQueryApi`** (ROLE_ADMIN) — `cot/search/date` GET, `cot/search/{id}` GET, *(bare)* GET
**`MissionApi`** — see §7.1 (99 mappings)
**`CopViewApi`** — `/cops` GET (`path`, `offset`, `size`), `/cops/hierarchy` GET
**`MetadataApi`** — `/sync/metadata/{hash}/{metadata}` PUT, `/sync/metadata/{hash}/keywords` PUT, `/sync/metadata/{hash}/expiration` PUT
**`SequenceApi`** — `/sync/sequence/{key}` GET
**`PropertiesApi`** — `/properties/uids` GET, `/properties/{uid}/all` GET+DELETE, `/properties/{uid}/{key}` GET+DELETE, `/properties/{uid}` PUT
**`FileManagerApi`** — `/files/metadata` GET, `/files/metadata/count` GET, `/files/{hash}` GET+DELETE+HEAD, `/files/{hash}/metadata` PUT
**`FileConfigurationApi`** ⚑ — `/files/api/config` GET+POST
**`CertManagerApi`** — `/tls/makeClientKeyStore` GET, `/tls/config` GET, `/tls/signClient` POST, `/tls/signClient/v2` POST
**`CertManagerAdminApi`** (ROLE_ADMIN) — `/certadmin/cert` GET, `/certadmin/cert/active|replaced|expired|revoked` GET, `/certadmin/cert/download/{ids}` GET, `/certadmin/cert/delete/{ids}` DELETE, `/certadmin/cert/revoke/{ids}` DELETE, `/certadmin/cert/{hash}` GET+DELETE, `/certadmin/cert/{hash}/download` GET
**`ProfileAPI`** — `/tls/profile/enrollment` GET, `/device/profile/connection` GET, `/device/profile/tool/{toolName}` GET, `/tls/profile/tool/{toolName}/file` GET, `/device/profile/tool/{toolName}/file` GET, `/device/profile/{name}/missionpackage` HEAD+GET
**`ProfileAdminAPI`** — see §10.5 (13 mappings)
**`OAuthApi`** ⚑ — `/login/auth` GET, `/login/redirect` GET, `/login/refresh` GET, `/login/authserver` GET, `/login/.well-known/openid-configuration` GET, `/logout` GET+POST, `/token/access` GET
**`TokenApi`** — `/token` GET, `/token/{token}` DELETE, `/token/revoke/{tokens}` DELETE
**`SecurityAuthenticationApi`** — `/authentication/config` GET+PUT+POST, `/security/config` GET+PUT, `/security/isSecure` GET, `/security/verifyConfig` GET
**`ConfigAPI`** — `/config` GET (ROLE_ADMIN), `/cachedConfig` GET, `/cachedInputConfig` GET
**`LDAPApi`** (ROLE_ADMIN) — `/groups` GET, `/groups/members` GET, `/groupprefix` GET
**`SubmissionApi`** (ROLE_ADMIN) — `/datafeeds/{name}` GET+PUT+DELETE, `/datafeeds` POST, `/inputs` GET+POST, `/inputs/{name}` GET+DELETE, `/inputs/{id}` PUT, `/inputs/config` GET+PUT, `/database/cotCount` GET, `/inputs/storeForwardChat/enabled` GET, `/inputs/storeForwardChat/enable|disable` PUT
**`DataFeedApi`** — `/datafeeds/bounds/{bbox}` GET, `/datafeeds/bounds/polygon` GET, `/datafeeds/stats/{uuid}` GET, `/datafeeds/stats` GET, `/datafeeds` GET, `/datafeeds/predicate` POST+PUT, `/datafeeds/predicate/{feedGuid:.+}` DELETE, `/datafeeds/predicate/{feedUuid:.+}` GET, `/datafeeds/{uuid}/cots/{cot_type}` GET, `/datafeeds/{uuid}/cots_types` GET
**`FederationApi`** (ROLE_ADMIN) — 38 mappings: `/outgoingconnections` GET/POST/PUT, `/outgoingconnections/{name}` GET/DELETE, `/outgoingconnectionstatus/{name}` GET/POST, `/activeconnections` GET, `/federates` GET, `/federatecontacts/{federateId}` GET, `/federategroups/{federateId}` GET/DELETE, `/federategroups` POST, `/federategroupsmap/{federateId}` GET/POST/DELETE, `/federate-outbound-groups-hop-limit/{federateId}` GET/POST/DELETE, `/federategroupconfig` POST, `/federateremotegroups/{federateId}` GET, `/federatecagroups/{caId}` GET/DELETE, `/federatecagroups` POST, `/federatecahops` POST, `/federatecatokenauth` POST, `/federatecertificates` GET/POST, `/federatecertificates/{fingerprint}` DELETE, `/federatedetails` PUT, `/federatedetails/{id}` GET, `/federatedetails/{federateId}` DELETE, `/fednum` GET, `/clearFederationEvents` GET, `/federatemissions/{federateId}` PU

T, `/generateAndSaveFederationJwtToken` POST, `/generateFederationJwtToken` POST
**`FederationConfigApi`** (ROLE_ADMIN) — `/federationconfig` GET+PUT, `/federationconfig/verify` GET
**`UIDSearchApi`** — `/uidsearch` GET
**`MapLayersApi`** — `/maplayers/all` GET, `/maplayers` POST+PUT, `/maplayers/{uid}` GET+DELETE
**`InjectionApi`** — `/injectors/cot/uid` GET+POST+DELETE, `/injectors/cot/uid/{uid}` GET (BASE_PATH constant)
**`RepeaterApi`** (class-level `@RequestMapping("/Marti/api/repeater")`) — `/list` GET, `/period` GET+POST, `/remove/{uid:.+}` GET
**`IconsetIconApi`** — `iconset` POST, `icon/{uid}/{group}/{name:.+}` GET, `iconseturl/{uid}` GET, `iconset/all/uid` GET, `iconurl` GET, `iconimage` GET
**`VideoConnectionManagerV2`** — `/video` GET+POST, `/video/{uid}` GET+PUT+DELETE
**`XmppAPI`** (ROLE_XMPP) — `/xmpp/transfer/{uid}/{filename}` GET+PUT
**`CITrapReportAPI`** — `/citrap` GET+POST, `/citrap/{id}` GET+PUT+DELETE, `/citrap/{id}/attachment` POST
**`ExCheckAPI`** — 18 mappings under `/excheck/**` (template, checklist, task, status, mission binding)
**`PluginManagerApi`** — `/plugins/info/all` GET, `/plugins/info/all/started` POST, `/plugins/info/started` POST, `/plugins/info/enabled` POST, `/plugins/info/archive` POST
**`PluginDataApi`** — `/plugins/{name:.+}/submit` GET+PUT+POST+DELETE, `/plugins/{name:.+}/submit/result` PUT
**`QoSApi`** — `/qos/delivery/enable` PUT, `/qos/delivery/set` PUT, `/qos/read/enable` PUT, `/qos/read/set` PUT, `/qos/dos/enable` PUT, `/qos/conf` GET, `/qos/ratelimit/delivery|read|dos/active` GET
**`RetentionApi`** (ROLE_ADMIN) — `/retention/policy` GET+PUT, `/retention/service/schedule` GET+PUT, `/retention/mission/{name}/expiry/{time}` PUT, `/retention/resource/{name}/expiry/{time}` PUT, `/retention/missionarchive` GET, `/retention/restoremission` POST, `/retention/missionarchiveconfig` GET+PUT
**`LocateApi`** ⚑ — `/locate/api` POST
**`RegistrationApi`** ⚑ — `/register/user` POST, `/register/token/{token:.+}` GET, `/register/admin/users` GET, `/register/admin/invite` POST
**`FileUserAccountManagementApi`** ⚑ (class `@RequestMapping("/user-management/api")`, ROLE_ADMIN) — `/new-user` POST, `/new-users` POST, `/list-users` GET, `/get-groups-for-user/{username}` GET, `/change-user-password` PUT, `/update-groups` PUT, `/update-group-users` PUT, `/delete-user/{username}` DELETE, `/list-groupnames` GET, `/users-in-group/{group}` GET
**`ErrorController`** ⚑ — `/error` (all verbs)
**`LoginAccessController`** ⚑ — `/login` (all verbs)

---

## 13. ATAK-critical items you did not list

These are the ones I would not omit from a wire-compatible implementation:

### 13.1 `GET /Marti/api/util/user/roles` — the read-only signal

`HomeApi.java:78-106`. Returns a **bare JSON array of strings** (no envelope):
```json
["ROLE_ANONYMOUS","ROLE_WEBTAK"]
```
Built from `SecurityContextHolder…getAuthorities()`, **plus** `"ROLE_READONLY"` appended when the caller has **no `Direction.IN` group** (lines 91-102). WebTAK uses this to decide whether to show write controls. `GET /Marti/api/util/isAdmin` returns a bare `true`/`false`.

### 13.2 `GET /Marti/api/home`

`HomeApi.java:60-63` — returns a bare **string path**, `"/Marti/metrics/index.html"` for admins else `"/webtak/index.html"`. Clients treat it as a redirect target, not a redirect.

### 13.3 Legacy servlets ATAK still calls

* `GET /Marti/GetTime` (`GetServerTimeServlet`) — server clock sync.
* `GET /Marti/KmlMasterSA/*`, `/Marti/LatestKML/*`, `/Marti/TracksKML/*`, `/Marti/ExportMissionKML/*` — KML feeds, `ROLE_ANONYMOUS`.
* `POST /Marti/ErrorLog` (`LogServlet`) — ATAK crash/error upload, `ROLE_ANONYMOUS` for POST, `ROLE_ADMIN` for GET/DELETE.
* `/Marti/vcm`, `/Marti/vcu`, `/Marti/vcs` — video connection manager/uploader/sender (`ROLE_ANONYMOUS` for GET/POST, `ROLE_ADMIN` for DELETE on `vcm`).
* `POST /Marti/sync/missioncreate` (`MissionPackageCreatorServlet`, `ROLE_ANONYMOUS`) — builds a data package server-side.

### 13.4 `/Marti/api/video` (`VideoConnectionManagerV2`)

`/video` GET+POST, `/video/{uid}` GET+PUT+DELETE — ATAK's video-alias sync. `ROLE_ANONYMOUS`.

### 13.5 `/Marti/api/injectors/cot/uid` (`InjectionApi`)

CoT detail injection by UID; GET/POST/DELETE at `BASE_PATH`, GET at `BASE_PATH + "/{uid}"`. DELETE is `ROLE_ADMIN`.

### 13.6 Blanket method denials

`security-context.xml` (lines ~295-296 of the intercept block):
```xml
<sec:intercept-url pattern="/Marti/**" method="DELETE"  access="ROLE_NONEXISTENT"/>
<sec:intercept-url pattern="/Marti/**" method="OPTIONS" access="ROLE_NONEXISTENT"/>
```
`ROLE_NONEXISTENT` is granted to nobody (`<sec:user-service>` defines only `usernonexistent`). These are **fall-through denials** — every DELETE and OPTIONS you want to support must be explicitly allowed by an earlier, more specific `intercept-url`. That is why the config lists `method="DELETE"` duplicates for `/Marti/api/missions/**`, `/maplayers/**`, `/files/**`, `/properties/**`, `/token/**`, etc. **Notably there is no `OPTIONS` allowance anywhere**, so CORS preflight to `/Marti/**` is 403 unless `<network allowAllOrigins="true">` short-circuits it in the filter.

Catch-alls at the end: `/Marti/**` → `ROLE_ADMIN`, then `/**` → `ROLE_ANONYMOUS`. So **anything under `/Marti/` not explicitly listed is admin-only**, while anything outside `/Marti/` is open.

### 13.7 `DataPackageFileBlocker`

`WAR/com/bbn/marti/sync/DataPackageFileBlocker.java` (297 lines) — inspects uploaded package contents against `TAKServerFilters.txt` / `TAKServer-rules.xml` at the repo root. Uploads can be rejected on content grounds independently of size/auth. Worth knowing exists; not required for compatibility.

---

## 14. Things I could not verify in this checkout

1. **`/shortver.txt` and `/ver.json` contents** — generated by Gradle at build time (`takserver-core/build.gradle:88-101`); not present as files. Body format is derived from the generator, not observed.
2. **`/oauth/token` error JSON** — no TAK-authored failure handler exists, so the response is whatever `OAuth2TokenEndpointFilter`'s default `OAuth2ErrorAuthenticationFailureHandler` produces in Spring Authorization Server 1.4.2 (`{"error":"invalid_grant"}` + 400, `invalid_client` + 401). Confirm against a live server.
3. **Exact marshalled bytes of `GET /Marti/api/tls/config`** — the namespace prefix allocation for the `QName("com.bbn.marti.config","certificateConfig")` root is JAXB-implementation-dependent. The element/attribute names are certain; the prefix and `xmlns` placement are not.
4. **`Content-Type` of `/Marti/api/tls/config`** — `ResponseEntity.ok().body(String)` with no explicit content type; depends on the request's `Accept`.
5. **`/oauth/check_token`** — granted in `security-context.xml` but **no implementing controller exists** in the tree. Expect 404.
6. **`/Marti/api/device/profile/enrollment`** — granted in `security-context.xml` but **no mapping exists**. The real path is `/Marti/api/tls/profile/enrollment`.
7. **`/Marti/sync/{hash}/metadata`** — `MetadataServlet` is dead code (never registered). Does not exist in 5.7.
8. **`/oauth/token_key`, JWKS endpoint** — no mapping anywhere. External verifiers are configured statically via `<authServer><key>` or a file path in `@issuer`.
9. **`$B/src/docs/TAK_Server_Configuration_Guide.pdf`** — the `src/docs` directory is not present in this sparse checkout (only `$B/src/docs` was listed as a sibling of `build.gradle`; `find` returned no PDF). All config semantics above come from `CoreConfig.xsd` and the Java, which are authoritative anyway.
10. **`ProfileService.getProfileDirectoriesForTool` NPE risk** — it can return `null` (line 364) on an illegal directory, and `getProfileDirectoryContent` immediately calls `directories.size()` (line 510) without a null check. Behaviour on that path is a 500 from the container, not the documented 404.
