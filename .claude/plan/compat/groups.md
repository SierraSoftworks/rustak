# Groups / channels (`/Marti/api/groups/*`) — wire contract

**Purpose.** The channel-membership surface ATAK's Channels UI and CloudTAK's server-connection
setup both depend on. Served on the mTLS Marti listener (`:8443`), client cert auth. Contract for
`rustak-server::identity::groups`/`members` and `marti::groups`.

Baseline: `plan.md` Appendix A.3 "Channels" and A.4 "Groups". This file expands both.

A **group** (TAK's "channel") is a name + a **direction**: `IN` = the holder may publish into it,
`OUT` = the holder may receive from it (see `streaming.md` §8 for the reachability rule this
feeds). A user can hold the same group name with both directions as two separate rows. Every group
has a `bitpos` — a stable, monotonically-increasing integer used as the group's index in the
group-vector bitstring stored per subscription/mission (`conventions.md` storage rules — rustak
should assign bitpos from a simple incrementing counter in SQLite, never reuse one).

## 1. `GET /Marti/api/groups/all`

```
GET /Marti/api/groups/all?useCache=<bool default false>&sendLatestSA=<bool default false>
```

| Response | `200`, envelope `{"version":"3","type":"com.bbn.marti.remote.groups.Group","data":[…],"nodeId":"…"}` |
|---|---|

- `useCache=false` (the default): return the caller's **`OUT`**-direction groups only. This is what
  a fresh, uncached client sees. **rustak does not implement this branch** — design 04 D8, recorded
  here 2026-09-18 (R-02 contract defect 4): every verified client hardcodes `useCache=true` (ATAK at
  07 §5 line 469, CloudTAK at 03 §3.4), so both get exactly TAK Server's behaviour and the branch
  would be dead code nothing exercises. A caller that passes `useCache=false` gets the cached answer.
- `useCache=true`: return the caller's full **active-group selection** (both directions, as the
  client last set them via `PUT …/active`). CloudTAK always calls with `useCache=true` — it
  deliberately defers channel-selection caching to the server so its horizontally-scaled replicas
  agree (03 §3.4).
- `sendLatestSA=true`: in addition to returning groups, push every currently-reachable peer's latest
  SA down the caller's *streaming* connection (a side effect on `:8089`, not on this HTTP response).
  ATAK sets this specifically when reacting to a `t-x-g-c` notification (see `streaming.md` §9).
- **Normalisation rustak must apply before serialising any `Group`** — ATAK's own `ServerGroup`
  parser (07 §5.2) rejects a group missing these, so never emit a group without them:
  - `bitpos` present and `>= 0` (ATAK drops the entire group if `bitpos` is absent or negative).
  - `created` present.
  - `type` present.
  - `direction` present.

### `Group` JSON

| Field | Type | Notes |
|---|---|---|
| `name` | string | required |
| `direction` | `"IN"` \| `"OUT"` | required |
| `created` | string, **`yyyy-MM-dd`** (date only, no time) | required; verified 06 §5.2 |
| `type` | `"SYSTEM"` \| `"LDAP"` | rustak has no LDAP source — always `"SYSTEM"` |
| `bitpos` | integer | required, `>= 0` |
| `active` | bool | always present (defaults `true`) |
| `description` | string | omit when absent |
| `distinguishedName` | string | LDAP-only; rustak never emits this |

Verified 06 §5.2; independently confirmed by ATAK's parser requirements (07 §5.2) and CloudTAK's
TypeBox schema, which matches field-for-field (03 §3.4).

## 2. `PUT /Marti/api/groups/active`

```
PUT /Marti/api/groups/active?clientUid=<optional device uid>
content-type: application/json        <-- lower-case header name as ATAK sends it; accept any casing
[<Group>, <Group>, …]                 <-- bare JSON array, NOT an envelope
```

- Each element is the same `Group` shape as §1, **except `created` is epoch milliseconds (a JSON
  number) here, not the `yyyy-MM-dd` string GET returns.** This asymmetry is real — verified
  independently in both 07 §5.1 (ATAK's client always serialises this way) and 06 §5.4 (the server
  reads a `Group[]` body with no date-format coercion, so a numeric `created` round-trips). `active`
  on each element is the field that actually matters — it's the caller's requested new selection.
- Invalid elements (missing `name`/`direction`/`type`) should be silently dropped from the request
  rather than rejecting the whole call — ATAK's own writer only ever emits well-formed ones.
