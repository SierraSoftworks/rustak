# M9-09 — AISStream delivers JSON in binary WebSocket frames; the AIS source now reads them

**Status: complete.** Every exit check is green except the workspace
`cargo fmt --check`, which fails only in M9-07's in-flight files (recorded
below with the per-file `rustfmt --check` that passes on mine). No file outside
the "files you own" list was edited — the two new metrics reach the heartbeat
through `status.rs` alone, which is why `ConnectionTx`/`ConnectionRx` became
handles rather than bare `watch` aliases (§2).

The bug in one line: `stream()` matched `Message::Text` and sent everything else
to `debug!("A frame this source does not read.")`; AISStream sends every message
— the subscription confirmation and every position report — as a **binary**
frame whose payload is the same UTF-8 JSON. It subscribed, and then it dropped
the entire feed at `debug`.

## What landed

| Area | Files |
|---|---|
| The frame reader, factored out and unit-tested | `rustak-plugin-ais/src/sources/aisstream/frames.rs` (new) |
| The connection loop, the key's journey, one-warn-per-reason | `rustak-plugin-ais/src/sources/aisstream.rs` |
| The counters, the notice, the heartbeat they reach | `rustak-plugin-ais/src/status.rs` |
| One paragraph in the AISStream section | `rustak-plugin-ais/README.md` |

`lib.rs`, `sources/mod.rs` and `sources/udp.rs` are untouched and still compile
against the changed `status.rs` types; `cargo check --workspace --all-targets`
passes, so `rustak-server`'s suites are unaffected.

## 1. Binary frames, and everything else a frame can be

`Frames::accept(Message) -> Result<Step, String>` is the whole frame loop, one
frame at a time, with the socket I/O left to the caller (`Step::KeepAlive` is
"flush the pong", `Step::Closed` is "the server closed"). `Text` and `Binary`
are read the same way — UTF-8, then JSON — and each JSON message is one of five
things:

| What arrived | What happens |
|---|---|
| An object with an `error` key, in any case | The connection fails with the server's words (§3) |
| `SubscriptionConfirmation` | Counted as decoded, logged once at `info` |
| A `wire::Envelope` | Counted as decoded, its observation sent to the tick |
| JSON that is none of those | `source.messages_ignored` += 1, `debug` |
| Bytes that are not UTF-8, or text that is not JSON | `source.frames_undecoded` += 1, `debug` |

`Ping`/`Pong`/`Close` are protocol, not data: they count toward neither counter
and do not count as "a frame arrived" for §4.

## 2. Why `ConnectionTx`/`ConnectionRx` are now handles

The brief puts the two counters in the `FeedStatus`/`status.rs` builder, and
`FeedStatus` is built in `lib.rs`, which is not mine to edit. Adding a field to
`FeedStatus` would have broken `lib.rs`'s struct literal, so the counters live
where `lib.rs` already reads from: the value behind the connection channel.

- The channel carries `SourceState { connection, dropped, notice }`.
- `ConnectionTx` keeps `send_replace(Connection)` — so `sources/mod.rs` and
  `udp.rs` are unchanged — and gains `frame_undecoded()`, `message_ignored()`,
  `dropped()`, `notice()`, `clear_notice()`. Each is a `send_modify`, so a
  counter bump never replaces the connection and never blocks the tick.
- `ConnectionRx::borrow()` answers an owned `SourceState`, and `SourceState`
  derefs to its `Connection`, which is what keeps `udp.rs`'s
  `matches!(*state.borrow(), Connection::Connected { .. })` compiling and
  reading the way it did.

The counters are per-process, not per-connection: they outlive the connection
that dropped the frames, which is what an operator comparing two heartbeats
wants.

The `source` metric gains `frames_undecoded`, `messages_ignored` and — when
there is one — `notice`:

```json
"source": {"kind": "aisstream.io", "state": "connected", "since": "…",
           "frames_undecoded": 412, "messages_ignored": 0,
           "notice": "Connected to aisstream.io for two minutes and none of the 412 frames that arrived could be decoded (412 undecoded, 0 ignored)."}
```

A connected source carrying a notice reports `degraded`, not `healthy`, and the
notice is appended to the heartbeat's sentence — a socket that is open and
understands nothing is exactly the failure that otherwise reads as healthy.

## 3. An error reply is a failure, and never carries the key

A JSON object with an `error` key (matched case-insensitively; the service's
documented shape for a rejection is not published, so the key is all this
matches on) ends the connection with
`the service refused the subscription (<the server's text>)`. That reason
becomes `Connection::Reconnecting { last_error }` — so the Services page says
it — and the ordinary capped backoff applies, so an invalid key waits 1 s, 2 s,
4 s … 60 s rather than spinning.

Two things about the key:

- **It is redacted out of the server's text.** The socket task now also holds
  the bare key (`run`/`stream` take `api_key: &Secret`) for no other purpose
  than `text.replace(key, "***")`, guarded against an empty key so that an
  unset credential cannot turn the marker into a separator. A test feeds
  `{"error": "Api Key rsk_supersecret Is Not Valid"}` and asserts the failure
  keeps `Api Key … Is Not Valid` and loses `rsk_supersecret`.
