# M9-08 — Feed plugin fixes from the first live run (adsb.lol rate limit, quiet logs)

**Status: complete.** Every deliverable in the brief is implemented and every
exit check named in it is green, with one caveat recorded below: the
workspace-wide `cargo fmt --check` fails in two of M9-10's in-flight files and
in none of mine.

The two production findings are fixed at their root. The cadence is no longer
one number for three different services and no longer deaf to what a provider
says about it; and no log line in either plugin is written per poll or per
message any more — every run of the same thing is announced once, reminded
about every five minutes with a count, and closed with one line.

## 1. The cadence

**Per-provider defaults** (`Provider::default_poll`, `sources/aggregator.rs`):

| `provider` | Default `poll` | Why |
|---|---|---|
| `adsb_lol` | **10 s** | Observed: `429`, `Retry-After: 10`, on about every other request at 5 s |
| `adsb_fi` | **5 s** | Documents a ceiling of one request a second; 5 s is a polite margin |
| `airplanes_live` | **10 s** | Unverified (our probe was refused `403`), so the conservative one |

`[settings.source] poll` became `Option<chrono::Duration>` — there is no single
serde default that could be right for three services, so the default is
resolved from the provider in `Source::open` rather than written into the
`#[serde(default)]`. A `poll` that is present wins, clamped **up** to a floor of
`POLL_FLOOR = 2s` for every provider, with an `info` line naming what was asked
for and what is being used. A `poll` slower than the floor is used as written.

**Adaptation** (`SourceState::wait_for`, `SourceState::adapt`,
`sources/state.rs`). A `429` carrying `Retry-After: N` is waited out for N. Two
of those inside `LIMIT_WINDOW = 10` polls raise the effective interval to
`max(current, N)` for the rest of the process, said once at `info`:

```
adsb.lol asks for 10s between requests; polling at that rate from now on.
```

It is only ever raised — a later, smaller `Retry-After` does not talk it back
down, and nothing lowers it until a restart.

**One judgement call worth recording.** Only a delay the provider *stated*
moves the cadence. A `429` with no `Retry-After` is still waited out, on twice
the current interval, but never adapts: that number is our own guess, and
making a guess permanent compounds — twice the interval becomes the interval,
so the next guess is four times the original, and a feed would slow itself to
the 300-second `MAX_BACKOFF` over a week of flaky minutes. The fallback moved
out of the two call sites and into `wait_for(asked: Option<Duration>)`, which is
what makes "stated" and "guessed" distinguishable at all.

## 2. Log on state change

`sources/notice.rs` (**new**, both crates) is the state machine, modelled on
M9-07's `rustak-client/src/sidecar/link_health.rs` in shape, wording and
cadence: pure, clock-injected (`at(seconds)` in the tests, nothing waits),
`Report::{Quiet, First, Reminder, Recovered}`, `REMIND_EVERY = 300s`. The
difference from `link_health` is that `Reminder` and `Recovered` carry a
**count** as well as a duration, which is what the brief's
"adsb.lol rate-limited 23 times in the last 5m" needs.

The ADS-B copy also takes a **settling period**, which the AIS copy does not.
An outage run ends the moment something works; a rate-limit run must not,
because adsb.lol refuses about every other request and a run that ended on the
next success would be a new `First` every other request — precisely the noise
being fixed. The rate-limit run therefore ends after five minutes without a
`429`. The AIS crate has no upstream that behaves that way, so its copy is the
simpler shape.

**It is a copy, not a shared type, and that is deliberate.** The only crate both
plugins and the server depend on is `rustak-client`, which this brief does not
own, and a sidecar's log cadence is not part of the SDK's contract with a
plugin. Both files say so in their module docs. If a third plugin wants it, the
right move is to promote it into `rustak-client::sidecar` in a brief that owns
that crate.

### The audit

