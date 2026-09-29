# M10-04 — ESB and FIRMS on the shared sidecar conventions

**Done.** Both plugins now read the Services-page `area` through M9-14's
retrying `ServiceSettings` and apply a change while running; their
source-state logging follows log-on-state-change with the shared five-minute
reminder; a stated `Retry-After` is honoured above an explicit `poll`, and
tested so; and each logs a periodic counters line in the shape of
`The feed is publishing.`. No change to `rustak-client`.

## What changed, and why

### 1. Server-side settings through `ServiceSettings` (both)

- `EsbSidecar` / `FirmsSidecar` now keep the `SidecarContext`, a
  `ServiceSettings`, the area in effect and `area_from` (`FROM_FILE` /
  `FROM_SERVER`, public constants, same strings as AIS/ADS-B).
- `start` reads once through `ServiceSettings::refresh_at`; every tick reads
  again on `ServiceSettings`' own cadence (30 s after a failure, 5 min after a
  success), so an override whose first read failed is applied on the next
  window. The `… is watching` line carries `area_from`; a change while running
  is one `info` line with `?area` and `area_from`.
- **Decision — applying without reopening.** ADS-B reopens its source on an
  area change. Here that would throw away the schedule, the backoff and any
  `Retry-After` the provider asked for, and ask ESB/FIRMS again at once. So each
  feed trait gained a defaulted, additive method (`OutageFeed::rescope`,
  `HotspotFeed::set_area`) and the publisher a setter
  (`OutagePublisher::set_scope`, `Hotspots::set_area`). PowerCheck drops held
  entries the new scope does not admit (no more detail requests for them);
  FIRMS recomputes its request boxes for the next scheduled poll; `Hotspots`
  forgets detections outside the new area (counted as `expired`). No delete is
  ever sent; out-of-area markers leave maps on their own `stale`.
- **Decision — a document without `area`** (or `area: null`) hands the choice
  back to the file's area, labelled `FROM_FILE`. An unreadable `area` is one
  `warn` and the area already in effect stays. (ADS-B keeps the current area
  when `area` disappears; this is the one deliberate difference.)
- A public `tick_at(now)` on each sidecar carries the injected clock for the
  settings cadence and the counters line; `Sidecar::tick` calls it with
  `Utc::now()`.

### 2. Log-on-state-change for failures and refusals (both)

- `sources/notice.rs` in each plugin is a copy of ADS-B's `Repeated` / `Report`
  / `humanised` / `REMIND_EVERY` (header rewritten; code identical).
- `SourceState` in each now holds two runs: the outage (`Repeated::new(0)`, ends
  on the first answer) and the rate limit (settles after
  `max(REMIND_EVERY, 3 × interval)` without a refusal, so a provider refusing
  every other poll is one run). First → one line (`warn` for an outage, `info`
  for a refusal, naming whether the wait was stated or our guess); reminders at
  most every `REMIND_EVERY` with a count; one recovery line; everything else
  `debug`.
- Before: ESB logged every `429` at `info` and never reminded during an outage;
  FIRMS logged every `429` at `info` as "asked us to wait" even when no delay was
  stated, and had no reminder either.
- `succeeded_at` / `failed_at` / `wait_for_at` take the instant and return the
  `Report`(s) they acted on, which is what the tests assert instead of
  capturing logs. The instant-free wrappers keep their old signatures.
- A FIRMS `429` is still an *answer* (connected, ends an outage run with its
  recovery line), as before.