- **The warn is once per reason.** `run` remembers the last reason it reported:
  the first occurrence is `warn`, repeats are `debug`, and a clean close
  forgets it. A key the service will never accept used to write the same line
  every minute for the life of the process; the heartbeat is what keeps saying
  it now. (This is the same shape M9-08 is about to apply to both plugins'
  logging; it is deliberately only this one line's worth.)

## 4. Two silences, each said once per connection

Per the orchestrator's mid-task correction, the warning keys on **decoded
messages**, not on position reports — Dublin Bay at night is about one report
every twenty seconds, and a quiet box must never warn. Two minutes after
subscribing:

| Condition | What is said |
|---|---|
| No frame of any kind arrived | *Subscribed to aisstream.io two minutes ago and not one frame has arrived; check the API key and the bounding box.* |
| Frames arrived, none decoded | *Connected to aisstream.io for two minutes and none of the N frames that arrived could be decoded (X undecoded, Y ignored).* |
| Anything decoded, confirmation included | Nothing. |

Each fires once per connection, at `warn` and into the heartbeat notice, and a
notice is taken back (with one `info`) the moment something does decode. The
check runs after every frame and, so that a connection nothing ever arrives on
is still heard from, on one `sleep_until` wake-up in the connection's `select!`
— disabled again as soon as the deadline has passed, so it cannot spin.

The clock is `tokio::time::Instant`, so the three silence tests drive it with
`#[tokio::test(start_paused = true)]` and `tokio::time::advance` rather than
capturing logs.

## 5. Tests

Ten new tests in `frames.rs`, three in `status.rs`, all on hand-written JSON —
no captured traffic is copied into rustak:

- a binary frame and a text frame carrying the same `PositionReport` both
  become an `Observation`, and nothing is counted as dropped;
- `{"MessageType": "SubscriptionConfirmation"}` is recognised and counted as
  neither undecoded nor ignored;
- an error reply, in both `error` and `Error` spellings, ends the connection
  with the server's words and without the key;
- a non-UTF-8 binary frame and a text frame of nonsense are counted, not fatal;
- an unrecognised message type is counted as ignored, not undecoded;
- ping and close are answered rather than decoded;
- each of the two silences fires once, a confirmed quiet box never warns, and a
  standing notice is withdrawn when the stream starts decoding;
- the counters, the notice and the `degraded` state reach the heartbeat, and
  taking back a notice does not forget the counts.

`sources::aisstream::tests::a_decoded_message_becomes_a_track_on_the_next_poll`
now feeds its two messages through `Frames::accept` as binary frames, which is
the path production uses. The old
`nonsense_on_the_wire_is_dropped_rather_than_dropping_the_connection` is gone:
its three fixtures are covered frame-by-frame in `frames.rs`, and one of them
(`{"MessageType":"Error","Error":"unauthorised"}`) asserted the behaviour this
brief deliberately reverses.

## 6. What I could not verify

- **Against the live service.** No API key was used in this session; the
  binary-frame shape is taken from the brief, the orchestrator's independent
  probe (first frame ~200 ms after subscribing is
  `{"MessageType":"SubscriptionConfirmation"}`) and aisstream.io's
  documentation page, which states the service sends binary frames with UTF-8
  JSON payloads.
- **The exact error-reply shape.** The documentation page does not publish one;
  it only says an invalid subscription gets no confirmation and lists "an
  invalid key" among the reasons a connection is closed. The matcher is
  therefore deliberately loose — any top-level key spelled `error` in any case,
  with the value rendered as-is — and an AIS envelope has no such key, so a
  real message cannot trip it.

## 7. Exit checks

```
$ cargo fmt --check            # workspace
exit: 1
Diff in …/rustak-client/src/http.rs
Diff in …/rustak-client/src/sidecar/control_link.rs
Diff in …/rustak-client/src/sidecar/link_health.rs
Diff in …/rustak-server/tests/sidecar_trust.rs
```

**All four are M9-07's in-flight files; none is mine.** Per the brief, the
per-file check on my own:

```
$ rustfmt --check --edition 2024 rustak-plugin-ais/src/status.rs \
      rustak-plugin-ais/src/sources/aisstream.rs \
      rustak-plugin-ais/src/sources/aisstream/frames.rs
exit: 0
```

```
$ cargo clippy -p rustak-plugin-ais --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.45s

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-plugin-ais --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.37s
   Generated …/target/doc/rustak_plugin_ais/index.html

$ ./scripts/check-file-length.sh
exit: 0
# status.rs 176, aisstream.rs 154, aisstream/frames.rs 187 functional lines

$ cargo test -p rustak-plugin-ais
running 76 tests
test result: ok. 76 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s

$ cargo check --workspace --all-targets     # not required; run because status.rs's types changed
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 48.39s
```

## 8. For the deployment

Pull the new image and watch for one `info` line about 200 ms after
*"Subscribed to the AIS stream."*: **AISStream confirmed the subscription.**
If that line is absent two minutes later, the heartbeat and one `warn` now say
which of the two silences it is. `source.frames_undecoded` climbing while
`tracked` stays at zero is the signature of this bug returning.

## 9. For M9-08

`rustak-plugin-ais/src/sources/aisstream.rs`'s `run` already does the
first-at-`warn`, repeats-at-`debug` dance for the connection reason, and
`frames.rs` keeps its per-frame chatter at `debug`; the `ConnectionTx::notice`
handle is the AIS equivalent of ADS-B's `SourceState` for anything M9-08 wants
to surface instead of logging.
