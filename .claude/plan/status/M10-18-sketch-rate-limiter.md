# M10-18 — A rate limiter with fixed memory and no contended lock

**Done.** The sign-in limiter is now two tiers of count-min sketch (tier 1 the
address, tier 2 the address-and-subject pair), atomic cells packed for a single
compare-and-swap, conservative update proved and tested never to undercount
under concurrency, a display-only ring of lockout events for the console, and
the M10-11 endpoints, API types, console card and e2e spec adapted. The map,
its ceiling, its sweep and its eviction are gone.

## Revision (2026-10-01): tier-1 defaults raised to 300 / 3 000

**Decision (maintainer, via the orchestrator).** Because of the CloudTAK case
(one address fronting many users), the tier-1 defaults are
`address_attempts = 300` and `network_attempts = 3000`, not 30 and 300. The
pair (`attempts = 10`) is unchanged. Figures elsewhere in this note that say 30
or 300 describe the first round.

**What changed.**
- `config/auth.rs` defaults and the defaults test; `config/validate.rs`
  advice; `config.example.toml`; `docs/deployment.md` "Sign-in lockouts" (the
  table, the one-address bound, the proxy and CloudTAK exposure at 300, the
  flood arithmetic); the `ratelimit.rs` module docs; the `sketch.rs` sizing
  argument; the demo fixture's `source` estimate (30 → 300). The console names
  no default.
- Tests no longer lean on the default. `ratelimit.rs`'s helper sets 20 / 60;
  `view.rs` sets 12 / 36 and loops to 12; `web/api/lockouts.rs` starts every
  server with `attempts = 3`, `address_attempts = 8`, `network_attempts = 24`.
  The /64-and-/48 test uses 4 / 12. Only `config::auth`'s defaults test pins
  300 / 3 000. The "one address cannot do it alone" test writes out 300 / 10
  and now asserts 300 failures, one address lockout and at most 30 account
  lockouts.
- `workload_support`'s comment: its explicit 10 000 still covers it
  (`the_refusals` makes 11–29). Nothing else relied on reaching the address
  lock at 30: the whole suite passes at the new defaults, and e2e and node-tak
  passed even at an allowance of 2 in the first round.

**What the new defaults weaken.**
- One address now locks up to about **30** pair keys per lockout period
  before tier 1 stops it, not 3. One IPv6 /48 locks about **300**.
- The flood needed for false refusals is ten times cheaper.
  16 384 lockouts in force (0.24 % false refusals) takes about 550 IPv4
  addresses or 55 /48s within one lockout. 65 536 lockouts (16 %) takes about
  2 200 addresses or 220 /48s. An IPv6 /32 holds 65 536 /48s.
- Behind a misconfigured proxy, or for CloudTAK, one script still locks out
  everybody. It now takes 300 failures a minute instead of 30.
  Ten failures against one account still lock that account out of CloudTAK for
  everybody, because the pair is (CloudTAK's address, account). Tier 1 never
  addressed that, and the new default does not change it.

**Checks this round.**
- `cargo fmt --check`, clippy `-D warnings`, `cargo doc -D warnings` and
  `check-file-length.sh` all pass.
- `cargo test -p rustak-api -p rustak-server`: 41 result lines, 2 650 passed,
  0 failed.
- UI: fmt, wasm clippy and `cargo test` (167 passed) pass, and `trunk build`
  succeeds.
- `e2e/tests/settings.spec.ts` against a server built with that bundle: 6 passed.

## Where the brief was not met exactly

1. **"No lock on any request path" — the event ring uses per-slot `try_lock`.**
   Decisions touch only atomics. But when a lockout *begins* (not on a check,
   not on an ordinary failure) the event is written into a ring slot guarded by
   a `parking_lot::Mutex`, taken with `try_lock` only: a busy slot is skipped,
   after four busy slots the event is dropped and counted. A writer never waits.
   A lock-free seqlock over atomic words would remove the mutex, at the cost of
   a hand-rolled protocol; I judged the non-blocking `try_lock` the simpler
   correct construction for display-only data. Losing an event can hide a
   lockout from the listing, never change a decision.
2. **Allowances are 16-bit.** Cells hold `until:32 | count:16 | window:16`; an
   `attempts`/`address_attempts`/`network_attempts` above 65 535 behaves as
   65 535 (documented in `RateLimitConfig`, `config.example.toml`, the docs).
   No configuration in the repository sets more than 10 000.
3. **Windows are a fixed grid from the limiter's start**, not one per key (a
   sketch has no per-key state for "first failure"). Worst case, a burst
   straddling a boundary lands `2 × (allowance − 1)` before locking — the old
   per-key window allowed the same at its own boundary. Chosen because it makes
   "never undercounts within a window" provable.