- Response: `200`, empty body.
- Applying the change: recompute the caller's active-group cache from `active` on each element; if a
  live streaming connection exists for that principal, re-authenticate it against the new set.
- **`t-x-g-c` emission**: only send the group-change notification (`streaming.md` §9) when
  `clientUid` was supplied on the request **or** the change was made by an admin on someone else's
  behalf. A bare `PUT …/active` with no `clientUid` changes the cache silently — no notification.
  When `clientUid` is present, the notification's `uid` gets `.{clientUid}` appended and it is sent
  to the user's **other** devices, not back to the one that made the change (06 §5.4, §5.5).
- Also accept `PUT /Marti/api/groups/activebits` (body = bare JSON array of `bitpos` integers) as an
  equivalent shorthand if convenient to implement; it is optional (not used by ATAK or CloudTAK).

## 3. `t-x-g-c` over the stream

See `streaming.md` §9 for the exact XML. Triggers:
- A successful `PUT …/active` with `clientUid` present (§2).
- Any server-side group-membership change an admin makes on a connected user (e.g. via the admin
  UI), which rustak should treat as the forced/broadcast case: send to **all** of that user's
  devices, no exclusion.

Both ATAK and CloudTAK react to `t-x-g-c` by clearing that server's map items and re-fetching
`GET /Marti/api/groups/all?useCache=true&sendLatestSA=true` (07 §5.3, 03 §4.3) — so the notification
alone is not enough; the subsequent `GET` must return the fresh state.

## 4. ATAK UI gating (background only — not server behaviour)

ATAK only shows the Channels UI at all when preference `prefs_enable_channels` (boolean) is true,
and only lists a given server when `prefs_enable_channels_host-<host>` (string `"true"`) is set —
both are local ATAK preferences, not anything rustak serves, but they explain why a correctly-
implemented `/groups/all` might appear to do nothing in a fresh ATAK install. rustak's enrollment
profile can set `prefs_enable_channels=true` via the generated `.pref` (see `profiles.md`) to turn
this on automatically. Verified 07 §5.4.

## 5. CloudTAK-specific requirements

- CloudTAK's channel filter is built as `groups.filter(g => g.active).map(g => g.bitpos)` — so
  `bitpos` and `active` are load-bearing for every CloudTAK feature gated by channel, not cosmetic.
- Before creating any Data Sync (mission), CloudTAK **force-activates every group** the user holds:
  it calls `GET …/all?useCache=true`, and if any returned group has `active: false`, immediately
  calls `PUT …/active` with the full list, every entry's `active` forced `true`. rustak must accept
  this — a mission-creation flow will otherwise stall waiting on a groups round-trip that never
  changes anything. Verified 03 §3.4.
- CloudTAK never calls `/Marti/api/groups/groupCacheEnabled` — safe to omit or stub `{"version":"3",
  "type":"java.lang.Boolean","data":true,"nodeId":"…"}` if implemented for other clients.

## Gotchas

- `created` is a **date string** (`yyyy-MM-dd`) on the way out of `GET …/all`, but the **same field**
  is **epoch milliseconds** on the way into `PUT …/active`. This is not a documentation error —
  reproduce it exactly, both directions (§1, §2).
- A group with `bitpos` missing or negative is invisible to ATAK — never emit one; assign every
  group a `bitpos >= 0` at creation time (§1).
- `PUT …/active`'s `content-type` header is sent lower-case by ATAK (`content-type`, not
  `Content-Type`) — accept case-insensitively as HTTP requires, don't string-match it.
- Sending `t-x-g-c` without `clientUid` on the triggering `PUT` is correct — that's the "no device
  identified, don't notify" path, not a missed notification (§2).
- CloudTAK's force-activate-all-groups call before mission creation is not a bug in CloudTAK to work
  around — it is expected traffic the groups endpoint must handle cleanly every time (§5).

## Verified in

- `research/06-takserver-http-api-verified.md` §5 (`GroupsApi`, `Group` JSON, `bitpos` assignment,
  `PUT …/active` semantics, `t-x-g-c` emission rule) — authoritative.
- `research/07-atak-client-verified.md` §5 (ATAK's channels client, `ServerGroup` parsing/validation,
  preference gating) — authoritative for ATAK.
- `research/03-cloudtak-node-tak-contract.md` §3.4 (CloudTAK's `groups.ts`, force-activate-before-
  mission-create behaviour) — authoritative for CloudTAK.
- `plan.md` Appendix A.3 "Channels", A.4 "Groups" — baseline digest, expanded here.
