I have everything I need. Here's the report.

---

# OpenTAKServer (OTS) — Marti API surface & internals

**Version examined:** `1.7.13-post.60+3c5d337`, HEAD `719258c` (2026-09-15, *"Corrected the format of server connection data packages, fixes #360"*). All paths relative to the OTS repo root.

## 0. Architecture at a glance

Three separate processes, glued by RabbitMQ:

| Process | Entry | Role |
|---|---|---|
| Flask web app | `opentakserver/app.py:561` `start()` | All HTTP (`/Marti/*`, `/api/*`, `/oauth/*`, `/files/*`), socket.io UI |
| EUD handler | `opentakserver/eud_handler/eud_handler.py:139` `main()` | TCP 8088 / SSL 8089 / UDP 8087 CoT streams (`--ssl`, `--udp` flags) |
| CoT parser | `opentakserver/cot_parser/cot_parser.py:1375` `main()` | Forked pool, consumes `cot_parser` queue, parses + **routes** CoT |

RabbitMQ exchanges declared in `app.py:169-179`:

```python
channel.exchange_declare("dms", durable=True, exchange_type="direct")
channel.exchange_declare("cot_parser", durable=True, exchange_type="direct")
channel.exchange_declare("chatrooms", durable=True, exchange_type="direct")
channel.exchange_declare("missions", durable=True, exchange_type="topic")   # For Data Sync mission feeds
channel.exchange_declare("groups", durable=True, exchange_type="topic")     # For channels/groups
channel.exchange_declare("firehose", durable=True, exchange_type="fanout")  # A firehose of all CoT data
channel.exchange_declare("flask-socketio", durable=False, exchange_type="fanout")
```

**Client-cert auth is done by nginx, not OTS.** nginx terminates TLS on 8443 and forwards the PEM URL-encoded in a header. `blueprints/marti_api/marti_api.py:25-58`:

```python
# Verifies the client cert forwarded by nginx in the X-Ssl-Cert header
# Returns the parsed cert if valid, otherwise returns None
def verify_client_cert() -> x509.Certificate | None:
    cert_header = app.config.get("OTS_SSL_CERT_HEADER")   # default "X-Ssl-Cert"
    ...
    """
        Note to future self, get the common name (which is the username) like this
        cert.subject.get_attributes_for_oid(NameOID.COMMON_NAME)[0].value
    """
```
It re-verifies the cert signature against `ca.pem` with SHA256/PKCS1v15 manually (`marti_api.py:42-48`). **CN == OTS username** is the universal identity mapping.

There's a commented-out global guard in `mission_marti_api.py:50-56` — most `/Marti` routes are effectively unauthenticated unless they explicitly call `verify_client_cert()`.

---

## 1. Route table

### 1.1 Enrollment / TLS — `blueprints/marti_api/certificate_enrollment_api.py`

| Method | Path | Auth | Params / body | Response |
|---|---|---|---|---|
| GET | `/Marti/api/tls/config` (`:65`) | **none enforced** (comment says "require basic auth" but no code) | – | XML `ns2:certificateConfig`, `Content-Type: text/plain; charset=UTF-8` |
| POST | `/Marti/api/tls/signClient/` (`:83`) | Basic (`Authorization`) | – | `"" 200` (stub) |
| POST | `/Marti/api/tls/signClient/v2` (`:90`) | Basic | query `clientUID` **or** `clientUid`; body = raw CSR (with or without PEM armor); `version` ignored | JSON or XML depending on `Accept` — see §2 |

The basic-auth handler (`:27-61`):
```python
# flask-security's http_auth_required() decorator will deny access because ATAK doesn't do CSRF,
# so we handle basic auth ourselves
def basic_auth(credentials):
```
It accepts, in order: LDAP bind (if `OTS_ENABLE_LDAP`), a matching Flask-Security password, **or** `Token.verify_token(password)` — i.e. the QR-enrollment JWT is presented as the HTTP Basic *password*.

### 1.2 Version / config / misc — `blueprints/marti_api/marti_api.py`

| Method | Path | Auth | Response |
|---|---|---|---|
| GET | `/Marti/api/clientEndPoints` (`:61`) | none (`# TODO: Add group support ?group=__ANON__`) | `{"version":3,"type":"com.bbn.marti.remote.ClientEndpoint","data":[{callsign,uid,username,lastEventTime,lastStatus}],"nodeId":…}` — note `version` is an **int** here |
| GET | `/Marti/api/version/config` (`:88`) | none | `{"version":"3","type":"ServerConfig","data":{"version":<ots ver>,"api":"3","hostname":<host>},"nodeId":…}` |
| GET | `/Marti/ExportMissionKML` (`:104`) | none | KML/KMZ file; params `startTime,endTime,uid,format`, plus `multiTrackThreshold,extendedData,optimizeExport` — `# Not sure what these three are supposed to do` |

Also in `ots_api/api.py`:
| GET | `/files/api/config` (`:182`) | none | `{"uploadSizeLimit": 400}` — docstring: *"Required by CloudTAK"* |
| GET | `/api/health` (`:194`) | none | `{"status":"healthy"}` |
| GET | `/api/truststore` (`:529`) | **none** — *"Downloads the server's truststore with no authentication required."* | `truststore-root.p12` as `truststore_root_<host>.p12` |
| GET | `/api/itak_qr_string` (`:600`) | session | plain text `OpenTAKServer_<host>,<host>,8089,SSL` |

### 1.3 Contacts / subscriptions

| Method | Path | File:line | Auth | Response |
|---|---|---|---|---|
| GET | `/Marti/api/contacts/all` | `contacts_marti_api.py:9` | none | **Bare JSON array** (not wrapped in version/type/data): `[{filterGroups:[], notes:<username>, callsign, team, role, takv:"<platform> <version>", uid}]`. Team defaults `"Cyan"`, role `"Team Member"` |
| GET | `/Marti/api/subscriptions/all` | `group_marti_api.py:454` | none | `{"version":"3","type":"SubscriptionInfo","data":[],"messages":[],"nodeId":…}` — **always empty**; reads `sortBy,direction,page,limit` and ignores them |

### 1.4 Groups / channels — `blueprints/marti_api/group_marti_api.py`

