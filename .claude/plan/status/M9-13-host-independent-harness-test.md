# M9-13 — A harness test that measures the behaviour, not the host

**Status: done.** The release-blocking test no longer contains a clock, cannot fail because a host is
slow, and — unlike the assertion it replaces — actually notices the regression it is named after.
Every exit check passes on the final tree.

## The failure

`the_first_batch_a_feed_produces_reaches_the_stream_on_a_clean_start`
(`rustak-server/tests/feed_sidecars.rs`, added by M9-11) took `Instant::now()` *before*
`RunningFeed::start` — a server boot, two enrolments, the watcher's handshake — and asserted the first
vessel arrived in under 4 s. In CI run 35673702081 that start-up alone took 41.3 s (38.2 s on a quicker
runner, per the coordinator): seven tests in the binary boot a server each, in parallel, under
`-Cinstrument-coverage`; the suite took 116.76 s against 3.3 s here. The assertion measured the runner.

It was also blind. Its comment reasoned that a dropped first batch "cannot arrive again for five"
seconds (`min_interval`). It can: both feed plugins answer `Connected` with
`FeedPublisher::refresh_all()`, so a discarded first batch goes out again on the *next tick*, one second
later — comfortably inside a 4 s window even when the hold is broken. M9-11's note says the race "does
not reproduce locally in the harness"; it does (below) — the stopwatch simply could not see it.

## What the test asserts now

A count, read from the running sidecar:

```rust
feed.eud.expect_uid("AIS-244660000", FIRST_CONNECT_HOLD + EXPECT).await.expect("the first vessel arrives");
let published = feed.stream.published();
let discarded = feed.stream.discarded();
...
assert!(published >= 1, …);      // these are the running sidecar's own counters, not a dead copy
assert_eq!(discarded, 0, …);     // nothing was discarded ahead of the first batch that arrived
```

- `Link` now counts every event it hands to a live connection and every event it discards, and of the
  discards how many happened before the stream had *ever* connected (it already knew:
  `Discards::ever_connected`). The counters are a `StreamStats` behind an `Arc` on the
  `SidecarContext` (`ctx.stream_stats()`), so the harness keeps a handle before moving the context into
  `drive`. `RunningFeed` exposes it as `feed.stream`.
- **Why there is no race in reading it.** A discard is recorded synchronously inside `publish`, before
  that call returns, and the loop publishes batches one after another on one task. `published` is
  bumped per event immediately after `feed()` and *before* the flush, so nothing can be on the wire
  uncounted. A vessel at the EUD therefore implies every earlier discard is already recorded, and
  `published >= 1`. The `published >= 1` assertion is what stops `discarded == 0` being vacuously true
  of a counter nobody writes to.
- **The hold's own bound is no longer a host bet either** (coordinator's instruction). Production
  holds the first tick for at most `FIRST_CONNECT = 10 s`; a handshake slower than that is a first batch
  legitimately discarded with no product fault. The bound is now a property of the context:
  `SidecarContext::with_first_connect_hold(Duration)`, default `FIRST_CONNECT` (10 s, unchanged,
  pinned by a test), deliberately a builder and **not** a configuration key — a deployment has no reason
  to change it. `feed_support` runs every sidecar with `FIRST_CONNECT_HOLD = 120 s`, so the only way a
  first batch can be discarded there is the hold not working.
- The only durations left in the test are the hung-test wait (`FIRST_CONNECT_HOLD + EXPECT` = 125 s,
  longer than the hold so a connect that uses the hold up is still waited for) and nothing is compared
  against it.

### On a host ten times slower than mine

Here the test takes ~0.9 s alone, of which `RunningFeed::start` is most; the stream handshake is a few
milliseconds and sidecar-spawn → first vessel is well under half a second.

- **10× slower:** start-up ~6–9 s — *not measured by anything any more*; handshake tens of ms against a
  120 s hold; first vessel < 5 s after spawn against a 125 s hung-wait. Reads `published = 5,
  discarded = 0`, passes. Identical numbers, because they are counts.
