# M10-11 — The rate limiter's lockouts are visible

**Done.** Every lockout now remembers when it began and how many failures earned
it; refusals and lockouts are counted per class of key; administrators can list
what is locked out now (`GET /api/v1/auth/lockouts`) and clear one key at a time
(`POST /api/v1/auth/lockouts/clear`, audited); the console shows both on
**Settings → Security** as a fourth card, *Sign-in lockouts*. What earns a
lockout, the thresholds and the windows are unchanged.

## What changed, and why

### The limiter (`rustak-server/src/auth/ratelimit.rs` + `ratelimit/{class,view}.rs`)

- **Classes.** The key is `(address, subject)`, and the subjects that exist in
  the code fall into three classes, now `rustak_api::LockoutClass`:
  - `address` — an endpoint's own name, where only the address tells callers
    apart: `passkey`, `setup-token`, `auth-token`, `oauth-token`,
    `workload-identity`;
  - `client` — `oauth-client:<id>` from the code grant;
  - `account` — anything else, which is a username (Basic on enrolment routes,
    the password grant, a workload identity's named account).
  `class::classify` maps subject → (class, shown key); `class::subject_for` is
  the inverse and refuses a pair `classify` could never have produced (so
  "clear the account called `passkey`" cannot forgive every passkey ceremony
  from that address — tested at both layers).
- **The endpoint names now live in one place**, `ratelimit::subjects`, and the
  five callers' constants point at them (one-line edits, see *Files outside the
  brief*). The code grant's `oauth-client:` prefix is private to
  `auth/oauth_server/code_grant.rs` (M10-06's file), so it is duplicated as
  `class::CLIENT_PREFIX` with a comment; if the two ever diverged, a client
  lockout would be listed and counted as an account's — still listed, still
  clearable.
- **Counters.** `class::Counters`: relaxed `AtomicU64` arrays indexed by class —
  refusals (a `check` that answered `Err`) and lockouts started (a
  `record_failure` that returned `Some`). Labels are classes only.
- **A bucket's lockout is a `Lock { since, until, failures }`** instead of a bare
  `locked_until`, so the listing can say when it began and what earned it.
  Behaviour is identical: same condition to lock, same "reported once, not
  extended" rule, same sweep and eviction order.
- **`view.rs`**: `lockouts(limit)` — one pass under the mutex collecting
  references to active lockouts, `select_nth_unstable_by` to keep the newest
  `limit`, clone only those; never sweeps, inserts or touches a time.
  `clear(address, subject)` removes the bucket (failures included, as a success
  would) **only if it is locked out now**; otherwise `None`.
- Internal `*_at(now, …)` variants so tests inject the clock.

### Metrics — "exposed the way the server's other metrics are"

There is no metrics exporter in the server today: `StreamMetrics` is relaxed
atomics read in-process, and tracing-batteries' OpenTelemetry battery is built
without its meter provider (`rustak-core/src/telemetry.rs`). So the counters
follow `StreamMetrics` — atomics — and are exposed in the admin response
(`counters`, `counting_since`). If a meter provider is switched on later, an
observable counter over `Counters::snapshot()` with a `class` attribute is a
few lines.

### One limiter per process, not per listener (`services/mod.rs`, `web/server.rs`)

`build_public` and `build_marti` each built **their own** `RateLimiter`, though
`marti_services`' comment said the limiter was shared "so an attacker cannot
double their allowance by alternating ports". It was not shared, and an admin
endpoint on `[web.public]` would never have seen lockouts earned on `:8443` —
which is exactly where the M9-14 workload-identity lockout happened. The
limiter now lives on `AppContext` (`rate_limiter()`), both listeners and
`TestServer` take it from there, and the admin endpoint reads it. This does
change behaviour in one respect: the allowance is now per process rather than
per listener, which is what the code always claimed.

### The endpoints (`rustak-server/src/web/api/lockouts.rs`)

- `GET /api/v1/auth/lockouts` → `Lockouts { lockouts, total, counters,
  counting_since }`, newest first, bounded at `MAX_LISTED_LOCKOUTS = 200`.
  `Administrative` extractor, behind the same bearer gate as its neighbours;
  that gate is not rate limited, so an administrator already signed in can read
  it from an address that is itself locked out. Logged at `debug` only.
- `POST /api/v1/auth/lockouts/clear` with `ClearLockoutRequest { class, address,
  key }` → `200` with the cleared `Lockout`; `400` for a class/key pair that
  cannot exist; `404` when nothing is locked out under it. Audited as
  `lockout.cleared`, category `administration`, actor = the administrator,
  subject = the account/client (or the address for an `address` lockout), detail
  = class, key, address, failures, started/ends. One `info` line naming the
  class and actor, not the key.
- Both added to the `PROTECTED` route table (401 without a session).

### UI (`rustak-ui`)

- `pages/settings_lockouts.rs`: `LockoutsCard` on the Security page (admin-only
  route) — rows in the existing `entity-row` style (`.lockout-list` /
  `.lockout-row` in `styles.scss`, and added to the narrow-screen single-column
  list), each with a `StatusPill` for the failure count and a `ConfirmButton`
  ("Clear" → names the key and address → "Clear it"); an `EmptyState` when
  nothing is locked out; "Showing the N most recent of M" when truncated; a
  one-line counter summary.
- `api/lockouts.rs` + `fixtures/lockouts.rs` (one lockout of each class, RFC 5737
  addresses; clearing removes it, clearing again is the server's 404 sentence).
- e2e: two tests appended to `e2e/tests/settings.spec.ts` (the neighbouring
  page's spec) — the real server says "Nothing is locked out." to an
  administrator, and the demo card lists three rows, asks before clearing, and
  drops the cleared row.

### Docs

`docs/deployment.md` — new **Sign-in lockouts** section (what a lockout is, the
three classes, the in-memory/shared limiter, how to see and clear one in the
console and with `curl`, the 200-row bound, what is audited and logged, and
what the counters tell you). `config.example.toml` — one sentence under
`rate_limit` pointing at the console and endpoint.

## Tests, and how each behaves on a host ten times slower

None measures elapsed time; the lockout durations are 15 minutes and the clock
is injected wherever a test reasons about time.

- `auth::ratelimit` — **the existing flood test no longer asserts `< 5 s`**
  (a host-dependent upper bound the wave rules forbid, in a file this brief
  owns). It now counts full-map scans (`Buckets::prunes`) across ten thousand
  checks at one injected instant and asserts at most one — the property the
  timing was a proxy for. `a_window_that_has_run_out_starts_counting_again` no
  longer sleeps 60 ms against a 30 ms window; it injects `start` and
  `start + 2m`. New: refusals and lockouts are counted by class and never by
  key. A slower host changes none of these.
- `auth::ratelimit::class` — classification, round trip, refused impossible
  pairs, counters per class. Pure.
- `auth::ratelimit::view` — listed with begin/end/failures; reading neither
  extends, ends, creates a lockout nor counts as a refusal; an expired lockout
  is not listed (`at + 16m`, injected); bounded newest-first with `total`;
  clearing forgives key and failures; clearing touches one key and only a
  locked one. All at injected instants.
- `web::api::lockouts` — contract: `200`, `application/json`, JSON shape
  (`total`, `counters` ×3, `counting_since`, a lockout's `class`/`address`/
  `failures`/timestamps), typed round trip; `403` on both routes for a
  non-administrator and the lockout survives; clear → `200`, only that key
  forgiven, audit entry with actor/class/address; `404` for nothing locked,
  `400` for an account called `passkey`. Driven by `record_failure` on the
  shared limiter — counts, not time.
- `web::api::tests` — the two new routes answer `401` without a session.
- `rustak-api::lockout` — wire names and a serialised `Lockout`.
- `rustak-ui` — `settings_lockouts` (descriptions, the confirmation sentence,
  the counter summary for zero and non-zero) and `fixtures::lockouts` (one per
  class; cleared once, refused the second time). Pure functions.
- e2e — Playwright's own generous waits; assertions are on content and row
  counts.

## Exit checks

Run in this worktree after the last edit:

| Check | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | clean |
| `./scripts/check-file-length.sh` | clean — and, because it only sees tracked files, the new files were measured by the same awk by hand: largest is `settings_lockouts.rs` at 189, `services/mod.rs` is 241 |
| `cargo test -p rustak-api` | 167 passed, 0 failed |
| `cargo test -p rustak-server --no-fail-fast` | exit 0. The lib binary's summary line was lost from the captured log, so `--lib` was re-run alone: 2031 passed, 0 failed, 2 ignored. The 26 other result blocks (integration binaries + doctests): 289 passed, 0 failed |

UI (the `ui` job's steps). **Note:** cargo refuses to build `rustak-ui` inside
this worktree — it is nested under the main checkout, whose `Cargo.toml`
becomes the enclosing workspace once the worktree's own root excludes
`rustak-ui`. So `rustak-api`, `rustak-ui` and `docs/` were copied to the
session scratchpad (outside any repository) and built there; the files are
byte-identical to the worktree's (`rsync` immediately before each run).

| Check | Result |
|---|---|
| `cargo fmt --all --check` (rustak-ui) | clean |
| `cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings` | clean |
| `cargo test` (rustak-ui) | 155 passed |
| `trunk build` | success |
| `npm run typecheck` (e2e) | clean |
| `npx playwright test` (full suite, against a `rustak` built with that bundle) | 70 passed |

## Files

New:
- `rustak-api/src/lockout.rs`
- `rustak-server/src/auth/ratelimit/class.rs`
- `rustak-server/src/auth/ratelimit/view.rs`
- `rustak-server/src/web/api/lockouts.rs`
- `rustak-ui/src/api/lockouts.rs`
- `rustak-ui/src/fixtures/lockouts.rs`
- `rustak-ui/src/pages/settings_lockouts.rs`
- `.claude/plan/status/M10-11-limiter-visibility.md`

Changed (in the brief):
- `rustak-api/src/lib.rs` — module and re-exports
- `rustak-server/src/auth/ratelimit.rs`
- `rustak-server/src/web/api/mod.rs` — module, routes, `PROTECTED` entries
- `rustak-ui/src/api/mod.rs`, `rustak-ui/src/fixtures/mod.rs`,
  `rustak-ui/src/pages/mod.rs`, `rustak-ui/src/pages/settings_security.rs`,
  `rustak-ui/styles.scss`
- `e2e/tests/settings.spec.ts`
- `docs/deployment.md`, `config.example.toml`

Changed outside the brief's list (smallest change each):
- `rustak-server/src/services/mod.rs` — `AppContext` owns the limiter
  (`rate_limiter()`); needed so both listeners and the admin endpoint share one.
- `rustak-server/src/web/server.rs` — both listeners take `context.rate_limiter()`
  instead of building their own.
- `rustak-server/src/testing/context.rs` — `TestServer.limiter` is the context's.
- `rustak-server/src/web/api/{passkey,auth,setup}.rs`,
  `rustak-server/src/marti/oauth.rs`, `rustak-server/src/auth/workload/mod.rs` —
  each subject constant now points at `ratelimit::subjects::*` (same strings).

Not touched: `auth/oauth_server/**` (M10-06), `auth/cert.rs` (M10-10),
`stream/**`.

## Open

- `auth/oauth_server/code_grant.rs`'s private `CLIENT_SUBJECT_PREFIX` should
  become `crate::auth::ratelimit::CLIENT_PREFIX` once M10-06 has landed (one
  line; left alone because that file is M10-06's).
- `rustak-server/tests/*.rs` (six integration harnesses) still build their own
  `RateLimiter::new(...)` beside the context's. Harmless — none reads the admin
  endpoint — but they could take `context.rate_limiter()` for consistency.
- The limiter's per-address key still lets a distributed guesser through
  (R-01 L1); the new `account` counters make that visible, they do not stop it.
- No metrics exporter exists; see *Metrics* above.