| Where | Was | Now |
|---|---|---|
| `adsb/sources/state.rs` `wait_for` | `info` on **every** `429` | `First` → `info` with the delay; repeats → `debug`; reminder → `info` with a count; recovery → `info` |
| `adsb/sources/state.rs` `failed` | `warn` on the first of a run, then **silence** | `First` → `warn`; repeats → `debug`; reminder every 5m → `warn` with how long and how many; recovery → `info` with the duration and the count |
| `adsb/src/lib.rs` `tick` | `warn` **per tick** on every failed poll | `debug` — `SourceState` has already announced it and will remind |
| `adsb/sources/opensky.rs` `401` retry | `info` per poll in a `401` loop | `debug` |
| `adsb/sources/opensky/token.rs` renewal | `info` per renewal (per poll in a `401` loop) | first → `info`; later ones → `debug`; recovery → `info` |
| `adsb/sources/opensky/token.rs` renewal failure | `warn` per poll while the token endpoint is down | `First` → `warn`; repeats → `debug`; reminder → `warn` with a count |
| `adsb/sources/aggregator.rs` `open` clamp | `warn` | `info`, per the brief |
| `ais/sources/aisstream.rs` `run` | `warn` once per **distinct reason**, `debug` for a repeat of the same one; no reminder, no recovery line | one run: `First` → `warn`, repeats → `debug`, reminder every 5m with a count, recovery → `info` naming the duration and the count |
| `ais/sources/aisstream.rs` clean close | `info` on **every** close | `debug`; the subscription that follows is what speaks, and only if it is news |
| `ais/sources/aisstream.rs` "Subscribed" | `info` on **every** reconnection | first subscription → `info`; a reconnection ending a run of failures → `info` with the duration; otherwise `debug` |
| `ais/sources/udp.rs` bind failure / listener stop | `warn` per attempt, and the rebind `info` per attempt | one run through `Repeated`: `warn`, `debug`, reminder, and one `info` on the rebind naming the duration and the count |

One behaviour was deliberately dropped: AISStream no longer re-announces at
`warn` when the *reason* changes mid-run. That matched neither M9-07 nor the
brief, and a flapping reason was itself a way to get a line per attempt. The
latest reason is still on the heartbeat (`Connection::Reconnecting.last_error`),
and the five-minute reminder carries it too.

**Already correct, left alone:** every parse-skip notice in both plugins was
already `debug` (`ais/sources/udp.rs::decode`, `ais/sources/aisstream/frames.rs`
for non-UTF-8 frames, unreadable JSON, unrecognised messages and a full buffer;
`adsb` routes its parse failures through `SourceState::failed`). The
"Connected for two minutes and decoded nothing" notice from M9-09 is already
once per connection. `adsb/sources/aggregator.rs`'s `error!` on the third `403`
fires once and stops the source. **The "The feed is publishing." counters line
stays at `info` every 5 minutes** — it is `rustak-client/src/feed/publish.rs`
(`REPORT_EVERY = 300s`), which this brief does not own and did not touch.

## 3. The two new metrics keys, exactly as implemented

In `metrics.source`, beside the existing `kind` / `name` / `connection` /
`since` / `last_error`:

```json
"source": {
  "kind": "aggregator",
  "name": "adsb.lol",
  "connection": "connected",
  "since": "2026-09-21T21:04:11Z",
  "last_error": null,
  "poll_effective_s": 10,
  "rate_limited": 2
}
```

- **`source.poll_effective_s`** — `u64` seconds. `SourceState::interval()`,
  which is the cadence *as it stands*: the configured one, or whatever a
  provider has since asked for. The Dublin deployment was polling at half the
  rate its file said and nothing on the Services page showed it.
- **`source.rate_limited`** — `u64`, a lifetime count of `429`s this source has
  been sent (`SourceState::rate_limited()`). Monotonic, so it is a counter a
  dashboard can difference.

This is the ADS-B heartbeat only. The AIS plugin has no polled upstream and no
rate limit, so nothing was added to `rustak-plugin-ais/src/status.rs`.

## 4. Two clocks became one

`SourceState` held `next_attempt` as a `std::time::Instant` while everything
else about it was wall-clock. That made "two 429s inside ten polls" and the
five-minute reminder untestable without sleeping. It is now a
`DateTime<Utc>` like the rest, with `ready()` / `ready_at(now)`,
`succeeded()` / `succeeded_at(now)`, `failed()` / `failed_at(now)` and
`wait_for()` / `wait_for_at(now)` in exactly `link_health.rs`'s pairing. Every
state test now moves the clock by hand and the suite sleeps nowhere.

The cost is the one `link_health` already pays: scheduling now follows the wall
clock, so an NTP step moves the next attempt. Bounded by `MAX_BACKOFF` in one
direction and by a poll in the other, and consistent with the rest of the tree.

## 5. A file that had to be split