- Note for slow pollers: when attempts are further apart than five minutes
  (FIRMS' default 10 m poll, backoff to 1 h), each attempt after the first in a
  run is its own reminder line. That is "no more often than the shared cadence"
  as the brief states it; it is at most one line per attempt at ≥ 5 min apart.

### 3. `poll` is a floor, not a pin (both)

Both already clamped a stated delay to `max(interval)`; this is now explicit in
the docs, the start-up line ("`poll` is the fastest it is asked; a Retry-After
it states is waited out above that"), `config.example.toml`, the READMEs, and
covered by tests at the state and the feed level. Each feed gained a public
`poll_at(now)` (the trait `poll` calls it with `Utc::now()`) so the feed-level
test drives the schedule with an injected clock against a wiremock upstream.
The interval returns to the operator's `poll` after the wait; an adopted,
persistent floor (ADS-B's M9-12 cadence) was not brought across — the brief
asks only that a stated `Retry-After` is honoured above `poll`.

### 4. Periodic counters line (both)

Neither publishes through `FeedPublisher` (ESB has `OutagePublisher`, FIRMS
`Hotspots`), and `FeedPublisher`'s `REPORT_EVERY` is private, so each got its
own `report_at(now) -> bool` at the same 300 s cadence, called once per tick —
no extra timer:

- ESB: `The outage feed is publishing. outages=… fault=… planned=… restored=…
  customers=… offered=… published=… suppressed=… expired=…`
- FIRMS: `The fire feed is publishing. tracked=… pending=… offered=…
  published=… republished=… suppressed=… expired=…`

### 5. Structure

`rustak-plugin-esb/src/sources/powercheck.rs` was at 295/300 functional lines,
so the detail-request half moved to a child module
`sources/powercheck/details.rs` (same `impl PowerCheckFeed`, private-field
access as a child). Now 233.

## Files

Changed:
- `rustak-plugin-esb/src/lib.rs`
- `rustak-plugin-esb/src/publish.rs`
- `rustak-plugin-esb/src/sources/mod.rs`
- `rustak-plugin-esb/src/sources/powercheck.rs`
- `rustak-plugin-esb/src/sources/powercheck_tests.rs`
- `rustak-plugin-esb/src/sources/state.rs`
- `rustak-plugin-esb/README.md`
- `rustak-plugin-esb/config.example.toml`
- `rustak-plugin-firms/src/lib.rs`
- `rustak-plugin-firms/src/hotspots.rs`
- `rustak-plugin-firms/src/sources/mod.rs`
- `rustak-plugin-firms/src/sources/firms.rs`
- `rustak-plugin-firms/src/sources/state.rs`
- `rustak-plugin-firms/README.md`
- `rustak-plugin-firms/config.example.toml`

Added:
- `rustak-plugin-esb/src/sources/notice.rs`
- `rustak-plugin-esb/src/sources/powercheck/details.rs`
- `rustak-plugin-firms/src/sources/notice.rs`
- `.claude/plan/status/M10-04-esb-firms-conventions.md`

Nothing outside `rustak-plugin-esb/` and `rustak-plugin-firms/` was touched.

## New tests, and how each behaves on a host ten times slower

All use hand-moved instants or wiremock fixtures; none asserts an upper bound
on elapsed host time.

| Test | What it proves | 10× slower host |
|---|---|---|
| esb `lib::an_area_whose_first_read_failed_is_applied_when_the_read_works` | 503 on the first config read, area applied on `tick_at(now + 31s)`, only the in-area outage held, 2 control requests | Unaffected: the retry is judged against the injected `now + 31s`, which is only later on a slower host |
| esb `lib::an_area_the_administrator_takes_away_gives_the_choice_back_to_the_file` | `{}` after an override returns to `FROM_FILE` | Same, `now + 6 min` |
| firms `lib::` same two tests | Same, over the CSV fixture (4 of 6 detections in a 5 km circle) | Same |
| esb/firms `state::an_outage_is_said_once_when_it_starts_and_once_when_it_ends` | `First`, then `Quiet`, one `Recovered`, then `Quiet` | Pure; clock is by hand |
| esb/firms `state::a_refusal_is_said_once_when_it_starts_and_once_when_it_is_over` | One `First`, not re-announced on an interleaved answer (ESB: one `Reminder` with a count), one `Recovered` after settling | Pure |
| esb/firms `state::an_explicit_poll_is_a_floor_a_stated_retry_after_raises` | `poll = 1m`, `Retry-After` 600/900 s: not ready before, ready at; back to 1 m after | Pure |
| esb `powercheck::an_explicit_poll_is_a_floor_that_a_stated_retry_after_raises` / firms `firms::…` | Feed-level: polls at +0/60/120/599 s make one request, +600 (ESB) / +900 (FIRMS) the second | Counts requests against injected instants; slower requests change nothing |
| esb `publish::the_counters_are_logged_at_once_and_then_every_five_minutes`, firms `hotspots::` same | `report_at` due at 0, 300, 600 and not between | Pure |
| esb `publish::a_new_scope_is_what_the_next_offer_is_judged_by`, firms `hotspots::a_new_area_forgets_what_is_outside_it`, firms `firms::a_new_area_is_what_the_next_request_asks_for` | Scope/area changes apply without reopening | Pure |

Also run (not changed): `rustak-server --test feed_sidecars` (drives
`EsbSidecar` through the harness) — 8 passed.

## Exit checks

1. `cargo fmt --check` — pass.
2. `cargo clippy --workspace --all-targets -- -D warnings` — pass.
3. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` — pass.
4. `./scripts/check-file-length.sh` — pass. (It reads `git ls-files`, so the
   three new untracked files were counted by hand with the same awk: 91, 91 and
   78 functional lines.)
5. `cargo test -p rustak-plugin-esb` — 101 passed; `cargo test -p
   rustak-plugin-firms` — 68 + 7 (end-to-end) passed.

## For the brief that lifts a shared `SourceState` (duplication found)

Across the four feed plugins:

- **`notice.rs` (`Repeated`, `Report`, `humanised`, `REMIND_EVERY = 300 s`)**:
  now in ADS-B, ESB and FIRMS identically; AIS has an older variant without the
  `settle` argument (a run always ends on the first clear). Lifting ADS-B's
  version covers all four (`Repeated::new(Duration::ZERO)` is AIS's behaviour).
- **`SourceState`** exists four times with different shapes:
  - ADS-B: `Cadence` (adopted stated floors, ×1.5 guessed raises, easing back),
    `wording`, `outage` + `limit` runs, `rate_limited`, `ever_connected`,
    `MAX_BACKOFF = 300 s`, Retry-After capped at `MAX_BACKOFF`.
  - ESB: fixed interval, `outage` + `limit` runs, `last_success` (used for the
    two-hour `HOLD`), `wait_for` returns the delay (detail requests are held
    for as long), no `ever_connected` (derived from `last_success`),
    `MAX_BACKOFF = 900 s`, `MAX_RETRY_AFTER = 3600 s`, a `429` leaves the
    connection state alone.
  - FIRMS: fixed interval, `outage` + `limit` runs, `ever_connected`,
    `rate_limited`, `MAX_BACKOFF = 3600 s` which is also the Retry-After cap,
    a `429` counts as *connected* and ends an outage run.
  - AIS: `status.rs` `SourceState` is a connection-state enum published over a
    `watch` channel for streaming sources (aisstream WebSocket, UDP) — a
    different shape from the three polled ones.
  The common core of the three polled ones: `new_at`, `ready_at`,
  `succeeded_at`, `failed_at`, `wait_for_at`, doubling backoff with
  `MAX_DOUBLINGS = 6`, `since`, `last_error`, and the two runs. Differences to
  parameterise: backoff ceiling, Retry-After ceiling, whether a `429` is an
  answer, the adaptive cadence (ADS-B only), and the log wording's noun
  ("The ADS-B source", "ESB PowerCheck", "The FIRMS source").
- **`retry_after(headers)`** (seconds or HTTP date): ADS-B (with a header name
  and an `_at` variant), ESB and FIRMS — three copies.
- **`USER_AGENT` + `http_client()`**: ADS-B, ESB (takes default headers) and
  FIRMS — three copies of the same builder.
- **The `area` / `area_from` / `configured` block in `lib.rs`**: AIS, ADS-B,
  ESB and FIRMS each hold `context`, `ServiceSettings`, `area`, `area_from`, and
  the same `FROM_FILE` / `FROM_SERVER` strings. AIS/ADS-B read a whole
  `FeedConfig` (area + symbology); ESB/FIRMS read only `area`.
- **Counters line**: `FeedPublisher::report` (AIS, ADS-B), `OutagePublisher::
  report_at` (ESB), `Hotspots::report_at` (FIRMS) — the same 300 s
  "due since last" check three times. A public `REPORT_EVERY` or a tiny
  `Cadence`/`Every` helper in `rustak_client::feed` would serve all.

## Open

- ESB and FIRMS do not implement `Sidecar::config_schema` /
  `validate_config`, so the Services page has no drawn form for their `area`
  (AIS/ADS-B use `FeedConfig`). Not asked for here; worth a follow-up if the
  admin UI should offer the same form.
- An area change applies to FIRMS/ESB requests from the next scheduled poll
  (up to `poll`, 10 m / 5 m by default) rather than at once, deliberately, to
  keep the rate-limit schedule; say if an immediate request is preferred.
- Still never run against the live FIRMS or PowerCheck APIs (as before).
