# D-02 — Manual compatibility checklists for ATAK, WinTAK, iTAK and CloudTAK

**Status: complete.** Four manual checklists in `docs/compat/`, one linking sentence each in
`docs/interop.md` and `README.md`. Markdown only; no `git`/`but` write was made.

## What landed

| File | Lines | Covers |
|---|---:|---|
| `docs/compat/atak.md` | 197 | QR enrolment, config-package import (cross-referenced to `profiles.md`), Channels UI, chat (incl. the `b-t-f-s` bounce), peer file-share, Data Sync (create/subscribe/marker/file/log), device-profile application (cross-referenced), disconnect/reconnect, revocation, known gaps |
| `docs/compat/wintak.md` | 147 | The same sections, run against WinTAK; server-side behaviour stated as fact (it doesn't depend on the client), client-visible behaviour marked **to confirm** throughout, since WinTAK is closed-source and outside this project's research corpus |
| `docs/compat/itak.md` | 73 | The one iTAK path rustak actively builds (the `itak` config-package variant, cross-referenced to `profiles.md` §5's existing unverified-keystore-path note) as the only real gate; everything else listed as **to confirm, if attempted**, matching `plan.md`'s "best-effort only" scope decision |
| `docs/compat/cloudtak.md` | 167 | Server configuration/first login (CloudTAK's three-step `PATCH /api/server`), Channels UI, chat, Data Sync (marker/file/log/package) with an explicit "run the automated `interop/cloudtak` suite first" framing, disconnect/reconnect (CloudTAK's own re-subscribe-on-reconnect behaviour), revocation, known gaps |

Modified: `docs/interop.md` (one sentence, linking the ATAK-UI-gap paragraph to `docs/compat/`) and
`README.md` (one bullet in the Documentation list). Nothing else was touched.

## Facts vs. "to confirm"

Every checklist step is backed by a citation at the bottom of its file (`.claude/plan/research/`,
`.claude/plan/compat/*.md`, or a specific `status/` note). Anything without a verified source is
marked **to confirm** rather than asserted:

- **ATAK**: fully verified — `research/07-atak-client-verified.md` reads ATAK-CIV's source directly.
  Two items are marked to confirm even here because the source checkout doesn't cover them: whether
  ATAK's Data Sync plugin auto-resubscribes on reconnect, and (inherited from `profiles.md`) the
  iTAK/WinTAK keystore-path question doesn't apply to ATAK itself.
- **WinTAK**: nothing was read from WinTAK source (closed-source, not in the research corpus). Steps
  that are genuinely server-side and don't depend on which client is connected are stated as fact and
  cross-referenced to the ATAK checklist; every client-visible behaviour (trust prompts, preference
  names, UI reactions) is marked to confirm.
- **iTAK**: matches `plan.md`'s explicit "best-effort only" scope decision. Only the config-package
  variant is a real, already-partly-verified gate (`profiles.md` §5); everything else is listed as
  unattempted rather than guessed at.
- **CloudTAK**: mostly verified via `research/03-cloudtak-node-tak-contract.md` and the `interop/
  cloudtak` suite's own design decisions (`status/M4-03-interop-cloudtak-compose.md`). Two items are
  to confirm because they depend on CloudTAK's own UI reactivity, which wasn't read from source in
  the same depth as its API client: its reaction to `t-x-g-c`/`t-x-m-d`, and whether it surfaces a
  `b-t-f-s` chat bounce.

## Decisions worth recording

1. **Cross-reference `profiles.md` rather than duplicate it.** `profiles.md` (M3-02) already owns the
   device-profile and config-package manual gate in detail, including the exact preference-import
   traps and the iTAK keystore-path open question. `atak.md`/`wintak.md`/`itak.md` point at its
   specific sections instead of re-deriving the same checklist, so a future correction to that gate
   only needs to happen in one place. This does mean a full run of `atak.md` includes reading a
   second file (§2 and §7 both defer to it) — noted explicitly in each place so it isn't missed.
2. **WinTAK and iTAK are structured as deltas over `atak.md`, not standalone documents.** Most of the
   wire contract is server-side and identical regardless of which client connects (routing,
   notifications, revocation-at-handshake); duplicating the full ATAK prose for two clients this
   project has never read source for would have produced text that reads as more verified than it is.
   Both files instead say "run atak.md §N with this client" and mark only the client-specific parts as
   open questions.
3. **CloudTAK's page leads with "run the automated suite first."** Unlike the other three clients,
   CloudTAK already has a full-stack nightly suite (`interop/cloudtak`) that asserts the same Data
   Sync round trip this checklist walks through by hand. The page is framed around what that suite
   cannot see (rendering, operator-visible errors, CloudTAK's own UI reactivity) rather than repeating
   its assertions, per the brief's instruction not to re-litigate what M4-03 already proves.
4. **Known-gaps sections stay narrow.** Per the brief, each "Known gaps" section lists what rustak
   *deliberately* does not do (video, federation, QUIC, ExCheck, plaintext/anonymous streaming) — the
   now-fixed chat-bounce gap from `status/M2-09-eud-interop-scenarios.md` (resolved by M1-08) is
   **not** listed as a gap, since it no longer is one; it's cited instead as a regression check inside
   the Chat section of `atak.md`.

## Exit checks

Markdown only; no build/test/lint tooling applies. Verified by reading each new file back in full
after writing it, and by grepping the admin-UI route table (`rustak-ui/src/app.rs`) and page headings
to confirm every UI reference in these checklists (`/admin/devices` "Devices", `/admin/users/
{username}` "Account", `/admin/groups` "Channels", `/admin/missions` "Missions", `/admin/packages`
"Data packages", `/admin/clients` "Clients", `/admin/activity` "Activity" with its ten
`AuditCategory` values) names a route and a label that actually exist in `rustak-ui/src/app.rs` and
`rustak-api/src/audit.rs` today, rather than a guessed-at page name.

```
$ grep -c '^## ' docs/compat/atak.md docs/compat/wintak.md docs/compat/itak.md docs/compat/cloudtak.md
docs/compat/itak.md:5
docs/compat/cloudtak.md:9
docs/compat/wintak.md:12
docs/compat/atak.md:12

$ grep -n 'docs/compat' docs/interop.md README.md
docs/interop.md:18:checklist — see [`docs/compat/atak.md`](compat/atak.md), and its WinTAK,
README.md:152:- [`docs/compat/`](docs/compat/) — manual compatibility checklists for ATAK, WinTAK, iTAK and
```

## Verified in

- `.claude/plan/research/07-atak-client-verified.md`, `03-cloudtak-node-tak-contract.md` —
  authoritative client-behaviour sources.
- `.claude/plan/compat/{streaming,enrollment,groups,missions,files,profiles,oauth,cloudtak}.md` — the
  wire contracts every expected outcome traces back to.
- `.claude/plan/status/{M1-08-chat-bounce,M2-09-eud-interop-scenarios,M4-03-interop-cloudtak-compose}.md`
  — what the automated suites already prove, cited to avoid duplicating their coverage.
- `docs/compat/profiles.md` — the existing manual-checklist style this brief was asked to match, and
  the file the new checklists cross-reference rather than duplicate.
- `rustak-ui/src/app.rs`, `rustak-api/src/audit.rs` — the admin UI routes, page headings and audit
  categories cited as "evidence to look for" in every checklist.
