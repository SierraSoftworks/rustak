# M9-12 — adsb.lol refuses without naming a delay: adapt anyway, and say what actually happened

**Status: complete.** Every item under "Deliver" is implemented and every exit
check is green on the final tree, workspace-wide (nothing had to be scoped to
my crate). Taken over from an agent that was cut off by API errors; its partial
`sources/state/cadence.rs` was **kept and built on** — the design was the
brief's, it had simply never been compiled. What I changed in it is in §2.

## 1. The hypothesis, verified by reading `retry_after()`

**Before** (`sources/mod.rs`, M9-02/M9-08): the header value was trimmed and
accepted in exactly two forms — a bare `u64` (`"30"`), or an RFC 2822 date via
`chrono::DateTime::parse_from_rfc2822` (which covers IMF-fixdate, e.g.
`Sun, 06 Nov 1994 08:49:37 GMT`). Anything else — **`"10.5"`, `"10.0"`**, the
RFC 850 and `asctime` date spellings, a date with the wrong day name — and an
absent header all answered `None`, and `None` became
`asked.unwrap_or(self.interval * 2)` in `wait_for_at`. That is the `seconds=10`
at `poll = "5s"` and the `seconds=20` at `poll = "10s"`: always twice the
interval, never a number adsb.lol sent. Hypothesis confirmed: the provider sent
**no usable `Retry-After`**, M9-08 adapts only to a stated one, and the line
"asked us to wait … seconds=20" attributed our own fallback to the server.

**After**: `retry_after(headers, name)` / `retry_after_at(headers, name, now)`
accept, in this order —

| Form | Example | Result |
|---|---|---|
| Integer seconds | `30`, ` 0 ` | that many seconds |
| Decimal seconds, **rounded up** | `9.5` → 10, `0.001` → 1, `1e1` → 10 | not in RFC 9110; sent by limiters that think in ms |
| HTTP-date, IMF-fixdate | `Tue, 22 Sep 2026 01:02:00 GMT` | delay from `now`, whole seconds rounded up |
| HTTP-date, RFC 850 | `Tuesday, 22-Sep-26 01:02:00 GMT` | same |
| HTTP-date, `asctime` | `Tue Sep 22 01:02:00 2026`, `Sun Nov  1 …` | same |
| RFC 2822 with a numeric zone | `… 01:02:00 +0000` | same (what the old code accepted, kept first) |

Header name lookup is case-insensitive (`Retry-After`, `RETRY-AFTER`), month
names and `GMT` are read case-insensitively, and the day name is skipped rather
than checked (a wrong weekday still says when). `-1`, `inf`, `NaN`, `1e400`,
`10s`, an empty value, a date in the past and an impossible date all answer
`None`, which the caller now treats — and *describes* — as a refusal that named
no delay.

**What adsb.lol could be sending.** One request was made (of the three allowed),
`GET https://api.adsb.lol/v2/point/53.35/-6.26/25` with
`User-Agent: rustak-plugin-adsb/dev (+https://github.com/SierraSoftworks/rustak; M9-12 header check, single request)`.
It answered `200` with exactly these headers and nothing else:

```
HTTP/2 200
date: Tue, 22 Sep 2026 01:03:40 GMT
content-type: application/json
content-length: 2139
vary: Accept-Encoding
cache-control: no-store
```

No `Retry-After`, no `X-RateLimit-*` / `RateLimit-*` family, no `Server`. The
public `adsblol/api` README says only that limits are "dynamic based on the
environment load", and the application entry point (`src/adsb_api/app.py`)
configures no limiter, which points at the refusal coming from the ingress in
front of the app — and ingress-level limiters typically send a bare `429`. So
the most likely answer is **no header at all**; a decimal or otherwise
unparseable one cannot be ruled out without seeing a real `429`, which I was
told not to provoke and did not. Both cases are now handled: the first by §2,
the second by the wider parser.

## 2. The adaptation, as implemented

All in `sources/state/cadence.rs` (`Cadence`, pure, no clock, no logging — it
returns a `Change` and `SourceState` says it).

| Constant | Value | Meaning |
|---|---|---|
| `LIMIT_WINDOW` | 10 polls | two refusals this close are one rate limit (moved here from `state.rs`, same value, still re-exported) |
| raise factor | ×1.5, `div_ceil` to a whole second, at least +1 s | 10 → 15 → 23 → 35 → 53 → 80 → 120 |
| `MAX_ADAPTED` | 120 s | ceiling on what *our guessing* may raise it to |
| `CLEAN_RUN` | 60 consecutive successful polls | earns one step down |
| ease factor | ×2/3, rounded down, then `max(floor)` | 120 → 80 → 53 → 35 → 23 → 15 → 10 |
| floor | `max(configured, adopted stated delay)` | never decayed below |
| `RECENT_POLLS` | 20 | the heartbeat's window |
| unstated wait | `2 × effective interval`, ≤ `MAX_BACKOFF` (300 s), ≥ interval | M9-08's fallback, now computed *after* adapting |

