# Files / Enterprise Sync / data packages (`/Marti/sync/*`, `/Marti/api/files/*`) — wire contract

**Purpose.** The content-addressed file store: uploads, search, download, metadata, and the
mission-package (`.zip`) conventions ATAK and CloudTAK both use for attachments and data-package
transfer. Served on both HTTPS listeners. Contract for `rustak-server::files::*` and
`marti::{sync,files}`, backed by the content-addressed store in `store::content`
(`conventions.md` storage rules — files never live in SQLite, only their metadata rows do).

Baseline: `plan.md` Appendix A.3 "Packages" and A.4 "Enterprise Sync". This file expands both.

## 1. `GET /files/api/config` — the CloudTAK setup gate

```
GET /files/api/config          <- NOTE: no /Marti prefix
```
```json
{ "uploadSizeLimit": 400 }
```
`uploadSizeLimit` is an **integer, megabytes**. This is the **single highest-priority endpoint** in
the whole CloudTAK surface: `PATCH /api/server` (CloudTAK's admin setup call) validates a supplied
cert by calling exactly this endpoint and checking `uploadSizeLimit !== undefined` — until it
responds correctly, CloudTAK's setup wizard cannot save a working server connection at all. `ROLE_
ANONYMOUS`-equivalent auth (no client cert or bearer required). Verified 06 §9.10, 03 §1.2.

## 2. `POST /Marti/sync/upload` — generic Enterprise Sync upload

```
POST /Marti/sync/upload?name=&keywords=&creatorUid=&uid=&latitude=&longitude=&altitude=&groups=
Content-Type: multipart/form-data (part "assetfile" or "resource")   OR any other type with a raw body
```
- Accepts either a multipart part (`assetfile` — ATAK's name; `resource` — browser uploads) **or**,
  when `Content-Type` isn't multipart, the **raw request body** as the file content.
- Array-valued params (`keywords`, `groups`) accept repeated params **or** one comma-delimited
  value.
- `uid` auto-generates (random) if omitted. A file whose `uid` matches an existing map-item's uid is
  treated as an **attachment** of that item by TAK-ecosystem clients — pass it through, don't
  discard it.
- Enforce a configured size limit; reject with `400` over the limit and on empty bodies.
- **Response: `200`, body is the resulting `Metadata` object, `Content-Type: text/json`** (a
  historical quirk — not `application/json`; this endpoint predates the JSON-content-type strictness
  described in `cloudtak.md`, and it is fine to keep it distinct since no verified client treats it
  strictly). Field names are **Title/Camel-case**, values are **all strings**, keys with no value are
  omitted (not `null`):
  ```
  Altitude, DownloadPath, Keywords[], Latitude, Longitude, Hash, MIMEType, Name, Permissions[],
  Size, Remarks, SubmissionUser, PrimaryKey, SubmissionDateTime, UID, Contacts[], CreatorUid,
  Tool, EXPIRATION, PluginClassName, Groups[], MissionName
  ```
  `Size` and `PrimaryKey` are **JSON strings**, not numbers, even though they hold numeric values.
  `EXPIRATION` is the one all-caps key; everything else is Title/CamelCase. Verified 06 §9.2.

## 3. `GET /Marti/sync/search` — legacy Enterprise Sync search

```
GET /Marti/sync/search?keywords=&Filename=&MIMEType=&Name=&UID=&Tool=&PrimaryKey=&…   (case-insensitive param names)
```
```json
{ "resultCount": 2, "results": [ { "UID":"…","Name":"…","Hash":"…","PrimaryKey":"0","SubmissionDateTime":"2024-01-01T00:00:00.000Z", "SubmissionUser":"…","CreatorUid":"…","Keywords":["…"],"MIMEType":"…","Size":"1234","EXPIRATION":"-1","Tool":"…","Groups":["a","b"] } ] }
```
**Corrected (M3-01):** `Groups` is a JSON **array** here, not a comma string — it is one of the four
array-valued `Metadata.Field`s (`Keywords`, `Permissions`, `Contacts`, `Groups`, 06 §9.2), and the
comma string belongs to the *other* endpoint, `/Marti/api/files/metadata` (§8). The earlier sample
above conflated the two.

`Content-Type: text/json`. `resultCount` **is** a real JSON number. Every element is the same
`Metadata` shape as §2's response — Title-case keys, `Size`/`PrimaryKey` as strings,
`SubmissionDateTime` **padded-millis** `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'`. ATAK's package browser
requires the body to literally contain the substring `resultCount` (its own verification step) and
treats any element missing `UID`/`Name`/`Hash`/`SubmissionDateTime`, or with a negative
`PrimaryKey`, as fatal to the **whole** response — validate server-side before emitting rather than
letting a bad row corrupt the browse. Verified 06 §9.3, 07 §6.1.

## 4. `GET|HEAD /Marti/sync/content` — download

```
GET /Marti/sync/content?hash=|uid=&offset=&length=
```
`Hash` (case-insensitive param) wins over `uid` if both given. Headers: `api-version: 3` (the only
endpoint that sets this), `Content-Type` = the stored MIME type, `Content-Disposition: inline;
filename="<urlencoded>"`. **rustak never sets `Content-Encoding: gzip` here (M3-01, design 04 §5.2):**
the payloads are overwhelmingly zips and JPEGs, which do not compress, and both verified clients
accept identity. A `Content-Length` is always set instead, which is what makes a resumed download
work. Status `200`, or `206` when `length > 0` and it's a partial range, or `404` when
no metadata matches. ATAK's browse-and-download path appends `&receiver=<callsign>` to this URL when
following a `senderUrl` — accept and ignore that extra query param. Verified 06 §9.4.

## 5. `POST /Marti/sync/missionupload` + `GET /Marti/sync/missionquery` — the mission-package flow

This is the pair both ATAK's data-package sharing and CloudTAK's `Files.uploadPackage` use — it is
distinct from §2/§3 (Enterprise Sync proper) even though it stores into the same content-addressed
backend.

```
POST /Marti/sync/missionupload?filename=&creatorUid=&mimetype=application/x-zip-compressed
     &keyword=missionpackage&tool=public&groups=&Groups=
Content-Type: multipart/form-data, part "assetfile" (ATAK) or "resource" (browser)
```
- `filename` is **required**. A client-supplied `hash` query param is accepted but **ignored** — the
  server always computes its own hash. `keyword` defaults to `missionpackage`; always ensure that
  keyword is present in the stored `Keywords` so the package shows up in `/Marti/sync/search?
  keywords=missionpackage`.
- **Accept both `Groups` and `groups`** as the param name — node-tak deliberately sends the
  capitalised form "due to an apparent bug in TAK server" (03 §8.6); reproduce the tolerance, not
  the bug.
- Duplicate detection — **corrected (M3-01)**: TAK answers `403` for a repeat, which makes ATAK's
  "another EUD already shared this package" path a visible error. rustak instead **reuses the row and
  answers `200`** with the same URL (design 04 §5.2): the store is content-addressed, so the second
  upload of identical bytes under the same `filename` *is* the first one. A row already holding that
  hash under a **different** name is somebody else's listing, so that upload gets its own row with a
  minted uid rather than overwriting theirs.
- **Response: `200`, `Content-Type: text/plain`, body is a bare URL**:
  ```
  https://{host}:{marti-port}/Marti/sync/content?hash=<hash>
  ```
  This exact string becomes the `senderUrl` in the `b-f-t-r` CoT notification the uploading client
  sends next (§9) — don't wrap it in JSON, don't add a trailing newline the client would have to
  trim. Verified 06 §9.6, 07 §6.2.

```
GET /Marti/sync/missionquery?hash=<sha256>
```
Same response shape (the same content-URL string), `200`; **`404`** if unknown. This is ATAK's
"do I already have this on the server?" probe before uploading. Verified 06 §9.6, 07 §6.2.

## 6. Metadata mutation

```
PUT /Marti/api/sync/metadata/{hash}/{tool|mimetype}     body = raw text value
PUT /Marti/api/sync/metadata/{hash}/keywords             body = JSON array of strings
PUT /Marti/api/sync/metadata/{hash}/expiration?expiration=<millis>
```
**Only these four fields are mutable** via this API: `tool`, `mimetype`, `keywords`, `expiration`.
Not `name`, not `creatorUid`, not `groups` — reject any other `{metadata}` path segment with `400`.
All three routes: `200` on success, `404` if the hash doesn't exist, empty body either way. Note
`/Marti/sync/{hash}/metadata` (no `/api/`, no `/Marti/api/sync/…`) **does not exist** — it's dead
code in real TAK Server; don't implement it as an alias, implementers have been burned expecting it.
Verified 06 §9.1, §9.5.

## 7. `DELETE /Marti/sync/delete`

```
GET|POST|DELETE /Marti/sync/delete?Hash=|PrimaryKey=<repeatable>
```
All three HTTP verbs are accepted (not just DELETE). `Hash` wins if present; otherwise delete every
listed `PrimaryKey`. **Response is HTML, status `200`**, even on success:
```html
<html><head><title>Enterprise Sync Status</title></head><h1>Success</h1><p>Deleted N resource(s).</p></html>
```
Reproduce the HTML-on-success behaviour if you want byte-compatible legacy tooling; a JSON success
body is not wrong for any *verified* client (neither ATAK nor CloudTAK parses this response body),
but note it explicitly so nobody "fixes" it into JSON expecting a client regression. Verified 06
§9.8.

## 8. `/Marti/api/files/*` — the modern file-manager surface

```
GET    /Marti/api/files/metadata?page=&limit=&mission=&missionPackage=false&name=&sort=&ascending=true
GET    /Marti/api/files/metadata/count?mission=&missionPackage=false
GET    /Marti/api/files/{hash}
HEAD   /Marti/api/files/{hash}
DELETE /Marti/api/files/{hash}                       (always 200, exceptions swallowed)
PUT    /Marti/api/files/{hash}/metadata?user=&expiration=&keywords=<repeatable>
```
`GET …/metadata` → `{"version":"3","type":"Files","data":[…],"nodeId":"…"}`, each element a **flat
map of strings**:
```json
{ "Name":"…","User":"…","Creator":"…","Size":"12kB","Time":"Wed May 01 12:00:00 UTC 2024",
  "MimeType":"…","Keywords":"a,b,c","Expiration":"2024-05-01T00:00:00"|"none","Hash":"…","Groups":"a,b" }
```
`Size` is **humanised** (`"12kB"`, not a byte count). `Time` is emitted from Java's default
`Date.toString()` in TAK Server. **Corrected (M3-01):** rustak emits that same
`EEE MMM dd HH:mm:ss UTC yyyy` rendering rather than RFC 3339, per design 04 D5 — CloudTAK reads
`entry.Time` out of this map for its packages page (§3.12 of report 03), and the safe answer for a
display string a client already reads is the one it has always been handed. The `Time` of
`HEAD /Marti/api/files/{hash}` is the padded instant instead, because that map is the metadata
rendering rather than the file manager's. `Expiration` is either
an ISO-ish timestamp with the trailing `Z` omitted, or the literal string `"none"`.

**`GET /Marti/api/files/metadata?missionPackage=true&name={pkg}`** is called **directly** by
CloudTAK (bypassing node-tak) to populate a package's "channels" column, matched on `entry.Hash` and
reading `entry.Groups` (comma string) / `entry.Time`. Treat this as **Tier 1** for CloudTAK's
package UI even though the rest of `/files/**` is lower priority. Verified 06 §9.9, 03 §3.12.

`GET /Marti/api/sync/search` (note: **`/api/sync/`**, distinct from `/Marti/sync/search` in §3) is
the modern JSON-native variant: `{"version":"3","type":"Resource","data":[…]}` with **lowerCamelCase**
`Resource` objects (`filename, keywords[], mimeType, name, submissionTime, submitter, uid, hash,
size, creatorUid, tool, latitude, longitude, altitude, expiration, groups[]`). `submissionTime` uses
**unpadded**-millis format. Neither ATAK nor CloudTAK call this route in the verified traces — safe
to implement after the Title-case legacy routes. Verified 06 §7.17, §9.7.

## 9. File-share CoT (`b-f-t-r` / `b-f-t-a`)

These are ordinary CoT messages carried over the stream (see `streaming.md` for framing/routing),
not HTTP — documented here because they're part of the file-transfer flow. Own-words rendering of
the verified shapes (05 §9, 07 §6.4):

**Offer**, `type="b-f-t-r"`, `how="h-e"`, `stale = +10s`:
```xml
<event version="2.0" uid="{fresh-uuid}" type="b-f-t-r" time="{t}" start="{t}" stale="{t+10s}" how="h-e">
  <point lat="0.0" lon="0.0" hae="9999999.0" ce="9999999" le="9999999"/>
  <detail>
    <fileshare filename="{name}" senderUrl="{url}" sizeInBytes="{n}" sha256="{hash}"
               senderUid="{uid}" senderCallsign="{callsign}" name="{name}"/>
    <ackrequest uid="{fresh-uuid}" ackrequested="true" tag="{name}"/>
  </detail>
</event>
```
- `senderUrl`: when the file was uploaded to rustak first (the normal server-hosted case, §5), this
  is the exact `…/Marti/sync/content?hash=…` URL that upload returned — **rustak never rewrites a
  client's `senderUrl`**, it's just relayed like any other CoT. It only *originates* this shape
  itself in two cases: substituting an oversized (>64 KiB) protobuf frame (`streaming.md` §3), and
  device-profile/mission-archive delivery notifications, where `senderUrl` should point at
  `/Marti/api/cot/xml/{uid}` or the equivalent content URL.
- `<ackrequest>` is only present when the sender wants a receipt.

**Ack**, `type="b-f-t-a"`, `how="m-g"`, `stale = +10s`, `uid` = the **receiver's own** contact uid
(not derived from the request):
```xml
<event version="2.0" uid="{receiver-uid}" type="b-f-t-a" time="{t}" start="{t}" stale="{t+10s}" how="m-g">
  <detail><ackresponse uid="{ackrequest-uid}" senderUid="{receiver-uid}" success="true" tag="{name}" sha256="{hash}" sizeInBytes="{n}"/></detail>
</event>
```
Route it like any addressed CoT (typically `<marti><dest uid=…>` back to the original sender). Both
`b-f-t-r` and `b-f-t-a` are ordinary routed messages, not control types — they go through the normal
reachability check in `streaming.md` §8, they are not special-cased by the router itself.

## 10. Mission-package manifest (`MANIFEST/manifest.xml`)

Shared by data packages, mission archives, and device-profile bundles (see `profiles.md` for the
profile-specific parameter list). Shape (JSON/XML *shapes* are facts, not copied text — own
rendering, verified 07 §6.6, 06 §7.10, §10.2):
```xml
<MissionPackageManifest version="2">
  <Configuration>
    <Parameter name="uid" value="…"/>
    <Parameter name="name" value="…"/>
    <Parameter name="onReceiveImport" value="true"/>
    <Parameter name="onReceiveDelete" value="true|false"/>
    <!-- more Parameter entries as needed -->
  </Configuration>
  <Contents>
    <Content zipEntry="file0/attachment.jpg" ignore="false">
      <Parameter name="localpath" value="…"/>
      <Parameter name="isCoT" value="true|false"/>
      <Parameter name="contentType" value="…"/>
    </Content>
  </Contents>
</MissionPackageManifest>
```
- `Configuration` requires at least `name` and `uid` to be considered valid by ATAK's importer.
- Every `Content/@zipEntry` path is **relative to the directory containing `MANIFEST/`** — a package
  may be nested one level deep (the "right-click compress a folder" case); don't assume
  `MANIFEST/manifest.xml` is at the zip root.
- Mission archives additionally append `<Groups><Group name="…"/>…</Groups>` and
  `<Role type="…"><Permission name="…"/>…</Role>` siblings of `Configuration`/`Contents` — not part
  of the base schema above, mission-archive-specific.
- Zip filenames should sort deterministically for reproducible test fixtures; TAK Server sets every
  zip entry's mtime to epoch 0 — worth doing the same so golden-file tests aren't timestamp-flaky.

## 11. CloudTAK-specific requirements

- CloudTAK's attachment flow is **two calls**: `POST /Marti/sync/upload` (§2, get a `Hash`) then
  `PUT /Marti/api/missions/{guid}/contents {"hashes":["<hash>"]}` (see `missions.md` §7) — implement
  both, in that order, as one logical operation from a test-writer's perspective.
- CloudTAK's whole-package upload is the separate `PUT /Marti/api/missions/{name}/contents/
  missionpackage?creatorUid=` (raw zip stream) — not the `/Marti/sync/missionupload` path.
- **Accept `Content-Type: application/json` responses being parsed strictly** — see `cloudtak.md`
  §"Content-Type" for why this matters on every *JSON* Marti endpoint; the `text/json` responses in
  this file (§2, §3) are a documented historical exception, not something to "fix" to
  `application/json`, since doing so wouldn't break any verified client either way — pick whichever
  is less work, but if in doubt, match the legacy value exactly for maximum interop with older
  tooling that might string-match it.

## Gotchas

- `/Marti/sync/missionupload`'s response is a **bare URL string**, `text/plain` — not JSON, not
  wrapped, no trailing content (§5).
- Accept both `Groups` and `groups` on `missionupload` — this is a documented client workaround for
  a real server's own bug, not something to "correct" by picking one (§5).
- `Size`/`PrimaryKey` are **strings** everywhere in the legacy Enterprise Sync JSON (§2, §3), but
  **numbers** in the modern `/Marti/api/sync/search` `Resource` shape (§8) — don't unify the two.
- `/Marti/sync/{hash}/metadata` does not exist — don't implement it as a convenience alias for
  `/Marti/api/sync/metadata/{hash}/…` (§6).
- `senderUrl` in a `b-f-t-r` is relayed verbatim by rustak in the normal case; rustak only
  *originates* one for the oversized-protobuf substitution and profile/archive delivery (§9).
- `DELETE /Marti/sync/delete` returns HTML on success by convention — don't let a test assert
  `Content-Type: application/json` against it (§7).

## Verified in

- `research/06-takserver-http-api-verified.md` §9 (`UploadServlet`, `SearchServlet`,
  `ContentServlet`, `MetadataApi`, `MissionPackage{Upload,Query}Servlet`, `DeleteServlet`,
  `FileManagerApi`, `FileConfigurationApi`) — authoritative.
- `research/07-atak-client-verified.md` §6 (ATAK's mission-package upload/download/search client,
  `b-f-t-r`/`b-f-t-a` construction and parsing, manifest schema) — authoritative for ATAK.
- `research/05-takserver-streaming-auth-verified.md` §9 (server-side `b-f-t-r` construction, the
  64 KiB substitution case) — authoritative for the stream-side file-share notification.
- `research/03-cloudtak-node-tak-contract.md` §3.12–§3.13, §5.5, §8.6 (`files.ts`/`package.ts`,
  the two-step attachment flow, the `Groups` capitalisation workaround) — authoritative for
  CloudTAK.
- `plan.md` Appendix A.3 "Packages", A.4 "Enterprise Sync" — baseline digest, expanded here.