4. **A lockout racing at the threshold may be reported twice.** Two failures of
   one key completing concurrently at its threshold can both see it unlocked
   and both report a lockout (counter +2, two ring events — the listing
   de-duplicates by key). Never zero. No production caller uses
   `record_failure`'s return value.

## What changed, and why

### The sketch (`rustak-server/src/auth/ratelimit/`)

- `sketch.rs` — `Table`: `DEPTH = 4` rows × `WIDTH = 2¹⁶` `AtomicU64` cells,
  allocated once and zeroed (2 MiB a tier). Cell = `until:32 | count:16 |
  window:16`. Row `i`'s column is `(h₁ + i·h₂) mod WIDTH` from one 64-bit hash
  (Kirsch–Mitzenmacher). A cell from an older window reads as zero and is
  restarted by the next write (windows run out per cell, nothing sweeps); one up
  to two windows *ahead* of the writer is counted in its own window rather than
  wound back. `record` is conservative update with a whole-update restart when
  any CAS fails; everything is `SeqCst`. The rustdoc states the property and
  the proof (completed updates read distinct least counts `m` — a common
  min-cell can only be swapped from count `m` once, and disjoint min-sets would
  need a cycle that one `SeqCst` order forbids — so `N` completed updates leave
  every cell ≥ `N`). `locked_until` = least `until` across the key's cells, so a
  key is refused only when every cell is locked (Stochastic Fair Blue).
  `locked_sample` reads a fixed stride of 4 096 cells for the console.
- `key.rs` — the address: IPv4 /32, IPv6 /64 and /48, IPv4-mapped → IPv4, no
  address → a key of its own; the subject folded as `Username::parse` folds
  (trim, per-character lower case) and streamed into the hash, so no
  allocation; `ShownKey` keeps the first 96 bytes on the stack for display.
  SipHash-2-4 (`siphasher` crate) under a 128-bit key from `rand::random`
  (OS-seeded CSPRNG), never logged or in `Debug`; domain byte per kind (host
  /32-or-/64, network /48, pair).
- `clock.rs` — whole seconds since the limiter's epoch; window index mod 2¹⁶;
  lockout rounded up to a whole second.
- `events.rs` — the ring (256 slots, newest first, display only); `Counted` is a
  key with its tier, hash, class, address and shown subject.
- `ratelimit.rs` — `RateLimiter`: `check` (tier 1 at each level, tier 2;
  `Err` with the longest remaining), `record_failure`, new
  `record_failures(address, &[subjects])` (one attempt guessing at several
  subjects: address counted once), `record_success` now a documented no-op,
  `footprint()`. Hand-written `Debug` without the key or cells. Module docs
  replace "Why the sweep is on a clock and the map has a ceiling" with "Why
  there is no map", plus "A success forgives nothing".
- `view.rs` — `lockouts`: events not cleared, de-duplicated by key, kept only if
  the sketch still refuses that key, `ends_at` from the sketch; plus
  `tiers: [TierFill]`. `clear(class, address, key)` → `Unclearable::{NotAKey,
  NotLocked}`; zeroes the key's cells, marks its events cleared; a key whose
  event has left the ring is still cleared and described (started = until −
  lockout).
- `class.rs` — `subject_for(Source, _)` is `None`; docs.

### Configuration (`config/auth.rs`, `config/validate.rs`, `config.example.toml`)

`attempts`/`window`/`lockout` keep meaning and defaults and govern tier 2. New
`address_attempts = 30` and `network_attempts = 300`, same window and lockout;
`deny_unknown_fields` kept; `validate.rs` refuses any of the three at 0.