| Method | Path | Line | Auth | Notes |
|---|---|---|---|---|
| GET | `/Marti/api/groups/groupCacheEnabled` | 21 | none | `{"version":"3","type":"java.lang.Boolean","nodeId":…,"data":<OTS_ENABLE_CHANNELS bool>}` |
| GET | `/Marti/api/groups/all` | 33 | **client cert required** | `{"version":"3","type":"com.bbn.marti.remote.groups.Group","nodeId":…,"data":[…]}`. `useCache` query param is **never read anywhere in the codebase**. 400 + `"Groups are only supported on SSL connections"` when no cert |
| GET | `/Marti/api/groups` | 102 | none | LDAP only; requires `groupNameFilter` starting with `OTS_LDAP_GROUP_PREFIX` and ending `_READ`/`_WRITE`; type `com.bbn.marti.remote.groups.LdapGroup` |
| GET | `/Marti/api/groups/members` | 161 | none | LDAP member count; type `java.lang.Integer` |
| GET | `/Marti/api/groupprefix` | 216 | none | `{"version":"3","type":"java.lang.String","data":<prefix or "">,"nodeId":…}` |
| PUT | `/Marti/api/groups/activebits` | 230 | none | reads `clientUid` + JSON bits, **returns `"" 200`, does nothing** |
| PUT | `/Marti/api/groups/active` | 238 | client cert | body = JSON array of `{name, direction, active}`; binds/unbinds RabbitMQ queues. See §6 |
| GET | `/Marti/api/groups/update/<username>` | 386 | none | stub `{"version":"","type":"","data":true,"messages":[""],"nodeId":…}` |
| GET | `/Marti/api/groups/<group_name>/<direction>` | 400 | none | single group; (buggy — filters `Group.group_name` which doesn't exist) |
| GET | `/Marti/api/groups/activeForce` | 472 | none | requires `?username=`; returns a **bare** group object (no version/data envelope) with empty strings + `type` `LDAP`/`SYSTEM` |
| GET | `/Marti/api/groups/user` | 496 | none | requires `?username=`; stub with empty `data` |

### 1.5 Data Sync missions — `blueprints/marti_api/mission_marti_api.py`

| Method | Path | Line | Auth |
|---|---|---|---|
| GET | `/Marti/api/missions` | 347 | client cert; query `tool`, `passwordProtected`, `defaultRole` |
| GET | `/Marti/api/missions/guid/<mission_guid>` | 312 | `check_permission(guid)`; query `password` |
| GET | `/Marti/api/missions/<mission_name>` | 648 | **none** |
| PUT, POST | `/Marti/api/missions/<mission_name>` | 441 | client cert; query `creatorUid`\|`uid`, `description`, `tool`, `group`, `defaultRole`, `password` |
| DELETE | `/Marti/api/missions/<mission_name>` | 685 | JWT (or iTAK cert) + `MISSION_OWNER` |
| PUT, DELETE | `/Marti/api/missions/<mission_name>/password` | 763 | JWT + owner; query `password` |
| GET | `/Marti/api/missions/guid/<guid>/invitations` | 406 | none |
| GET | `/Marti/api/missions/<name>/invitations` | 407 | none |
| GET | `/Marti/api/missions/all/invitations` | 408 | none |
| GET | `/Marti/api/missions/invitations` | 409 | none (query `clientUid`) |
| PUT | `/Marti/api/missions/<name>/invite/<invitation_type>/<invitee>` | 817 | `check_permission` |
| DELETE | `/Marti/api/missions/<name>/invite/<invitation_type>/<invitee>` | 946 | `check_permission` |
| POST | `/Marti/api/missions/<name>/invite` | 1008 | `check_permission`; JSON array `[{type, invitee, role:{type}}]` |
| GET | `/Marti/api/missions/<name>/subscriptions/roles` | 1089 | `check_permission` |
| GET | `/Marti/api/missions/guid/<guid>/subscriptions/roles` | 1108 | `check_permission` |
| GET | `/Marti/api/missions/<name>/subscriptions` | 1295 | `check_permission` → `data` is a **flat array of clientUid strings** |
| PUT | `/Marti/api/missions/<name>/role` | 1132 | JWT + owner; query `clientUid`, `role` (absent ⇒ kick) |
| GET | `/Marti/api/missions/guid/<guid>/role` | 1264 | `check_permission`; returns `mission.roles[0]` (!) |
| PUT | `/Marti/api/missions/<name>/subscription` | 1352 | client cert (+ optional Bearer) |
| PUT | `/Marti/api/missions/guid/<guid>/subscription` | 1353 | ditto |
| DELETE | `/Marti/api/missions/<name>/subscription` | 1534 | JWT / iTAK cert |
| PUT | `/Marti/api/missions/<name>/keywords` | 1317 | `check_permission`; JSON array of strings |
| GET | `/Marti/api/missions/<name>/changes` | 1576 | `check_permission`; `squashed` read then ignored |
| POST | `/Marti/api/missions/logs/entries` | 1602 | `check_permission(request.json["missionNames"][0])` |
| GET | `/Marti/api/missions/<name>/log` | 1663 | `check_permission` |
| PUT | `/Marti/api/missions/guid/<guid>/contents` | 1857 | `check_permission` |
| PUT | `/Marti/api/missions/<name>/contents` | 1858 | JSON `{hashes:[], uids:[]}`, query `creatorUid` |
| DELETE | `/Marti/api/missions/guid/<guid>/contents` | 2059 | JWT/iTAK; query `uid` or `hash` |
| DELETE | `/Marti/api/missions/<name>/contents` | 2060 | ditto |
| PUT | `/Marti/api/missions/<name>/contents/missionpackage` | 2221 | Bearer required; **returns `"" 200`, stub** |
| GET | `/Marti/api/missions/<name>/cot` | 2232 | `check_permission` → `<events>` XML |
| GET | `/Marti/api/missions/guid/<guid>/cot` | 2233 | ditto |
| GET | `/Marti/api/missions/<name>/layers` | 2268 | **none**; `return ""` (completely empty body, no JSON) |
| POST | `/Marti/sync/upload` | 1697 | client cert; query `name`, `keywords[]`, `creatorUid`/`CreatorUid` |
| PUT | `/Marti/api/sync/metadata/<content_hash>/keywords` | 1820 | none |

**There is no `/Marti/api/missions/{name}/archive` route** — `grep -ri archive` finds only a marker-detail tag. No zip building for missions at all.

**No `API_VERSION` / `API-Version` header handling exists anywhere** (only `PLUGIN_API_VERSION = "1.0.0"` in `plugins/BasePlugin.py:13`). The `version` field in responses is hard-coded `"3"` (string) except `/Marti/api/missions` and `/Marti/api/clientEndPoints` which emit int `3`.

### 1.6 Enterprise sync / data packages — `blueprints/marti_api/data_package_marti_api.py`

| Method | Path | Line | Auth | Response |
|---|---|---|---|---|
| POST | `/Marti/sync/missionupload` | 178 | none | **plain-text URL body**: `https://{host}:{OTS_MARTI_HTTPS_PORT}/Marti/api/sync/metadata/{hash}/tool` |
| GET, PUT | `/Marti/api/sync/metadata/<file_hash>/tool` | 222 | none | PUT: body is the keyword string → stored on `DataPackage.keywords`, `200`. GET: the zip file |
| GET | `/Marti/api/sync/search` | 252 | none | `{"version":"3","type":"gov.tak.api.comms.takserver.mission.data.Resource","data":[…],"messages":[],"nodeId":…}` — lowerCamel keys (`filename,keywords,mimeType,name,submissionTime,submitter,uid,creatorUid,hash,size,tool,groups,expiration,latitude,longitude,altitude`). `# TODO: Support keywords` |
| GET | `/Marti/sync/search` | 316 | none | `{"resultCount":N,"results":[…]}` with **Title-case** keys (see §5) |
| GET, HEAD | `/Marti/sync/content` | 351 | none | `?hash=` → the file; falls back to `MissionContent` table; HEAD → 200/404 |
| GET | `/Marti/sync/missionquery` | 392 | none | plain-text URL (same as missionupload) or 404 JSON |
| GET | `/Marti/api/files/metadata` | 413 | none | `{"version":"3","type":"…Resource","data":[{Name,User,Creator,Size(<-human string!),Time,MimeType,Keywords,Expiration,Hash}],"nodeId":…}` |

### 1.7 Device profiles — `blueprints/marti_api/device_profile_marti_api.py`

| Method | Path | Line | Auth | Response |
|---|---|---|---|---|
| GET | `/Marti/api/tls/profile/enrollment` | 249 | comment: `# Authentication for /Marti endpoints handled by client cert validation` / `# EUDs hit this endpoint after a successful certificate enrollment` | `application/zip` streamed |
| GET | `/api/connection` | 264 | – | same handler |
| GET | `/Marti/api/device/profile/connection` | 265 | `# EUDs hit this endpoint when the app connects to the server if repoStartupSync is enabled` | zip; query `syncSecago`, `clientUid` |

**OTS never returns HTTP 204.** Even when nothing changed it builds and returns a full zip (with `preference.pref` + `truststore-root.p12` at minimum).

### 1.8 CoT query — `blueprints/marti_api/cot_marti_api.py`

Module docstring (`:13-16`):
> *"Right now OpenTAKServer only uses a few of these for Data Sync. The rest were added as place holders based on the API docs until I find an example of them actually being used by a TAK client"*

| GET | `/Marti/api/cot` (`:19`) | `"" 200` |
| GET | `/Marti/api/cot/xml/<uid>` (`:24`) | raw stored CoT XML, or 404 JSON |
| GET | `/Marti/api/cot/xml/<uid>/all` (`:39`) | `<events>` wrapping matched events; params `secago`, `start`, `end` |
| GET | `/Marti/api/cot/sa` (`:76`) | reads `start,end,left,bottom,right,top`, returns `"" 200` |

### 1.9 Video — `blueprints/marti_api/video_marti_api.py` (listed only)

`GET|POST /Marti/vcm` (`:24`, XML `<videoConnections>`), `GET /Marti/api/video` (`:138`), `POST /Marti/api/video` (`:156`), `GET /Marti/api/video/<uid>` (`:187`), `DELETE /Marti/api/video/<uid>` (`:210`). Notable ATAK quirk at `:114`: for iTAK the `<address>` is rewritten to a full `protocol://host:port/path` URL instead of a bare host.

### 1.10 CI Trap (Reports plugin) — `blueprints/marti_api/citrap_api.py`

`GET /Marti/api/citrap` (`:37`), `POST /Marti/api/citrap` (`:65`, body is a zip containing `report.xml`), `GET /Marti/api/citrap/<id>` (`:248`, cert + group check), `PUT` (`:302`) and `DELETE` (`:311`) `/Marti/api/citrap/<id>` — both **stubs returning `""`**, `POST /Marti/api/citrap/attachment` (`:320`) — stub, `# body is JSON`.

### 1.11 ATAK update server (`/api/packages`) — `blueprints/ots_api/package_api.py`

All client-cert-gated: `GET /api/packages/<package_name>` and `/api/packages/<atak_version>/<package_name>` (`:25-26`), `HEAD|GET /api/packages/product.infz` (`:56,82`), `HEAD|GET /api/packages/<atak_version>/product.infz` (`:68,92`), `GET /api/packages/repositories.inf` (`:158`). `product.inf` is a CSV of `platform,plugin_type,package_name,name,version,revision_code,file_name,icon_filename,description,apk_hash,os_requirement,tak_prereq,file_size` zipped alongside icons (`create_product_infz`, `:101-156`).

### 1.12 OAuth / tokens

`GET|POST /oauth/token` — `blueprints/ots_api/token_api.py:25`. See §7.
`POST|GET|DELETE /api/atak_qr_string` — `token_api.py:83, 163, 205`.

---

## 2. Enrollment & CA (`certificate_authority.py`, `ca_config.py`)

### CA generation (`create_ca()`, `:26-120`) — all via `subprocess` shelling out to `openssl`

1. Writes `ca_config.cfg` (contents = `ca_config.py:3-43`), then:
   `openssl req -new -sha256 -x509 -days {OTS_CA_EXPIRATION_TIME=3650} -extensions v3_ca -keyout ca-do-not-share.key -out ca.pem -passout pass:{OTS_CA_PASSWORD=atakatak} -config ca_config.cfg -subj /C=WW/ST=XX/L=YY/O=ZZ/OU=OpenTAKServer/CN=OpenTAKServer-CA`
   Key: `default_bits = 2048` RSA. Extensions `[v3_ca]`: `basicConstraints=critical,CA:TRUE`, `keyUsage=critical, cRLSign, keyCertSign`.
2. `openssl x509 -addtrust clientAuth -addtrust serverAuth -setalias {CA_NAME} -out ca-trusted.pem`
3. `openssl pkcs12 [-legacy] -export -in ca-trusted.pem -out truststore-root.p12 -passout pass:atakatak -nokeys -caname {CA_NAME}` — **`-legacy` is used when `openssl list -providers` returns 0**, which is basically always (`use_legacy = not subprocess.call(...)`, `:68`).
4. Generates `ca.crl` via `openssl ca -gencrl` with `crl_index.txt` / `unique_subject = no`.
5. `issue_certificate("opentakserver", server=True)` for the server cert.

### Server cert (`issue_certificate` + `sign_csr(server=True)`, `:122-339`)

- CSR: `openssl req -new -newkey rsa:2048 -sha256 …` (RSA-2048, SHA-256) with the same subject + `CN=opentakserver`.
- SAN config rendered from `ca_config.py:45-90` `server_config`: `subjectAltName = @alt_names` with `IP.1 = <cn>` if the CN matches `^\d{1,3}.\d{1,3}.\d{1,3}.\d{1,3}$`, else `DNS.1 = <cn>`.
- `[server]` extensions: `basicConstraints=critical,CA:FALSE`, `keyUsage=critical, digitalSignature, keyEncipherment`, `extendedKeyUsage = critical, clientAuth, serverAuth`.
- Signing: `openssl x509 -sha256 -req -days 3650 -CA ca.pem -CAkey ca-do-not-share.key -set_serial {random.randint(10000,100000)} -extensions server -extfile …` — **serial is a small random int; no serial registry, collisions possible**.
- The CA cert is **appended** to the server `.pem` (chain file, `:316-328`).
- Also emits `opentakserver.nopass.key` and `opentakserver.pub` — `# Generate public key for PyJWT to validate tokens` (`:232`). This RSA key pair is the JWT signing key for *everything* (mission tokens, enrollment QR tokens, OAuth tokens).

### Client CSR signing (`sign_csr(server=False)`)

Same command but `-extensions client` from `ca_config.cfg`:
```
[ client ]
basicConstraints=critical,CA:FALSE
keyUsage=critical, digitalSignature, keyEncipherment
extendedKeyUsage = critical, clientAuth
```
Validity 3650 days. **The client's CSR subject is used verbatim** — OTS does not rewrite the subject; it only reads the CN for logging/DB (`certificate_enrollment_api.py:112`).

### `/Marti/api/tls/config` exact XML (`certificate_enrollment_api.py:65-80`)

```python
root_element = Element("ns2:certificateConfig")
root_element.set("xmlns", "http://bbn.com/marti/xml/config")
root_element.set("xmlns:ns2", "com.bbn.marti.config")
name_entries = SubElement(root_element, "nameEntries")
first_name_entry = SubElement(name_entries, "nameEntry")
first_name_entry.set("name", "O");  first_name_entry.set("value", OTS_CA_ORGANIZATION)
second_name_entry = SubElement(name_entries, "nameEntry")
second_name_entry.set("name", "OU"); second_name_entry.set("value", OTS_CA_ORGANIZATIONAL_UNIT)
return tostring(root_element), 200, {"Content-Type": "text/plain; charset=UTF-8"}
```
Wire form:
```xml
<ns2:certificateConfig xmlns="http://bbn.com/marti/xml/config" xmlns:ns2="com.bbn.marti.config"><nameEntries><nameEntry name="O" value="ZZ" /><nameEntry name="OU" value="OpenTAKServer" /></nameEntries></ns2:certificateConfig>
```
No XML declaration, no `<validityPeriod>`.

### `/Marti/api/tls/signClient/v2` response (`:128-236`)

The cert body is stripped of PEM armor and newlines-around-armor before being embedded.

```python
# iTAK expects a JSON response but with the Content-Type header set to text/plain for some reason
if (request.headers.get("Accept") == "text/plain"
        or request.headers.get("Accept") == "application/json"
        or request.headers.get("Accept") == "*/*"
        or not request.headers.get("Accept")):
    response = {"signedCert": signed_csr, "ca0": cert, "ca1": cert}
else:
    enrollment = Element("enrollment")
    signed_cert = SubElement(enrollment, "signedCert"); signed_cert.text = signed_csr
    ca = SubElement(enrollment, "ca");                  ca.text = cert
    response = tostring(enrollment).decode("utf-8")
    response = '<?xml version="1.0" encoding="UTF-8"?>\n' + response
```
So: **XML is `<enrollment><signedCert>…</signedCert><ca>…</ca></enrollment>`** — a *flat* `<ca>` sibling, not nested inside `<signedCert>`. JSON is `{signedCert, ca0, ca1}` where `ca0 == ca1 == ` the root CA (duplicated).

Content-Type dispatch (`:220-236`), note the bogus `Content-Encoding` header:
- `Accept: text/plain` → the **dict** returned with `Content-Type: text/plain`, `Content-Encoding: charset=UTF-8` (Flask stringifies the dict → this is a bug for non-JSON-tolerant clients)
- `Accept: application/json` or `*/*` → `jsonify(response)`
- anything else → XML with `Content-Type: application/xml`, `Content-Encoding: charset=UTF-8`

Side effects: creates/links the `EUD` row (`uid` ← `clientUID`/`clientUid`) to the authenticated user, and a `Certificate` row with `expiration_date = today + OTS_CA_EXPIRATION_TIME days`, `server_port = OTS_MARTI_HTTPS_PORT`, `cert_password = OTS_CA_PASSWORD`.

### Manual (non-enrollment) `.p12` + data package — `generate_zip()` (`:344-542`)

Triggered by `POST /api/certificate` (`ots_api/api.py:274`) which calls `ca.issue_certificate(username, False)` → `generate_zip(common_name)`. Produces **three** artifacts and registers each as a `DataPackage` row (`api.py:303-348`).

Hard-coded folder UUIDs: `parent_folder = "80b828699e074a239066d454a76284eb"`, `folder = "5c2bfcae3d98c9f4d262172df99ebac5"`.

**The `.pref` (ATAK/WinTAK), verbatim from `:355-377`:**
```xml
<?xml version='1.0' standalone='yes'?>
<preferences>
    <preference version="1" name="cot_streams">
        <entry key="count" class="class java.lang.Integer">1</entry>
        <entry key="description0" class="class java.lang.String">OpenTAKServer_{{ server }}</entry>
        <entry key="enabled0" class="class java.lang.Boolean">true</entry>
        <entry key="connectString0" class="class java.lang.String">{{ server }}:{{ ssl_port }}:ssl</entry>
        <entry key="caLocation0" class="class java.lang.String">cert/{{ server_filename }}</entry>
        <entry key="caPassword0" class="class java.lang.String">{{ cert_password }}</entry>
        <entry key="clientPassword0" class="class java.lang.String">{{ cert_password }}</entry>
        <entry key="certificateLocation0" class="class java.lang.String">cert/{{ user_filename }}</entry>
    </preference>
    <preference version="1" name="com.atakmap.app_preferences">
        <entry key="deviceProfileEnableOnConnect" class="class java.lang.Boolean">true</entry>
        <entry key="displayServerConnectionWidget" class="class java.lang.Boolean">true</entry>
        <entry key="appMgmtEnableUpdateServer" class="class java.lang.Boolean">true</entry>
        <entry key="atakUpdateServerUrl" class="class java.lang.String">https://{{ server }}:{{ marti_port }}/api/packages</entry>
        <entry key="repoStartupSync" class="class java.lang.Boolean">true</entry>
        <entry key="updateServerCaLocation" class="class java.lang.String">cert/{{ server_filename }}</entry>
        <entry key="updateServerCaPassword" class="class java.lang.String">{{ cert_password }}</entry>
    </preference>
</preferences>
```
`server_filename` = `truststore-root.p12`, `user_filename` = `{common_name}.p12`, `cert_password` = `OTS_CA_PASSWORD`. **There is no `useAuth0` and no `enrollForCertificateWithTrust0`** — this package is for certs that are already issued.

Inner manifest (`:379-391`):
```xml
<MissionPackageManifest version="2">
   <Configuration>
      <Parameter name="uid" value="{{ uid }}"/>
      <Parameter name="name" value="OpenTAKServer_{{ server }}"/>
      <Parameter name="onReceiveDelete" value="true"/>
   </Configuration>
   <Contents>
      <Content ignore="false" zipEntry="{{ folder }}/preference.pref"/>
      <Content ignore="false" zipEntry="{{ folder }}/{{ server_filename }}"/>
      <Content ignore="false" zipEntry="{{ folder }}/{{ user_filename }}"/>
   </Contents>
</MissionPackageManifest>
```

Outer manifest (`:393-402`) wraps the inner zip — `# Create outer DP...because WinTAK` (`:465`):
```xml
<MissionPackageManifest version="2">
   <Configuration>
      <Parameter name="uid" value="{{ uid }}"/>
      <Parameter name="name" value="OpenTAKServer_{{ server }}_CONFIG"/>
   </Configuration>
   <Contents>
      <Content ignore="false" zipEntry="{{ folder }}/{{ internal_dp_name }}.zip"/>
   </Contents>
</MissionPackageManifest>
```
No `onReceiveDelete` on the outer one.

iTAK variant (`:493-510`, `# Generate iTAK zip`) — **flat zip, no MANIFEST at all**, file is `config.pref` + `{cn}.p12` + `truststore-root.p12` at the zip root, and keys are **un-suffixed** and live under `com.atakmap.app_preferences` rather than `cot_streams`:
```xml
<preferences>
  <preference version="1" name="cot_streams">
    <entry key="count" class="class java.lang.Integer">1</entry>
    <entry key="description0" ...>OpenTAKServer_{{ server }}</entry>
    <entry key="enabled0" ...>true</entry>
    <entry key="connectString0" ...>{{ server }}:{{ ssl_port }}:ssl</entry>
  </preference>
  <preference version="1" name="com.atakmap.app_preferences">
    <entry key="displayServerConnectionWidget" ...>true</entry>
    <entry key="caLocation" class="class java.lang.String">cert/truststore-root.p12</entry>
    <entry key="caPassword" class="class java.lang.String">{{ cert_password }}</entry>
    <entry key="clientPassword" class="class java.lang.String">{{ cert_password }}</entry>
    <entry key="certificateLocation" class="class java.lang.String">cert/{{ common_name }}.p12</entry>
  </preference>
</preferences>
```
Returns `["{cn}_CONFIG.zip", "{cn}_CONFIG_iTAK.zip"]`. Note `os.chdir()` at `:451` — this process-global chdir is a real landmine.

### `/Marti/api/tls/profile/enrollment` zip — `create_profile_zip(enrollment=True)` (`device_profile_marti_api.py:71-244`)

Zip layout:
- `MANIFEST/manifest.xml` — `<MissionPackageManifest version="2">` with `Parameter name="uid"` (random UUID), `name="Device Profile"`, `onReceiveDelete="true"`
- `5c2bfcae3d98c9f4d262172df99ebac5/preference.pref`
- `5c2bfcae3d98c9f4d262172df99ebac5/truststore-root.p12`
- On enrollment only: every `maps/*.xml` from `opentakserver/maps/` (38 map source XMLs), each listed in `<Contents>`
- Any `Packages` rows with `install_on_enrollment=True` (APKs) and `DataPackage` rows with `install_on_enrollment=True`

Hard-coded `.pref` entries under `com.atakmap.app_preferences` (`:103-146`):
| key | class | value |
|---|---|---|
| `appMgmtEnableUpdateServer` | Boolean | `true` |
| `atakUpdateServerUrl` | String | `https://{host}:{OTS_MARTI_HTTPS_PORT}/api/packages` |
| `repoStartupSync` | Boolean | `true` |
| `deviceProfileEnableOnConnect` | Boolean | `true` |
| `updateServerCaLocation` | String | `/storage/emulated/0/atak/cert/truststore-root.p12` (absolute Android path — breaks WinTAK/iTAK) |
| `updateServerCaPassword` | String | `OTS_CA_PASSWORD` |
| `prefs_enable_channels_host-{hostname}` | **String** | `"true"` |
| `prefs_enable_channels` | **String** | `"true"`/`"false"` from `OTS_ENABLE_CHANNELS` |

Plus per-user LDAP-derived prefs via `get_ldap_attributes()` (`:29-68`): `locationTeam`, `atakRoleType`, `locationCallsign`, and any `ots_*`-prefixed LDAP attribute becomes a raw preference key.

`/Marti/api/device/profile/connection` uses `enrollment=False` and filters `DeviceProfiles.connection==True, active==True`, honours `syncSecago` (compares `publish_time >= now - syncSecago`) and `clientUid` (`eud_uid == clientUid OR eud_uid IS NULL`). `# TODO: Support data packages and plugins per EUD` (`:188`).

### QR enrollment

`token_api.py:83` `POST /api/atak_qr_string` — docstring:
> *"Generates a QR string for ATAK certificate enrollment. ATAK certificate enrollment via QR code only works if your server has a Let's Encrypt certificate."*

```python
response["qr_string"] = (
    f"tak://com.atakmap.app/enroll?host={urlparse(request.url_root).hostname}"
    f":{app.config.get('OTS_SSL_STREAMING_PORT')}"
    f"&username={username}&token={token.generate_token()}"
)
```
Note the port used is **8089 (SSL streaming)**, not 8443. `TODO: Fix this before January 19, 2038 03:14:07Z` (`:118`) for the ms-vs-s epoch heuristic. The token is a RS256 JWT `{sub,iat,iss:"OpenTAKServer",aud:"OpenTAKServer"[,max][,nbf][,exp]}` signed by `opentakserver.nopass.key`; `Token.verify_token` (`models/Token.py:98`) decodes it, re-hashes the decoded claims with SHA-256 and looks that hash up in the `tokens` table, checks `disabled` and `max` vs `total_uses`, then increments `total_uses`. That token is then accepted as the HTTP Basic *password* at `/Marti/api/tls/signClient/v2`.

**No `tak://com.atakmap.app/import?...` QR is generated anywhere.** The only other QR string is the iTAK one (`/api/itak_qr_string`, plain text `OpenTAKServer_<host>,<host>,8089,SSL`).

---

## 3. EUD handler / streaming

### Transport (`eud_handler/`)

- `EudServer` (`EudServer.py:6`) = `socketserver.ForkingTCPServer`, port from `OTS_TCP_STREAMING_PORT` (8088), `max_children = 9999999`.
- `EudServerSSL` (`EudServerSSL.py:16-43`): `ssl.PROTOCOL_TLS_SERVER`, **`verify_mode = ssl.CERT_REQUIRED`**, chain `certs/opentakserver/opentakserver.pem` + `opentakserver.nopass.key`, `load_verify_locations(cafile=ca.pem)`, `do_handshake_on_connect=False`, port 8089.
- `EudServerUdp` (`EudServerUdp.py:6`) = `ForkingUDPServer` on 8087.
- Per-connection: each handler opens its **own** `pika.SelectConnection` in a background thread (`EudHandler.setup`, `:161-186`).

### Framing — XML only, regex-split

`EudHandler.handle()` (`:96-130`):
```python
data = self.request.recv(65536)
cot += data.decode("utf-8")
cot_list = re.split("</event>|</auth>", cot)
if len(cot_list) < 2: continue
for c in cot_list:
    if "<event" in c:  fromstring(c + "</event>"); self.handle_cot(c + "</event>")
    elif "<auth>" in c: fromstring(c + "</auth>"); self.handle_auth(c + "</auth>")
```
**There is no TAK Protocol v1 support at all.** No `t-x-takp-v` / `-q` / `-r` negotiation, no `0xbf` magic byte, no varint length prefix, no `TakMessage` protobuf. `grep -i "takp|0xbf|varint|TakMessage"` returns nothing. The only protobufs in the tree (`proto/atak.proto`, `atak_pb2.py`) are the **Meshtastic ATAK-plugin `TAKPacket`**, used solely by the Meshtastic bridge in `cot_parser.py`/`meshtastic_controller.py`. Message boundaries are also broken by design for multi-byte UTF-8 split across `recv()` calls (`.decode("utf-8")` per chunk).

### `<auth>` handling (`handle_auth`, `:340-435`)

Guard is `if self.is_ssl and not self.is_authenticated and (auth or self.common_name)` — i.e. **`<auth>` is only processed on the SSL listener**; on plain TCP (8088) it is parsed and silently dropped, and no authentication is ever required. Parses `<auth><cot username password uid/></auth>` and:
- LDAP path: `ldap_manager.authenticate()`, then create/link the `EUD` by `uid`.
- Non-LDAP: `verify_password(password, user.password)`; on success create/link `EUD(uid) → user.id`.
- Cert path: `EudHandlerSSL.setup()` (`EudHandlerSSL.py:20-28`) pulls `commonName` out of `getpeercert()["subject"]` and calls `handle_auth("")`, so a valid client cert alone authenticates: `"{} is ID'ed by cert".format(user.username)` (`:408`). Inactive users are disconnected.

Every `handle_cot` re-checks `if self.is_ssl and not self.is_authenticated: return` (`:443`).

### Ping/pong (`pong()`, `:132-159`) — **buggy**

```python
if event.attrs.get("type") == "t-x-c-t":
    cot = Element("event", {..., "type": "t-x-c-t-r", "uid": "{}-pong".format(event.attrs.get("uid")), ...})
    SubElement(cot, "point", {...})
    try:
        self.request.send(event.encode())   # <-- sends the ORIGINAL event back, `cot` is discarded
```
It builds the correct `t-x-c-t-r` pong with uid `<orig-uid>-pong`, then **echoes the client's own ping back** instead. Returns `True`, which short-circuits `handle_cot` so pings are never published.

### Identity & queue binding (`parse_device_info`, `:486-757`)

Triggered on the first event that has `<takv>` or `<contact>` (`:494-497`) — comment:
> *"EUDs running the Meshtastic and dmrcot plugins can relay messages from their RF networks to the server so we want to use the UID of the "off grid" EUD, not the relay EUD"*

Only treated as an EUD when `contact and uid and not uid.endswith("ping") and (self.user or not self.is_ssl)` (`:502`). Skips queue setup for `platform in ("OpenTAK ICU", "Meshtastic", "DMRCOT")`.

Binds (all on the per-connection channel):
- `queue_declare(self.callsign)` **and** `queue_declare(self.uid)` — two queues per EUD, both consumed with `auto_ack=True`.
- SSL + has `GroupUser(direction=OUT)` rows → `queue_bind(exchange="groups", queue=uid, routing_key=f"{group.name}.OUT")` for each **enabled** membership.
- SSL + no memberships → `"__ANON__.OUT"`, logging `"{callsign} doesn't belong to any groups, adding them to the __ANON__ group"`.
- **Plain TCP** → `"__ANON__.OUT"` unconditionally: `"{callsign} is connected via TCP, adding them to the __ANON__ group"` (`:625-638`).
- `queue_bind(exchange="missions", routing_key="missions", queue=uid)` — the global mission-broadcast key.
- DMs: `queue_bind("dms", queue=uid, routing_key=uid)` **and** `queue_bind("dms", queue=callsign, routing_key=callsign)` — comment `:595`: *"The DMs queue also binds by callsign since the `<dest>` tag in CoT messages can be by callsign instead of UID"*.

Per-connection mission-feed bindings (`missions.{name}`) are added later by the HTTP subscribe endpoint (`mission_marti_api.py:1518-1519`), not here.

`on_message` (`:330-338`) is the only outbound path:
```python
body = json.loads(body)
if body["uid"] != self.uid:
    self.request.send(body["cot"].encode())
```
i.e. **loop suppression is by comparing the publisher UID to this connection's UID**, nothing else.

### What a newly connected client gets

**Nothing.** There is no state replay: no last-known SA of other EUDs, no mission bootstrap, no `t-x-c-t` handshake. The client only starts receiving from the moment its queues are bound. (The web UI gets a bootstrap via `/api/map_state`, `ots_api/api.py:541`, but EUDs don't.)

### Publish path (`publish_cot`, `:459-484`)

Every accepted event is published twice, with `expiration = OTS_RABBITMQ_TTL` (86400000 ms):
```python
# Route all CoTs to the firehose exchange for plugins and users that connect directly to RabbitMQ
self.rabbit_channel.basic_publish(exchange="firehose", body=json.dumps({"uid": self.uid, "cot": str(event)}), routing_key="")
# Route all cots to the cot_parser direct exchange to be processed by a pool of cot_parser processes
self.rabbit_channel.basic_publish(exchange="cot_parser", body=json.dumps({"uid":…, "cot":…, "user_id":…}), routing_key="cot_parser")
```

### Routing (`cot_parser.py`)

`route_cot()` (`:1146-1216`) — the authoritative fan-out:
```python
if not uid or uid == OTS_NODE_ID:
    # This is a server generated CoT (i.e. ADS-B scheduled job) which was already properly routed
    return
destinations = event.find_all("dest")
if destinations:
    for destination in destinations:
        # ATAK and WinTAK use callsign, iTAK uses uid
        if "callsign" in destination.attrs and destination.attrs["callsign"]:
            publish(exchange="dms", routing_key=destination.attrs["callsign"], …)
        # iTAK uses its own UID in the <dest> tag when sending CoTs to a mission so we don't send those to the dms exchange
        elif "uid" in destination.attrs and destination["uid"] != uid:
            publish(exchange="dms", routing_key=destination.attrs["uid"], …)
        # CoT messages belonging to Data Sync missions (i.e. <dest mission="mission name" /> are handled by cot_parser
if not destinations and not user_id:
    # Publish all CoT messages received by TCP and that have no destination to the __ANON__ group
    publish(exchange="groups", routing_key="__ANON__.OUT", …); return
if not destinations:
    group_memberships = GroupUser(user_id=…, direction=Group.IN, enabled=True)
    if not group_memberships: publish("groups", "__ANON__.OUT", …)   # Default to the __ANON__ group if the user doesn't belong to any IN groups
    for membership in group_memberships:
        publish("groups", routing_key=f"{membership.group.name}.OUT", …)
```
Key asymmetry: a sender's **IN (write)** memberships determine which `<group>.OUT` keys it publishes to; a receiver's **OUT (read)** memberships determine which keys its queue is bound to. `<dest>` short-circuits group routing entirely — a DM bypasses channel enforcement.

### `<dest mission="…">` — `generate_mission_change(uid, event)` (`:1035-1144`)

For each `<dest mission="X">`: publish the raw event to `missions` / `missions.X`; upsert a `MissionUID` row (extracting `color@argb|value`, `usericon@iconsetpath`, `point@lat/lon`, `contact@callsign`); create a `MissionChange(ADD_CONTENT)`; then publish a second, `t-x-m-c` change CoT to the same routing key. **The change message is published twice** (once at `:1129`, again in the `mission_changes` loop at `:1137`) — duplicate `t-x-m-c` on the wire.

### GeoChat (`parse_geochat`, `:414-557`)

Reads `<__chat chatroom id parent groupOwner>` and `<chatgrp uid0 uid1 …>`; `<remarks time>`. `# Sometimes WinTAK seems to send GeoChat CoTs without remarks` → returns early if no remarks (`:420`). Sender is taken from `chatgrp@uid0`. Every `uid*` attr on `<chatgrp>` becomes a `ChatroomsUids` row; `groupOwner == "true"` + `uid0` sets `Chatroom.group_owner`. A DM is detected as `dest@callsign == chat@chatroom` (`:474-478`). **GeoChat routing itself is not special-cased** — chat CoTs go through the normal `<dest>`/group path. The `chatrooms` exchange is declared but never published to or bound.

### Specific CoT types

- **`t-x-d-d`** (`:1260-1280`): marks the EUD `Disconnected`, updates `last_event_time`, emits to socket.io. It is *also* routed normally. On socket close, `close_connection()` (`EudHandler.py:191`) publishes `{"uid":…, "cot": None, "disconnected": True, "user_id":…}` to `cot_parser`, which triggers `send_disconnect_cot()` (`:944-1033`) — builds a `t-x-d-d` with `<link relation="p-p" uid=… type="a-f-G-U-C"/>` and `<_flow-tags_ TAK-Server-f1a8159ef7804f7a8a32d8efc4b773d0="…"/>`, publishes it to every enabled OUT group, unbinds the EUD's queue from those groups and from every `missions.{name}`.
- **`t-x-m-c`** — server-generated only, see §4.
- **`t-x-g-c`** (group change) — **not handled anywhere**; no grep hit.
- **`b-f-t-r` / `b-f-t-a` fileshare** — **effectively unhandled**. The only mention is `fileshare = event.find("fileshare")` at `EudHandler.py:488`, assigned and never used. OTS never rewrites `senderUrl`, never emits a `b-f-t-r`. `grep -rn "senderUrl\|b-f-t"` → one hit, the dead variable. Data-package sharing works purely because the sender's own `b-f-t-r` (with its own `senderUrl`) is relayed verbatim via `<dest>` and the recipient fetches from `/Marti/sync/content?hash=`.

---

## 4. Data Sync internals

### JSON shapes

**Mission** (`models/Mission.py:97-141` `to_json`) — keys: `name, description, chatRoom, baseLayer, bbox, path, classification, tool, defaultRole, keywords[], creatorUid, createTime, externalData[], feeds[], mapLayers[], inviteOnly, expiration, guid, uids[], contents[], passwordProtected, missionChanges[], qr_code, owner, groups[]`.
`defaultRole` is replaced with the **full role object** (`{"type":…, "permissions":[…]}`), not a string (`:132-139`).
`qr_code` is a non-standard extra: `f"{url}:{OTS_SSL_STREAMING_PORT}:ssl,{url}-{OTS_MARTI_HTTPS_PORT}-ssl-{name},{name}"`.
`to_marti_json()` (`:143-150`) differs from `to_json()` only in `groups`: array of **name strings** instead of `{id,name}` objects. Confusingly, `GET /Marti/api/missions` and `/guid/<guid>` use `to_marti_json`, while `PUT`, `/contents` and `GET /Marti/api/missions/<name>` use `to_json`.

**MissionChange** (`models/MissionChange.py:60-78`): `isFederatedChange, type, contentUid, missionName, timestamp, creatorUid, serverTime, missionGuid` — **`missionGuid` is set to `self.mission_uid`**, the CoT UID, not the mission's GUID. Optional `contentResource` (the inner `data` of `MissionContent.to_json()`) and `details` (`MissionUID.to_details_json()`).

**MissionRole / subscription** (`models/MissionRole.py:61-77`): `{clientUid, username, createTime, role:{type, permissions[]}}`. Role constants and canonical permission sets are at `:15-42` (`OWNER_ROLE` = 8 perms, `SUBSCRIBER_ROLE` = READ+WRITE, `READ_ONLY_ROLE` = READ). Wrapped as `{"version":"3","type":"MissionSubscription","data":[…]}`.

**MissionLogEntry** (`models/MissionLogEntry.py:51-63`): `{id, content, creatorUid, entryUid, missionNames:[…], servertime (lowercase 's'!), dtg, created, contentHashes, keywords}`. Envelope type `com.bbn.marti.sync.model.LogEntry`.

**MissionContent** (`models/MissionContent.py:49-65`): `{"data": {keywords, mimeType, name, submissionTime, submitter, uid, creator_uid (snake_case outlier), hash, size, expiration}, "timestamp":…, "creatorUid":…}`.

**MissionUID** (`models/MissionUID.py:42-54`): `{data:<uid>, timestamp, creatorUid, details:{type, callsign, iconsetPath, color, location:{lat,lon}}}`.

**MissionInvitation** (`models/MissionInvitation.py:64-74` `to_marti_json`): `{missionName, invitee, role:[MISSION_SUBSCRIBER], type, creatorUid, createTime, token:"", missionGuid}` — `invitee` is the **SQLAlchemy relationship object**, not a string (serialization bug); `role` is an array of one string, not a role object; `token` is always empty.

### Tokens

`generate_token(mission, eud_uid)` (`mission_marti_api.py:144-175`) — RS256 JWT signed with `certs/opentakserver/opentakserver.nopass.key`. Documented payload:
```python
"""
jti: Unique UUID for the token
iat: Time token was issued. Can be used to invalidate a token if it was issued before a security event occurred
sub: The thing the token identifies, the EUD's UID in this case. Used to verify EUD roles, ie MISSION_SUBSCRIBER, MISSION_OWNER, or MISSION_READ_ONLY
MISSION_NAME: The mission this token is for
MISSION_GUID: The guid of the mission this token is for
"""
payload = {"jti": str(uuid.uuid4()), "iat": int(time.time()), "sub": eud_uid,
           "iss": urlparse(request.base_url).hostname,
           "MISSION_NAME": mission.name, "MISSION_GUID": mission.guid}
```
No `exp`. Verified in `verify_token()` (`:59-77`) with `opentakserver.pub`, `algorithms=["RS256"]`, **no audience/issuer check**.

**Header: `Authorization: Bearer <jwt>` only.** `MissionAuthorization` appears nowhere in the codebase.

`check_permission()` (`:129-141`) — the ATAK/iTAK fork:
```python
def check_permission(mission_name=None, mission_guid=None):
    if "iTAK" not in request.user_agent.string:
        token = verify_token()
        if mission_name and (not token or token["MISSION_NAME"] != mission_name): → 401
        elif mission_guid and (not token or token["MISSION_GUID"] != mission_guid): → 401
    else:
        cert_is_valid = verify_itak_certificate(mission_name, mission_guid)
```
with (`:80-83`):
```python
# iTAK sucks and doesn't send a token for some reason...
def verify_itak_certificate(mission_name=None, mission_guid=None) -> MissionRole | tuple:
```
which falls back to: client-cert CN → user → `?creatorUid=` must be an EUD owned by that user → an existing `MissionRole` row for that `(clientUid, username, mission_name)`.

Note `check_permission` only compares the token's mission to the requested one; it never checks the role's *permissions*. Ownership is checked ad-hoc in delete/password/role endpoints.

### `t-x-m-c` XML template — `generate_mission_change_cot()` (`models/MissionChange.py:81-203`)

```python
event = Element("event", {"version": "2.0", "uid": str(uid), "type": cot_type,   # default "t-x-m-c"
    "how": "h-g-i-g-o",
    "start": iso8601_string_from_datetime(mission_change.timestamp),
    "time":  iso8601_string_from_datetime(mission_change.timestamp),
    "stale": iso8601_string_from_datetime(mission_change.timestamp + timedelta(minutes=2)),
    "access": "Undefined"})
SubElement(event, "point", {"ce":"9999999","le":"9999999","hae":"0.0","lat":"0.0","lon":"0.0"})
detail = SubElement(event, "detail")
mission_element = SubElement(detail, "mission", {
    "type": str(mission_change.change_type), "tool": "public",      # <-- tool hard-coded "public"
    "name": mission_name, "guid": str(mission.guid),
    "authorUid": str(mission_change.creator_uid)})
mission_changes_element = SubElement(mission_element, "MissionChanges")
mission_change_element = SubElement(mission_changes_element, "MissionChange")
```
Then, conditionally:
- **content** → `<contentResource>` with child elements `creatorUid, expiration(-1), groupVector(0), hash, mimeType, name, size, submissionTime, submitter, uid`, plus `<contentUid>`.
- **cot_event** → `<details type=… [color] [callsign] [iconsetPath]><location lon lat/></details>`
- **mission_uid** → same `<details>` shape built from the DB row.
- Always: `<missionGuid>`, `<creatorUid>`, `<isFederatedChange>` (lowercased bool), `<missionName>`, `<timestamp>`, `<type>`.

The event `uid` is the **content/CoT uid**, not a fresh UUID (`:90-95`).

Other mission CoTs, all in `mission_marti_api.py`:
- `t-x-m-n` new mission, `generate_new_mission_cot` (`:178-209`) — `<detail><mission type="CREATE" tool name guid authorUid/>`
- `t-x-m-i` invite / `t-x-m-r` role change, `generate_invitation_cot` (`:212-275`) — `<mission type="INVITE" tool name guid authorUid token=<jwt>><role type=…><permissions><permission type=…/>…`. Note `type` is always `"INVITE"` even for `t-x-m-r`.
- `t-x-m-d` delete, `generate_mission_delete_cot` (`:278-309`) — `<mission type="DELETE" …/>`
- `t-x-m-c-l` log entry, `MissionLogEntry.generate_cot()` (`:65-100`) — `<mission type="CHANGE" tool="public" …/>`
All use `point ce/le=9999999, hae/lat/lon=0` and a 1-hour stale (2 min for the change/log ones).

### `/missions/{name}/cot` (`:2232-2265`)

```python
"""
Used by the Data Sync plugin to get all CoTs associated with a feed. Returns the CoTs encapsulated by an
<events> tag
"""
events = Element("events")
for cot in cots: events.append(fromstring(cot[0].xml))
return Response(response=tostring(events).decode("utf-8"), status=200, mimetype="application/xml")
```
Root element is `<events>`. No XML declaration. Selection is `CoT.mission_name == name`, which is populated in `insert_cot` (`cot_parser.py:108-111`) from `<dest mission="…">`.

### `guid` vs `name`

Both exist but `name` is the primary key (`models/Mission.py:22`); `guid` is a plain nullable column. GUID routes generally resolve to a name first. Several GUID handlers are broken: `mission_contents` (`:1859-1863`) calls `check_permission(mission_name)` with `mission_name=None` on the GUID route (⇒ permission check silently passes), and then writes `MissionContentMission.mission_name = None`. `delete_content` on the GUID route builds `mission_change.mission_name = None` too.

---

## 5. Data packages & device profiles

### Manifest generation — `create_data_package_zip()` (`data_package_marti_api.py:98-175`)

For a non-zip upload, OTS wraps it into a package itself: the file goes to `{md5_of_file}/{secure_filename}` and:
```python
manifest = Element("MissionPackageManifest", {"version": "2"})
config = SubElement(manifest, "Configuration")
SubElement(config, "Parameter", {"name": "uid", "value": str(uuid.uuid4())})
SubElement(config, "Parameter", {"name": "name", "value": secure_filename(filename + extension)})
contents = SubElement(manifest, "Contents")
content = SubElement(contents, "Content", {"ignore": "false", "zipEntry": f"{md5_hash}/{…}"})
if extension in ("kml","kmz"):
    SubElement(content, "Parameter", {"name": "name", "value": …})
    SubElement(content, "Parameter", {"name": "contentType", "value": "KML"})
    SubElement(content, "Parameter", {"name": "visible", "value": "true"})
zipf.writestr("MANIFEST/manifest.xml", tostring(manifest))
```
The zip is then renamed to `{sha256_of_zip}.zip` on disk; that sha256 is the package's identity everywhere. Comment at `:264`: *"DataPackage uses the hash as the UID for some reason"*.

### Response formats

**`POST /Marti/sync/missionupload`** → **plain text body, no JSON, no quotes**:
```
https://{hostname}:{OTS_MARTI_HTTPS_PORT}/Marti/api/sync/metadata/{file_hash}/tool
```
`# ATAK sends data packages as zips with a file name but no extension` (`:188`) — so `if not extension and "zip" in file.mimetype: extension = "zip"`.

**`GET /Marti/sync/missionquery?hash=`** → identical plain-text URL, or `{"success":false,"error":"File not found"} 404`.

**`GET /Marti/sync/search`** (`:316-348`) — the **Title-case** shape ATAK expects:
```python
{"UID": dp.hash, "Name": dp.filename, "Hash": dp.hash, "CreatorUid": dp.creator_uid,
 "SubmissionDateTime": dp.submission_time.strftime("%Y-%m-%dT%H:%M:%S.000Z"),
 "EXPIRATION": "-1", "Keywords": ["missionpackage"], "MIMEType": dp.mime_type,
 "Size": "{}".format(dp.size), "SubmissionUser": submission_user,
 "PrimaryKey": "{}".format(dp.id), "Tool": dp.tool if dp.tool else "public"}
```
wrapped in `{"resultCount": N, "results": [...]}`. Note: `EXPIRATION` is all-caps, `Keywords` is hard-coded `["missionpackage"]`, `Size`/`PrimaryKey` are stringified, and `SubmissionDateTime` uses a literal `.000Z` (distinct from the `iso8601_string_from_datetime` format used elsewhere: `%Y-%m-%dT%H:%M:%S.%f` truncated by 2 + `"Z"` ⇒ 4 fractional digits, `functions.py:145-149`).

**`POST /Marti/sync/upload`** (`mission_marti_api.py:1697`) → same Title-case shape but **not** wrapped:
```python
{"UID": content.uid, "SubmissionDateTime": …, "MIMEType": content.mime_type,
 "SubmissionUser": content.submitter, "PrimaryKey": content_pk, "Hash": content.hash,
 "CreatorUid": creator_uid, "Name": file_name}
```
iTAK quirks in this handler:
```python
# Older versions of iTAK use CreatorUid instead of creatorUid            (:1721)
# When uploading data packages, iTAK doesn't include an extension. If the user agent is iTAK and
# the content type is zip, assume that iTAK is uploading a data package  (:1727-1728)
# For some reason iTAK changes file names to a timestamp with the format YYYYMMDD-HHMMSS so the file name in the DB
# needs to be updated                                                    (:1793-1794)
```
and in `save_data_package_to_db` (`data_package_marti_api.py:71-77`):
```python
data_package.creator_uid = request.args.get("CreatorUid")  # iTAK
data_package.creator_uid = request.args.get("creatorUid")  # All other TAK clients
data_package.creator_uid = eud_uid                          # ...then clobbered again
```
(the first two assignments are dead).

**Fileshare CoT**: OTS emits none and rewrites none. See §3.

### Device profile zip

Covered in §2. Summary: always `MANIFEST/manifest.xml` + `5c2bfcae3d98c9f4d262172df99ebac5/preference.pref` + `5c2bfcae3d98c9f4d262172df99ebac5/truststore-root.p12`; enrollment additionally carries `maps/*.xml` and enrollment-flagged APKs/data packages; connection carries connection-flagged ones filtered by `syncSecago`/`clientUid`. **Never 204** — on exception it returns `"" 500`.

---

## 6. Groups / channels

`models/Group.py`. Constants: `IN = "IN"  # Write`, `OUT = "OUT"  # Read`, `SYSTEM`, `LDAP`.

`to_marti_json_in()` (`:96-113`):
```python
# Remove the _READ and _WRITE suffixes when using LDAP for groups
remove_read  = re.compile(re.escape("_read"),  re.IGNORECASE)
remove_write = re.compile(re.escape("_write"), re.IGNORECASE)
group_name = remove_read.sub("", self.name); group_name = remove_write.sub("", group_name)
return {
    "name": group_name,
    "direction": Group.IN,
    "created": iso8601_string_from_datetime(self.created or now).split("T")[0],
    "type": self.type,
    "bitpos": self.bitpos,
    "active": True,
    "description": self.description or "",
}
```
**`created` format:** built from `iso8601_string_from_datetime` (`%Y-%m-%dT%H:%M:%S.%f`, truncated 2, `+ "Z"`) then `.split("T")[0]` ⇒ **a bare date, `"2026-09-17"`**. No `distinguishedName` key is emitted here (it only appears in `serialize()`/`to_json()` for the web UI). `active` is hard-coded `True` — the per-user `GroupUser.enabled` flag is *not* reflected. `to_marti_json_out()` is the same dict with `direction` flipped to `OUT`.

There's a second, incompatible shape in `models/GroupUser.py:28-36` `to_marti_json()` where `"created": int(self.group.created.timestamp())` (a unix int) — **it is never called** by any route.

`bitpos`: `__ANON__` is pinned to 2 (`app.py:390`), and `Group.get_next_bitpos()` (`:61-70`) defaults to 3 and otherwise takes `max(bitpos)+1` — a plain counter, *not* a bitmask position. `Group.to_json()` (web UI) renders it as `"{0:b}".format(self.bitpos)`.

**`/Marti/api/groups/all`** (`group_marti_api.py:33-99`): requires a client cert (400 `"Groups are only supported on SSL connections"` otherwise). CN → user → all `GroupUser` rows; emits `to_marti_json_in()` for `direction==IN` rows and `to_marti_json_out()` otherwise. If the user has no groups:
```python
logger.info(f"{username} has no groups, defaulting to __ANON__")
group = Group(); group.name = "__ANON__"; group.type = Group.SYSTEM; group.bitpos = 2
response["data"].append(group.to_marti_json_out())
response["data"].append(group.to_marti_json_in())
```
LDAP mode instead maps `*_write` → IN, `*_read` → OUT.
**`useCache` is never read.**

**`PUT /Marti/api/groups/active`** (`:238-383`): client cert → CN → user. Then:
```python
# CloudTAK doesn't send the clientUid so the group subscription is changed for all of their EUDs
if request.args.get("clientUid"): uids.append(request.args.get("clientUid"))
else:
    for eud in user.euds: uids.append(eud.uid)
```
Body is a JSON array of `{name, direction, active}`. For each matching `GroupUser` subscription it sets `enabled = active` (skipped under LDAP) and then, per EUD uid:
```python
if active:  channel.queue_declare(queue=uid); channel.queue_bind(queue=uid, exchange="groups", routing_key=f"{group.name}.{direction}")
else:       channel.queue_unbind(queue=uid, exchange="groups", routing_key=f"{group.name}.{direction}")
```
403 if the user isn't in the named group. Returns `"" 200`.
Note it binds both directions to the *receive* queue — binding `NAME.IN` to a consumer queue means a client that activates a write-channel will also receive anything published on the IN key.

**Assignment:** groups are assigned server-side only, by an administrator, via `PUT /api/groups` (`ots_api/group_api.py:324`) with `{users:[…], group_name, direction}` — one `GroupUser` row per (user, group, direction). Also `PUT /api/users/groups` (`user_api.py:544`). LDAP mode derives them from LDAP group membership with the `OTS_LDAP_GROUP_PREFIX` filter.

**`__ANON__`** is created at startup with `bitpos = 2` (`app.py:385-392`), alongside system groups `ADS-B`, `AIS`, `Meshtastic` (`OTS_ADSB_GROUP` etc.). `GroupMission.group_id == 1` is used as "the `__ANON__` mission group" by hard-coded ID in `mission_marti_api.py:388, 545, 1404`.

**Routing enforcement:** see §3 — publish side uses IN memberships, consume side uses OUT bindings, `<dest>` bypasses both, and plain-TCP clients are forced into `__ANON__.OUT` only.

---

## 7. OAuth / tokens

`GET|POST /oauth/token` — `blueprints/ots_api/token_api.py:25-80`, function literally named `cloudtak_oauth_token`:
> *"Provides an OAuth token for TAKX and CloudTAK"*

**No `grant_type` handling at all.** It reads `username` and `password` from **either query args or form body** (`request.args.get(...) or request.form.get(...)`), bleach-cleans them, authenticates against LDAP or Flask-Security, then:
```python
token = jwt.encode({
    "exp": now + timedelta(days=365),
    "nbf": now,
    "iss": "OpenTAKServer",
    "aud": "OpenTAKServer",
    "iat": now,
    "sub": user.username,
}, key.read(), algorithm="RS256")   # key = certs/opentakserver/opentakserver.nopass.key
return jsonify({"access_token": token, "token_type": "Bearer", "expires_in": 365*24*60*60})
```
Alg **RS256**, signed by the server cert's private key. No `refresh_token`, no `scope`, no client credentials. Failure → `{"success": false, "error": "Invalid username or password"} 400` (not the RFC `{"error":"invalid_grant"}`). It also logs `request.data` and `request.form` at **warning** level — i.e. it writes plaintext passwords to the log (`:36-37`).

Notably, **this token is never accepted anywhere**. `Marti` mission endpoints validate against `MISSION_NAME`/`MISSION_GUID` claims; `basic_auth` validates against the `tokens` table hash. An `/oauth/token` JWT satisfies neither.

**No `/Marti/api/token` endpoints exist.**

**CloudTAK-specific behaviours** (complete list, `grep -i cloudtak`):
1. `ots_api/api.py:182-190` — `GET /files/api/config` → `{"uploadSizeLimit": 400}`, *"Required by CloudTAK"*.
2. `group_marti_api.py:246` — *"CloudTAK doesn't send the clientUid so the group subscription is changed for all of their EUDs"*.
3. `token_api.py:26-28` — the `/oauth/token` endpoint itself.

There is no special-cased `Content-Type: application/json` handling for CloudTAK. The only content-type branching is iTAK's (`request.content_type == "application/x-zip-compressed"` in `/Marti/sync/upload`) and the `Accept`-header branch in `signClient/v2`.

---

## 8. Plugin system

Three files: `plugins/BasePlugin.py`, `plugins/Plugin.py`, `plugins/PluginManager.py`.

**Discovery:** Python **entry points**, group `"opentakserver.plugin"` (`Plugin.py:26`). `PluginManager.get_plugin_entry_points()` → `metadata.entry_points(group=self._group)` (`PluginManager.py:42-43`). Plugins are ordinary pip packages; `OTS_PLUGIN_PREFIXES = ["ots-", "ots_"]` and `OTS_PLUGIN_REPO = "https://repo.opentakserver.io/brian/prod/"` (`defaultconfig.py:127-129`).

**Contract** (`Plugin.py:13-39`) — abstract methods a plugin must implement:
```python
class Plugin(BasePlugin):
    group = "opentakserver.plugin"
    blueprint: Blueprint | None = None
    @abstractmethod def activate(self, app: Flask, enabled: bool) -> None: ...
    @abstractmethod def stop(self) -> None: ...
    @abstractmethod def get_info(self) -> dict | None: ...
    @abstractmethod def load_metadata(self) -> {}: ...
```
`BasePlugin.PLUGIN_API_VERSION = "1.0.0"` (never checked at load time).

**Lifecycle** (`app.py:533-540`, `PluginManager.activate`): at startup, for each plugin — upsert a `Plugins` DB row (`name` lowercased, `distro`, `author`, `version` from `load_metadata()`), call `plugin.activate(app, enabled=<db flag>)`, then **`self._app.register_blueprint(plugin.blueprint)`** if the plugin exposes one. On SIGINT, `stop_plugins()` calls each `stop()`.

**What a plugin can do:**
- **Register HTTP routes** — it gets a full Flask `Blueprint` registered on the main app (no URL prefix enforced, no namespace isolation). `Plugin.get_plugin_routes(url_prefix)` (`:46-53`) walks `app.url_map` and collects its own GET/POST routes for UI display.
- **Publish/consume CoT** — not via any plugin API; it's handed the `Flask` app and is expected to open its own `pika` connection. The `firehose` fanout exchange exists exactly for this: *"Route all CoTs to the firehose exchange for plugins and users that connect directly to RabbitMQ"* (`EudHandler.py:468`). To inject CoT a plugin publishes to `cot_parser`/`cot_parser` (for parsing+routing) or directly to `groups`/`<name>.OUT`, `dms`/`<uid|callsign>`, `missions`/`missions.<name>` — exactly as the scheduled ADS-B/AIS jobs do (`blueprints/scheduled_jobs.py:80-120`), using `{"uid": OTS_NODE_ID, "cot": "<event…>"}` as the body. Publishing with `uid == OTS_NODE_ID` makes `route_cot` skip re-routing (`cot_parser.py:1147-1149`), so the plugin owns its own fan-out.
- **UI** — only indirectly, via socket.io (`extensions.socketio`) and the OTS-UI reading `get_info()`.
- **No sandboxing**: plugins are in-process, share `db.session`, `app.config`, and the same DB models.

**Management API** (`ots_api/plugin_api.py`, all `@roles_required("administrator")`):
`GET /api/plugins` (`:17`, → `{"success":true,"plugins":[plugin.get_info(), …]}`), `POST /api/plugins` (`:31`, upload `.zip|.whl|.gz` to `UPLOAD_FOLDER`), `GET /api/plugins/repo` (`:69`), `GET /api/plugins/<plugin_name>` (`:75`, → `load_metadata()` + `enabled`), `POST /api/plugins/<name>/disable` (`:102`), `POST /api/plugins/<name>/enable` (`:118`).

Install/uninstall itself is a **socket.io** event, not HTTP: `@socketio.on("plugin_package_manager")` + `@administrator_only` (`PluginManager.py:140-299`). Actions `install` / `install_local` / `delete` shell out to `pip` (`f"{sys.executable} -m pip --no-input install {distro} -i {OTS_PLUGIN_REPO}"`) and **stream pip's stdout/stderr back over the socket** line by line via a `selectors` loop — a nice pattern for a sidecar manager UI. Distro names must start with an `OTS_PLUGIN_PREFIXES` entry unless the action is `install_local`.

---

## Things worth stealing / avoiding for a Rust server

**Worth copying (these are the empirically ATAK-compatible bits):**
- Nginx-terminated mTLS with the PEM in `X-Ssl-Cert`, CN == username — clean separation, and it's how OTS gets ATAK enrollment working.
- The `signClient/v2` `Accept`-header fork and the `{signedCert, ca0, ca1}` / `<enrollment><signedCert/><ca/></enrollment>` duality.
- The double-wrapped data package (`X_CONFIG.zip` containing `X.zip`) *because WinTAK*, and the separate flat iTAK zip.
- Plain-text URL responses from `/Marti/sync/missionupload` and `/Marti/sync/missionquery`.
- The Title-case `/Marti/sync/search` result keys with `EXPIRATION` all-caps and stringified `Size`/`PrimaryKey`.
- Mission tokens as short RS256 JWTs carrying `MISSION_NAME` + `MISSION_GUID` + `sub=<clientUid>` in `Authorization: Bearer`.
- Accepting both `clientUID`/`clientUid` and `creatorUid`/`CreatorUid`.
- Dual DM routing keys (uid **and** callsign).

**Known-wrong in OTS — do not port:**
- No TAK Protocol v1 at all; regex framing on `</event>` with per-chunk UTF-8 decode.
- `pong()` echoes the ping instead of the `t-x-c-t-r` it just built.
- No state replay to newly connected clients.
- `MissionChange.missionGuid` carries the CoT UID; `MissionInvitation.invitee` serializes an ORM object; `MissionInvitation.token` is always `""`.
- Duplicate `t-x-m-c` publish per `<dest mission>`.
- `bitpos` is a counter, not a bit index; `active` in the group JSON ignores `GroupUser.enabled`.
- No `archive`, no `layers` (returns an empty body, not JSON), no 204 on unchanged device profiles.
- `os.chdir()` inside `generate_zip`; random 5-digit cert serials; `/oauth/token` logging plaintext passwords.
- `GET /Marti/api/missions/<name>` and most group/contacts/cot endpoints have no auth check whatsoever.
