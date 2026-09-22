# M9-15 — A quiet client on a busy stream must not be dropped

**Done.** The server's idle timer now measures both directions, every disconnect
names its cause and how long the connection lasted, the client SDK pings on
outbound silence as well as inbound, and a reconnecting client's `attempts` no
longer reads like a failure count. A deployment changes nothing.

## The rule, as implemented

> A connection is reclaimed for idleness only when **nothing has been received
> from it and nothing has been successfully written to it** for
> `[stream.tls] idle_timeout`.

`rustak-server/src/stream/liveness.rs` (new) holds one `Liveness` per
connection: two millisecond clocks relative to the accept, plus the cause of
death. The read loop marks `last_rx` on every framed message, *before* parsing —
a client sending nonsense is still a client — and the writer marks `last_tx`
when a flush returns, which is the point at which the bytes have left this
process.

`read_loop` no longer wraps the read in `tokio::time::timeout`. It selects on
`sleep_until(liveness.idle_deadline(idle_timeout))`; when that fires it re-reads
the deadline, and a write that landed while the task was parked simply moves it
and the sleep is re-armed. Cost is at most one extra wake-up per `idle_timeout`
per connection.

**No server-initiated ping.** Nothing in `compat/streaming.md` has the server
ping, and a completed write already says what a ping would.

**A vanished peer under outbound traffic** is reclaimed by the two paths that
already existed, and both are now deterministic about it:

- `write_timeout` — the peer's socket stops taking bytes, the writer's flush
  overruns, and the writer now *records the cause and cancels the connection's
  token* instead of returning quietly and leaving the read side to wait out the
  idle timeout. Test: `writer::tests::a_peer_that_stops_reading_is_given_up_on_and_named_as_a_write_timeout`.
- drop-then-close — the queue fills, `close_after_drops` consecutive drops call
  `ConnHandle::close(LeaveReason::SlowConsumer)`.

A peer that has vanished with **nothing** going either way is reclaimed by the
idle timeout exactly as before; by then it has also missed its own 15 s ping.

## The leave reasons

Ten, and they are the causes that exist in the code rather than the brief's
draft list. `replaced` is not among them: nothing in the listener replaces a
connection (two devices may legitimately share a `clientUid`, and the registry
indexes a list). The brief's `write_timeout` is split in two, because a peer
that stopped reading and a socket that broke are different things to an
operator. `account_disabled` and `administrator` are close paths the brief's
list did not mention and that `live.rs`, `marti/subscriptions.rs` and
`web/api/clients.rs` have always had.

| `reason` | Set where |
|---|---|
| `client_closed` | `read_loop`, on end-of-file |
| `idle` | `read_loop`, when `max(last_rx, last_tx) + idle_timeout` has passed |
| `read_error` | `read_loop`, on a framed read error |
| `write_timeout` | `writer::{flush,fed}`, on overrunning `write_timeout` |
| `write_error` | `writer::{flush,fed}`, on an encode or I/O failure |
| `slow_consumer` | `ConnHandle::send`, at `close_after_drops` consecutive drops |
| `revoked` | `hub::disconnect_by_fingerprint` (the revocation hook) |
| `account_disabled` | `LiveState::disconnect_by_user` |
| `administrator` | `DELETE /api/v1/clients/{uid}`, `DELETE /Marti/api/subscriptions/{uid}` |
| `shutdown` | a cancelled token with no cause recorded — i.e. the listener draining |

`ConnHandle::close` takes a `LeaveReason` now, so a close path cannot forget to
name one: the compiler asks. The first cause recorded wins (`Liveness::ended`,
a `compare_exchange` on an `AtomicU8`), because a peer that vanishes mid-flush
produces a write timeout *and then* a read error, and only the first is
actionable. `StreamMetrics.left` counts one per cause
(`metrics::LeaveCounters`, indexed by the variant so a new cause cannot lose
its counter).

## The keepalive constants

