# Enrollment (`/Marti/api/tls/*`) — wire contract

**Purpose.** The certificate-enrollment surface both ATAK-CIV and CloudTAK use to turn a
username/one-time-token (or client password, for CloudTAK) into a client certificate. Served on the
public HTTPS listener (`:8446`), Basic-auth gated, no client cert required. This is the contract for
`rustak-server::auth::setup`/`pki::issue` and the `/Marti/api/tls/**` routes in `marti::tls`.

Baseline: `plan.md` Appendix A.3 (ATAK) and A.2 (CloudTAK). This file expands both; do not contradict.

rustak's credential for this flow is a **one-time enrollment token** (QR / typed) for normal EUD
enrollment, or an opt-in expiring **client password** for CloudTAK (see `oauth.md` and
`conventions.md` "Security defaults"). Both are sent as the **Basic auth password**; the username is
the account the cert will be issued for.

## 1. `GET /Marti/api/tls/config`

| | |
|---|---|
| Auth | HTTP Basic (username + enrollment token or client password) |
| Response | `200`, XML, root element **`certificateConfig`** in the JAXB-generated namespace form ATAK and CloudTAK both parse; CloudTAK's `xml2js` client literally requires the root tag to read `ns2:certificateConfig` |
| Content-Type | `text/plain` is what TAK Server actually emits (no explicit XML content type is set); be liberal — emit `application/xml` if convenient, but don't assume the client checks it (06 §3.4) |

Rendering (own words, exact structure — verified 06 §3.4, cross-checked against CloudTAK's parser in
03 §2.1 step 3a):
```xml
<ns2:certificateConfig xmlns="http://bbn.com/marti/xml/config" xmlns:ns2="com.bbn.marti.config">
    <nameEntries>
        <nameEntry name="O" value="rustak"/>
        <nameEntry name="OU" value="rustak"/>
    </nameEntries>
</ns2:certificateConfig>
```
- The `xmlns:ns2` value is the **literal string `com.bbn.marti.config`**, not a real namespace URI —
  reproduce it verbatim for byte-for-byte ATAK/TAK-ecosystem tooling compatibility, but note
  CloudTAK's own parser doesn't care about the namespace URI, only the local name `nameEntries` /
  `nameEntry` and the `name`/`value` attributes.
- **`nameEntry` must be emitted as ≥2 elements.** CloudTAK's `xml-js` "compact" mode collapses a
  single-element array to a bare object, and its `for (const ne of nameEntries.nameEntry)` then
  throws on a non-iterable. Always emit at least `O` and `OU`.
- **A `nameEntry` value must be non-empty.** commoncommo's `generateCSR` passes each value to
  OpenSSL's `X509_NAME_ENTRY_create_by_NID`, which rejects a zero-length directory string (the
  minimum length for `OU` is 1), so padding the pair with `value=""` fails every enrolment at
  `EnrollUpdate: step 1 … status 14 (CSR generation failed using provided parameters)` — seen on the
  first real commoncommo run, nightly 35369773295. Pad with the organisation instead, which is also
  TAK Server's own default shape (`O=TAK` beside `OU=TAK`).
- `<nameEntries>` is the only optional child; when absent, the element is bare
  `<ns2:certificateConfig .../>`.

## 2. CSR shape ATAK builds against this response

- CN = the enrollment username (the account, not the device).
- Then every `<nameEntry name= value=>` appended **in document order** — so `O` before `OU` if that
  is the order returned. rustak must validate the CSR's RDN set is exactly `CN` + the configured
  name entries (case-insensitive on both name and value) and reject anything else (06 §3.2).
