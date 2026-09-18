# CloudTAK — cross-cutting requirements

**Purpose.** Rules that apply across *every* Marti endpoint when the client is CloudTAK
(`dfpc-coe/CloudTAK` + `@dfpc-coe/node-tak`), plus the pieces of CloudTAK's own architecture that
explain *why* those rules exist. Area-specific detail (missions, groups, files, …) lives in the
other `compat/*.md` files — this one is the index and the home for rules that don't belong to a
single area.

Baseline: `plan.md` Appendix A.2. This file expands it; do not contradict it.

## 1. The three-URL model

CloudTAK stores exactly one `server` row with three independent base URLs — it does not assume they
share a host or port:

| CloudTAK field | Role | rustak listener |
|---|---|---|
| `url` (`ssl://host:8089`) | CoT stream | `stream/listener_tls.rs`, `:8089` |
| `api` (`https://host:8443`) | Marti HTTPS, mTLS | Marti listener, `:8443` |
| `webtak` (`https://host:8446`, or `:443`) | OAuth + cert enrolment, username/password | public listener, `:8446` |

rustak serves Marti on **both** `:8443` and `:8446` (`plan.md` "Listeners" table — the public
listener carries "full Marti (for CloudTAK `webtak`)"), so all three of CloudTAK's URLs resolve
against one process; that's a deployment simplification, not something CloudTAK requires. CloudTAK
will work identically if the three point at genuinely different hosts. Verified 03 §1.1.

**Do not put credentials in query strings or logs for any of these** (`conventions.md` secrets
rules) — CloudTAK's own admin-cert / client-password fields are exactly the kind of material that
must never appear in tracing output.

## 2. The connectivity smoke test

`PATCH /api/server` (CloudTAK's own admin endpoint, not rustak's) validates a newly-entered server
by calling exactly `GET /files/api/config` (`files.md` §1) — nothing else. Until that succeeds,
CloudTAK's setup wizard cannot save a working connection at all, so this is the single
highest-priority endpoint for any CloudTAK bring-up test. Verified 03 §1.2.

**The *first* configuration exercises three surfaces, not one** (read from
`api/stateless/routes/server.ts` at 13.90.0 while building `interop/cloudtak`, and an addition to
§1.2 rather than a correction of it). On a CloudTAK whose `server.auth` is still empty the call is
**unauthenticated**, and it does three things in order:

1. if the body carries `auth: {cert, key}`, it validates that pair by calling `GET /files/api/config`
   through it — the smoke test above, over mTLS on `api`;
2. it **requires** a `username` and `password` as well, runs the `/oauth/token` password grant
   against `webtak` with them, and calls `Credentials.generate()` (`/Marti/api/tls/config` +
   `/Marti/api/tls/signClient/v2`) to enrol a certificate of its own for that account;
3. it makes that account CloudTAK's **system administrator** — the first successful pair wins, for
   the life of the installation.

So a bring-up test that gets past this one call has already proved `/files/api/config`, the password
grant and enrollment. An operator supplying `auth` without credentials is refused with
`Initial configuration must include valid TAK Username & Password to set System Administrator`, and
once `server.auth` is set the endpoint requires an administrator bearer token like any other.

## 3. TLS trust is asymmetric across the three URLs

- `api` (mTLS, `:8443`) and the stream (`:8089`): CloudTAK connects with `rejectUnauthorized: false`
  — a self-signed / internal-CA server certificate is fine on both.
- `webtak` (`:8446`): CloudTAK's OAuth/enrolment calls go through Node's `undici fetch` with **full
  system-CA verification, with no override available in CloudTAK's own config.** A self-signed or
  internal-CA certificate on this listener **breaks login outright** unless the operator sets
  `NODE_EXTRA_CA_CERTS` on the CloudTAK container.

