# M10-08: The last tests that depend on how fast the host is

**Status: done.** Every exit check passes on the final tree. No production code changed:
`rustak-client/src/http.rs` was mutated temporarily to prove the new tests fail, then restored and
checked byte-identical against a backup with `diff`.

## 1. `EXPECT` is now a hung-wait (`rustak-server/tests/stream_support/mod.rs`)

- `EXPECT` went from 5 s to **60 s**. Its rustdoc now says it is a hung-wait, that nothing may compare
  an elapsed time against it, and that reaching it must never count as a pass.
  `FIRST_CONNECT_HOLD + EXPECT` in `feed_sidecars` is now 180 s.
- `Harness::await_connected` / `await_callsign` used to run 200 polls of 10 ms, which is about 2 s
  on a quick host and a bet on a slow one. Both now wait until an `EXPECT` deadline.
- **`settle()` was using `SETTLE` as an upper bound on something that will happen.** It pumped
  400 ms of quiet, and then its callers asserted `mode() == Proto`. On a host where the offer and
  request round trip takes longer than 400 ms, it returns early and the assertion reads `Xml`, a
  false failure. It now pumps in 20 ms slices until `negotiation().is_settled()` (the server offers on
  every connection; the client either takes protobuf or settles on XML), with `EXPECT` as the
  hung-wait. It fails on any event, on a close, or on an error. Only after that does it take the
  `SETTLE` of quiet. The callers are in `stream_store`, `stream_session` and `hostile_server_name`,
  and none of them needed an edit.
- **New `expect_closed(&mut Eud) -> StreamError`.** `alpha.expect(|_| true, EXPECT).await.is_err()` was
  used to assert "the connection ended". A timeout is also an `Err`, so that assertion **passed
  against a server that never closed the connection**, after 5 s before this change and after 60 s
  with the new value. `expect_closed` fails on any event, and on `Timeout` or `RxTimeout`. Only an
  I/O or codec end counts as closed.

### EXPECT uses whose meaning changed

| Where | Before | Now |
|---|---|---|
| `stream_session.rs` `revoking_a_certificate_ends_the_session_it_bought` | `expect(.., EXPECT).is_err()` | `expect_closed` |
| `stream_session.rs` `switching_an_account_off_ends_the_session_it_already_had` | same | `expect_closed` |
| `stream_session.rs` `a_certificate_from_another_authority_never_reaches_the_application` | `expect(.., SETTLE).is_err()`, so a server that let the certificate in and then sent nothing passed | `expect_closed`, then `connected() == 0` |
| `stream_idle.rs` `a_client_that_neither_sends_nor_receives_is_reclaimed_as_idle` | `expect(.., local 20 s).is_err()` (also backed by the `Idle` counter) | `expect_closed` |
| `feed_sidecars.rs` UDP case | 40 rounds × 250 ms (10 s for the sidecar to enrol, connect and bind) | resend every 250 ms until an `EXPECT` deadline |
| `feed_sidecars.rs` `a_feed_publishes_nothing_for_a_track_outside_its_area` | outsider must not arrive within 400 ms | outsider is placed **first** in the fixture. The replay, the publisher (`pending: Vec`) and the link keep that order down one connection, so a leaked outsider would arrive before the five insiders. The test reads all five and asserts none was the outsider. This is an ordering, and a slow host cannot fake it. |
| `feed_sidecars.rs` `one_vessel_reported_ten_times_in_a_second_is_published_once` | `expect_none(SETTLE)` only | adds `feed.stream.published() == 1` at arrival. The link counts each event on `feed()` and flushes after the batch, so arrival means the whole batch was counted. The `SETTLE` check stays as a second line. |

Every other `EXPECT` use is a positive wait (`expect`/`expect_uid` for a message that will come,
`until`, `timeout(EXPECT, seen.recv())`, the health poll), so the new constant is the only change they
need. That includes M10-07's files (`workload_identity`, `sidecar_enrolment`, `sidecar_trust`,
`hostile_server_name`): I did not edit them, and none of their `EXPECT` uses needs more than the new value.

## 2. The event-feed timeout tests are on a clock the test holds (`rustak-client/src/control/events.rs`)