- **The CI runner** (~45–60× slower through start-up: 0.7 s → 38–41 s): same. For the hold to matter the
  loopback TLS handshake would have to take > 120 s, roughly four orders of magnitude over local; for
  the hung-wait, the sidecar would need > 125 s from spawn to first delivery, where CI has so far always
  managed it inside the old 5 s.
- It fails only if (a) something *was* discarded before the first delivered batch — the product fault —
  or (b) nothing arrives for 125 s, reported as "the first vessel arrives", i.e. hung.
- Verified under load: whole suite ×6 with 10 busy-loop processes pinning every core — 7/7 each time.

## Does it bite? (hold removed, then restored)

The first-connect hold in `run.rs` was replaced with a no-op, tests run, file restored from a backup by
a `trap` and verified byte-identical with `diff` each time (`grep -c 'link.settle(first_connect)'` = 1
at the end; exit checks were run after the final restore).

| Test | Hold removed | Verdict |
|---|---|---|
| Harness: `the_first_batch_…_on_a_clean_start` (new count) | **failed 9 of 11 runs**: `5 events were discarded … (5 of them before the stream had ever connected)`. Passed twice, when the loopback handshake happened to beat the first tick and nothing *was* discarded. | Sensitive, but the symptom itself is a race on loopback, so not deterministic. |
| Unit: `a_clean_start_discards_nothing_before_the_first_connection` (new, `run.rs`) | **failed 5 of 5**, run alone, in 0.26 s: `left: (0, 1)` — (published, discarded). | **The effective, deterministic guard.** |
| Unit: `a_server_that_is_not_there_holds_the_first_tick_for_the_bound_and_no_longer` (new, paused clock) | **failed 2 of 2**: `the first tick came early: 0ns`. | Deterministic; guards that the hold exists and is bounded at 10 s / at what was asked. |
| Unit: `the_first_tick_publishes_into_a_connection_that_is_actually_up` (M9-11, **unchanged** as instructed) | failed once when run alongside other tests (10 s `Timeout`), **passed 4 of 4 run alone**. | Not a reliable guard on its own: a first tick with no hold races a loopback TCP connect, and alone the connect wins. M9-11's "verified to fail without the fix" holds only under contention. |

How the new unit guard is made deterministic: the port is *bound but not listening* when the sidecar
starts (a `TcpSocket` we own, so nothing can steal it), so connections are refused; a first tick that
does not wait is discarded every time, and the test sees `(0, 1)`. Only then does the test call
`listen`, the link finds the server (after its 1 s backoff on Linux; macOS completes the pending
connect), and a first tick that *did* wait publishes: `published = 1, discarded = 0`, confirmed at the
peer. The 250 ms pause before checking is not an assertion about speed — with the hold in place there is
nothing to publish into however long it is, so a slow host makes it easier to pass, never harder. It
runs with a 120 s hold and 180 s hung-waits for the same reason as the harness.

My first version of that unit test used a listener that was already up and passed 4 of 5 runs with the
hold removed; I replaced it rather than ship a guard that mostly does not guard.

## What changed

- `rustak-client/src/sidecar/mod.rs` — `pub struct StreamStats` (`published()`, `discarded()`,
  `discarded_before_first_connection()`; `pub(crate)` recorders); `SidecarContext::stream_stats()`,
  `with_first_connect_hold()`, `first_connect_hold()`; both carried by the hand-written `Clone`. Tests:
  a clone shares the counters; the hold is 10 s unless told otherwise and survives a clone.
  236 functional lines.
- `rustak-client/src/sidecar/link.rs` — `Link` takes the context's `StreamStats`; `publish` counts per
  event fed, `discard` and the no-`[server] stream` path count what they drop. No behaviour or log line
  changed. Test: a batch dropped before the first connection is counted as exactly that, and one dropped
  after an outage is not. 254 functional lines.