**Consequence for rustak's TLS design** (`plan.md` "TLS exposure" decision): operators running
CloudTAK against rustak's `internal` CA mode must either import the internal CA into CloudTAK's
container trust store, or point `webtak` at a listener with an ACME-issued certificate. Document
this clearly in `docs/deployment.md` — it is the most common CloudTAK bring-up failure. Verified 03
§2.4, §8.5 (tracked upstream as CloudTAK issue #983, open).

## 4. Content-Type exactness — the highest-risk wire detail in the whole surface

`node-tak`'s response handling does a **strict string equality** check:
```
res.headers.get('content-type') === 'application/json'
```
`application/json; charset=UTF-8` (or any parameter suffix) falls into the fallback branch and
returns a **raw string** instead of parsed JSON — callers that then index into it
(`missions.data[0]`) get a `TypeError`. **Emit exactly `Content-Type: application/json` with no
parameters from every Marti JSON endpoint** (`conventions.md` "exact Content-Type values" rule).
Two node-tak call sites (`package.ts`, `files.ts#upload`) have a defensive `JSON.parse` fallback for
exactly this failure mode — but most call sites (`Mission.*`, `Group.*`, `MissionLayer.*`,
`Contacts.*`, `Client.*`) have **no such fallback** and will break outright. Verified 03 §8.1.

The Enterprise Sync legacy endpoints (`files.md` §2, §3) are a documented historical exception —
they use `text/json`, and no verified client parses them strictly, so that value is safe to keep as
emitted by every real TAK Server today.

## 5. Never redirect; status codes are load-bearing

```
if (res.status < 200 || res.status >= 400) { /* throw */ }
```
**3xx is treated as success** by node-tak, and the redirect target's body (or lack thereof) is
parsed as the payload — which will fail unpredictably. **Never emit a 3xx from any Marti route**
(`conventions.md` "Never emit 3xx on Marti routes"). Non-2xx bodies are sniffed for HTML and wrapped
with a parsed summary — a plain-text or JSON error body is fine, TAK Server's own Tomcat HTML error
pages are what that sniffing exists for, not a requirement to imitate. Verified 03 §8.3.

## 6. Identity / UID conventions to accept

- **Connection UID** (a CloudTAK "Connection" = a machine identity with its own client cert): the
  certificate's **subject DN, its RDN components reversed and comma-joined**. Node prints a
  certificate subject as newline-separated `key=value` pairs, least-significant-first; reversing
  gives conventional (`CN=…,OU=…,O=…`) order. rustak doesn't need to *compute* this — it's just what
  ends up as the stream/Marti principal's uid when the CN-extraction rule in `enrollment.md` issues
  a cert with more than one RDN — but tests and fixtures should exercise a multi-RDN subject (CN +
  O + OU) to make sure the round-trip is stable.
- **User UID**: `ANDROID-CloudTAK-{email}` — CloudTAK's per-logged-in-user stream identity. Accept
  this as an ordinary EUD uid; don't validate uid shape against ATAK's own `ANDROID-*` device-id
  conventions.
- **Data Sync creator UID**: `connection-{connectionId}-data-{dataId}` — used as `creatorUid` on
  mission creation and content uploads. Again, just an opaque string.

Verified 03 §1.5.

## 7. Implementation priority tiers (for sequencing briefs, not for skipping work)

| Tier | What breaks without it | Endpoints (see the owning file for detail) |
|---|---|---|
| 1 — nothing works | login, setup, stream connect | `POST /oauth/token`, `GET/POST /Marti/api/tls/*` (`enrollment.md`, `oauth.md`), `GET /files/api/config`, `GET /Marti/api/version`, `GET/PUT /Marti/api/groups/*` (`groups.md`), TLS 8089 + `t-x-c-t`→`t-x-c-t-r` (`streaming.md`) |
| 2 — a core page breaks | contacts, missions, packages | `GET /Marti/api/contacts/all` (`contacts.md`), mission CRUD/subscribe/cot (`missions.md`), `/Marti/sync/{search,upload,missionupload,content}` + `/Marti/api/files/metadata` (`files.md`), `GET /Marti/api/clientEndPoints` |
| 3 — feature pages degrade gracefully | changes/layers/role/logs/invitations/kml/metadata-mutation | see `missions.md` §13, `files.md` §6 |
| 4 — admin only | subscriptions/all, injectors, repeater, certadmin | `contacts.md` §3 |
| 5 — stub as empty | video | `GET /Marti/api/video` → `{"videoConnections": []}`, reject writes — see §8 below |

## 8. Video: stub, don't implement

CloudTAK's video integration is broken against **every** real TAK Server deployment today (writes
malformed entries — e.g. `port: -1` — that break iTAK's feed download; tracked as CloudTAK issue
#1347) and explicitly documented as non-functional by the OpenTAKServer project too. **Return
`{"videoConnections": []}` from `GET /Marti/api/video` and reject writes** (`404`/`501`) rather than
implementing the real feed-management semantics — this avoids the malformed-entry bug family
entirely and matches every other server operators run CloudTAK against. Verified 03 §3.18, §8.9.

## 9. Explicitly out of scope for CloudTAK (implement for ATAK/admin parity only, if at all)

Verified as **zero references** in CloudTAK's routes or the CloudTAK-exercised parts of node-tak:
`/Marti/api/version/config`, `/Marti/api/util/user/roles`, `/Marti/api/security/*`,
`/Marti/api/authentication/config`, `/Marti/api/qos/*` (doesn't exist in node-tak at all),
`/Marti/api/device/profile/*` (see `profiles.md`), `/Marti/api/missions/all/invitations` (CloudTAK
uses `?clientUid=` instead — `missions.md` §13), `/Marti/api/pagedmissions`,
`/Marti/api/iconset/all/uid`, `/locate/api`, `/Marti/api/user-management/*`. Don't spend M2–M4
effort here before the Tier 1–3 list above is green. Verified 03 §6.

## 10. Known gotchas not already covered in an area file

- **`allowGroupChange`**: node-tak's `Mission.update()` auto-sets `allowGroupChange=true` whenever
  the caller supplies a `group` field — treat this as a normal flag, not a sign of a hostile client
  (full detail in `missions.md` §3, §"Gotchas").
- **Unknown-hash on `/Marti/api/certadmin/cert/{hash}` reported as `500`** rather than `404` — low
  impact (admin-only path), but if implementing cert-admin, node-tak treats a `500` here as
  equivalent to "not found" and falls back to a list call; a clean `404` also works fine and is
  preferred for a fresh implementation.
- **CloudTAK can emit mission-destined CoT that looks like plain broadcast** when an ETL layer has
  mission-sync enabled (CloudTAK issue #1160, a client-side bug) — don't assume every mission
  content arrives via a well-formed `<dest mission=…>`; see `missions.md` §11.

## 11. OpenTAKServer comparison (background, not a target)

OpenTAKServer (OTS) is the other free-software TAK server CloudTAK is sometimes run against; the
project's own feature-comparison table and CloudTAK's issue tracker both make clear it is **not** a
reliable CloudTAK backend today (broken video, broken uploads/data-packages per its own docs). Its
codebase is a useful source of "things that look plausible but are wrong" — see the "Known-wrong in
OTS" list pulled into the relevant area files (`streaming.md` for the missing-negotiation/buggy-pong
items, `groups.md` for the `bitpos`-as-counter and `active`-ignoring-`enabled` items,
`missions.md`/`files.md` for the serialization bugs). Do not treat OTS source as a second
authoritative reference the way TAK Server's own source is — it is cited in this project **only**
for pitfalls to avoid, per `research/04-opentakserver-source-map.md`, never for facts to copy.
Verified 03 §7, 04 "Things worth stealing / avoiding".

## Verified in

- `research/03-cloudtak-node-tak-contract.md` — authoritative for all of CloudTAK's own behaviour:
  §1 (architecture, three URLs, identity model), §2.4 (TLS asymmetry), §6 (implementation tiers),
  §7 (OTS comparison), §8 (known gotchas).
- `research/04-opentakserver-source-map.md` "Things worth stealing / avoiding" — OpenTAKServer
  pitfalls only, cited here and in the area files it informs.
- `plan.md` Appendix A.2 — baseline digest, expanded here.
- See also: `enrollment.md`, `oauth.md`, `groups.md`, `contacts.md`, `missions.md`, `files.md`,
  `profiles.md`, `streaming.md` for the area-specific wire contracts this file indexes.
