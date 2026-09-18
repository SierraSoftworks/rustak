# M0-19 — Runtime shutdown and storage hardening — complete

Brief: `.claude/plan/briefs/M0-19-runtime-hardening.md`.
Read first: `conventions.md`; status files `M0-12`, `M0-14`, `M1-05`, `M2-03`;
`rustak-server/src/{runtime.rs,web/server.rs,db/connection.rs}`, `rustak-core/src/runtime.rs`,
`rustak-server/Dockerfile`.

The brief predates the stream (M1-05) and Marti (M2-03) listeners, so "the actix servers *and*
the stream listener drain" is now **three** components on one deadline rather than two.

## What was built

| File | Functional lines (limit 300) | What changed |
|---|---:|---|
| `rustak-core/src/runtime.rs` | 115 | `Shutdown` carries a second token: `abort()`/`aborted()`/`is_aborted()`. The signal handler is 1st = drain, 2nd = abort the drain, 3rd = `exit(130)`, sequenced over an mpsc channel a test can drive |
| `rustak-server/src/config/server.rs` | 67 | `[server] shutdown_timeout` (default `"8s"`), `shutdown_budget()`, `listener_drain_seconds()` |
| `rustak-server/src/config/validate.rs` | 218 | the new `shutdown` rule: positive, and at most `MAX_SHUTDOWN_TIMEOUT` (60s) |
| `rustak-server/src/config/mod.rs` | 95 | re-exports `MAX_SHUTDOWN_TIMEOUT` |
| `rustak-server/src/runtime.rs` | 210 | `drain()` bounds the join by the budget and by the abort; `close_database()` runs on every path under a fixed `DATABASE_CLOSE_TIMEOUT` (2s) |
| `rustak-server/src/web/server.rs` | 111 | both `HttpServer`s take `listener_drain_seconds()` instead of the `SHUTDOWN_TIMEOUT_SECONDS = 10` constant, which is gone |
| `rustak-server/src/stream/mod.rs` | 173 | `StreamRuntime` carries the budget; the store task's fixed 10s wait becomes the budget |
| `rustak-server/src/stream/listener_tls.rs` | 165 | `drain_connections()` after the accept loop; the three listener-wide bounds become one `ListenerLimits` argument |
| `rustak-server/src/db/connection.rs` | 234 | documentation only, plus one test — see item 3 |
| `rustak-server/Dockerfile` | — | `STOPSIGNAL SIGTERM` and the comment explaining the arithmetic |
| `config.example.toml` | — | the `[server] shutdown_timeout` block |
| `docs/deployment.md` | — | a **Stopping cleanly** section, `stop_grace_period` in compose, `TimeoutStopSec` in the unit file, the key in the config walk-through |

**11 new tests**: 3 in `rustak-core::runtime`, 3 in `config::server`, 1 in `config::validate`,
3 in `rustak-server::runtime`, 1 in `db::connection`. No manifest changes, no new dependencies.

## The four items

### 1. The shutdown budget is `[server] shutdown_timeout`, and it is 8s

`docker stop` sends `SIGTERM`, waits **ten** seconds, then `SIGKILL`s. actix's `shutdown_timeout`
was also **ten**, and the WAL `TRUNCATE` runs *after* the listeners stop — so an installation with
one idle browser tab open was killed at the exact moment the drain gave up, and the checkpoint
`runtime::run_all` exists for never ran. M0-12 flagged this; it reproduces.

As built the budget covers **all three** listeners, because they are stopped by the same
cancellation and they all wait for the same kind of thing:

- both `HttpServer`s get `listener_drain_seconds()` — the budget **minus one second**, so the
  runtime's own wait is the one that reports a drain which overran rather than the two racing to
  log it;
- the stream listener's accept loop now **drains** rather than returning the moment the token is
  cancelled. Every connection task already holds one semaphore permit for its whole life, so
  `drain_connections` acquires all of them: that is exactly "has everybody finished?", with no
  second registry to keep in step with the first. Its store task's fixed ten-second wait became the
  budget too;
- `runtime::drain` bounds the join of all five components, so a component that ignores its token is
  bounded by the runtime rather than by the orchestrator's `SIGKILL`.

`8 + 2 = 10` is the whole reason for the default: eight for the drain, two for the checkpoint,
inside Docker's default grace with nothing to spare. That arithmetic is now written down in three
places an operator might look — `config.example.toml`, the `Dockerfile` (with `STOPSIGNAL SIGTERM`
made explicit), and `docs/deployment.md` § **Stopping cleanly**, which states the rule as *raise
`shutdown_timeout` and you must raise the orchestrator's grace period with it*.

Validation refuses `0s` and anything over `60s`. Sixty is not a technical limit: it already
outlasts every default grace period there is, so a longer budget is one that can only ever end in
the `SIGKILL` it exists to avoid, and the refusal is where somebody who genuinely needs longer
learns that their orchestrator needs changing too.