Rules: the first `429` is a bad minute and moves nothing. A `429` within
`LIMIT_WINDOW` polls of the previous `429`: if it **stated** a delay,
M9-08's rule — `effective = max(effective, stated)`, and `stated` becomes part
of the floor for the life of the process; if it **named none**, ×1.5. Sixty
clean polls ease one notch; **any** refusal resets the clean count; a failed
poll (outage, 403, 5xx) is neither clean nor a refusal and moves nothing.

Judgement calls worth a reviewer's eye:

- **One window for both kinds of refusal.** M9-08's `limited_at` counted only
  stated 429s. It now counts any 429, so "unstated then stated" inside the
  window adopts the stated delay, and "stated then unstated" backs off ×1.5. The
  brief says "within `LIMIT_WINDOW` polls of the previous `429`" and I read
  that literally. Two stated ones behave exactly as before (M9-08's own test
  still passes unmodified, and is extended with 300 clean polls to prove no
  decay).
- **A bug fixed in the inherited file:** `floor()` was
  `max(configured, stated).min(max(MAX_ADAPTED, configured))`, which would have
  eased a *stated* 200 s back down to 133 s — a stated delay decayed below,
  which the brief forbids. `MAX_ADAPTED` caps our guesses, not what we were
  told; the cap is gone and
  `a_stated_delay_above_our_own_ceiling_is_still_never_decayed_below` pins it.
- **Six of the inherited tests had never been run and were wrong**, all the
  same way: a helper that refuses twice was called repeatedly and expected one
  rung per *pair*, but every refusal after the first pair is already inside the
  window, so it is one rung per *refusal*. The code was right (it is what the
  brief says); the expectations were corrected.
- **AIMD probes; it does not find a number and keep it.** Against a provider
  that tolerates one request per 18 s, the source settles at 23 s, and every 60
  clean polls tries 15 s, is refused twice, and returns to 23 s: 2 refusals in
  ~63 polls (3 %) for ever, versus 50 % for ever before. That is the price of
  noticing when a "dynamic" limit lifts, and it is asserted
  (`over_a_thousand_polls_…`, < 1 in 20) rather than hidden.

## 3. `poll` is a floor, not a pin

`SourceState::configured()` is the interval the source was opened with;
`interval()` is the one in use and is never below it. Said in
`config.example.toml` (aggregator `poll`, and one sentence on OpenSky's, which
shares `SourceState`), the README, `settings.rs`'s doc comment, and once at
start-up:

```
Reading aircraft from a public aggregator every 10s (may be raised if the provider rate-limits, and eases back when it stops). adsb.lol is open data; … provider=adsb.lol lat=… lon=… radius_nm=… poll_s=10
```

## 4. What an operator sees (all in `sources/state/wording.rs`)

The run of rate limits keeps M9-08's cadence — first / five-minute reminder /
recovered, everything else `debug` — with wording that says who chose the
number. `seconds=` is now the wait actually applied and `stated=true|false`
says whose it was.

```
INFO  The ADS-B source refused a request (429) without naming a delay; waiting 20s before the next one.   source=adsb.lol seconds=20 stated=false
INFO  The ADS-B source asked us to wait 30s before the next request.                                       source=adsb.lol seconds=30 stated=true
INFO  The ADS-B source asked us to wait 1s before the next request; waiting 10s.                           (stated, but our interval or the 5m cap differs)
INFO  The ADS-B source has rate-limited us (429) 23 times in the last 5m00s; polling every 15s.           (reminder)
INFO  The ADS-B source has stopped rate-limiting us; 31 requests were refused (429) over 15m12s.          (recovered)
DEBUG The ADS-B source refused another request (429) without naming a delay; waiting 30s.
DEBUG The ADS-B source asked us to wait 30s again; waiting 30s.
```

One `info` line per interval change, and only on a change (the ceiling is not
re-announced):

```
INFO  Polling adsb.lol every 15s after repeated rate limits (429) that named no delay; this eases back towards 10s after 60 clean polls.   source=adsb.lol seconds=15
INFO  Back to polling adsb.lol every 10s after 60 clean polls.                                                                           source=adsb.lol seconds=10
INFO  adsb.lol asks for 30s between requests; polling at that rate from now on.                                                          source=adsb.lol seconds=30   (M9-08's line, unchanged)
```

"Asked us to wait" is asserted never to appear in any unstated variant, the
reminder or the recovery line.

