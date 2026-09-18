# Contacts / client endpoints (`/Marti/api/contacts/*`, `/clientEndPoints`) — wire contract

**Purpose.** The read-only "who else is on this server" surfaces ATAK's contact list and CloudTAK's
contacts page use. Served on the mTLS Marti listener (`:8443`), client cert auth. Contract for
`rustak-server::marti::contacts` (backed by `stream::hub`'s live subscription registry, plus a
last-known cache for disconnected users).

Baseline: `plan.md` Appendix A.4 "Contacts". This file expands it.

## 1. `GET /Marti/api/contacts/all`

```
GET /Marti/api/contacts/all
```

**Bare JSON array — no `{version,type,data}` envelope.** This is the one contact/subscription
endpoint that deliberately breaks the envelope convention; do not wrap it. Verified 06 §6.1,
independently confirmed by CloudTAK's client, which parses it as a raw array with no `TAKList`
wrapper (03 §3.5, flagged there as "unlike almost everything else").

Every element, **all keys always present** (null when unset, never omitted):
```json
{ "filterGroups": [], "notes": "", "callsign": "…", "team": "…", "role": "…", "takv": "…", "uid": "…" }
```

| Field | Source |
|---|---|
| `uid` | the subscription's `clientUid` (**not** an internal subscription id) |
| `callsign` | last known callsign from SA |
| `team` | `__group/@name` from the last SA |
| `role` | `__group/@role` from the last SA |
| `takv` | `"{platform}:{version}"` string, not a structured object |
| `notes` | free text; emit `""` rather than `null` — CloudTAK's CLI formatter calls `.trim()` on it unguarded |
| `filterGroups` | rustak has no per-contact geospatial filter feature yet — always `[]` |

List every connected-or-recently-seen client the caller has group visibility into (`OUT` on at least
one group the contact holds `IN`, mirroring the streaming reachability rule in `streaming.md` §8).
`sortBy`/`direction` query params are accepted by TAK Server on this route but never actually applied
— safe to accept and ignore rather than implement real sorting (06 §6.1).

## 2. `GET /Marti/api/clientEndPoints`

```
GET /Marti/api/clientEndPoints?secAgo=<int default 0>&showCurrentlyConnectedClients=<bool>&showMostRecentOnly=<bool>&group=<repeatable>
```

| | |
|---|---|
| Response | `200`, envelope `{"version":"3","type":"com.bbn.marti.remote.ClientEndpoint","data":[…],"nodeId":"…"}` |
| Cache headers | set `Cache-Control: must-revalidate, max-age=0, no-cache, no-store` and `Expires: 0` — this is the one endpoint TAK Server explicitly marks non-cacheable |

- `showCurrentlyConnectedClients` / `showMostRecentOnly`: accept as loosely-typed booleans (treat
  any value other than the literal string `"true"` as false); never 400 on an unexpected value —
  TAK Server's own params are declared as `String`, not `bool`, for exactly this reason (06 §6.2).
- `group` is **repeatable** (`?group=a&group=b`); when present, the caller must hold `OUT` on every
  named group or the whole request is rejected (`403`, standard error envelope — see `cloudtak.md`
  §"Errors"). No `group` param ⇒ no filtering beyond the caller's own visibility.
- `secAgo < 0` ⇒ `400`.

### `ClientEndpoint` JSON

| Field | Notes |
|---|---|
| `callsign`, `uid`, `username`, `team`, `role` | strings |
| `lastStatus` | **exactly** `"Connected"` or `"Disconnected"` — ATAK's parser does `enum.valueOf` on this and rejects the **entire response** if any element has anything else (07 §7.1) |
| `lastEventTime` | date-time string, see "Date formats" below |

`groups` is never serialised on this model — do not add it even though the server computes group
visibility to filter the list.

### Date format — `lastEventTime`

Render as `yyyy-MM-dd'T'HH:mm:ss.S'Z'` — **unpadded** milliseconds (one to three digits, no leading
zeros forced), literal `Z` (not a real offset). This is TAK Server's `COT_DATE_FORMAT` constant,
used specifically for this field (06 §1.6, §6.2). Contrast with `missions.md`, which pads to three
digits for most mission timestamps — `clientEndPoints` is one of the few unpadded-millis fields;
don't default to the padded form here.

ATAK fetches this list **once per connect string per run** (it never refreshes it automatically —
07 §7.1), so don't rely on ATAK re-polling to pick up a stale contact; the initial response after
connect is what the user sees until reconnect.

## 3. `GET /Marti/api/subscriptions/all` (admin/diagnostics — lower priority)

```
GET /Marti/api/subscriptions/all?sortBy=CALLSIGN&direction=ASCENDING&page=-1&limit=-1
```

Envelope `{"version":"3","type":"SubscriptionInfo","data":[…],"nodeId":"…"}`. This is a large
(~30-field) diagnostic view of every live subscription (ip, port, protocol, metrics, incognito flag,
etc.) used by the TAK Server admin UI. Neither ATAK nor CloudTAK's core flows call it — CloudTAK's
`node-tak` client defines it but no route in CloudTAK itself uses it (03 §3.7). Treat as **Tier 3**:
implement a reasonable subset (`callsign`, `clientUid`, `groups`, `role`, `protocol`, `incognito` at
minimum) once the admin UI needs it; not required for M2's node-tak/CloudTAK interop gates.

## Gotchas

- `/contacts/all` is a **bare array**. Every other list endpoint in this family uses the
  `{version,type,data}` envelope — this one deliberately doesn't. Don't "fix" it into an envelope.
- `lastStatus` must be exactly `"Connected"` / `"Disconnected"` (capitalised, no other values) or
  ATAK discards the **whole** `clientEndPoints` response, not just the bad element (§2).
- `lastEventTime` uses **unpadded** millisecond precision (`.S`), not the three-digit `.SSS` form
  used elsewhere in the mission API — verify against `missions.md`'s date-format table if unsure
  which one a given field needs.
- `clientEndPoints`'s `group` filter check is a hard `403` on any group the caller can't see, not a
  silent drop of that group from the filter.

## Verified in

- `research/06-takserver-http-api-verified.md` §6 (`ContactsApi`, `ContactManagerApi`,
  `SubscriptionApi`, exact JSON shapes and date formats) — authoritative.
- `research/07-atak-client-verified.md` §7.1 (ATAK's `GetClientListOperation`/`ServerContact`
  parsing, fetch cadence, mandatory-field rejection behaviour) — authoritative for ATAK.
- `research/03-cloudtak-node-tak-contract.md` §3.5–§3.7 (`contacts.ts`, `client.ts`,
  `subscriptions.ts` — envelope shapes, which are actually called) — authoritative for CloudTAK.
- `plan.md` Appendix A.4 "Contacts" — baseline digest, expanded here.