### API and console

- `rustak-api/src/lockout.rs`: `LockoutClass::Source` (wire `source`, appended
  so existing indices hold); `Lockout.prefix: Option<u8>` (serde default);
  `failures` documented as the estimate; `LockoutTier`, `TierFill { tier,
  sampled, locked, rows }` with `fraction()` and `false_refusal()`;
  `Lockouts.tiers` (serde default); `ClearedLockout { #[flatten] lockout, note }`
  and `CLEARED_NOTE` — the clear response is still readable as a `Lockout`.
- `web/api/lockouts.rs`: clear maps `Unclearable` to 400/404, answers
  `ClearedLockout` with the note that sharing keys are forgiven too; audit
  subject for a `source` lockout is its CIDR key; audit detail gains `prefix`.
- `rustak-ui`: card says "about N failures" (title explains the estimate),
  describes a tier-1 lockout as "Every sign-in from …", shows IPv6 prefixes,
  the confirmation says keys sharing all cells are forgiven too, and a note
  gives each tier's locked fraction and the false-refusal odds per million.
  Fixtures add one `source` lockout and fill figures. No `styles.scss` change
  was needed (`panel-note` already exists).
- `e2e/tests/settings.spec.ts`: four demo rows, new counter totals, the
  `Every sign-in` row, the estimates note and a tier's fill.

### Callers

Only `auth/workload/mod.rs` changed: its account-step refusal called
`record_failure` twice (endpoint subject and account), which would count the
address twice; it now calls `record_failures(address, &[RATE_LIMIT_SUBJECT,
account])` (item 3). Every other call site is unchanged.

## Item 7 — the address the limiter sees

`trust_proxy` **is** honoured on the way to the limiter. Every call site
(`auth/resolve.rs` Basic + workload, `auth/oauth_server/code_grant.rs`,
`auth/workload/grant.rs`, `web/api/{passkey,me,auth,setup}.rs`,
`marti/oauth.rs`) computes the address with
`web::helpers::request::client_address(config.server.trust_proxy, headers,
peer_addr)`. Nothing new was added. What a deployment should know (now in
`docs/deployment.md` and `config.example.toml`):

- behind a reverse proxy with `trust_proxy = false`, every caller is the proxy,
  and 30 failures a minute from anyone lock out everyone for 15 minutes — set
  `trust_proxy = true`;
- `client_address` believes the **left-most** `X-Forwarded-For` entry from any
  peer when `trust_proxy` is on (there is no trusted-proxy list), so it is only
  safe behind a proxy that overwrites the header; behind one that appends, a
  client picks its own address and walks around both tiers;
- an `X-Forwarded-For` entry that does not parse as a bare IP (e.g. one with a
  port, as some load balancers append) falls back to the peer — the proxy — so
  those requests share one tier-1 key;
- **CloudTAK** (and any server-side client that signs users in from its own
  address) puts all of its users in one tier-1 key: 30 failed CloudTAK logins a
  minute, against any accounts, lock out every CloudTAK login for 15 minutes.
  Documented with the advice to raise `address_attempts` for such deployments.

## Item 4 — what relied on a success forgiving

- Tests: only `auth::ratelimit::tests::a_success_forgets_what_came_before_it`,
  rewritten as `a_success_no_longer_forgives_the_failures_before_it`. The whole
  `rustak-server` suite (lib and the 38 other binaries) passed otherwise, so no
  other test depended on it.
- Behaviours: every `record_success` site (passkey ×3, setup, auth-token ×2,
  me, Basic in `resolve.rs`, password grant, code grant, workload ×2). The
  noticeable ones: the passkey subject is one key per address for all four
  ceremonies (R-01 L2), so an office NAT whose users cancel prompts now
  accumulates toward 10/min across successes; a client retrying a stale
  refresh token between good ones likewise.

## Item 9 — harnesses and suites

Measured by lowering the tier-1 allowance until something broke:

