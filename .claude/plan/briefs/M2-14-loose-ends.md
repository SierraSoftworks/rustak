# M2-14 — Loose ends from the review fixes and the backlog

Small, independent items; each gets a test. Read `.claude/plan/conventions.md` and `.claude/plan/backlog.md`, then:

1. **Channel-cache invalidation hook**: call `LiveState::channels_changed()` from `identity/groups.rs` on create/rename/delete/bitpos change so the stream's `GroupCache` refreshes immediately rather than within a second (M1-10 follow-up). Test: create a group, route to it at once.
2. **Console renders a `410` body**: `rustak-ui`'s API client converts every `410` to `ApiError::Gone` before reading the body; the mission detail page should show the deleted mission summary the server now returns (M3-06 deviation 6). Add the two branches and a fixture.
3. **`bound_at` on `GET /api/v1/clients/status`**: record when the stream listener bound (in `stream/live.rs`/`services/mod.rs`) and expose it; UI shows it.
4. **e2e launcher shutdown**: `e2e/scripts/start-server.mjs` kills the child, removes the scratch directory and exits at once on SIGTERM without awaiting the child, so the server checkpoints into a deleted directory; await the child's exit (bounded by the server's budget + 2 s) before cleanup.
5. **Missing-UI fields for files-mode TLS**: the settings TLS card shows `cert_file`/`key_file`/`loaded_at`/`note` when `source = "files"` and `needs_attention()` covers the "waiting for the files" state (M2-13 backlog note).
6. Remove every backlog line you close.

**Files you own:** `rustak-server/src/identity/groups.rs`, `rustak-server/src/stream/live.rs` (additive), `rustak-server/src/services/mod.rs` (additive), `rustak-server/src/web/api/clients.rs`, `rustak-api/src/{client,settings}.rs`, `rustak-ui/**` (the files named), `e2e/scripts/start-server.mjs`, tests, `.claude/plan/backlog.md`, your status file. The CI steward edits tests/CI; no other implementation agent is running. No `git`/`but` writes; files < 300 functional lines. Exit checks: the standard set plus `cd rustak-ui && trunk build && cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings` and `cd e2e && npm run typecheck && npx playwright test` (with `RUSTAK_E2E_CHROMIUM="$HOME/Library/Caches/ms-playwright/chromium-1234/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"`). Status: `.claude/plan/status/M2-14-loose-ends.md`.