### 2. A second signal ends the drain; it no longer ends the process

`Shutdown` now holds two tokens. `stop` is the one that was already there — scoped, so
`child()` can stop a single listener. `abort` is new and is *not* scoped: every clone and every
child shares it, because running out of time is a fact about the process rather than about one
connection. `abort()` implies `cancel()`.

The signal sequence is now:

| Signal | What happens |
|---|---|
| 1st | `cancel()` — drain, as before |
| 2nd | `abort()` — the drain resolves at once, **and `Database::close` still runs** under its 2s cap |
| 3rd | `exit(130)` — the escape hatch, and the only path that skips the checkpoint |

The third is deliberate rather than an oversight of the old behaviour: somebody who has asked three
times has said what they mean, and leaving no immediate exit at all would teach them to reach for
`kill -9`.

**The sequencing is driven by an mpsc channel**, not by the signal handlers directly:
`listen_for_signals` spawns a forwarder that sends one `()` per signal, and `sequence` consumes
them. A test can then deliver the second signal without sending one to the process running the test
suite — which is what made the old behaviour untestable, since asserting it would have meant
`exit(130)` inside the test binary.

`run_all` is unchanged in shape and still closes the database on every exit path, now through
`close_database`, which bounds it with `with_grace(DATABASE_CLOSE_TIMEOUT)`. Two seconds, fixed and
not configurable: by then nothing is being waited *for* — every listener has stopped and the log is
capped at 64 MiB — so a checkpoint that has not finished is blocked rather than slow, and the only
thing left to do about it is leave the log for the next start.

A drain that was given up on returns `Ok(())`, not an error. The server was asked to stop and it
has; turning somebody's slow connection into a non-zero exit status would make every restart look
like a failure to whatever is watching. The give-up is logged where it happens.

### 3. `Database::close`'s ordering is right, and the read pool does not downgrade the truncation

M0-12's reorder is correct and stands: `PRAGMA optimize` runs `ANALYZE`, which is a write, so it
has to come **before** the `TRUNCATE` or the log is empty for exactly as long as it takes the next
statement to fill it again. No change.

The read pool needed checking rather than changing, and the reason is worth recording. `TRUNCATE`
is the one checkpoint mode that can be refused outright — it needs every other connection to be
done with the log — and SQLite reports that as `SQLITE_BUSY` **on the pragma**, so a truncation
that did not happen looks exactly like one that did.

`close` does `drop(readers)` before the checkpoint, but `readers` is an `Arc` and the server calls
`context.db().clone().close()` with the context's own handle still alive, so that drop is *not* the
last one and the pool is not closed. It does not matter, and the new test is what says so: each
pooled read runs as its own statement on its own connection, and the read mark is released when
that statement finishes, so an **idle** reader holds nothing a checkpointer would wait out. What
actually guarantees the truncation is that `close` runs after every listener has stopped, so there
is no query left to be in the middle of.

`closing_truncates_the_write_ahead_log` (M0-12) could not have caught this: it closes the *only*
handle, so its `drop(readers)` really is the last one. The new
`the_log_is_truncated_while_the_server_still_holds_a_handle` reproduces the server's call shape —
a write, a read on every pooled connection, then `close()` with a clone still held — and asserts
`rustak.sqlite-wal` is 0 bytes. It passes, which is the finding.

### 4. Nothing to remove in `e2e/`

`e2e/scripts/start-server.mjs` has no double-`SIGTERM` work-around; M0-14 § 4 explicitly chose not
to add one and to record the log line instead. `e2e/` is untouched, as the brief directs.

The behaviour M0-14 recorded is now simply correct: Playwright's `gracefulShutdown` signals the
launcher's process group *and* the launcher forwards to its child, so the server does get two
`SIGTERM`s — and it now checkpoints on the way out instead of logging *"exiting immediately"* and
skipping it. That line of M0-14's status is out of date in the server's favour.

**One thing for whoever owns `e2e/` next, not changed here:**
`e2e/playwright.config.ts` sets `gracefulShutdown: { signal: "SIGTERM", timeout: 5_000 }`, which is
shorter than the 8s + 2s a real stop can take. It is harmless today because the e2e configuration
turns TLS off, and an idle HTTP/1.1 keep-alive connection does not hold actix's drain (measured
below) — but an e2e config that ever switches TLS on would be `SIGKILL`ed mid-checkpoint. Raising
that to `11_000` at the same time would be the whole fix.

## Decisions worth knowing

### The abort is shared by children; the stop is not

`child()` gives a listener its own stop token so that one failure does not cancel the others, and
that is deliberate. The abort had to go the other way: a connection still open when the operator
asks a second time is *precisely* what the abort exists to stop waiting for, so a child that did
not see it would be the one thing the feature cannot reach.