| | Value | Clock |
|---|---|---|
| `idle` | 15 s | inbound silence, ATAK's |
| `repeat` | 4.5 s | inbound silence, ATAK's |
| `dead` | 25 s | inbound silence, ATAK's — **unchanged, and still inbound only** |
| `outbound_idle` | **30 s** | ours: silence on what *we* have written |

`Keepalive::ATAK` still means exactly ATAK (`outbound_idle: ZERO`), so whatever
compares rustak against the reference client keeps comparing against the same
numbers. The shipped default is the new `Keepalive::DEFAULT` = ATAK + 30 s
outbound, which is what `StreamConfig::new` uses; `Keepalive::OFF` disables
everything, and `is_off()` now means "all three rules are off" rather than
"there is no death clock".

Any frame written resets the outbound clock (`TakStream::poll_outbox` calls
`record_tx` on each `start_send`), pings included — so a publishing sidecar
never sends one, and a receive-only sidecar sends exactly one every 30 s. When
the rule fires it also throttles itself on `repeat`, so a socket that will not
take the ping cannot be handed another on every poll.

## The lines an operator sees

Server, once per disconnect (`info`, inside the connection's span, which already
carries `conn`, `user` and `peer`):

```
A client left the stream. reason=idle connected_for=90.002s rx=5 tx=305 dropped=0
A client left the stream. reason=client_closed connected_for=11220.418s rx=1128 tx=90411 dropped=0
A client left the stream. reason=shutdown connected_for=412.88s rx=96 tx=3310 dropped=0
```

(`connected_for` is a `Duration` rendered with `Debug`, so it reads `90.002s`,
`11220.418s`, `1.2ms` — seconds throughout, never `1h02m`.)
The `debug` line that accompanies an idle reclaim now says what it means:
`A stream connection went quiet in both directions and was reclaimed. seconds=90`.

Client (sidecar) side, unchanged except where the brief asked:

```
The TAK stream is connected. endpoint=ssl://tak.example.com:8089 attempts=1 connects=1
Could not connect to the TAK stream yet; retrying. reason=… retry_in=1s attempts=1 endpoint=ssl://tak.example.com:8089
The TAK stream went away; reconnecting. reason=the server closed the connection retry_in=1s attempts=0 connects=17
The TAK server stopped answering; treating the connection as dead. silence=25.0s uid=SERVICE-firms
```

`attempts` is now "attempts since the last successful connection" — zero on a
healthy connection, and the length of the current outage while one is going on.
`connects` is the lifetime figure: how many times this process has had a
connection up, which is what says whether a stream is flapping. The first
failure of a process, before anything has ever connected, says *could not
connect yet* rather than claiming a stream went away.

## What a deployment must change

**Nothing.** `idle_timeout` keeps its name, its default (90 s) and its meaning;
the rule only ever reclaims *less* eagerly than before, so nothing that was safe
becomes unsafe. `write_timeout` is untouched. No new configuration key exists on
either side. FIRMS (and any other receive-only sidecar) is fixed twice over:
by the server rule, and — against older rustak builds and against TAK Server —
by the SDK's own outbound ping, which needs no configuration either.

## Tests, and what they do on a host ten times slower

**No test asserts an upper bound on host-dependent work.** The pre-existing
`writer::tests::a_peer_that_stops_reading_is_given_up_on_within_the_write_timeout`
did (`started.elapsed() < 2s`); it is now clock-injected and asserts the cause
and the cancellation instead.