## 5. Metrics and the heartbeat

```json
"source": { "…": "…", "poll_effective_s": 23, "poll_configured_s": 10, "rate_limited": 3 }
```

`poll_effective_s` = `SourceState::interval()` (adapted), **new**
`poll_configured_s` = `SourceState::configured()`, `rate_limited` unchanged
(lifetime count of 429s). `service_state`: `unhealthy` if never connected
(unchanged), `degraded` if reconnecting for more than two intervals
(unchanged), **`degraded` if more than half of the last 20 polls were refused**
— literally `refused > 10`, so it cannot fire before the eleventh poll — and
otherwise `healthy`, *including while the interval is raised*. The degraded
message is

```
adsb.lol is rate-limiting this feed: 11 of its last 12 requests were refused (429). Polling every 120s; 3 aircraft tracked.
```

and it clears by itself once the window is mostly answers again.

## 6. A claim from M9-08 that this brief disproves, corrected where I own it

M9-08 recorded "Observed: `429`, `Retry-After: 10`" for adsb.lol and justified
the 10 s default as "what it asked for". That `10` was the same fallback, read
back out of our own log at `poll = "5s"`. The README table, `config.example.toml`
and `aggregator.rs`'s docs now say what was actually observed (429 on about
every other request at 5 s, about one in five at 10 s, no `Retry-After` either
time) and that 10 s is a starting point. The default itself is unchanged — the
brief did not ask for it to move, and the source now finds its own rate. The
M9-08 status note is not mine to edit; the orchestrator may want a pointer there.

## 7. Tests (all clock-injected; nothing sleeps)

`cargo test -p rustak-plugin-adsb`: **158 unit + 10 end-to-end**; the unit suite was 121 before this brief.

- `sources/mod.rs` (8 on `retry_after`): integer; decimal rounded up; numbers
  that are not delays; all three HTTP-date spellings plus a numeric zone and the
  space-padded `asctime` day; header name and date case-insensitive; wrong day
  name; whole seconds rounded up against a fractional clock; unreadable / past
  / impossible dates.
- `sources/state/cadence.rs` (15): first refusal moves nothing; second inside
  the window ×1.5; outside the window nothing; the ladder up to 120 s and the
  ceiling not re-announced; the same ladder down and the floor; refusal resets
  the clean run; stated follows M9-08 and never decays (30 s, and 200 s above
  our ceiling); guessed raise eases back to a stated floor and stops; a smaller
  stated delay does not lower it; 5 s eases to 5 and not 4; the 20-poll window
  slides; more-than-half is 11, not 10, and not 8-of-8; a failed poll is neither.
- `sources/state/wording.rs` (5): **stated vs unstated selection**, both
  numbers when they differ, reminder/recovery attribute nothing, each `Change`.
- `sources/state.rs` (20): M9-08's suite, plus — one unstated 429 waits 2× and
  moves nothing; the second backs off and waits 2× the *new* interval;
  unstated 429s further apart than the window move nothing;
  **a refusal every fifth poll walks 10→15→23→35→53→80→120 and stops**;
  **a simulated provider that tolerates one request per 18 s** (`Grudging`,
  driven from `next_attempt` to `next_attempt`) is refused 3 times in the first
  6 polls, then **0 times in the next 24**, settling at 23 s; over 1 000 polls
  < 1 in 20 refused, interval always within 15–23 s, never below configured
  (asserted on every poll); 59 clean polls ease nothing, the 60th does, and
  600 more stop at the configured 10 s; a mid-run refusal restarts the count;
  `mostly_refused` = `(11, 12)`.
- `sources/aggregator.rs` (wiremock, 2 new): a bare `429` served twice raises
  10 → 15 s with `configured()` still 10 and no `last_error`;
  `Retry-After: 9.5` served twice is a *stated* 10 s.
- `health.rs` (2 new, 2 extended): `poll_configured_s` present; adapting to an
  unstated limit is `healthy` with 8 / 5 / 2; 10 refusals healthy, the 11th
  `degraded` with the message, ten answers later `healthy` again.

## 8. What I could not verify

- **A real adsb.lol `429`.** Never seen, by instruction. Whether the header is
  absent or malformed is inferred (§1). If it turns out to be present in a form
  this still does not read, the symptom is now visible rather than silent:
  `stated=false` on the first line of the run.
- **The numbers 1.5 / 60 / 120 against the live service.** They are the
  brief's. The simulation shows convergence for a fixed 18 s tolerance;
  adsb.lol's limit is "dynamic", and only a day of production logs
  (`poll_effective_s` over time, the count of "Polling … every" lines) will say
  whether 60 clean polls is too eager.