### actix gets one second less than the budget

Two timers armed on the same instant with the same value race to decide which of them logs. Giving
actix `budget - 1s` (floored at 1s, because actix reads zero as "cut everything off now") makes the
outcome deterministic: actix reports its own drain finishing, and if it does not,
`runtime::drain` is the one that says the budget ran out and names the setting to raise.

### The components are dropped when the drain is abandoned, not awaited

M0-12 chose `join` over `try_join` precisely so that a failing component does not leave the others
*dropped rather than stopped*. Abandoning the drain does drop them — but only after every one of
them has already been told to stop through the shared token, so what is dropped is the waiting
rather than the stopping, and the process is at most two seconds from exiting. That is the trade
the second signal asks for.

### `main.rs` is untouched, so a second signal now exits 0 rather than 130

The old `exit(130)` was inside the signal handler; there is nothing left to produce that status
without `main` checking `is_aborted()` after `run` returns, and `main.rs` is not this brief's file.
Exit 0 after a `SIGTERM` is what the single-signal path already did and what systemd and Docker
expect, so this is recorded rather than worked around. The third signal still exits 130.

### `clippy::large_futures` and `too_many_arguments`

Both fired on this change and both were fixed properly rather than allowed:

- the `drain(...)` call in `listen` is `Box::pin`ned — the five component futures and both
  `select!` arms live in that frame, and clippy is right that it does not belong on the stack of
  every caller up to `main`. That also brought `run_all` and `lib.rs::run` back under the
  threshold;
- `listener_tls::run` would have taken eight positional arguments, so `handshake_timeout`,
  `max_connections` and `drain` became one `ListenerLimits`. They arrive together and are all read
  from the configuration at bind time, so it reads better as well as counting better.

## Manual verification

`[server] shutdown_timeout = "8s"`, `[web.public.tls] mode = "internal"`, one **idle HTTP/2**
connection held open — a browser tab that has finished loading, which is the case M0-12 flagged.
Full script and logs in the session scratchpad; the numbers are:

```
case=one-signal:  exit=0 elapsed=7.06s wal=0 bytes
    INFO  Received a shutdown signal; draining connections.
    INFO  The public listener has stopped.

case=two-signals: exit=0 elapsed=0.51s wal=0 bytes
    INFO  Received a shutdown signal; draining connections.
    WARN  Received a second shutdown signal; connections that are still open will be cut off.
          The database is still checkpointed before we exit.
    WARN  No longer waiting for connections to close; the database is still checkpointed
          before we exit.
```

7.06s is the budget minus one, which is what actix was given; it used to be 10.0s, i.e. Docker's
entire grace period, with the checkpoint still to come. Both cases leave `rustak.sqlite-wal` at
**0 bytes**; the two-signal case used to exit 130 with the log still there.

Plain HTTP/1.1 is worth a footnote: an idle h1 keep-alive connection does **not** hold actix's
drain (both cases returned in 0.41s over plaintext), so this only ever bit TLS listeners, where
ALPN negotiates `h2`.

## Exit checks

Run against the final tree. **Two other agents were writing inside `rustak-server` throughout**
(`marti/{groups,contacts,subscriptions}`, and a `profiles`/`files` brief), so several of these were
run repeatedly before they could be made to mean anything; the results below are the final ones and
the single remaining red is named at the bottom.

```
$ cargo test -p rustak-core
running 139 tests
test result: ok. 139 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.17s
running 14 tests                                                            (doc-tests)
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
```

```
$ cargo test -p rustak-server --features testing --no-fail-fast
   unittests src/lib.rs      1208 passed; 0 failed; 2 ignored
   unittests src/main.rs        0 passed
   tests/bootstrap.rs           3 passed; 0 failed
   tests/enroll_flows.rs        9 passed; 1 failed          ← not this brief's; see below
   tests/enroll_oauth.rs       10 passed; 0 failed
   tests/marti_channels.rs      9 passed; 0 failed
   tests/marti_contract.rs     14 passed; 0 failed
   tests/profiles_contract.rs  13 passed; 0 failed
   tests/stream_routing.rs     11 passed; 0 failed
   tests/stream_session.rs     10 passed; 0 failed
   tests/stream_store.rs        8 passed; 0 failed
   tests/sync_contract.rs      13 passed; 0 failed
   Doc-tests rustak_server      5 passed; 0 failed
```

The eleven tests this brief added, run on their own:

