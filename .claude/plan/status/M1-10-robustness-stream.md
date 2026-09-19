# M1-10 — Robustness fixes from R-03: the stream side

**Status: complete for the six findings the brief assigns.** C1, H1, H2, M2, M4 and M7 are fixed and
tested. C1 and H1 — the two the orchestrator asked for first — are done, and both carry a test that
does not terminate against the old code.

## Disposition, finding by finding

| # | Disposition | Where |
|---|---|---|
| **C1** | Fixed | `stream/{replay,subscription,connection,metrics,live}.rs`, `config/stream.rs`, `config/validate.rs`, `config.example.toml` |
| **H1** | Fixed; TCP keepalive not done (out of brief — see deviations) | `stream/{writer,connection,metrics}.rs`, `config/stream.rs`, `config.example.toml` |
| **H2** | Fixed; the cap is in `dest.rs`, not in `rustak-cot` (see deviations) | `stream/groups.rs` (new), `stream/{dest,router,live,metrics}.rs` |
| **M2** | Fixed | `stream/{hub,registry}.rs` |
| **M4** | Fixed | `stream/mod.rs` |
| **M7** | Fixed | `cot_store/{mod,latest,writer}.rs`, `stream/router.rs` |

## Files modified

```
config.example.toml
rustak-server/src/config/stream.rs
rustak-server/src/config/validate.rs
rustak-server/src/cot_store/latest.rs
rustak-server/src/cot_store/mod.rs
rustak-server/src/cot_store/query.rs            (test call sites only)
rustak-server/src/cot_store/writer.rs
rustak-server/src/marti/groups.rs               (one log line — see deviations)
rustak-server/src/missions/archive.rs           (one test call site)
rustak-server/src/stream/connection.rs
rustak-server/src/stream/dest.rs
rustak-server/src/stream/groups.rs              (new)
rustak-server/src/stream/hub.rs
rustak-server/src/stream/live.rs
rustak-server/src/stream/metrics.rs
rustak-server/src/stream/mod.rs
rustak-server/src/stream/registry.rs
rustak-server/src/stream/replay.rs
rustak-server/src/stream/router.rs
rustak-server/src/stream/subscription.rs
rustak-server/src/stream/writer.rs
rustak-server/tests/api_v1_live.rs              (one test call site)
rustak-server/tests/marti_cot.rs                (one test call site)
rustak-server/tests/stream_store.rs
```

## C1 — the connect-time replay

Four changes, and the first two are the fix.

1. **`ConnHandle::send_replay`** — `send().await` under a timeout, and **exempt from the close
   counter**. It increments `tx_msgs` and touches neither `dropped` nor `dropped_consecutive`, so a
   client is never closed over its own map. That was the critical half: above `queue_len +
   close_after_drops` peers the `try_send` loop's discards were consecutive drops, and the 513th
   closed a connection the client had not yet read a byte from.
2. **`replay_latest_sa` is `async`** and sends through it. The writer task is already running by the
   time the replay starts (`connection::run` spawns it first, and the comment there now says why),
   so awaiting is real backpressure on one joining client rather than a stall.
3. **A deadline**, `[stream.limits] write_timeout`, over the whole replay rather than per message —
   `timeout_at` against one deadline is both. A peer that completed its handshake and then stopped
   reading would otherwise park a connection task that has not started reading and so cannot be
   reclaimed by the idle timeout. What does not fit is counted and logged, and the connection is
   still served.
4. **`queue_len` is validated against `max_connections`** at load, as a warning naming both numbers.
   `config.example.toml` documents the relationship on both keys.

`StreamMetrics::replay_dropped` is the counter the review asked for. It is separate from
`dropped_queue` because it means something different: non-zero here with `dropped_queue` at zero is
a configuration to change, not a network to investigate.

**Tests.** `a_client_joining_a_full_house_receives_all_of_it_and_is_not_closed` registers 900 fake
subscribers, connects one client with the shipped 256-deep queue and a task draining it, and asserts
all 900 arrive, `replay_dropped == 0`, and the connection is not closed. Against the old code that
connection is closed part-way through. `a_replay_nobody_is_reading_gives_up_rather_than_waiting_for_ever`
pins the other side: a 4-deep queue nothing drains takes 4 of 20, counts 16, and leaves the
connection open.

## H1 — the writer task's lifetime

Three bounds, because the writer can stall in three different places.