**Decision.** I kept a real `reqwest` client over real loopback, with tokio's paused clock, and the
clock **never auto-advances**. I checked reqwest 0.13.5's source: the connect timeout
(`connect.rs` `with_timeout`), the read timeout (`ReadTimeoutBody`, plus the one during the pending
response) and the total timeout are all `tokio::time` sleeps, so they honour a paused clock.

An in-memory transport is not possible through reqwest's public API: `connector_layer` must return
reqwest's sealed `Conn`. The auto-advance hazard the brief names is real. Tokio's time driver parks
with a zero timeout and then advances even when that park made an I/O task ready, because `did_wake`
only records unparks. So nothing in these tests is plainly `.await`ed. The `frozen()` helper polls a
future and calls `yield_now` between polls. A runtime with a deferred task only ever does
`park_yield`, which never advances the clock. The clock moves only on the test's
`tokio::time::advance`, and `frozen` asserts that it did not move on its own. Real time is still
spent waiting for loopback I/O, but no timer can fire meanwhile. The only real-time bound is
`HUNG = 60 s`, which fails a hung test and measures nothing.

A keep-alive comment produces no event, so there is no way to see it has been read. The tests
therefore wrap the stream's (private) body in a byte-counting `inspect` from inside the test module.
The wrap sits outside reqwest's body, so a counted byte has already reset the read timer. The test
waits for the count before advancing again, which gives a deterministic order: consume, then advance.

All clients are built by the production constructors (`ControlClient::new`, `http::client`), so the
tests now assert the **shipped** numbers (`FEED_IDLE_TIMEOUT` 65 s, `CONNECT_TIMEOUT` 10 s,
`DEFAULT_TIMEOUT` 30 s) at no cost in wall time. The old tests used hand-built clients.

| Test | Proves |
|---|---|
| `a_feed_that_goes_silent_is_ended_at_the_idle_timeout_and_not_before` | open at `FEED_IDLE_TIMEOUT − 1 ms`, ended at `FEED_IDLE_TIMEOUT` |
| `keepalive_comments_hold_a_silent_feed_open_past_the_idle_timeout` | 5 comments at 20 s intervals (100 s > 65 s), then the event at 120 s still arrives |
| `a_feed_has_no_total_timeout_where_an_ordinary_call_would_be_cut` | an hour of keep-alives, then an event; and the contrast: the same feed through the ordinary client (the pre-fix fallback) is open at 30 s − 1 ms and cut at 30 s, although a byte arrived at 20 s |
| `opening_a_feed_is_bounded_by_the_connect_timeout` (new) | a server that accepts TCP and never answers the TLS handshake: still pending at 10 s − 1 ms, an error at 10 s, which is the connect timer rather than the 65 s read timer |

The whole group runs in about 0.04 s. The wiremock tests and `SOON` are gone from this module, and
`sse_server` now takes owned `String`s (no more `Box::leak`).

**Does it bite?** I made temporary mutations to `http::feed_client`, then restored it and checked it
with `diff`:
- read timeout +300 s: the idle test fails.
- a `.timeout(DEFAULT_TIMEOUT)` added: the idle, keep-alive and no-total tests fail.
- `read_timeout(idle / 5)` (13 s, below the 20 s keep-alive): the idle, keep-alive and no-total tests fail.
- `connect_timeout` removed: the connect test fails, at the `HUNG` wait after 60 s real time.

## 3. On a host ten times slower

- **Event-feed tests:** the result is the same. The virtual timeline is fixed, and a slow host only
  stretches the real-time waits for loopback I/O, during which no timer can fire. The test fails
  only if a single I/O step takes more than 60 s of real time, and then it reports "hung".
- **`EXPECT` waits:** they return when the thing arrives. The 5 s that used to fail a slow CI start
  now has 12× the headroom, and a failure reads as hung, not slow.
- **`settle()`:** waits for the negotiation itself, however long it takes up to 60 s, so it no longer
  fails falsely on a slow host.
- **`expect_closed`:** returns on the close. A slow host only delays it.
- **Area test:** ordering-based, so it is unaffected. **Dedupe test:** the count is exact at arrival.
  It could read 2 only if more than 5 s (`min_interval`) passed between the batch being published
  and the vessel arriving, which is the same exposure the existing `SETTLE` check already had.