- `rustak-client/src/sidecar/run.rs` — the hold reads `context.first_connect_hold()` instead of the
  constant. Two new tests (table above). M9-11's guard untouched. 157 functional lines.
- `rustak-server/tests/feed_support/mod.rs` — `RunningFeed::stream`, `FIRST_CONNECT_HOLD`.
- `rustak-server/tests/feed_sidecars.rs` — the one test rewritten; no other hunk.

Scope note: the brief said "additive accessor only" for the client files. `StreamStats` and
`stream_stats()` are that. `with_first_connect_hold` goes one step further (a builder, and `run.rs`
reading it) on the coordinator's mid-task instruction to make the bound injectable; production
behaviour is unchanged and the default is pinned by a test. `docs/plugins.md` does not mention either
addition — it is outside my file list; the rustdoc does.

## Audit: clock comparisons against fixed thresholds

Grepped `rustak-server/tests/**`, `rustak-client/{src,tests}/**` and the three plugin crates for
`Instant::now()`, `.elapsed()`, `Utc::now()`, `SystemTime::now()`, `duration_since`, comparisons against
`Duration::…`, and short `timeout(`/`sleep(` in tests. **One instance of the mistake, the one above.
Nothing else needed fixing.** Everything looked at:

| Where | What | Verdict |
|---|---|---|
| `rustak-server/tests/feed_sidecars.rs:212` | 4 s bound from before server start to first arrival | **The mistake. Fixed.** |
| `rustak-server/tests/bootstrap.rs:452` | `started.elapsed() < SHUTDOWN_TIMEOUT` (10 s) around `server.stop()` | Not the same: both ends are inside the test and bracket only the behaviour under test (an idle server stops without waiting out actix's 10 s drain, which is what the number is). `stop()` already enforces the same bound as its hung-wait. Passed on the slow run. Left. |
| `rustak-client/src/sidecar/run.rs` `the_first_tick_happens_at_once…` | `< 1 s` across `drive` with no stream and no control, against a one-hour interval | No I/O, no crypto, microseconds of work; separates "now" from "an hour". Left. |
| `rustak-client/src/sidecar/link.rs` `settling_a_link_with_no_stream_answers_at_once` | `< 500 ms` across a call that awaits nothing | Same. (Could be `now_or_never()` and lose the clock entirely; not the same mistake, so left.) |
| `rustak-server/tests/cloudtak_onboarding.rs:187-189` | `expires_at` within (now, now + 11 min) | A slow host makes the upper check truer; the lower needs ten minutes. Left. |
| `sidecar_trust.rs:216`, `sidecar_enrolment.rs:230`, `services_flow.rs:223` (`until`, `EXPECT` 5 s); `feed_sidecars.rs:592` | poll-until-deadline | Hung-waits, kept as instructed. **Flag:** with every `expect_uid(.., EXPECT)` in `feed_sidecars`, these are the tightest hung-waits in scope — 5 s from sidecar spawn across register + handshake + first tick. They passed on the slow run (suites took 63–82 s), but they are what goes next if runners get slower. |
| `bootstrap.rs:277/296` (120 s), `acme_plain_listener.rs:94` (20 s), `oidc_provider.rs:414` (20 s), `enroll_flows.rs:107` (20 s) | readiness polls | Generous hung-waits. Left. |
| `rustak-client/src/control/events.rs` SSE tests (M9-11) | real timers: a 150 ms *total* timeout that must outlast loopback connect + headers (else `feed_over` unwraps an error); 250 ms idle; 400 ms idle against 100 ms keep-alives | Not upper-bound assertions — the timeout *is* the behaviour — so not the same mistake, and they passed on the slow run (262 tests, 5.6 s). **Flag:** a 150–300 ms scheduling stall fails them. Most likely next host-dependent flake in scope. **Since widened:** the flag came true in CI run 35789923247 (a `rustak-server`-only PR; the binary took 17.4 s against 2.4 s locally) — the 250 ms read timeout also bounds *opening* the feed, so a stall failed the open and `feed_over` unwrapped the error. Now a 1 s total timeout against an event 2 s in; 1 s idle; 1.6 s idle against 400 ms keep-alives. Same orderings and the same 4:1 ratio, so the same behaviours are asserted; each survives a stall of roughly a second, and all sit under the 5 s `SOON` hung-wait. Still real socket timers: `reqwest`'s timeouts are not the paused tokio clock's to advance across real I/O. |
| `rustak-client/tests/stream_client.rs` (100 ms `expect_err`, 1 s expect), `stream_reconnect.rs` (5 s across a 1 s backoff; 500 ms absence) | in-memory duplex, or assertions of absence | A slow host cannot fail an absence check; the rest are hung-waits. Left. |
| `rustak-client/src/stream/keepalive.rs:187/201` | `silence()` vs 25 s / 15 s | Paused clock. Fine. |
| `rustak-plugin-adsb/tests/end_to_end.rs:297` | 50 ms sleep, then a second tick must fall inside `min_interval` (5 s) | Two wiremock round trips inside 5 s. Left. |
| `rustak-plugin-adsb/src/health.rs:289` | two 5 ms `thread::sleep`s against a 0 s interval | Lower bounds; slowness only helps. Fine. |
| `rustak-plugin-ais/src/sources/udp.rs:723` | 5 s poll loop for two UDP datagrams | Hung-wait. Left. |
| `rustak-client/src/feed/replay.rs:185` | `observed_at >= before` | Ordering, not a threshold. Fine. |
| `api_v1_live`, `marti_cot`, `stream_store`, `missions_flow`, `acme_directory`, `oauth_flows`, `hostile_server_name`, `workload_identity:393`, plugin `mapping`/`lib` tests | `Utc::now()` as fixture timestamps or ±1 h windows | No elapsed-time comparison. Fine. |
| `keepalive.rs`, `reconnect.rs`, `connection.rs`, `publish.rs`, `link_health.rs`, `workload.rs`, `event_feed.rs`, plugin `state.rs`/`token.rs`/`frames.rs`/`status.rs` | production clock reads | Not tests; the state machines take an injected `now` in theirs. |

## Exit checks

Run on the final tree, after the last restore.

```
$ cargo fmt --check
exit 0

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 13.79s
exit 0

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 18.56s
   Generated …/target/doc/rustak_api/index.html and 8 other files
exit 0

$ ./scripts/check-file-length.sh
exit 0

$ cargo test -p rustak-client
test result: ok. 267 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.13s
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
exit 0

$ cargo test -p rustak-server --test feed_sidecars --test services_flow --test sidecar_trust --test sidecar_enrolment
     Running tests/feed_sidecars.rs
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.42s
     Running tests/services_flow.rs
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.32s
     Running tests/sidecar_enrolment.rs
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.50s
     Running tests/sidecar_trust.rs
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.97s
exit 0
```

(262 → 267 in the client library: five new tests.)

## What I could not verify

- **Linux.** Everything ran on macOS. The new unit guard relies on a bound-but-not-listening TCP socket
  refusing connections, which is standard on Linux too (no listener, so RST) and only changes *when*
  the link connects after `listen` (one 1 s backoff instead of at once) — with the hold in place it
  passes either way, but I have not watched it do so on a Linux runner.
- **CI itself.** I read the failing job's log (`gh run view`, read-only) but have not seen this change
  run there.

## Files

Changed:

- `rustak-client/src/sidecar/mod.rs`
- `rustak-client/src/sidecar/link.rs`
- `rustak-client/src/sidecar/run.rs`
- `rustak-server/tests/feed_support/mod.rs`
- `rustak-server/tests/feed_sidecars.rs`

Added:

- `.claude/plan/status/M9-13-host-independent-harness-test.md`

No `git`/`but` writes. Nothing under `.github/**`, `docs/**`, or any other agent's status note.