- **A provider that states a delay no longer than the current interval and
  keeps refusing** (`Retry-After: 1` on every 429 at a 10 s poll) adapts to
  nothing, exactly as under M9-08: the brief keeps M9-08's rule for stated
  delays and I did not widen it. Not observed anywhere; flagged because it is
  the same shape of gap as this one.
- **OpenSky** shares `SourceState`, so a bare 429 from it now backs off too. Its
  suite passes; its real 429s carry `X-Rate-Limit-Retry-After-Seconds` and take
  the stated path, unchanged.
- The sidecar `tick` (5 s in the example) quantises every interval upward: 23 s
  is polled at the first tick at or after 23 s. Unchanged behaviour, noted
  because `poll_effective_s` reports 23, not 25.

## 9. Files

New:
- `rustak-plugin-adsb/src/sources/state/cadence.rs` (148 functional lines) — inherited, fixed, extended
- `rustak-plugin-adsb/src/sources/state/wording.rs` (72)
- `.claude/plan/status/M9-12-adsb-unstated-rate-limits.md`

Changed:
- `rustak-plugin-adsb/src/sources/state.rs` (213) — stays `state.rs` beside `state/`, the `opensky.rs` + `opensky/token.rs` shape; owns a `Cadence`, logs through `wording`
- `rustak-plugin-adsb/src/sources/mod.rs` (76) — `retry_after` / `retry_after_at`, `http_date`, re-exports `CLEAN_RUN`, `MAX_ADAPTED`, `RECENT_POLLS`
- `rustak-plugin-adsb/src/sources/aggregator.rs` (213) — start-up line, corrected docs, two wire tests
- `rustak-plugin-adsb/src/health.rs` (67) — `poll_configured_s`, refused-majority `degraded`
- `rustak-plugin-adsb/src/settings.rs` — doc comment on `poll` only
- `rustak-plugin-adsb/README.md`, `rustak-plugin-adsb/config.example.toml`

Nothing outside `rustak-plugin-adsb/**` and this note was touched; no `git` or
`but` write was run. **The two new files are untracked**, so
`scripts/check-file-length.sh` (which reads `git ls-files`) does not see them
yet; I counted them with the script's own `awk` program and the counts are
above.

## Exit checks (final tree, after `cargo fmt -p rustak-plugin-adsb`)

**`cargo fmt --check`** (whole workspace)

```
exit=0
```
(no diff anywhere in the workspace)

**`cargo clippy --workspace --all-targets -- -D warnings`**

```
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.72s
exit=0
```

**`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`**

```
 Documenting rustak-plugin-adsb v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-adsb)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.28s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 8 other files
exit=0
```

**`./scripts/check-file-length.sh`**

```
exit=0
```
(no output: every tracked file is under 300; the two untracked ones are 148 and 72)

**`cargo test -p rustak-plugin-adsb`**

```
     Running unittests src/lib.rs (target/debug/deps/rustak_plugin_adsb-3c9107ca3c2c434f)
test result: ok. 158 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
     Running unittests src/main.rs (target/debug/deps/rustak_plugin_adsb-c2bd45fb9a551a1c)
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/end_to_end.rs (target/debug/deps/end_to_end-f1fa1a15816e671d)
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
exit=0
```

**`cargo test -p rustak-server --test feed_sidecars`**

```
running 7 tests
test the_first_batch_a_feed_produces_reaches_the_stream_on_a_clean_start ... ok
test the_adsb_sidecar_publishes_its_replayed_aircraft_with_their_altitudes ... ok
test the_ais_sidecar_publishes_its_replayed_vessels_to_a_device_on_the_channel ... ok
test the_adsb_sidecar_publishes_what_a_readsb_receiver_serves ... ok
test one_vessel_reported_ten_times_in_a_second_is_published_once ... ok
test a_feed_publishes_nothing_for_a_track_outside_its_area ... ok
test the_ais_sidecar_publishes_what_a_receiver_sends_it_over_udp ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.39s
exit=0
```

**`cargo run -p rustak-plugin-adsb -- --config rustak-plugin-adsb/config.example.toml --check`**

```
2026-09-22T01:23:41.759809Z  INFO rustak_client::sidecar::run: Loaded the configuration for rustak-plugin-adsb 0.1.0. descriptor=ServiceDescriptor { name: ServiceName(adsb), display_name: None, version: Some("0.1.0"), capabilities: [Capability(cot.publish)], endpoints: ServiceEndpoints { stream: None, marti: None, control: None } } file=rustak-plugin-adsb/config.example.toml
2026-09-22T01:23:41.760020Z  INFO rustak_client::sidecar::run: The configuration is valid; --check does not start the sidecar.
exit=0
```
