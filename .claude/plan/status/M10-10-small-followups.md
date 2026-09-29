# M10-10 status

## Changed
1. `rustak-server/src/auth/cert.rs`: `auth.resolve.cert` no longer uses `err(Debug)`. `client_cert` now wraps a private `resolve` and calls `log_failure` on `Err`, once per failure, at the level `level_of` gives: `Rejected` and `RateLimited` -> DEBUG, `Forbidden` (subject/row mismatch) -> WARN, `Unavailable` -> ERROR. The span carries the fingerprint; only a cause string is logged, no certificate material. The per-site `debug!`/`warn!` calls were removed so nothing logs twice.
2. `docs/plugins.md`: one paragraph at the end of "Testing one" covering `stream_stats()` (`published`, `discarded`, `discarded_before_first_connection`) and `with_first_connect_hold` (default 10s, why tests set it long).

## Decisions
- Tests exercise the `level_of` mapping (one unit test, all variants); no event capture was used.
- The old `warn!` for a certificate row with no account is now DEBUG (`Rejected`), since it is a refusal like the others; `AuthFailure` has no variant that distinguishes it. Flag if you want it kept at WARN.
- Central logging loses the per-site details (`revoked` flag, username). The cause is a coarse string.

## Files
- rustak-server/src/auth/cert.rs
- docs/plugins.md
- .claude/plan/status/M10-10-small-followups.md

## Slow host
The new test is pure and has no timing.

## Orchestrator's change at integration (2026-09-29)

The agent's `cert.rs` moved every log line into one wrapper and so lost what the
sites knew: the `revoked` flag, the username of a disabled account, the subject
and account of a mismatch, and the `warn` for a certificate row with no account
(it maps to `Rejected`, which the wrapper logged at `debug`). What landed is
narrower: the per-site lines stay exactly as they were on main, `err(Debug)`
goes, and a wrapper (`log_unsaid`) logs only what no site announces —
`Unavailable` at `error`, `RateLimited` at `debug`. `level_of` and its test were
not taken.