- **Waiting.** `writer::run` takes the connection's `Shutdown` and selects it against `rx.recv()`.
  `rx.recv()` only ends when *every* sender is dropped, and a `ConnHandle` clone held by something
  mid-route keeps one alive. What cancellation ends is the wait, not the work: `next()` still drains
  whatever is already queued, because the close is a statement about the future and the router
  counted those messages as delivered a moment ago.
- **Writing.** `feed`, `flush` and `close` each run under `[stream.limits] write_timeout`
  (default 30 s). `feed` is included deliberately: it buffers rather than writing, but `FramedWrite`
  writes through past its high-water mark, so on a stalled socket it awaits too.
- **Neither.** `connection::run` now holds the `JoinHandle` and **aborts** it after the five-second
  drain. A `timeout` around a `JoinHandle` stops waiting without stopping anything, and because
  `tokio::io::split` closes the socket only when both halves drop, the task that survived held the
  descriptor and the TLS session for the life of the process. `StreamMetrics::writer_aborted`
  counts each one.

**Tests.** `a_peer_that_stops_reading_is_given_up_on_within_the_write_timeout` writes eight messages
into a 16-byte duplex whose peer never reads, with a 100 ms budget, and asserts the writer returns.
Against the old code that test does not terminate.
`a_closed_connection_stops_the_writer_even_with_a_sender_still_held` holds a sender open across the
close and asserts the writer still stops — and that the message queued before the close is written.

**Not done: TCP keepalive.** The review's third suggestion (`TCP_KEEPALIVE` via `socket2` at
`listener_tls.rs`) is not in the brief's deliverable list and would add a workspace dependency. The
write timeout is what actually reclaims the socket; keepalive would only shorten the detection
window for a peer that is neither reading nor being written to. Recorded for whoever wants it.

## H2 — the hostile `<marti>`

Three changes, and all three are needed: the cap alone still leaves 64 database reads per message,
and the cache alone still leaves a quarter of a million lookups.

1. **`MAX_DESTS = 64`** in `dest::partition`, with the excess dropped and counted
   (`StreamMetrics::dests_truncated`). Truncated rather than refused: a client this server does not
   recognise is still worth delivering to the people it named first.
2. **Every list is deduplicated** as it is built — callsigns, uids and groups. A linear scan rather
   than a set, because the list is at most 64 long.
3. **`stream/groups.rs` (new): `GroupCache`.** `<dest group>` resolves against a cached
   `GroupIndex` instead of `db.groups().get_by_name(..)`. A miss re-reads the channel table at most
   once per second, behind a `tokio::sync::Mutex` so a burst of misses is one read rather than one
   each — a miss is exactly what an attacker sends, so re-reading on every one would hand the denial
   of service straight back. `GroupCache::invalidate`, reachable as `Router::channels_changed` and
   `LiveState::channels_changed`, is there for whatever creates or deletes a channel and would
   rather not wait the second.

No lock is held across a database call: `group_recipients` resolves the bit position first and only
then takes the hub's read lock, which it releases before returning. The registry read-lock hold the
review also flagged (`Hub::resolve` iterating the key list under the lock) is bounded by the same
cap — at most 64 keys instead of 250,000.

`select_recipients` now takes a `Selecting<'_>` context struct. Five separate parameters would have
put it at eight and tripped `clippy::too_many_arguments`, and the five are the same for every
message the listener routes.

**Tests.** `a_hostile_destination_list_is_capped_and_deduplicated` partitions the review's exact
frame — 250,000 `<dest group="__ANON__"/>` — and asserts one group, 249,936 counted as truncated,
and under 50 ms. `a_flood_of_misses_does_not_become_a_flood_of_reads` sends 100,000 misses at the
cache and asserts under two seconds, which is only possible if they are not reaching SQLite. Plus
dedup, cap-keeps-the-first, and a created-then-invalidated channel.

## M2 — a callsign change re-indexes

`Hub::apply_event` now reads the callsign and `client_uid` before and after
`Subscription::apply_event` and calls `Registry::reindex` on each. `reindex` returns immediately
when nothing moved, so the ordinary case — a client reporting the same name it reported two seconds
ago — costs two string comparisons and no map work. `first_identity` no longer gates the indexing;
it is still reported, because callers use it for other things.

This closes the leak in the same place as well as the fault: `unindex` only ever removes the name a
subscription is *currently* carrying, so every rename used to leave a `HashMap` entry holding a dead
`ConnId`, for the life of the process.

