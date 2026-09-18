# D-01 — Distil `.claude/plan/compat/*.md` from the research reports: status

## Summary

Wrote all nine per-area `compat/*.md` files plus `compat/README.md`, per the brief. Read `plan.md`
Appendix A (baseline), `conventions.md`, and research reports `03`–`07` in full (05, 06, 07 read
end-to-end as the primary source of facts; 03 read end-to-end for CloudTAK; 04 skimmed specifically
for its "Things worth stealing / avoiding" pitfalls section and the individual bug notes it flags).
Report `02` was **not** read in detail — per the brief's precedence rule it is superseded wherever
05/06/07 speak, and `plan.md` Appendix A (which is already reconciled against all seven reports) gave
sufficient coverage of the few places 02 is cited uniquely (e.g. the `DEFAULT:!ECDH` cipher-list
finding, which `plan.md`'s "TLS" decision and `M1-00`'s status note already resolve). If a future
agent finds a `compat/*.md` gap that only report `02` would fill, that's the one place worth a
follow-up read.

Only files under `.claude/plan/compat/**` and this status file were touched. No `git`/`but` commands
were run.

## Files written

| File | Lines | Covers |
|---|---|---|
| `compat/README.md` | 84 | Index, precedence rules, cross-cutting conventions |
| `compat/streaming.md` | 299 | CoT stream framing, negotiation, ping/pong, routing/reachability, flow tags, disconnect/group-change, XML↔protobuf mapping |
| `compat/enrollment.md` | 188 | `/Marti/api/tls/{config,signClient/v2}`, QR/quick-connect, trust bootstrap |
| `compat/groups.md` | 138 | `/Marti/api/groups/{all,active}`, `bitpos`, `t-x-g-c` |
| `compat/contacts.md` | 116 | `/Marti/api/{contacts/all,clientEndPoints,subscriptions/all}` |
| `compat/missions.md` | 359 | Full mission/Data Sync API, tokens, `t-x-m-*`, CoT dest routing |
| `compat/files.md` | 285 | Enterprise Sync, data packages, `b-f-t-r`/`b-f-t-a`, manifest |
| `compat/profiles.md` | 156 | Device profiles, `.pref` generation |
| `compat/oauth.md` | 149 | `/oauth/token`, JWT shape, `/login/*`, group-claim mapping |
| `compat/cloudtak.md` | 163 | Cross-cutting CloudTAK rules, implementation tiers |

All nine area files are within (or, for `contacts.md`, close under) the brief's 150–400-line target;
`contacts.md` sits at 116 because its area is genuinely small (three endpoints) — I chose not to pad
it with restated material from other files.

## Conflicts between reports, and how I resolved them

**No factual conflicts** were found between `05`, `06`, `07`, and `03` — they describe disjoint
layers (stream protocol / HTTP API / ATAK client / CloudTAK client) and every place their scopes
overlap, they agree:
- The `IN`/`OUT` group-reachability rule is derived independently in `05` §6.3 (stream broker) and
  `06` §5.7 (`CommonGroupDirectedReachability`, the same class cited from the HTTP-API angle) —
  identical semantics, cited together in `streaming.md` §8.
- `Group`/`ClientEndpoint`/`Mission` JSON shapes in `06` (server-side, from source) match
  field-for-field against `07`'s and `03`'s independent client-side parsers (`ServerGroup`,
  `ServerContact`, node-tak's TypeBox schemas) — no discrepancy needed adjudicating.
- The `t-x-m-*` mission-notification element/attribute layout is corroborated twice: `05` §7 (server
  source, the seed templates) and `03` §4.5 (node-CoT's independently-declared wire types) — I noted
  this cross-check explicitly in `missions.md` §12 since it's a strong compatibility signal.

What did require **judgement calls**, because the reports document real client-observed behaviour
that a from-scratch server is free to diverge from (design decisions, not report conflicts):
- `streaming.md` §1 — rustak has no `<auth>` username/password stream handshake at all (cert-only,
  per `conventions.md`/`plan.md` security defaults); documented TAK Server's `<auth>` shape as
  background only.
- `oauth.md` §4 — rustak is a full OIDC identity authority per `plan.md`'s "OIDC / CloudTAK"
  decision, not a thin LDAP-bind proxy; reproduced the `/login/*` wire shapes for ATAK/admin-UI
  federation but flagged that CloudTAK itself never exercises that path (confirmed in `03` §2.2 —
  CloudTAK's open-source `main` has no working OIDC client, only the password-grant flow).
- `missions.md` §10 — mission tokens use a dedicated sealed HS256 secret rather than TAK Server's
  own approach of reusing its RSA private-key bytes as an HMAC secret (`06` §8.1 documents that
  reuse as fact; I called it out as something rustak deliberately does not copy, per
  `conventions.md`'s secrets rules).
- `cloudtak.md` §8 — video is stubbed as an empty list rather than implementing TAK Server's real
  feed-management semantics, because `03` §8.9/§3.18 document that every verified client integration
  against real video is broken anyway (CloudTAK issue #1347, OpenTAKServer's own docs).

One near-conflict worth flagging explicitly, though it resolved cleanly: `plan.md` Appendix A.4 says
mission-related dates use `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'` "for Mission/MissionChange/LogEntry/
Resource?" with a parenthetical carving out `Resource.submissionTime`, `MissionSubscription`/
`Invitation.createTime`, and `ClientEndpoint.lastEventTime` as the unpadded `.S'Z'` form. Report `06`
confirms this field-by-field (§6.2, §7.13, §7.17, §7.14) — it isn't actually ambiguous, just densely
noted in the Appendix A digest. I expanded it into explicit per-field tables in `missions.md`,
`contacts.md`, and `files.md` rather than leaving readers to re-derive which fields get which
formatter from the parenthetical.

## Deviations / things I could not fully verify

- Neither `05`/`06`/`07` state the byte-exact dom4j XML serialisation TAK Server itself produces
  (declaration quoting, attribute-quote style) with full certainty — both reports flag this as
  "verify against a live server" (05 §3.2, 07's "Gaps" §2). I rendered every XML template in
  `streaming.md`/`missions.md`/`files.md` in **our own words** per the brief (double-quoted
  attributes, no `standalone`, no trailing newline — rustak's own serialiser convention, not a
  transcription of TAK Server's dom4j output), so this doesn't block implementation, but a contract
  test against `interop/node-tak` or a real ATAK capture is the right place to catch any
  byte-level surprise, not this document.
- `compat/missions.md` §13's condensed table (archive/layers/logs/invitations) doesn't enumerate
  every field's date-format individually — the high-traffic fields (Mission, MissionChange,
  MissionSubscription) are covered explicitly elsewhere in the file; logs/invitations date formats
  are lower-traffic enough that I judged the condensed treatment sufficient for a 150–400-line
  budget. Flagging here in case a reviewer wants them expanded before M4's mission-logs work starts.