`sources/opensky.rs` went to 316 functional lines, over the 300 limit. The
OAuth2 half is now `sources/opensky/token.rs` — the same `file.rs` +
`file/part.rs` shape `aisstream.rs` + `aisstream/frames.rs` already uses — as a
`Tokens` type owning the credential, the token, the renewal run and the renewal
count (`new`, `authenticated`, `bearer`, `ensure`, plus a `#[cfg(test)] held`).
`opensky.rs` is 200 and `token.rs` is 170. `OpenSkyFeed`'s public API did not
change; three tests moved to `token.rs`, where the thing they assert now lives.

## Tests

**`sources/notice.rs`** (9 ADS-B, 8 AIS): first occurrence announced; the nine
after it quiet; the reminder at 300s carrying 23, then quiet, then a second
reminder carrying 2 — the count is since the last reminder, not since the run
began; the end of a run naming the count and the duration; a run that never
started says nothing when it does not happen; a second run is announced again;
a clock that stepped backwards is not a negative run; the durations read the way
an operator says them. ADS-B only: **a run that has to settle is not over the
first time it does not happen** — `happened`, `cleared`, `happened`, `cleared`
all stay one run, and five minutes without it is the end.

**`sources/state.rs`** (12): the eight that were there, rewritten onto the
injected clock, plus —

- `two_stated_rate_limits_inside_ten_polls_raise_the_interval_for_good` — one
  `429` leaves 5 s alone; the second raises it to 10 s; a later `Retry-After: 1`
  does not talk it back down.
- `two_rate_limits_further_apart_than_the_window_are_two_bad_minutes` — eleven
  polls apart changes nothing.
- `a_rate_limit_with_no_retry_after_waits_but_never_moves_the_cadence`.
- `a_provider_that_refuses_every_other_request_is_one_run_of_notices` — 24
  success/`429` pairs are **one** run (the operator's complaint, as an
  assertion), and five minutes without a `429` ends it.

**`sources/aggregator.rs`** (`wiremock`, 5 new): a live `429` with
`Retry-After: 10` served twice raises the interval to 10 s and counts two rate
limits — driven through `fetch` rather than `poll`, because `poll` correctly
honours the ten seconds the first `429` just asked for and the suite would
otherwise sleep for them; each provider's default poll, and none faster than the
floor; `poll = "1s"` clamps to 2 s; no `poll` gets the provider's own; a `poll`
of 30 s is used as written.

**`settings.rs`** (2 new): an aggregator with no `poll` opens at the provider's
own rate (10/5/10, end to end from TOML through `Source::open`), and
`poll = "1s"` is clamped rather than refused.

**`health.rs`** (1 new, 1 extended): both new fields on a healthy heartbeat, and
a source that was asked twice to slow down reporting `poll_effective_s: 10` and
`rate_limited: 2`.

**`sources/opensky/token.rs`** (3, moved and widened): half a credential is no
credential in all three shapes; a feed with no token sends no bearer; the secret
never reaches a `Debug`.

## Exit checks

**`cargo fmt --check` (whole workspace)** — fails, in M9-10's files only:

```
Diff in /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/missions/archive.rs:436:
Diff in /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/missions/archive.rs:444:
Diff in /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/tests/qx_probe.rs:14:
Diff in /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/tests/qx_probe.rs:21:
```

Neither file is mine and neither is in a crate I own. The equivalent scoped
check over every file in both plugin crates is clean:

```
$ rustfmt --check --edition 2024 $(git ls-files 'rustak-plugin-adsb/**/*.rs' 'rustak-plugin-ais/**/*.rs') \
    rustak-plugin-adsb/src/sources/notice.rs rustak-plugin-adsb/src/sources/opensky/token.rs \
    rustak-plugin-ais/src/sources/notice.rs
(no output)
```

**`cargo clippy --workspace --all-targets -- -D warnings`**

```
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.23s
```

The first attempt failed with `couldn't read rustak-server/tests/qx_probe.rs: No
such file or directory` — M9-10 creating and removing that file underneath the
build. Retried rather than cleaning the shared target directory, as instructed,
and it passed. The scoped run is also clean:

```
$ cargo clippy -p rustak-plugin-adsb -p rustak-plugin-ais --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.27s
```

**`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`**

```
 Documenting rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.58s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 8 other files
```

**`./scripts/check-file-length.sh`**

```
(no output: every file is under 300 functional lines)
```

**`cargo test -p rustak-plugin-adsb -p rustak-plugin-ais`**

```
running 121 tests
test result: ok. 121 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
running 10 tests
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
running 84 tests
test result: ok. 84 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
```

(ADS-B 118 → 121 unit plus 10 end-to-end; AIS 84 unit, unchanged in number —
the AIS additions are the 8 in its own `notice.rs`, which replaced nothing.)

