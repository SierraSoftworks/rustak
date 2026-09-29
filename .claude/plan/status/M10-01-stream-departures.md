# M10-01 — A routine departure must not read as an error

**Done.** A sidecar stopped through its shutdown token now closes its TAK
stream (flush, then `close`, so TLS sends `close_notify`) and the server records
`client_closed`. An end-of-file without `close_notify` and a connection
reset/abort are a new cause, `client_vanished`, counted separately;
`read_error` is kept for real faults. `connected_for` is seconds to one decimal,
logged as a number.

## What changed and why

- **Harness closes its stream** (`rustak-client/src/sidecar/{run,link}.rs`).
  `Link::close(within)` takes the `Reconnecting` connection and, if it is
  connected, runs `SinkExt::close` (which flushes the outbox and then shuts the
  transport down — `close_notify` on TLS) under `tokio::time::timeout`. A link
  with no stream, or one that is not connected, is just dropped. A failed or
  timed-out close is a `debug` line; nothing is returned, so the exit code is
  unaffected. `tick_until_shutdown` calls it after the loop ends and **before**
  `Sidecar::stop`, bounded by `CLOSE_WITHIN` (2s, new public const in
  `sidecar::run`) or `shutdown_grace` if shorter. `stop` still gets its full
  grace. Only the shutdown path closes; a plugin error that ends the loop with
  `?` exits 1 as before without the close (the process is dying on an error
  anyway).
  No change was needed in `rustak-client/src/stream/{connection,reconnect}.rs`:
  `TakStream::poll_close` and `Reconnecting::poll_close` already flush and
  close correctly (the existing `stream_idle` test proving `client_closed` via
  `SinkExt::close` shows it).
- **Server names the cause honestly** (`rustak-server/src/stream/liveness.rs`,
  `connection.rs`, `metrics.rs`). New `LeaveReason::ClientVanished = 11`
  (`client_vanished`), appended so existing discriminants/indices are
  unchanged; `LeaveCounters` picks it up automatically from `ALL`.
  `LeaveReason::from_read_error(&CodecError)` walks the error's source chain
  and downcasts to `io::Error`: `UnexpectedEof` (how rustls reports EOF without
  `close_notify`), `ConnectionReset`, `ConnectionAborted` → `ClientVanished`;
  everything else (including `InvalidData`, which is how a TLS alert or corrupt
  record arrives, and all framing/parse errors) → `ReadError`. The mapping is
  a table in the rustdoc. A clean end (reader yields `None`) remains
  `ClientClosed`. The read loop logs vanished and failed reads with different
  `debug` messages.
- **`connected_for`**: `Liveness::connected_secs()` rounds to one decimal and
  the disconnect line records it as an `f64` field (`connected_for=91.3`).
- **Docs**: `docs/deployment.md` leave-reason table (new `client_vanished` row,
  `client_closed`/`read_error`/`write_error` rewritten, example line and
  `connected_for` description); `docs/plugins.md` (stream closed on shutdown,
  before `stop`).

## Decisions

- Close before `stop`, not after: the brief ties the close to the shutdown
  token firing, and the stream has no more use once the loop has ended. The
  close's bound is separate from and much shorter than `stop`'s grace.
- `ConnectionAborted` is treated as vanished alongside reset (same meaning to
  an operator).
- A new integration file rather than growing `stream_idle.rs`.
- The integration test locates the enrolled certificate files from
  `Harness::enroll`'s documented layout (`data_dir/clients/<user>/<uid>/`)
  instead of adding accessors to the shared `tests/stream_support/mod.rs`, to
  avoid touching a shared file other briefs may edit.

## Files

Changed:
- `rustak-client/src/sidecar/link.rs` — `Link::close`; no-op close asserted in an existing test.
- `rustak-client/src/sidecar/run.rs` — close on shutdown, `CLOSE_WITHIN`, module docs.
- `rustak-server/src/stream/liveness.rs` — `ClientVanished`, `from_read_error`, `connected_secs`, tests.
- `rustak-server/src/stream/connection.rs` — classification in `read_loop`, numeric `connected_for`, tests.
- `rustak-server/src/stream/metrics.rs` — doc, test.
- `docs/deployment.md`, `docs/plugins.md`.

Added:
- `rustak-server/tests/stream_departures.rs`
- `.claude/plan/status/M10-01-stream-departures.md`

## Tests, and a host ten times slower

- `stream_departures::a_sidecar_that_is_stopped_closes_its_stream_rather_than_vanishing`
  — real TLS listener, sidecar driven through `drive`, waits for the
  `Negotiated` event (so nothing is in flight), cancels the shutdown token,
  then waits for `client_closed == 1` and asserts `client_vanished` and
  `read_error` are 0. Only state-change waits with a 30s hang budget; a slow
  host just waits longer. The 2s close bound is the only time-dependent part:
  on a host so slow that a loopback flush + `close_notify` takes over 2s the
  close would be abandoned and the test would see `client_vanished` — two
  orders of magnitude of headroom.
- `stream_departures::a_client_that_drops_its_socket_without_closing_has_vanished`
  — drops an `Eud` over TLS; waits for `client_vanished == 1`, asserts
  `client_closed`/`read_error` are 0. Either a FIN (→ `UnexpectedEof`) or an
  RST (unread data in the client's buffer → `ConnectionReset`) maps to the same
  cause, so it is not sensitive to ordering.
- Unit: `liveness` classification (EOF/reset/aborted, wrapped chain,
  `InvalidData`/other/framing, message text ignored), `connected_secs` on a
  paused clock; `connection` read loop with `Broken(kind)` for `InvalidData` →
  `read_error` and EOF/reset → `client_vanished`; `metrics` counts. None
  depends on host speed.

Checked the sidecar test is a real regression test: with the `link.close`
call commented out it fails with `expected 1 disconnect(s) for client_closed,
saw [(ClientVanished, 1)]` — the production fault, reproduced.

Pitfall found: `std::io::Error::source()` skips a wrapped custom error and
returns that error's *own* source, so a plain `source()` walk misses an
`io::Error` wrapped in another `io::Error`. `from_read_error` descends through
`io::Error::get_ref()` for that reason.

## Exit checks

All run in the worktree and passing: `cargo fmt --check`; `cargo clippy
--workspace --all-targets -- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo
doc --workspace --no-deps`; `./scripts/check-file-length.sh`; `cargo test -p
rustak-client` (all ok); `cargo test -p rustak-server --no-fail-fast` (all ok,
lib 2020 passed / 2 ignored).

## Open

- A plugin error that ends the loop does not close the stream (the server
  will record `client_vanished`). Out of the brief's scope; easy to add.
- `write_error` is not reclassified; a peer that vanishes mid-write is still
  `write_error`.
