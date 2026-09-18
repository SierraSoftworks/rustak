# M2-11 — Stream and harness tidy-ups from the backlog

Small, independent items from `.claude/plan/backlog.md`; each gets its own test. Read `.claude/plan/conventions.md` and the status files named per item.

1. **Account-level active channels table.** Replace M2-06's `kv`-backed account-level selection with a `user_group_state` table (migration `0012`), consulted at stream registration so a device enrolled after an account-level change routes correctly; keep the `PUT /Marti/api/groups/active` semantics (`marti/channels.rs`, `identity/members.rs`, `stream/resolver.rs`, `M2-06-marti-groups-contacts.md` finding 1).
2. **`[retention] cot_history_max_rows`** enforced through a `stream_segments` query (`cot_store/retention.rs`, `M1-05-stream-server.md`).
3. **Negotiation knob threading:** replace the process-wide `AtomicU8` with the value threaded through `Negotiation::new` (`M2-09-eud-interop-scenarios.md` has the 3-line patch; `config/stream.rs`, `stream/negotiation.rs`, `stream/{connection,mod}.rs`).
4. **node-tak / eud probe race:** the surface probe must wait for the "listener is bound" log line or retry the stream probe for up to 5 s (`interop/shared/src/probe.ts`, `interop/node-tak/src/surfaces.ts`, `interop/eud/src/surfaces.ts`).
5. **`main.rs` exit status** on a second signal (M0-19's one-liner) and `e2e/playwright.config.ts` `gracefulShutdown` raised above the server's budget.

**Files you own:** the ones named above plus `rustak-server/migrations/0012_*.sql`, `tests/stream_*.rs` additions, your status file. Other agents are editing `stream/router.rs`, `stream/dest.rs`, `stream/control.rs` (M1-08), `web/api/**`, `pki/acme/**`, `auth/oauth_server/**`; re-read shared files (`stream/mod.rs`, `config/stream.rs`) immediately before each edit. No `git`/`but` writes; files < 300 functional lines. Exit checks: the standard set plus `cd interop/node-tak && npm test` (25/25 or 24/25 with the mission-invitation skip) and `cd interop/eud && npm test`. Status: `.claude/plan/status/M2-11-stream-harness-tidy.md`.
