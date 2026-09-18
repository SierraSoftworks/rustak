# Device profiles (`/Marti/api/device/profile/*`, `/Marti/api/tls/profile/*`) — wire contract

**Purpose.** The enrolment-time and on-connect configuration bundles ATAK pulls automatically — used
by rustak to push `.pref` settings (e.g. turn on Channels) and, later, config packages. Served on
both HTTPS listeners. Contract for `rustak-server::profiles::*`.

Baseline: `plan.md` Appendix A.3 "Device profiles". This file expands it.

**Not used by CloudTAK.** `node-tak` defines a `profile.ts` client but no CloudTAK route calls it
(03 §3.21) — this whole surface exists purely for ATAK-CIV compatibility. Implement it for the M3
milestone's ATAK checklist, not as a CloudTAK interop gate.

## 1. Endpoints

| Trigger | Path | Port / auth | `clientUid` |
|---|---|---|---|
| Right after enrollment | `GET /Marti/api/tls/profile/enrollment` | `:8446`, Basic | required |
| Every stream connect (gated, see §4) | `GET /Marti/api/device/profile/connection?syncSecago=` | `:8443`, cert | required |
| Every stream connect | `GET /Marti/api/device/profile/tool/{tool}?syncSecago=` | `:8443`, cert | required |
| Tool + explicit files | `GET /Marti/api/tls/profile/tool/{tool}/file?relativePath=<repeatable>` | `:8446`, Basic | required |
| Tool + explicit files (cert variant) | `GET /Marti/api/device/profile/tool/{tool}/file?relativePath=<repeatable>` | `:8443`, cert | required |

⚠️ **There is no `/Marti/api/device/profile/enrollment` mapping** in real TAK Server despite some
config granting the path — the enrolment profile only ever lives at `/Marti/api/tls/profile/
enrollment`. Don't implement both as aliases; implement only the `tls/` one for enrolment.
`relativePath` values get a leading `/` forced and only a literal space is percent-encoded by
ATAK's client — don't expect full URL-encoding on that param. Verified 06 §10.1, 07 §2.1–§2.2.

## 2. Response semantics — shared by all five

| Condition | Status | Body |
|---|---|---|
| Nothing to send | **204 No Content** | empty |
| Something to send, single file, `/tool/{t}/file` | **200** | raw file bytes, `Content-Disposition: attachment; filename=<name>` (unquoted), `Content-Type` = best-effort guess from filename (may be absent) |
| Something to send, >1 file | **200** | zip, `Content-Type: application/zip`, `Content-Disposition: attachment; filename=profile.zip` |
| `/enrollment`, `/connection`, `/tool/{t}` (always packaged, never raw) | **200** | zip (`profile.zip`, see §3) |
| Nothing left after `If-Modified-Since` filtering (`/tool/{t}/file` only) | **304 Not Modified** | empty |

- Every response that returns content sets `Last-Modified`, formatted **RFC 1123**
  (`DateTimeFormatter.RFC_1123_DATE_TIME`, UTC) — ATAK stores this verbatim and sends it back as
  `If-Modified-Since` on the next request to `/tool/{t}/file`. rustak should truncate stored
  timestamps to whole seconds before comparing, to avoid spurious "modified" results from
  millisecond jitter.
- `syncSecago=-1` (the default on `/tool/{t}`) means "everything"; a positive value means "only
  profiles updated within the last N seconds". First run has no prior sync time, so treat it as
  "everything" too.