| Test | Clock | On a 10× slower host |
|---|---|---|
| `liveness::tests::*` (4) | `start_paused`, `advance` only | Identical. No wall-clock anything |
| `writer::tests::a_peer_that_stops_reading_is_given_up_on_and_named_as_a_write_timeout` | `start_paused`; auto-advance reaches the 100 ms write deadline because nothing else can run | Identical: asserts the recorded cause and the cancelled token, not a duration |
| `writer::tests::a_completed_flush_is_what_keeps_a_quiet_connection_alive` | `start_paused`; advances 1800 s and asserts the deadline moved by exactly 1800 s | Identical |
| `connection::tests::a_client_that_is_being_written_to_is_never_reclaimed_for_saying_nothing` | `start_paused`; ten rounds of `advance(60s)` + `wrote()` against a 90 s timeout | Identical. Asserts the loop is still running after each round, and that it then ends as `shutdown` |
| `connection::tests::a_connection_with_nothing_in_either_direction_is_reclaimed_as_idle` | `start_paused`; the runtime auto-advances to the deadline | Identical |
| `connection::tests::{a_client_that_closes_its_end_is_not_a_failure, a_socket_that_fails_is_told_apart_from_a_client_that_left, a_close_from_outside_carries_the_cause_it_was_given, the_server_stopping_is_its_own_cause}` | No timers at all | Identical |
| `keepalive::tests::*` (9) | `start_paused`, `advance` only | Identical |
| `stream::connection::tests::*` (client, 3) | `start_paused`; drains the peer with `now_or_never` after a bounded number of `yield_now`s | Identical — a fixed number of yields, not a deadline |
| `reconnect::tests::*` (6) | No timers | Identical |
| `stream_idle.rs::a_client_that_only_receives_is_never_reclaimed_for_saying_nothing` | Real listener, real TLS, `idle_timeout = 2s`. Relays until **its own elapsed time** passes `2 × idle_timeout` — a *lower* bound | Slower: fewer rounds in the same ≈4 s, each with a larger gap. The gap is a 50 ms sleep plus one loopback relay; it would have to grow 40× before approaching the 2 s timeout |
| `stream_idle.rs::a_client_that_neither_sends_nor_receives_is_reclaimed_as_idle` | Real listener, `idle_timeout = 500ms`; waits for the connection to end and for the counter, with a 20 s hung-wait budget | Slower to reach the assertion, same result. The budget only fails a hang |
| `stream_idle.rs::{a_client_that_closes_its_end_is_recorded_as_having_left, a_revoked_certificate_names_itself_on_the_way_out, a_listener_that_is_draining_says_so_rather_than_blaming_the_client}` | Real listener; poll a counter with the same 20 s hung-wait budget | Same |

The integration suite is a real regression test, not a shape test: with
`Liveness::wrote` stubbed out (the old read-idle behaviour) the first test fails
in 2.2 s with `Io(Custom { kind: UnexpectedEof, … })` at the line that expects
the relayed message — which is the production fault, reproduced. Stub removed
again afterwards; `git status` shows no stray edit.

**`write_timeout` is unit-tested rather than provoked end to end**, as the brief
allows. Filling a real TLS peer's receive window from an in-process test means
megabytes of traffic and a client that is guaranteed not to read, neither of
which is deterministic; `tokio::io::duplex(16)` is. `read_error` is likewise
covered by a unit test with a reader that fails, because provoking a TCP reset
through `tokio-rustls` from the client side is not deterministic either (a clean
close through `poll_close` arrives as `client_closed`, and the integration suite
asserts that).

## Exit checks

`cargo fmt --all -- --check` — exit 0, no output.

`cargo clippy --workspace --all-targets -- -D warnings` — exit 0:

```
    Checking rustak-plugin-example v0.1.0 (/Users/…/rustak/rustak-plugin-example)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 19.29s
```

`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` — exit 0:

```
 Documenting rustak-server v0.1.0 (/Users/…/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 19.25s
   Generated /Users/…/rustak/target/doc/rustak_api/index.html and 10 other files
```

`./scripts/check-file-length.sh` — exit 0, no output. (`stream/connection.rs`
242 functional lines, `subscription.rs` 270, `liveness.rs` 122,
`keepalive.rs` 117, `reconnect.rs` 231 — the limit is 300.)

`cargo test --workspace` — exit 0, 65 `test result: ok` lines, 3686 tests, 0
failures. The suites this brief touches:

```
Running tests/stream_idle.rs      -> test result: ok. 5 passed; 0 failed; … finished in 4.57s
Running tests/stream_session.rs   -> test result: ok. 11 passed; 0 failed; … finished in 1.55s
Running tests/stream_client.rs    -> test result: ok. 12 passed; 0 failed; … finished in 0.32s
Running tests/stream_reconnect.rs -> test result: ok. 2 passed; 0 failed; … finished in 1.01s
```