- Key: RSA nominally 4096-bit (07 §1.2, with a documented upstream bug meaning the actual bit length
  is not contractually guaranteed by that client — accept any RSA key rustak's own policy allows).
  Signature: SHA-256.
- CloudTAK's CSR: `CN = <username>` + `O`/`OU` read from the same response, RSA key size chosen by
  `node-forge`'s `pem.createCSR` (not independently verified here — accept standard RSA CSRs).

## 3. `POST /Marti/api/tls/signClient/v2`

```
POST /Marti/api/tls/signClient/v2?clientUid=<device-uid>[&version=<client-version-string>]
Authorization: Basic <base64(username:token-or-password)>
Content-Type: application/octet-stream   (ATAK) | unset (CloudTAK — string body, no header)
Accept: application/xml   (ATAK)  |  application/json  (CloudTAK, or Accept omitted defaults to JSON)

body = base64 of the DER CSR, PEM armour optional
```

- **Method is POST.** OpenTAKServer's docs list this under PUT — accept only POST; do not add a PUT
  alias (04 gotcha, corroborated 03 §2.1 step 3b and §8.10).
- **Auth: accept Basic** (mandatory — both clients send it) **and also accept Bearer** as a fallback,
  since CloudTAK obtains an OAuth token first as part of login even though it does not use it for
  this call (03 §2.1 step 3b: "the OAuth token is obtained … but then not used for the signing
  call"). Reject the request only if neither is present or valid.
- **`version` query param**: value is not otherwise inspected; ATAK sends its own version string,
  CloudTAK sends a fixed `"3"`. Presence alone marks the cert as "channels-capable" (see §4).
- **Body**: raw base64. Strip `-----BEGIN CERTIFICATE REQUEST-----` / `-----END CERTIFICATE
  REQUEST-----` if present, then base64-decode (whitespace/newlines inside the base64 must be
  tolerated). Do not require PEM armour — CloudTAK sends bare, unwrapped base64 with no headers at
  all in some configurations; ATAK sends armour with the header lines only.
- **CN check**: CSR's `CN` must equal the authenticated (Basic) username, **case-insensitive**. On
  mismatch, fail the request (do not silently rewrite the subject).
- **`clientUid`**: the device UID; persist it alongside the issued cert for revocation/audit lookups.
  For CloudTAK's per-service enrollment, `clientUid` is `"<username> (ETL)"` — an arbitrary string
  with a trailing space and parenthetical; do not validate it as a UID shape.

### Response — negotiated by `Accept`

**JSON** (default; also used when `Accept` is `*/*`, missing, or explicitly `application/json` —
this is what CloudTAK always requests):
```json
{ "signedCert": "<bare base64 DER, no PEM armour>", "ca0": "<bare base64 DER>", "ca1": "<bare base64 DER>" }
```
`ca0`, `ca1`, … are one key per certificate in the issuing chain (root first or last is not load-
bearing to either client — CloudTAK just pushes whichever are present into an array). Emit as many
`caN` keys as needed; omit the field entirely if there is no chain to return.

**XML** (only when `Accept: application/xml` — this is ATAK):
```xml
<?xml version="1.0" encoding="UTF-8"?><enrollment><signedCert>{base64}</signedCert><ca>{base64}</ca><ca>{base64}</ca></enrollment>
```
ATAK's parser treats **every child element except one named `signedCert`** as a CA, so `<ca>` naming
is a convention, not a requirement it checks — but use `<ca>` for clarity. No namespace, no
`<validityPeriod>` element, no trailing newline in the base64 bodies.

**Status codes**: `200` on success for **both** representations. **A `201` is treated as failure by
ATAK** (07 §1.2 HTTP status mapping: only `200 → SUCCESS`) — never return `201` from this endpoint.
`401` → bad credentials, `403` → access denied, `404`/`410` → no such resource; anything else is a
generic failure to both clients.

## 4. Certificate issuance parameters

| Property | Value |
|---|---|
| Key usage | `digitalSignature`, `keyAgreement`, `nonRepudiation` |
| Extended key usage | `clientAuth` (1.3.6.1.5.5.7.3.2); add `challengePassword` (1.2.840.113549.1.9.7) when the `version` query param was present — this is the "channels-capable" marker some clients gate optional UI on. ATAK itself performs **no EKU/KU inspection** on the returned cert (07 §1.8), so this is for TAK-ecosystem parity, not a functional requirement for ATAK. |
| Serial | rustak should use a real random/sequential serial with no collision risk — TAK Server's own 31-bit non-cryptographic random (06 §3.5) is a known weak point, don't copy it |
| Validity | configurable (default suggestion: 365 days), `notBefore` backdated slightly (TAK Server uses 720 minutes) to tolerate clock skew |
| Subject | TAK Server copies the CSR subject verbatim; **rustak replaces it** with `CN=<authenticated user>` + configured `O`/`OU` (only the CSR's public key survives; SANs/extensions dropped) — the CN is still validated against the user (design 03 §3, M2-01). Clients never inspect the subject beyond CN. |

## 4a. The third call: the enrolment profile, with the token already spent

ATAK's enrolment is **three** Basic-authenticated calls carrying the same credential, and the third
one is not optional:

```
GET  /Marti/api/tls/config
POST /Marti/api/tls/signClient/v2?clientUid=<uid>&version=<v>     ← spends a one-time token
GET  /Marti/api/tls/profile/enrollment?clientUid=<uid>            ← 0.4 s later, same credential
```

The third call is made **unconditionally** — it is not gated by `deviceProfileEnableOnConnect`, which
gates only the on-connect fetches — and ATAK reconnects the stream only after it (07 §1.5 step 6,
§2.4). Anything but `200`/`204`/`304` is a `ConnectionException` inside ATAK (07 §2.3), reported as
`Failed to get profile: Enrollment (401)` and then `TAK server registration failed`.

**So a server that spends the one-time token on the signing call must still answer the profile
fetch.** rustak does, through a grace window (`[auth] enrollment_grace`, 10 minutes by default):

- **only** `GET /Marti/api/tls/profile/enrollment` and `GET /Marti/api/tls/profile/tool/**` — not
  a second `signClient`, not `tls/config`, not `/oauth/token`, not the Marti API;
- **only** for the `clientUid` the signing call recorded with the spend, so a token spent by one
  device fetches nothing for another, and a signing call that named no `clientUid` leaves no grace
  at all;
- **only** until `spent_at + enrollment_grace`, after which the token is simply spent;
- and a revocation ends it immediately — revoking a credential clears the spend it is measured
  from.

The token's own expiry is not re-checked inside the window: the spend is proof it was live, and an
`enrollment_token_ttl` of 15 minutes would otherwise strand a device that scanned the code at 14:59.

Verified against a real ATAK 5.6 enrolment, 2026-09-19 (`.claude/plan/status/M2-15-field-report.md`).
CloudTAK never reaches this call — it holds a reusable client password, which is not spent — which is
why neither interop suite caught the `401`.

## 5. QR code / quick-connect

`tak://com.atakmap.app/enroll?host=<host[:port[:proto]]>&username=<user>&token=<enrollment-token>`
— all three params required. `port` defaults to `8089` if omitted; `proto` is only honoured if it is
literally `quic`, otherwise the streaming protocol is forced to `ssl`. The enrollment HTTP call
itself **always** targets the configured enrollment port (`:8446` for rustak), independent of the
`host` port in the QR — that port is only used for the resulting stream connect string. `token` is
sent as the Basic password against `GET /Marti/api/tls/config` and `POST …/signClient/v2`. Verified
07 §1.7.

**Trust bootstrap**: "quick connect" (typed host/user/token, or the QR flow) verifies the enrollment
HTTPS endpoint against **public CAs with hostname verification on**, then stores whatever CA chain
the sign response returns as the client's private truststore for the stream connection. There is no
"trust anyway" prompt on failure — a `SERVER_NOT_TRUSTED` error is a hard failure and the user must
retry. rustak's public listener should therefore present a certificate chain that validates under
common CA bundles (ACME) for the QR flow to work without a manual truststore import; the `internal`
CA mode requires operators to distribute the CA out of band (07 §1.4, §1.6; `plan.md` TLS section).

## 6. What ATAK persists after a successful enrollment

- Private key (kept client-side; never round-trips through rustak again).
- The client P12, keyed by **host + streaming port**.
- If quick-connect: the returned CA chain, as the truststore for that host.
- Connect string: `"{host}:{port}:{ssl|quic}"`; `enrollForCertificateWithTrust=true`,
  `enrollUseTrust=false`; it does **not** set `useAuth` — the enrolled stream authenticates purely by
  client cert, never by `<auth>` credentials (07 §1.5). This is exactly rustak's own stream-auth
  model (§1 above), so nothing extra is required server-side for this to work.
- It then fetches the enrollment device profile (see `profiles.md` §"Enrollment profile") before
  reconnecting the stream, **unconditionally** (not gated by the `deviceProfileEnableOnConnect`
  preference that gates the on-connect profile fetches).

## 7. CloudTAK-specific requirements

- CloudTAK performs this whole flow with `Authorization: Basic <username>:<client-password>` from
  its own stored server-connection credentials (see `oauth.md` for how the password is obtained via
  `/oauth/token`, and note the signing call itself does **not** reuse that OAuth bearer — §3 above).
- CloudTAK re-enrolls automatically whenever a stored cert is within **7 days** of expiry, using the
  password it already has cached — so a client-password-based deployment must accept repeated
  `signClient/v2` calls for the same identity without complaint (treat each as a fresh issuance, not
  an error).
- CloudTAK's response parsing requires **exactly** the JSON shape in §3 — no extra required fields,
  but `signedCert` must be present and must be bare base64 (no PEM banner). It reconstructs the PEM
  armour itself.
- CloudTAK's connectivity smoke test before any of this (`GET /files/api/config`, see `files.md`)
  must succeed first, or the setup wizard never reaches the enrollment step (03 §1.2).

## 8. The assertion credential (rustak's own, M9-06)

**Not a TAK contract.** Nothing in ATAK, CloudTAK or TAK Server knows about this; it is an
extension rustak accepts on the *same* routes, and every rule above still holds byte for byte for
the clients that do not use it.

A sidecar under Nomad or Kubernetes presents the workload identity its orchestrator already gave
it — a short-lived JWT the orchestrator signed — instead of a one-time enrolment token:

- **Where.** `GET /Marti/api/tls/config` and `POST /Marti/api/tls/signClient/v2` (and the two
  `/Marti/api/tls/profile/**` routes), plus `POST /oauth/token` with
  `grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer` and `assertion=<jwt>` (RFC 7523 §2.1),
  which answers the password grant's three fields and no `refresh_token`.
- **How.** `Authorization: Bearer <jwt>` — and, on the enrolment routes only, HTTP Basic with the
  JWT as the **password**, because a `commoncommo`-shaped client has a username box and a password
  box and nothing else. The Basic *username* is ignored: the server's binding rules decide the
  account, not the header. Accepting both is the same accommodation §3 already makes for CloudTAK.
- **Who it is.** `[auth.workload]` maps (issuer, namespace claim, subject claim + prefix) to exactly
  one existing account of kind `service`. A token two rules disagree about is refused rather than
  resolved.
- **What changes on the wire.** Nothing. The request bodies, the `Accept` fork, the bare-base64
  response and the `200`-not-`201` rule are identical; only the credential in the header differs.
- **What changes in the register.** The certificate's `issued_via` is `workload_identity` rather
  than `enroll_v2_json`/`enroll_v2_xml`, which is what `[auth.workload] revoke_previous` matches on
  when a rescheduled task's new certificate supersedes the one the old node still holds.
- **What is *not* spent.** There is no one-time token, so there is nothing to claim and nothing to
  release: the `claim`/`release` dance of §3 does not apply, and a second enrolment by the same
  workload is expected rather than refused.

## Gotchas

- `signClient/v2` on `201` is a **failure signal to ATAK** — always `200` on success (§3).
- The enrolment is **three** calls with one credential, not two: the profile fetch that follows the
  signing call carries the token the signing call spent, and a `401` there fails the whole
  registration on the device (§4a).
- `nameEntry` must be a real array (≥2 entries) in the `tls/config` XML, or CloudTAK's XML-to-JS
  compaction breaks the client (§1).
- Accept Basic **and** Bearer on `signClient/v2`; CloudTAK sends Basic here even though it holds a
  bearer token from the login step (§3).
- `signedCert` and each `caN` must be bare base64 — no `-----BEGIN CERTIFICATE-----` armour; both
  clients add or strip it themselves and disagree on which one does the wrapping (§3).
- CN comparison against the authenticated username is case-insensitive (§3).
- Never require the CSR to be PEM-armoured; strip banner lines only if present (§3).
- OpenTAKServer's docs claim `PUT` for this endpoint — real ATAK/CloudTAK always use `POST`; do not
  special-case PUT (04, "known-wrong in OTS").
- A workload assertion that this server will not accept must leave the request **exactly as it found
  it**, so the enrolment token an installation has always used still works on the same route; only
  an assertion that verified and was then refused for a reason of ours ends the request (§8).

## Verified in

- `research/07-atak-client-verified.md` §1 (ATAK enrollment sequence, ports, trust bootstrap,
  post-success state) and §2 (the device-profile calls, their auth and their status handling) —
  authoritative for ATAK.
- `.claude/plan/status/M2-15-field-report.md` — the first real ATAK enrolment against a production
  server, which is where §4a comes from.
- `research/06-takserver-http-api-verified.md` §3 (`CertManagerApi` exact request/response,
  issuance parameters, status-code mapping) — authoritative for the HTTP contract.
- `research/03-cloudtak-node-tak-contract.md` §2.1 steps 3a/3b, §2.4 (CloudTAK's enrollment client,
  re-enrolment window, TLS trust asymmetry) — authoritative for CloudTAK.
- `research/04-opentakserver-source-map.md` "Worth copying" / "Known-wrong" (the `Accept`-header
  fork and JSON/XML duality are worth keeping; the PUT-vs-POST confusion is not) — OTS pitfalls only.
- `plan.md` Appendix A.3, A.2 — baseline digest, expanded here.
