# `compat/` — wire compatibility contracts

Distilled, implementation-ready wire contracts for ATAK-CIV and CloudTAK compatibility: exact
paths, verbs, params (with defaults), status codes, `Content-Type` values, JSON/XML field names and
casing, date formats, and CoT envelope `type` strings. Every file cites the `research/` report
section(s) it was verified against under **Verified in**. Implementation and reviewer agents should
read the relevant file(s) here rather than the raw research reports — this directory is the
authoritative, agent-facing digest; `plan.md` Appendix A is the one-page baseline these files
expand and must never contradict.

## Files

| File | Covers |
|---|---|
| [`streaming.md`](streaming.md) | CoT stream (`:8089`): XML/protobuf framing, TAK Protocol v1 negotiation, ping/pong, `<marti><dest>` routing, group reachability, flow tags, disconnect/group-change notifications, XML↔protobuf field mapping |
| [`enrollment.md`](enrollment.md) | `/Marti/api/tls/{config,signClient/v2}`: CSR/cert issuance, QR/quick-connect, trust bootstrap |
| [`groups.md`](groups.md) | `/Marti/api/groups/{all,active}`: channel model, `bitpos`, `t-x-g-c` triggers |
| [`contacts.md`](contacts.md) | `/Marti/api/{contacts/all,clientEndPoints,subscriptions/all}` |
| [`missions.md`](missions.md) | `/Marti/api/missions/**`: Data Sync CRUD, contents, subscriptions, tokens, changes, layers, logs, invitations, `t-x-m-*` notifications, CoT `<dest mission>` routing |
| [`files.md`](files.md) | `/Marti/sync/**`, `/Marti/api/files/**`, `/files/api/config`: Enterprise Sync, data packages, `b-f-t-r`/`b-f-t-a`, mission-package manifest |
| [`profiles.md`](profiles.md) | `/Marti/api/{tls,device}/profile/**`: enrolment/connection/tool profiles, `.pref` generation |
| [`oauth.md`](oauth.md) | `/oauth/token`, `/login/*`: password grant, JWT shape (incl. CloudTAK's fragile parser), group-claim mapping |
| [`cloudtak.md`](cloudtak.md) | Cross-cutting CloudTAK rules: three-URL model, TLS trust asymmetry, `Content-Type`/status-code exactness, identity conventions, implementation-priority tiers, video stubbing |

Read `cloudtak.md` first if the task at hand is a CloudTAK interop brief — it indexes which parts of
the other files are CloudTAK-critical (Tier 1–2) versus lower priority.

## Precedence rules

Research reports `05`, `06`, and `07` were read directly from TAK Server / ATAK-CIV source and are
**authoritative** — every fact in `compat/*.md` traces back to one of them (or to `03` for CloudTAK,
or to `plan.md` for rustak's own architectural decisions). Where an earlier, less-verified report
disagrees with a later verified one, the verified one wins:

| Report | Role | Wins over |
|---|---|---|
| `05-takserver-streaming-auth-verified.md` | Streaming/auth/routing/notifications — authoritative | `02` |
| `06-takserver-http-api-verified.md` | Marti HTTP contracts — authoritative | `02` |
| `07-atak-client-verified.md` | ATAK client behaviour — authoritative | `02` |
| `03-cloudtak-node-tak-contract.md` | CloudTAK/node-tak requirements — authoritative for CloudTAK specifically | `02` where CloudTAK behaviour is described |
| `02-tak-protocol-atak-web-research.md` | Early protocol overview — background only; treat any unverified claim from it as **unconfirmed** unless a `compat/*.md` file cites it directly | — |
| `04-opentakserver-source-map.md` | OpenTAKServer source map — cited **only** for pitfalls (bugs to avoid, non-standard behaviour), never as a source of facts to replicate | never wins; not authoritative for anything rustak should copy |

No conflicts were found between `05`/`06`/`07`/`03` while writing this directory — they describe
disjoint layers (stream protocol, HTTP API, ATAK client, CloudTAK client) and agree everywhere their
scopes overlap (e.g. the `IN`/`OUT` group-reachability rule appears independently in both `05` §6.3
and `06` §5.7 with matching semantics; the `Group`/`ClientEndpoint`/mission JSON shapes in `06`
match field-for-field against `07`'s and `03`'s independent client-side parsers). Where this
directory had to make a **judgement call** because the reports describe real client-observed
behaviour that a from-scratch server is free to diverge from (not a conflict between reports, but a
place rustak's own design intentionally differs from TAK Server's), it's called out explicitly in
the owning file — see in particular:

- `streaming.md` §1 — rustak has no `<auth>` username/password stream handshake at all (cert-only,
  per `conventions.md` security defaults); TAK Server's `<auth>` shape is documented as background
  only, in case a legacy client sends one unprompted.
- `oauth.md` §4 — rustak is a full OIDC identity authority (`plan.md` "OIDC / CloudTAK" decision),
  not a thin LDAP-bind proxy like a typical TAK Server deployment; the `/login/*` wire shapes are
  reproduced for ATAK/admin-UI federation, but CloudTAK itself never exercises that path (§2.2 in
  `research/03`) — it only ever needs the password-grant shape in §1.
- `missions.md` §10 — mission tokens use a **dedicated sealed secret**, not the server's TLS/RS256
  key reused as an HMAC secret (TAK Server's own approach, which this project deliberately does not
  copy — see `conventions.md`'s secrets rules).
- `cloudtak.md` §8 — video is stubbed as an empty list rather than implementing TAK Server's real
  feed-management semantics, because every verified client integration against it is broken anyway.

## Conventions used across every file in this directory

- **Never copy code or comment text** from GPL sources (`atak-civ`, TAK Server, OpenTAKServer).
  JSON/XML shapes, endpoint paths, field names, status codes, and other wire facts are not
  copyrightable expression and are documented directly; XML/CoT templates are rendered in this
  project's own words from the verified structure, never transcribed source strings.
- **Exact `Content-Type`** — most JSON Marti endpoints require exactly `application/json` (no
  `charset` parameter) because CloudTAK's client does strict string equality on it; a handful of
  legacy Enterprise Sync endpoints are a documented `text/json` exception. See `cloudtak.md` §4.
- **Never 3xx on a Marti route** — CloudTAK treats any 3xx as success and parses the redirect body.
  See `cloudtak.md` §5.
- **Date formats are field-specific, not global** — this project uses at least three distinct
  formats depending on which field of which model is being serialised (padded-millis
  `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'`, unpadded-millis `yyyy-MM-dd'T'HH:mm:ss.S'Z'`, and bare-date
  `yyyy-MM-dd`). Each file's tables call out which format each field uses; don't assume one
  formatter for the whole codebase.
- **"Verified in" pointers** are section-level, not just report-level, so a reviewer can jump
  straight to the source material for any claim.
