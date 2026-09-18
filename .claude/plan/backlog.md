# Backlog — small follow-ups recorded by the orchestrator

Items too small for a brief of their own, or waiting for a milestone. Remove a line when it lands.

- **The e2e launcher does not wait for the server it stopped.** `e2e/scripts/start-server.mjs` answers `SIGTERM` by killing the child, removing the scratch directory and calling `process.exit(130)` at once, so the server checkpoints into a directory that has already gone. Playwright's `gracefulShutdown` is now 15 s (M2-11 item 5), which only helps once the launcher awaits the child's exit before cleaning up. (`e2e/scripts/start-server.mjs`.) Found by M2-11.
- **GeoChat bounce (`b-t-f-s`)** — brief `M1-08-chat-bounce.md` exists; launch when `stream/router.rs` is free.
- **`[web.public] plain_bind` is parsed and validated but never bound** — an `http-01` ACME deployment, and the `80 → 443` redirect design 03 §3 describes, both want a plaintext listener on it serving `pki::acme::http01_routes` and `301`ing everything else. (`web/server.rs`, `runtime.rs`.) Found by M2-10.
- **The ACME resolver and the `http-01` token map are process-wide statics** — both become ordinary handles once `services/mod.rs` can take a `Late<AcmeState>` slot. (`services/mod.rs`, `pki/acme/{mod,challenge}.rs`.) Found by M2-10.
- **No admin-UI panel for TLS** — `GET /api/v1/settings/tls` and `POST /api/v1/settings/tls/renew` exist and `TlsStatus::needs_attention()` is there for the banner; nothing in `rustak-ui` reads them, so a failed renewal is visible only in the audit log and the API. Found by M2-10.
- **Wildcard ACME names pass `--check` but no challenge rustak implements can validate one** — a wildcard needs `dns-01`. Either implement it or refuse a wildcard at `--check`. (`config/acme.rs`, `pki/acme/order.rs`.) Found by M2-10.
- **OAuth2 authorize flow and `/login/*` OIDC federation** for CloudTAK SSO — M5 (design 03 M5.1–M5.5).
- **`interop/cloudtak` compose nightly** — M4 gate (plan → Verification).
- **Config-package UI** — the server has `POST /api/v1/config-packages` (M3-02); the admin UI has no page for it yet, nor for packages/profiles/clients/CoT browser (design 04 §8.3).
- **Local toolchain is older than CI's stable** and this machine cannot reach static.rust-lang.org; CI is the authority for new clippy lints until that changes.

## Admin API polish found by the operations UI (M3-04) — bundle into one brief once M3-03 has landed
- `GET /api/v1/missions?include_deleted=true` (the row renders `deleted_at` but nothing can list soft-deleted missions) and a `410` detail body that carries the deleted mission.
- `GET /api/v1/missions/{guid}` hard-codes `item_count: 0` per layer — one join.
- `POST /api/v1/clients/{uid}/incognito` should answer with the updated client, not the request.
- Report the stream listener's state (off vs quiet) so `GET /clients` = `[]` is unambiguous — a field on `GET /api/v1/health` or `/clients`.
- `GET /api/v1/cot` lacks the time window the per-uid history has.
- `ApiError::Gone`'s message is wizard-specific and now reachable from mission detail.
- Package `expiration` is epoch ms with negative-means-never; consider RFC 3339 nullable on `/api/v1`.
- Optional: a dry run for `POST /config-packages`.