```
$ cargo test -p rustak-server --features testing --lib -- runtime:: config::server config::validate db::connection
test config::server::tests::the_drain_budget_leaves_room_for_the_checkpoint_inside_dockers_grace ... ok
test config::server::tests::actix_is_given_a_second_less_than_the_budget ... ok
test config::server::tests::a_budget_the_validator_would_have_refused_falls_back_rather_than_vanishing ... ok
test config::validate::tests::a_drain_nobody_would_wait_out_is_refused ... ok
test runtime::tests::everything_stopping_on_its_own_is_reported_rather_than_waited_out ... ok
test runtime::tests::a_drain_that_outlives_its_budget_is_given_up_on ... ok
test runtime::tests::a_second_signal_ends_the_wait_without_waiting_the_budget_out ... ok
test runtime::tests::the_checkpoint_runs_after_a_drain_that_was_abandoned ... ok
test db::connection::tests::the_log_is_truncated_while_the_server_still_holds_a_handle ... ok
test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 1163 filtered out; finished in 0.13s

$ cargo test -p rustak-core runtime::
test runtime::tests::the_first_signal_drains_and_the_second_stops_waiting ... ok
test runtime::tests::a_child_shares_the_abort_even_though_it_has_its_own_stop ... ok
test runtime::tests::aborting_a_child_leaves_the_process_alone ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 130 filtered out; finished in 0.06s
```

```
$ cargo clippy --workspace --all-targets -- -D warnings          # the CI command, no features
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.85s

$ cargo clippy -p rustak-server -p rustak-core --all-targets --features rustak-server/testing -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 19.23s

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-core -p rustak-server --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 15.31s

$ cargo fmt --check
(no output, exit 0)

$ ./scripts/check-file-length.sh
(no output, exit 0)
```

```
$ cargo run -p rustak-server -- --config config.example.toml --check
config.example.toml is valid: rustak would listen on 0.0.0.0:8446, with data in ./data.
exit=0
```

```
$ cd rustak-ui && trunk build
2026-09-18T15:24:27.627538Z  INFO ✅ success

$ cd .. && cargo build -p rustak-server
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.50s

$ cd e2e && npm run typecheck
> tsc --noEmit
(no output, exit 0)

$ RUSTAK_E2E_CHROMIUM="…/chromium-1234/chrome-mac-arm64/Google Chrome for Testing.app/…" npx playwright test
Running 20 tests using 1 worker
  ✓   1 [setup] › tests/setup.spec.ts:29:1 › the first-run wizard turns a token on disk into an administrator who can sign in (978ms)
  … 18 more …
  ✓  20 [chromium] › tests/smoke.spec.ts:42:1 › the application boots and renders (271ms)
  20 passed (15.4s)
```

`RUSTAK_E2E_CHROMIUM` is needed on this machine only because Playwright's expected browser build is
not the one downloaded here; it is not a change to the suite.

**The one check still red, and it is not this brief's:**

```
$ cargo test -p rustak-server --features testing --test enroll_flows
---- the_profile_endpoints_answer_no_content_until_they_have_something_to_send ----
panicked at rustak-server/tests/enroll_flows.rs:632:9:
  /Marti/api/tls/profile/enrollment?clientUid=ANDROID-1: ATAK reads 'nothing for you' and
  carries on to the stream
  left: 401   right: 204
```

That is the concurrent `profiles` brief's endpoint returning `401` where its own test wants `204`,
in a file and a module this brief does not touch. Everything else in the suite is green. Re-run
`cargo test -p rustak-server --features testing` once that brief lands.

## Notes for the orchestrator and the briefs that follow

- **A new listener has one more obligation now.** It must drain within
  `config.server.shutdown_budget()` and give up when `shutdown.aborted()` fires. `runtime::drain`
  will cut it off at the deadline either way, but a component that cuts itself off reports a clean
  stop, and one that does not produces the *"Connections were still open when the shutdown budget
  ran out"* warning on every stop.
- **`Shutdown::abort()` is also a way to stop a server outright** — it implies `cancel()` — which
  is the shape a test wants when it does not care about the drain.
- **The 8 + 2 arithmetic is load-bearing and written down in four places**: `config.example.toml`,
  `rustak-server/Dockerfile`, `docs/deployment.md` § Stopping cleanly, and
  `config::server::DEFAULT_SHUTDOWN_TIMEOUT`'s doc comment. Changing one means changing all four;
  `the_drain_budget_leaves_room_for_the_checkpoint_inside_dockers_grace` fails if the default moves.
- **`DATABASE_CLOSE_TIMEOUT` is public** (`crate::runtime::DATABASE_CLOSE_TIMEOUT`), because
  `config::server` documents the default budget in terms of it.
- **M0-14 § 4 is out of date** in the server's favour — see item 4 above — and
  `e2e/playwright.config.ts`'s 5s `gracefulShutdown` is the one loose end, harmless while the e2e
  configuration serves plaintext.
- **`rustak-server/src/main.rs` was not touched**, so a second signal now exits 0 rather than 130.
  If an exit status that distinguishes "stopped impatiently" is wanted, it is one
  `shutdown.is_aborted()` check in `main` after `run` returns.
