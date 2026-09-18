# M3-06 — Admin API polish found by the operations UI

**Read first:** `.claude/plan/conventions.md`; `.claude/plan/backlog.md` → "Admin API polish found by the operations UI (M3-04)"; status `M3-03-admin-api-packages-clients-cot.md`, `M3-04-admin-ui-operations.md` (the "Endpoint gaps found" list and what the UI does today), `M4-02-missions-sync-notify.md` (`web/api/{missions,missions_view}.rs`), `M2-08-identity-api-gaps.md` (authz matrix test style).

**Deliver (all `/api/v1`, DTOs in `rustak-api`, tests per change, UI kept compiling):**
1. `GET /missions?include_deleted=true` and a `410` detail response that carries the deleted mission summary (`MissionDetail` with `deleted_at`), so the list can show soft-deleted missions and the detail page can render the outcome.
2. `GET /missions/{guid}` layer `item_count` from a real join.
3. `POST /clients/{uid}/incognito` answers the updated `ConnectedClient`.
4. Stream listener state: add `stream: { enabled, bound_at, connections }` to `GET /health` (or a small `GET /clients/status`), so `[]` from `/clients` is unambiguous; wire the UI's clients page to show "listener off" (you may edit `rustak-ui/src/pages/clients.rs` and its API client for this one item).
5. `GET /cot?secago|start|end` time window, same semantics as the per-uid history.
6. `ApiError::Gone` message made generic (the wizard-specific text moves to the wizard handlers).
7. Package `expiration` on `/api/v1` as RFC 3339 nullable (`null` = never) while the Marti surface keeps its epoch-ms/`-1` form; adjust `rustak-ui` package types accordingly (tiny).
8. Drop the dead `oauth_tokens` table from migration `0003` via a new migration `0015` (M5-01 finding) if nothing reads it — grep first.

**Files you own:** `rustak-server/src/web/api/{missions,missions_view,clients,cot,health,packages,error}.rs` (+ route lines in `web/api/mod.rs`), `rustak-api/src/{mission,client,cot,package,health}.rs` (+ re-exports), `rustak-server/migrations/0015_*.sql`, the minimal `rustak-ui` edits named above, the corresponding tests, `.claude/plan/backlog.md` (remove what you close), your status file. One other agent (M4-03) is editing `interop/cloudtak/**` and `nightly.yml`; the CI steward edits CI/e2e/interop and tests. No `git`/`but` writes; files < 300 functional lines. Exit checks: the standard set plus `cd rustak-ui && trunk build && cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings` and `cd e2e && npm run typecheck && npx playwright test` (with the `RUSTAK_E2E_CHROMIUM` path from M2-07). Status: `.claude/plan/status/M3-06-admin-api-polish.md`.