**Tests.** `renaming_a_device_moves_it_in_the_callsign_index` (the new name resolves, the old one
resolves to nobody) and `a_rename_leaves_nothing_behind_in_the_index` (sixteen renames, then
unregister, then both index maps are empty).

## M4 — shutdown ordering

Exactly the change M1-09's status note specified. The CoT store gets its own `Shutdown`, held on
`StreamRuntime`, and `run` cancels it **after** `listener_tls::run` has returned and before the
`timeout(self.drain, self.store)`. The field carries the reason it is not
`context.shutdown().child()`, so the next person to tidy it finds the answer in place.

It is not `Shutdown::child()` because a child is cancelled with its parent, which is the bug. It
loses the parent's `abort` as a result, which costs nothing: the store is cancelled the moment the
listener returns, and an abort makes the listener return at once.

**Test.** `stream_store::what_the_stream_relayed_is_written_before_the_server_finishes_stopping`
relays a message, pumps the connection so it has certainly been routed and recorded, then stops
without waiting for the store and asserts the message is in `cot_latest`.

**Honest limit of that test.** It is an *invariant* test, not a reproduction of the race. The race
itself — a connection task inside `handle_inbound` when the store writer finishes its final drain
and drops the receiver — needs the connection task paused mid-route to force, which nothing in the
harness can do. The test guards the invariant ("stopping flushes what the stream relayed") and would
fail if the store were ever stopped before the drain or the wait removed. Note also that the
review's framing is slightly wider than what the code does: `read_loop` is `biased` on the
connection's `closing` token, so connections stop *reading* the moment shutdown is requested. The
lost window is therefore the race, not the whole eight seconds.

## M7 — nothing is encoded until the store wants it

`CotRecord` holds an `Arc<EncodedEvent>` instead of an owned `String` and an owned `Vec<u8>`, with
`xml()` and `proto()` accessors. Both encodings used to be forced in `CotRecord::new`, **on the
sending client's task**, before `CotStoreHandle::record` had a chance to `try_send` and possibly
drop the record — so an all-XML fleet paid a protobuf encode per message it would never transmit,
and all of it was wasted exactly when the store was behind, which is when the server is already
under pressure.

The two consumers now take what they need on the writer's own task, once, out of the `OnceLock` the
relay may already have filled: `history.append(.., record.proto())`, and `latest::upsert_batch`
binding `String::from_utf8_lossy(record.xml())` — borrowed, not copied, whenever the bytes are valid
UTF-8.

**Tests.** `a_record_shares_the_relayed_encoding_rather_than_copying_it` asserts pointer equality
with the relayed buffers (a copy would be a different allocation). `building_a_record_encodes_nothing`
builds 2,000 records and then encodes the same 2,000 events once each, and asserts the first is
faster than the second — which inverts against the old code, where building did the encoding.

## Deviations

1. **`MAX_DESTS` is enforced in `stream/dest.rs`, not in `rustak_cot::detail::marti::take_marti`.**
   The review put it at the parse site; `rustak-cot` is not in this brief's file list. The frame is
   already capped at 8 MiB and the XML parser already pays for the elements, so capping at
   `partition` removes every database read and every registry lookup the review is about. Moving it
   into `take_marti` later would also stop the `Vec<Dest>` allocation, and the review's fuzz target
   for it is still worth having.
2. **`cot_store/{mod,latest}.rs` were edited for M7.** The brief names `cot_store/writer.rs`, but
   `CotRecord` is declared in `mod.rs` and read in `latest.rs`; the finding cannot be fixed without
   both. `query.rs`, `missions/archive.rs`, `tests/api_v1_live.rs` and `tests/marti_cot.rs` each
   have exactly one mechanical call-site change (`&encoded` → `Arc::new(encoded)`).
3. **`LiveState::resend_latest_sa` now spawns**, and returns how many connections it started a
   replay for rather than how many events it sent. The replay applies backpressure now, so awaiting
   it inside `PUT /Marti/api/groups/active` would put an HTTP response behind somebody else's slow
   radio for up to `write_timeout` per connection. The map arriving is a side effect on the stream.
   One log line in `marti/groups.rs` changed with it (`events` → `connections`, and the wording).
   `LiveState` gained a `with_replay_budget` builder rather than a fifth constructor argument, so
   the two existing `LiveState::new` call sites — one of them in `web/api/clients.rs`, which this
   brief does not own — still compile unchanged.
