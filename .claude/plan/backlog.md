# Backlog — small follow-ups recorded by the orchestrator

Items too small for a brief of their own, or waiting for a milestone. Remove a line when it lands.

- **The e2e launcher does not wait for the server it stopped.** `e2e/scripts/start-server.mjs` answers `SIGTERM` by killing the child, removing the scratch directory and calling `process.exit(130)` at once, so the server checkpoints into a directory that has already gone. Playwright's `gracefulShutdown` is now 15 s (M2-11 item 5), which only helps once the launcher awaits the child's exit before cleaning up. (`e2e/scripts/start-server.mjs`.) Found by M2-11.
- **GeoChat bounce (`b-t-f-s`)** — brief `M1-08-chat-bounce.md` exists; launch when `stream/router.rs` is free.
- **ACME for the public listener** — config (`[acme]`) and the `acme_*` tables exist; no implementation brief yet (design 03 §ACME). Milestone M2 exit item.
- **OAuth2 authorize flow and `/login/*` OIDC federation** for CloudTAK SSO — M5 (design 03 M5.1–M5.5).
- **`interop/cloudtak` compose nightly** — M4 gate (plan → Verification).
- **Config-package UI** — the server has `POST /api/v1/config-packages` (M3-02); the admin UI has no page for it yet, nor for packages/profiles/clients/CoT browser (design 04 §8.3).
- **Local toolchain is older than CI's stable** and this machine cannot reach static.rust-lang.org; CI is the authority for new clippy lints until that changes.