**`cargo test -p rustak-server --test feed_sidecars`**

```
running 6 tests
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.24s
```

**Both plugins' `--check` on their example configs**

```
$ cargo run -q -p rustak-plugin-adsb -- --config rustak-plugin-adsb/config.example.toml --check
INFO rustak_client::sidecar::run: The configuration is valid; --check does not start the sidecar.

$ cargo run -q -p rustak-plugin-ais -- --config rustak-plugin-ais/config.example.toml --check
INFO rustak_client::sidecar::run: The configuration is valid; --check does not start the sidecar.
```

## What I could not verify

- **Against the live services.** Nothing here reached adsb.lol, adsb.fi,
  airplanes.live or OpenSky; every test is a `wiremock` mock serving fixtures
  written in this repository. The 10 s default for adsb.lol is the operator's
  observation from the Dublin run, not a measurement of mine, and the
  `airplanes_live` default remains a guess about an endpoint that has never
  answered us.
- **The log output itself.** Every assertion is on the state machine, as the
  brief asks — neither crate captures logs. What is asserted is which `Report`
  a run answers; that the `Report` is wired to the level the table above claims
  is by reading, not by test.
- **The adaptive path end to end through `Feed::poll`.** The wiremock test
  drives `AggregatorFeed::fetch` directly, because `poll` honours the wait the
  first `429` asks for and a `poll`-level test would have to sleep ten seconds
  or fake a clock across real network I/O. The `ready()` gate between them is
  covered by `the_interval_is_a_floor_under_the_request_rate` and by the
  `state.rs` suite.
- **`rustak-plugin-ais`'s reconnection logging end to end.** `Announced` and the
  UDP helpers are covered through `notice.rs`'s own suite; driving them would
  mean a WebSocket server or a port that fails to bind on a schedule, which the
  existing suites do not have and this brief did not ask for.

## Files

**ADS-B (`rustak-plugin-adsb/`)**

| File | What |
|---|---|
| `src/sources/notice.rs` | **New.** `Repeated`, `Report`, `humanised`, `REMIND_EVERY`; 9 tests |
| `src/sources/opensky/token.rs` | **New.** `Tokens` and the OAuth2 half split out of `opensky.rs`; 3 tests |
| `src/sources/state.rs` | The runs, the counters, the adaptation, one clock; `wait_for(Option<Duration>)`, `rate_limited()`, `ready_at`/`succeeded_at`/`failed_at`/`wait_for_at`, `LIMIT_WINDOW` |
| `src/sources/aggregator.rs` | `default_poll`, `POLL_FLOOR`, `min_interval`, `open(.., Option<Duration>)`, the `429` path, `terms()` wording; 5 new tests |
| `src/sources/opensky.rs` | `Tokens` in place of the credential fields; `429` path; `401` retry demoted to `debug` |
| `src/sources/mod.rs` | `mod notice;`, `LIMIT_WINDOW` re-export, module docs |
| `src/settings.rs` | `poll: Option<chrono::Duration>`, `AGGREGATOR_POLL`/`aggregator_poll()` removed; 2 new tests |
| `src/health.rs` | `source.poll_effective_s`, `source.rate_limited`; 1 new test |
| `src/lib.rs` | The per-tick `warn` demoted to `debug` |
| `README.md` | Aggregator cadence, the adaptive behaviour, adsb.lol's observed limit, the two metrics keys, the log-cadence paragraph; "one request a second" reworded as a ceiling rather than a recommendation |
| `config.example.toml` | The aggregator `poll` block rewritten |

**AIS (`rustak-plugin-ais/`)**

| File | What |
|---|---|
| `src/sources/notice.rs` | **New.** The same state machine without the settling period; 8 tests |
| `src/sources/aisstream.rs` | `Announced`; `stream` takes it; clean closes and repeat subscriptions demoted |
| `src/sources/udp.rs` | `cannot_listen` / `listening_again` through `Repeated` |
| `src/sources/mod.rs` | `mod notice;`, module docs |
| `README.md` | One paragraph: what reaches the log is a state change, not an attempt |

**Not touched:** `docs/plugins.md` — its ADS-B and AIS paragraphs describe
sources and terms and state no cadence, so there was nothing in them to correct.
`rustak-client/**`, `rustak-server/**`, CI files and
`.claude/plan/{plan,backlog}.md` were not edited. No `git` or `but` command was
run.