4. **The `queue_len` rule lives in `config/stream.rs`, not `config/validate.rs`.** It is a rule
   *within* one section, and `validate.rs` documents itself as the home of cross-section rules.
   `validate.rs` gained one line calling `config.stream.limits.validate()`. It also keeps that file
   under the 300-line limit, which it would otherwise have exceeded.
5. **No `stream_*.rs` integration test for H1's "task count returns to baseline".** Making a peer
   stop reading through the harness means filling the kernel's send buffer, whose size is not
   something a test can rely on — a flaky gate is worse than none. The two writer tests are the
   deterministic equivalent (the first does not terminate without the fix), and the file-descriptor
   soak is load test #3 in R-03, which the review itself scopes to a nightly `load_*` target.
6. **`StreamMetrics` gained `writer_aborted` and `dests_truncated`** beyond the `replay_dropped` the
   brief asks for. Each is the visibility signal for the fault it sits beside.

## For whoever integrates

* **`LiveState::channels_changed()` is not called yet.** `identity::groups::{create,patch,delete}`
  (`rustak-server/src/identity/groups.rs`) is where it belongs, and that file is not this brief's.
  Nothing is broken without it: the cache refreshes itself within a second of a miss, so a new
  channel is routable within a second of being created. Calling it makes that immediate.
* `stream/mod.rs` and `stream/live.rs` both changed in ways M1-09 also touched one line of
  (`with_open_history_logs`). That line is intact.

## Exit checks

```
$ cargo fmt --all --check
Clean.

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile — no lints.

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished — no warnings.

$ bash scripts/check-file-length.sh
Clean. (config/validate.rs is at the limit; see deviation 4.)

$ cargo test -p rustak-server --lib
running 1703 tests
test result: ok. 1701 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 82.49s

    New, from this brief:
    stream::replay::tests::a_client_joining_a_full_house_receives_all_of_it_and_is_not_closed
    stream::replay::tests::a_replay_nobody_is_reading_gives_up_rather_than_waiting_for_ever
    stream::writer::tests::a_peer_that_stops_reading_is_given_up_on_within_the_write_timeout
    stream::writer::tests::a_closed_connection_stops_the_writer_even_with_a_sender_still_held
    stream::dest::tests::a_hostile_destination_list_is_capped_and_deduplicated
    stream::dest::tests::a_name_repeated_is_a_name_resolved_once
    stream::dest::tests::a_list_past_the_cap_keeps_the_ones_the_sender_named_first
    stream::groups::tests::{a_channel_resolves_to_its_bit_position,
        a_channel_that_does_not_exist_resolves_to_nothing,
        a_flood_of_misses_does_not_become_a_flood_of_reads,
        a_channel_created_after_the_map_was_read_is_found}
    stream::hub::tests::renaming_a_device_moves_it_in_the_callsign_index
    stream::hub::tests::a_rename_leaves_nothing_behind_in_the_index
    cot_store::tests::a_record_shares_the_relayed_encoding_rather_than_copying_it
    cot_store::tests::building_a_record_encodes_nothing
    config::stream::tests::{the_write_timeout_has_a_default_an_installation_never_has_to_write,
        the_shipped_defaults_are_a_configuration_that_validates,
        a_queue_below_the_connection_ceiling_is_a_warning_rather_than_a_refusal,
        a_limit_that_could_never_work_is_refused_by_name}

$ cargo test -p rustak-server --tests --no-fail-fast
Every integration target green, 0 failures:
    acme_directory 5, api_v1_live 14, api_v1_packages 9, bootstrap 3, enroll_flows 14,
    enroll_oauth 11, marti_channels 11, marti_contract 14, marti_cot 11, mission_dest 11,
    mission_squash 4, missions_authz 7, missions_extras 14, missions_flow 14, oauth_flows 31,
    profiles_contract 14, services_flow 2, stream_channel_state 3, stream_routing 12,
    stream_session 11, stream_store 9, sync_contract 15.

    stream_store is 9 rather than 8: the new one is
    what_the_stream_relayed_is_written_before_the_server_finishes_stopping.

$ cargo test --workspace --no-fail-fast
Every crate green, 0 failures.

$ cd interop/node-tak && npm test
ℹ tests 25   ℹ pass 25   ℹ fail 0

$ cd interop/eud && npm test
ℹ tests 43   ℹ pass 43   ℹ fail 0
```

No `git` or `but` command was run.