- `applyOnEnrollment` / `applyOnConnect` boolean flags on each stored profile select which of these
  endpoints it's eligible for — a profile can be attached to enrollment only, connect only, both, or
  neither (in which case it's admin-managed but never auto-delivered).

Verified 06 §10.1, 07 §2.3.

## 3. Zip layout for `/enrollment`, `/connection`, `/tool/{name}`

```
file0/            file0/<profile-file-0-name>
file1/            file1/<profile-file-1-name>
…
MANIFEST/         MANIFEST/manifest.xml
```
Each delivered file gets its **own numbered directory** (`fileN/`) — this is the convention, not
just "put everything at the zip root". `MANIFEST/manifest.xml` uses the base shape from `files.md`
§10, with `Configuration` params `uid` (random), `name` (`"Enrollment"` | `"Connection"` |
`"<toolName>"`), `onReceiveImport="true"`, `onReceiveDelete="true"`. When `/tool/{t}/file` returns
more than one file it uses a slightly different manifest (`name="multiFile"`) that **recreates the
on-disk directory hierarchy** inside the zip instead of the `fileN/` convention — reproduce that
distinction if implementing the `/file` variant. Verified 06 §10.2, §10.4.

## 4. When ATAK actually fetches these (background — client behaviour, not server contract)

- Enrollment profile: fetched **unconditionally** right after a successful enrollment (`enrollment.md`
  §6), before the client reconnects the stream.
- Connection + tool profiles: fetched on **every** stream connect, but gated behind the ATAK
  preference `deviceProfileEnableOnConnect` (default **false**). Since this is a client-side default,
  a fresh ATAK install will never call `/device/profile/connection` unless something turns that
  preference on — and the enrollment profile is exactly the mechanism to turn it on (§5). If rustak's
  enrollment profile doesn't set it, operators will see profiles silently never fire on connect and
  may mistake that for a server bug. Verified 07 §2.4.

## 5. `.pref` files rustak's profile service should generate

Shared format — **exact bytes**, no whitespace between elements (own rendering of the verified
literal-concatenation logic, 06 §10.3):
```xml
<?xml version='1.0' standalone='yes'?><preferences><preference version="1" name="com.atakmap.app.civ_preferences"><entry key="{key}" class="class java.lang.String">{value}</entry>…</preference></preferences>
```
Notes:
- XML declaration uses **single quotes** and has **no `encoding` attribute** — this differs from the
  double-quoted, `encoding="UTF-8"` declaration used on CoT XML elsewhere in this project; don't
  normalise it to match.
- Preference group name is the literal **`com.atakmap.app.civ_preferences`** (ATAK-CIV's default
  SharedPreferences store; see also `enrollment.md`/ATAK's legacy-alias handling, which rewrites
  `com.atakmap.app_preferences` / `com.atakmap.civ_preferences` to the active package's own name —
  emit the civ-flavoured name directly rather than relying on alias rewriting).
- `class` attribute is the literal string **`class java.lang.String`** (redundant leading `class `
  token, not a mistake) — a missing `class` attribute crashes ATAK's importer entirely, so always
  emit it even for boolean-looking values.
- No XML escaping is applied by the reference generator; rustak should still properly escape
  key/value text (`&`, `<`, `>`, quotes) since ATAK's XML parser will still need valid XML — just
  don't add extra whitespace/pretty-printing.

Two concrete profiles worth generating from rustak's own settings:
- **`enable-channels.pref`** — when the target account should see the Channels UI automatically:
  `prefs_enable_channels = "true"`, `prefs_enable_channels_host-<host> = "true"` (see `groups.md`
  §4 for what these gate).
- A profile that sets **`deviceProfileEnableOnConnect = "true"`** on the enrollment profile
  specifically, so subsequent connect-time profile pushes (§4) actually fire.

## 6. `.pref` **import** semantics (background — what ATAK does with a `.pref` you send)

Only three preference group names are special-cased as connection loaders: `cot_streams`,
`cot_inputs`, `cot_outputs`. Every other `name=` is a generic keyed SharedPreferences import. Within
`cot_streams`, only these **indexed** keys are actually read by ATAK — sending others is harmless but
wasted:
```
count, description<i>, connectString<i>, enabled<i>, useAuth<i>, compress<i>, cacheCreds<i>,
caPassword<i>, clientPassword<i>, caLocation<i>, certificateLocation<i>,
enrollForCertificateWithTrust<i>, enrollUseTrust<i>, expiration<i>
```
**`username<i>`/`password<i>` are never read** — stream credentials come from ATAK's separate
credential store, keyed by host, driven by `cacheCreds`. Don't rely on pushing a username/password
pair via `.pref`; it has no effect. Verified 07 §2.5.

## 7. Admin API (`/Marti/api/device/profile/**`, lower priority)

Expose rustak's own equivalent under `/api/v1` rather than replicating this whole surface —
`plan.md`'s architecture already routes admin-managed profiles through the Yew UI's own API. Keep
the wire-facing pieces (§1–§6) as the only hard compatibility requirement; the admin CRUD shape
(`Profile{id, name, active, applyOnEnrollment, applyOnConnect, type, updated, tool, groups[]}`,
verified 06 §10.5) is documented here only as a reference if rustak ever needs to interoperate with
TAK-ecosystem admin tooling directly — not required for M3.

## Gotchas

- `204` (nothing to send) vs `200` + zip vs `304` (unchanged) — get the wrong status and ATAK either
  errors or silently does nothing when it should have imported something (§2).
- `deviceProfileEnableOnConnect` defaults to **false** on ATAK — a correct connection-profile
  implementation will appear to do nothing until the enrollment profile (or a manual `.pref`) turns
  it on (§4). Don't chase a phantom bug here.
- The `.pref` XML declaration is single-quoted with no `encoding=` — distinct from every other XML
  shape in this project (§5).
- `class="class java.lang.String"` is not a typo to "clean up" — omitting the redundant prefix
  crashes ATAK's `.pref` importer (§5).
- `username`/`password` keys in a `.pref`'s `cot_streams` block are silently ignored by ATAK — don't
  design a credential-push feature around them (§6).

## Corrections from the M3-02 implementation

- **`304` is not a redirect.** `marti::headers::assert_no_redirect` turns any `3xx` reaching it into
  a `500`, which would have made the `If-Modified-Since` contract in §2 unimplementable. `304` is
  now exempt: it carries no `Location` and is the documented answer to a conditional request.
- **Timestamps are truncated on both sides.** §2 asks for stored timestamps to be truncated to
  whole seconds before comparing. The *delivered* file's timestamp has to be truncated too, not
  just the stored one — `Last-Modified` cannot carry milliseconds, so a file whose `updated_at`
  is not exactly on a second would otherwise be reported as changed on every single request.
- **`class` is not always `String`.** §5 documents TAK Server's generator, which hard-codes
  `class java.lang.String`. rustak stores a class per entry (`String|Boolean|Integer|Long|Float`),
  because the preference editor needs it and because ATAK reads all five. The enrolment defaults
  and the host-scoped Channels flag stay `String` holding `"true"`, which is what ATAK's own server
  sends.
- **`<Role>` attribute name.** Design 04 §5.3 writes `<Role name=…>`; `compat/files.md` §10 writes
  `<Role type=…>`. The writer emits `name` (per the design) and the reader accepts either, so a
  package from a real TAK Server round-trips whichever it turns out to be. Unresolved until a
  mission archive is checked against a real client (M4).
- **iTAK keystore paths are unverified.** §6.3 of design 04 says the iTAK variant uses unsuffixed
  `caLocation`/`caPassword`/`certificateLocation`/`clientPassword`, but not what they resolve
  against. rustak writes `cert/truststore.p12` for both variants, matching ATAK's certificate
  sorter. Flagged in `docs/compat/profiles.md` §5 as a manual check.
- **A configuration package cannot include a client keystore.** rustak never holds a device's
  private key, so `include_client_cert: true` is refused in words rather than served by minting a
  key pair. The `.pref` builder still supports the certificate variant for the day a keystore has
  somewhere to come from.

## Verified in

- `research/06-takserver-http-api-verified.md` §10 (`ProfileAPI`/`ProfileAdminAPI`, exact status
  codes, zip layout, `.pref` generation logic, `syncSecago`/`If-Modified-Since` semantics) —
  authoritative.
- `research/07-atak-client-verified.md` §2 (ATAK's `DeviceProfileOperation`/`DeviceProfileClient`,
  `.pref` import parser, gating preferences) — authoritative for ATAK.
- `research/03-cloudtak-node-tak-contract.md` §3.21 (confirms this surface is unused by CloudTAK) —
  scope note only.
- `plan.md` Appendix A.3 "Device profiles" — baseline digest, expanded here.
