# Backlog — small follow-ups recorded by the orchestrator

Items too small for a brief of their own, or waiting for a milestone. Remove a line when it lands.

- **node-tak `stream.test.ts` probe race** — the surface probe can run before the CoT listener's bind log line; the scenario then skips intermittently. Make the probe wait for the "listener is bound" line (or retry the stream probe for a few seconds). (`interop/shared/src/probe.ts`, `interop/node-tak/src/surfaces.ts`.) Found by M4-01.
- **Account-level active channels need a table** — M2-06 stores the account-level selection in `kv`; a device enrolled after an account-level change routes permissively until it calls `PUT /groups/active`. Add `user_group_state` (migration) and consult it at registration. (`marti/channels.rs`, `identity/members.rs`, `stream/resolver.rs`.)
- **`[retention] cot_history_max_rows` is not enforced** (age only) — needs a `stream_segments` query. (`cot_store/retention.rs`.) Found by M1-05.
- **`main.rs` exit code on a second signal** is now 0; M0-19's status file has the one-line change if a distinguishing status is wanted.
- **e2e `gracefulShutdown` (5 s) is shorter than the server's 8 s + 2 s budget**; harmless while the e2e config is plaintext. (`e2e/playwright.config.ts`.)
- **`stream/negotiation.rs` knob is published through a process-wide `AtomicU8`**; M2-09's status file has the 3-line patch to thread it through `Negotiation::new` once `stream/{connection,mod}.rs` are free.
- **GeoChat bounce (`b-t-f-s`)** — brief `M1-08-chat-bounce.md` exists; launch when `stream/router.rs` is free.
- **ACME for the public listener** — config (`[acme]`) and the `acme_*` tables exist; no implementation brief yet (design 03 §ACME). Milestone M2 exit item.
- **OAuth2 authorize flow and `/login/*` OIDC federation** for CloudTAK SSO — M5 (design 03 M5.1–M5.5).
- **`interop/cloudtak` compose nightly** — M4 gate (plan → Verification).
- **Config-package UI** — the server has `POST /api/v1/config-packages` (M3-02); the admin UI has no page for it yet, nor for packages/profiles/clients/CoT browser (design 04 §8.3).
- **Local toolchain is older than CI's stable** and this machine cannot reach static.rust-lang.org; CI is the authority for new clippy lints until that changes.