```
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

all doctests ran in 2.18s; merged doctests compilation took 1.74s
```

## Files

New:

- `rustak-server/src/stream/liveness.rs` — the two-direction idle clock and
  `LeaveReason`, with the reasoning for both in its module documentation.
- `rustak-server/tests/stream_idle.rs` — the five integration tests above.

Changed (server):

- `stream/connection.rs` — the deadline-based read loop, the reason it returns,
  the `reason`/`connected_for` fields on the disconnect line, the per-reason
  counter, and six unit tests for the loop's exits.
- `stream/writer.rs` — `WriterContext.liveness`, `last_tx` on a completed flush,
  a recorded cause on every give-up, and cancelling the connection when the
  socket will not take bytes.
- `stream/subscription.rs` — `ConnHandle::close(LeaveReason)`, `with_liveness`.
- `stream/metrics.rs` — `LeaveCounters`, one slot per cause.
- `stream/mod.rs` — the module, and the `LeaveReason`/`Liveness` re-exports.
- `stream/{notify,live}.rs`, `marti/subscriptions.rs`, `web/api/clients.rs` —
  each close path names its cause.
- `rustak-server/Cargo.toml` — `tokio`'s `test-util` in `[dev-dependencies]`,
  which is what `tokio::time::pause`/`advance` need. Same line the client crate
  has carried since M1.

Changed (client):

- `stream/keepalive.rs` — `outbound_idle`, `Keepalive::DEFAULT`, `record_tx`,
  and the deadline arithmetic that now picks the earliest of three clocks.
- `stream/connection.rs` — `record_tx` on every frame written, and three
  wire-level tests of the outbound rule.
- `stream/mod.rs` — `StreamConfig` defaults to `Keepalive::DEFAULT`.
- `stream/reconnect.rs` — `attempts` reset on success, `connects()`, and the
  first-connect wording.
- `tests/stream_reconnect.rs` — asserts the new counter semantics (it held the
  old `attempts` meaning, and was the only test that did).

Docs:

- `config.example.toml` — `idle_timeout` states the rule and points at
  `write_timeout` for the case it is not for.
- `docs/deployment.md` — a new **When a stream connection is reclaimed**
  section: the rule, what `write_timeout` covers, the reason table with what to
  look at for each, and "nothing to change in a deployment".
- `docs/plugins.md` — the SDK keeps a quiet sidecar's stream alive on its own.
- `.claude/plan/compat/streaming.md` §6 — TAK clients ping on inbound silence
  only, so a server must not treat read-silence as death while it is writing;
  and a note that the 30 s outbound ping is rustak's own rule, not ATAK's.

Not touched: CI files, `.claude/plan/{plan,backlog}.md`, and the working tree's
uncommitted `backlog.md` edit. No `git`/`but` command was run.

`docs/ci.md` reasons about the `test` job's timing band and is the CI steward's
file, so it is left alone: for the record, `stream_idle` is a new binary costing
4.5 s here, which on the slow hosts that file describes is perhaps 15 s of a
22–28 minute job.

## What I could not verify

- **Production.** Nothing here was run against the Dublin server. The fault was
  reproduced in-process instead (see *Tests*), and the fix is asserted at both
  the unit and the real-TLS-listener level.
- **A real ATAK or CloudTAK against the new server.** The interop suites were
  not run; nothing in the wire contract changed, and the one behaviour a client
  could notice — the SDK's extra `t-x-c-t` every 30 s — is a message both
  clients already answer with a pong, and only rustak's own clients send it.
- **`write_timeout` end to end.** Unit-tested, as described above.
- **Whether 30 s is the right outbound interval.** It is half the shortest idle
  timeout a TAK Server deployment is likely to run (TAK Server's own is 1–5
  minutes) and a third of rustak's default, which leaves room for one lost ping.
  No deployment has been observed with a tighter one.