| Suite | Result | Change |
|---|---|---|
| `TestServer` users (lib unit tests, `tests/*.rs`) | whole suite passes at the defaults (`actix_web::test` requests carry no peer, so each server's tests share the "unknown" key) | none |
| `workload_support` (`tests/workload_identity.rs`) | passes at the default 30, **fails at 11**: `the_refusals` makes 11–29 counted failures from 127.0.0.1 | `address_attempts`/`network_attempts = 10_000` beside M10-07's `attempts` |
| `stream_support`, `feed_support` | no credential failures | none |
| e2e (`start-server.mjs`) | 77/77 at the defaults **and** at `address_attempts = 2` | none |
| `interop/node-tak` | 35/35 at the defaults and at 2; 6 fail at 1 (so it makes exactly one counted failure) | none |
| `interop/cloudtak`, `interop/eud` (nightly) | **not run** (Docker, ATAK build). No deliberate credential failures found reading their sources; only the nightly can prove it. If they trip tier 1, the place to set it is `interop/shared/src/launch.ts` `defaults()` as an `"auth.rate_limit"` table | none |

## Simulation (`auth::ratelimit::simulation_tests`)

Production geometry (4 × 65 536), fixed hash keys, one window, 50 000 fresh
probe keys per row. "Refused" locks `K` keys (10 failures each) and asks if a
never-failed key is refused; "one failure" fails `K` keys 9 times each (no
lockouts) in a second sketch with its own hash key and asks whether one failure
would lock a stranger.

| K | K / WIDTH | (1 − e^(−K/W))^D | refused, K locked | locked by one failure, K at nine |
|---|---|---|---|---|
| 100 | 0.0015 | 0.000000 | 0.000000 (0) | 0.000000 (0) |
| 1000 | 0.0153 | 0.000000 | 0.000000 (0) | 0.000000 (0) |
| 16384 | 0.2500 | 0.002394 | 0.002620 (131) | 0.002880 (144) |
| 32768 | 0.5000 | 0.023969 | 0.024380 (1219) | 0.024600 (1230) |
| 65536 | 1.0000 | 0.159661 | 0.157180 (7859) | 0.161200 (8060) |
| 131072 | 2.0000 | 0.558973 | 0.556420 (27821) | 0.562760 (28138) |

Asserted: zero at ≤ 1 000; within 3σ + 10 % of the formula above. Why 2¹⁶: a
real installation has a handful of lockouts; 0.24 % needs 16 384 in force at
once, which tier 1 makes cost thousands of addresses; doubling the width halves
`K/W` for another 2 MiB a tier and buys only the flood case.

## Tests, and a host ten times slower

Nothing measures elapsed time; every time is injected (`*_at`) or fixed.

- `sketch` (9): pack/unpack, slots in their rows, one key counts exactly,
  refused only when every cell is locked (and by the least `until`), threshold
  locks every cell once and is not extended, window rollover per cell keeps the
  lock, a cell ahead is not wound back, clear, the sample.
- `key` (8): /64 and /48 grouping, IPv4-mapped, no address, shown/parse round
  trip and refusals, case folding, domain separation, key dependence, a long
  subject cut at a character.
- `clock` (3), `events` (4: bounded newest-first, busy slot skipped, all-busy
  dropped and counted, clear marks all and answers newest), `class` (+1 assert).
- `ratelimit` (16): lockout on the crossing attempt; reported once, not
  extended; runs out at `+900 s`; other accounts/addresses unaffected; missing
  address its own key; success no longer forgives; window rollover; case
  folding; IPv4-mapped; one address × many usernames → exactly 30 failures,
  one tier-1 lockout, ≤ 3 account lockouts; /64 and /48 via 30 + 270 spread
  failures; `record_failures` counts the address once; unknown account counted
  like a real one; counters by class; `Debug` without the key.
- `view` (9): listed with estimate/prefix/tiers; tier-1 listed as its CIDR;
  looking changes nothing; expiry; bounded newest-first with `total`; clear
  forgives and unlists; clear one key only and only a locked one, 400-class
  refusal; clear an address by its key; clear after the event left the ring.
- `simulation_tests` (4): the table above (~1.8 s); never undercounts on a
  64-wide table with 2 000 keys, and does overcount somewhere; 16 threads ×
  4 000 failures on one key — exact alone, ≥ with 4 noise threads — in both
  tiers; a million distinct failing keys leave `footprint()` unchanged (< 5 MiB)
  (~2.5 s). On a host ten times slower these take ten times longer and assert
  the same counts; the thread test is released by a `Barrier` and asserts
  counts, so any interleaving must pass.
- `web::api::lockouts` (5): JSON shape with `prefix`, four counters and two
  tiers; 403s; clear answers `ClearedLockout` with the note and audits; a
  tier-1 lockout is listed as `198.51.100.4/32` and cleared by it, audited
  under that subject; 404/400.
- `config::auth` (+1), `config::validate` (+1 assert), `rustak-api` (+2),
  `tests/enroll_oauth.rs` `an_account_nobody_holds_runs_out_of_guesses_exactly_as_a_real_one_does`
  (password grant: `nobody` and `ada` both 401 ×3 then 429).
- UI: `settings_lockouts` (+2: source/IPv6 wording, fill note); fixtures (one of
  every class incl. `source`).

## Exit checks (this worktree, after the last source change)

| Check | Result |
|---|---|
| `cargo fmt --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | pass |
| `./scripts/check-file-length.sh` | pass; new files measured by hand with the same awk: `key.rs` 170, `ratelimit.rs` 167, `sketch.rs` 153, `view.rs` 121, `events.rs` 93, `clock.rs` 47 (`simulation_tests.rs` is exempt as `*_tests.rs`) |
| `cargo test -p rustak-api` | 180 passed |
| `cargo test -p rustak-server --no-fail-fast` | exit 0; 39 result lines, 2 470 passed, 0 failed (lib 2 105, 2 ignored) |

UI, built from a copy of the worktree in `/private/tmp/claude-501/m10-18-ui`
(cargo will not build `rustak-ui` nested under the main checkout):
`cargo fmt --all --check` pass; `cargo clippy --all-targets --target
wasm32-unknown-unknown -- -D warnings` pass; `cargo test` 167 passed; `trunk
build` success. e2e against a `rustak` built with that bundle: `npm run
typecheck` clean, `npx playwright test` **77 passed** (full suite, port 18946).
`interop/node-tak`: typecheck clean, 35/35.

## Files

New: `rustak-server/src/auth/ratelimit/{sketch,key,clock,events,simulation_tests}.rs`,
this note.

Changed (in the brief): `rustak-server/src/auth/ratelimit.rs`,
`ratelimit/{class,view}.rs`, `rustak-server/src/config/auth.rs`,
`rustak-server/src/web/api/lockouts.rs`, `rustak-api/src/lockout.rs`,
`rustak-ui/src/{api,fixtures}/lockouts.rs`, `rustak-ui/src/pages/settings_lockouts.rs`,
`e2e/tests/settings.spec.ts`, `rustak-server/tests/workload_support/mod.rs`,
`config.example.toml`, `docs/deployment.md` ("Sign-in lockouts"),
`rustak-server/tests/enroll_oauth.rs` (one test).

Changed outside the list (smallest change each):
- `Cargo.toml`, `rustak-server/Cargo.toml`, `Cargo.lock` — `siphasher = 1.0.3`
  (from the local registry; no other lock entry moved).
- `rustak-api/src/lib.rs` — re-exports.
- `rustak-server/src/config/validate.rs` — refuse `address_attempts`/
  `network_attempts = 0` like `attempts`.
- `rustak-server/src/auth/workload/mod.rs` — `record_failures` (item 3).

## Open

1. **CloudTAK and other server-side clients** share one tier-1 key (above).
   A per-deployment raise of `address_attempts` is the only lever; an
   allow-list of trusted client addresses, or keying tier 1 on what CloudTAK
   forwards, would be a new brief.
2. **`trust_proxy` has no trusted-proxy list** and takes the left-most
   `X-Forwarded-For` (R-01 L1); with tier 1 that matters more. A
   `trusted_proxies` setting taking the right-most untrusted hop would close it.
3. `docs/ci.md` still describes the old flood test's 100 000-entry map (history;
   not my file).
4. `cloudtak` and `eud` interop suites were not run (nightly only).
5. Ring events dropped because four slots were busy are counted
   (`Ring::dropped`) but only tests read the count; exposing it would be one
   more field on `Lockouts`.