- **UDP case:** keeps resending for up to 60 s instead of 10 s.

## 4. Runs

Test binaries run directly, via a script in my scratchpad.
- **10 in a row, idle machine:** all 10/10: `rustak-client` `control::events`, `feed_sidecars`,
  `hostile_server_name`, `marti_channels`, `mission_dest`, `services_flow`, `sidecar_enrolment`,
  `sidecar_trust`, `stream_channel_state`, `stream_idle`, `stream_routing`, `stream_session`,
  `stream_store`. The slowest single run was 6 s (`stream_idle`, which is 2 s idle windows by design).
- **Under load:** 2 runs each, beside a `CARGO_BUILD_JOBS=4 cargo build --release -p rustak-server`,
  with a machine load average of 139 (other agents were building too). All 2/2.

## 5. SETTLE uses I doubt (listed, not changed, because the files are outside mine)

All of these are genuine negatives in that a slow host can only produce a **false pass**, never a
false fail. Most race a relay they cannot see:
- **Unanchored:** nothing shows the message was even routed before the 400 ms ran out.
  `stream_routing.rs` :67 (other channel), :99 (receive-only publishes), :284 (callsign across
  channels), :343 (own flow tag), :383 (incognito broadcast); `stream_store.rs` :166, :195;
  `marti_channels.rs` :179 (channel switched off); `stream_session.rs` :83 (incognito replay), :279.
  The fix is the barrier `mission_dest::first_after` already uses: a second, allowed message on the
  same connection, and assert it arrives first.
- **Anchored:** a positive from the same fan-out was already seen. They are sound unless per-connection
  writer tasks lag by more than 400 ms relative to each other. `stream_routing.rs` :125, :172, :252,
  :397, :437, :474; `stream_store.rs` :137; `stream_session.rs` :223; `mission_dest.rs` :426/:430;
  `marti_channels.rs` :227, :682; `feed_sidecars.rs` :333 (now backed by a count).
- **Could fail falsely:**
  - `marti_channels.rs` `drain()` reads until 400 ms of quiet. A straggler arriving later hits a
    following `expect_none(SETTLE)`, for example `phone` at :227.
  - Fixed sleeps before a negative: `stream_session.rs:78` (150 ms for incognito to be processed
    before the newcomer connects) and `stream_routing.rs:379` (100 ms). If incognito is not applied
    in time, the relay happens and the test fails falsely.
  - `stream_channel_state.rs:159` and `stream_routing.rs:493` drain with a 150 ms `expect_none`.

## 6. Other host-bound waits seen, not mine to change

- `marti_channels.rs` `await_only`: 200 × 10 ms.
- `stream_store.rs:428`: 200 × 20 ms.
- `api_v1_map.rs:171`: 200 × 10 ms.
- `workload_identity.rs:285`: 200 × 10 ms (M10-07).
- `stream_idle.rs` `a_client_that_keeps_being_written_to…`: `IDLE = 2 s` is the server's idle timeout
  under test, on a real clock. A relay stalled for 2 s would reclaim BRAVO.
- `rustak-client/src/stream/testing.rs` `SETTLE_BUDGET = 250 ms` in `Eud::send`: the time a sender
  gets to finish its own negotiation. It is a real-time budget in shared test support.

## Files

Changed:
- `rustak-client/src/control/events.rs` (tests only)
- `rustak-server/tests/stream_support/mod.rs`
- `rustak-server/tests/feed_sidecars.rs`
- `rustak-server/tests/stream_session.rs` (EXPECT and SETTLE `is_err()` cases → `expect_closed`)
- `rustak-server/tests/stream_idle.rs` (same pattern → `expect_closed`; outside the named files, a minimal change)

Added:
- `.claude/plan/status/M10-08-host-independent-tests.md`

`rustak-server/tests/feed_support/mod.rs` needed no change. I made no `git` or `but` writes.

## Exit checks (final tree)

- `cargo fmt --check`: exit 0
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`: exit 0
- `./scripts/check-file-length.sh`: exit 0
- `cargo test -p rustak-client`: exit 0 (326 + 11 + 0 + 2 + 16)
- `cargo test -p rustak-server --no-fail-fast`: exit 0, all 37 test binaries ok (2377 tests)
