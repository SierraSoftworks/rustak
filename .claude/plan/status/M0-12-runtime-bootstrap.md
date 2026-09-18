# M0-12 — `main.rs` / `lib.rs::run` / `runtime.rs` and the bootstrap integration test — complete

Brief: `.claude/plan/briefs/M0-12-runtime-bootstrap.md` (including its addendum on the
debug-build telemetry gate).
Read first: `conventions.md`; `design/01-foundations-storage-ci.md` §3.1, §3.4, §8 step 12, §9;
status files `M0-04`, `M0-06`, `M0-09`, `M0-10`, `M0-11`, `M0-15`.

## What was built

| File | Functional lines (limit 300) | Contents |
|---|---:|---|
| `rustak-server/src/main.rs` | 74 | clap `Args` (`--config`/`--env`/`--check`), env file, telemetry bootstrap, `Shutdown::listen_for_signals`, `run`, telemetry flush; `check()` unchanged in behaviour |
| `rustak-server/src/lib.rs` | 76 | `run(config, session, shutdown)`, `build_context(…)`, `busy_timeout`, `install_crypto_provider`, `UNCONFIGURED_ISSUER` |
| `rustak-server/src/runtime.rs` | 117 | `run_all`, `listen`, `stopping_on_exit`, `serve`, `jobs`, `housekeeping`, `sweep_ceremonies`, `prune_revocations`, `announce_setup` |
| `rustak-server/tests/bootstrap.rs` | 279 (`tests/` is exempt) | three in-process integration tests over real sockets (feature `testing`) |
| `rustak-core/src/telemetry.rs` | 72 | `bootstrap` now asks for `with_debug_builds` (the addendum) |
| `rustak-client/src/sidecar/run.rs` | 99 | the local `session.enable()` workaround removed; it gets the fix from `bootstrap` |
| `rustak-server/src/db/connection.rs` | 234 | **one ordering fix in `Database::close`** — see "The WAL was not actually being truncated" below |

**12 new tests**: 4 unit in `lib.rs`, 4 unit in `runtime.rs`, 1 unit in `rustak-core::telemetry`,
3 integration in `tests/bootstrap.rs`. Each source file keeps its single trailing column-0
`#[cfg(test)] mod tests`.

No manifest changes. No new dependencies.

## The start-up sequence as built

`main` → env file → (`--check` exits here, with telemetry deliberately never brought up) →
`telemetry::bootstrap` → `Config::load` → `Shutdown::listen_for_signals` → `rustak_server::run` →
`telemetry::shutdown` → exit.

`run` → `install_crypto_provider()` → `build_context` → `runtime::run_all`.

`build_context` (the things that are files and keys):

1. `Database::open(database_path, [storage] reader_connections, busy_timeout)` — **first**, because
   opening it creates the data directory that the generated encryption key is then written into.
2. `SecretStore::load([auth] secret_key, previous_secret_keys, database_path)`.
3. `AppContext::new(config, db, secrets, session, shutdown)`.
4. `ContentStore::new(content_dir).prepare()` → `install_content`.
5. `JwtIssuer::load_or_create(db, secrets, [auth], issuer)` → `install_jwt`.

`run_all` (the things that have a lifetime):

6. `pki::load_or_create_root_ca(db, secrets, [pki], data_dir)` — which also refreshes
   `<data_dir>/pki/ca.crt`.
7. `auth::setup::ensure(db, setup_token_file())`, logging the **path** at `warn!` when a token was
   written.
8. `web::tls::resolve(config, db, secrets, Some(&ca))` → `web::build_public(context, tls)`.
9. The three long-lived components, joined.

`db.close()` (the WAL `TRUNCATE`) then runs on **every** exit path, including the ones where
nothing ever started — `run_all` is a thin wrapper around `listen()` for exactly that reason. A
listener that would not bind still leaves a migrated database with a log to fold back in.

## Decisions

### `build_context` / `run_all` is where the CA is loaded, not `build_context`

M0-11's handoff lists the CA between the secret store and the JWT issuer. It is loaded in
`run_all` instead, immediately before the TLS resolution that is the only thing in M0 which needs
it. Nothing depends on the order — `JwtIssuer` needs the database and the secret store, not the
authority — and it keeps `build_context` to "the storage", which is what an integration test wants
from it. `<data_dir>/pki/ca.crt` is still refreshed on every start, because `load_or_create_root_ca`
does that itself.

### The three components are joined, not `try_join`ed

Design §3.4 says `try_join`. `futures_concurrency`'s `try_join` returns on the first `Err` and
**drops** the other futures, which for the actix `Server` future means dropping it rather than
stopping it — the worker threads would still be accepting connections while `run_all` was running
`db.close()`. As built, each component is wrapped in `stopping_on_exit`, which cancels the shared
token however that component ends, and all three are `join`ed so that the other two are awaited to
completion after being told to stop. The result is `web.and(jobs).and(housekeeping)`, which is the
first failure in start-up order and `Ok(())` when everything wound down — the same contract
`try_join` was asked for, with a shutdown that actually shuts down. Every component is bounded:
the listener by actix's `shutdown_timeout`, the job host by `JoinSet::shutdown`, housekeeping by
its `biased` select.

### The third joined future is housekeeping, not a checkpoint task

Design §3.4 lists "checkpoints" as the third future. The WAL checkpoint is already a registered
job (`jobs::WalCheckpointJob`, M0-09) that arms itself on `[storage] checkpoint_interval`, so the
job host runs it and a separate task would be a second schedule for the same work. The third
future instead carries what M0-11's handoff asked M0-12 to schedule and which cannot be a queue
job: `auth::passkey_store::sweep` every ten minutes and `db.revoked_jtis().prune()` hourly. Both
are deliberately *not* queue jobs — they delete rows that have already expired, a missed sweep
costs nothing, and queueing them would mean a database write every ten minutes for the privilege.
Neither failure is fatal; both are logged and the loop carries on.

**`RateLimiter::sweep()` is not scheduled.** `web::server::build_public` builds the limiter inside
itself and does not return it, and there is nothing for M0-12 to hold. It is not a gap: M0-11's
limiter sweeps itself whenever the map passes 1024 entries, so it is bounded without a timer. If
`build_public` ever returns the limiter, the sweep belongs in `housekeeping` beside the other two.

### The WAL was not actually being truncated (one file outside this brief's list)

**This is the one place this brief edited a file it was not given**, and it is worth reading
before anything else here.

The manual `cargo run` + `SIGTERM` check left a **16 KB `rustak.sqlite-wal`** behind, and the
first version of `tests/bootstrap.rs` did not catch it: the assertion read the file *after* the
`tempfile::TempDir` had been dropped, so `metadata()` failed and `unwrap_or(0)` made the test pass
by reading nothing. Both were fixed.

The cause is in `Database::close` (M0-07), which ran

```rust
self.checkpoint(Checkpoint::Truncate).await?;          // log truncated to 0
self.writer.call(|c| c.execute_batch("PRAGMA optimize")) // …and then written to again
```

`PRAGMA optimize` runs `ANALYZE`, which is a write, so the truncation lasted exactly as long as it
took the next statement to undo it. Design §9 lists "`TRUNCATE` at shutdown" as the mitigation for
"WAL with many readers", and `runtime::run_all`'s whole reason for calling `close()` is that
guarantee, so this brief could either deliver it or document a promise the code does not keep.
The fix is a reordering, in the one file where it belongs:

```rust
let Self { writer, readers } = self;
writer.call(|c| c.execute_batch("PRAGMA optimize")).await?;  // statistics first
drop(readers);                                               // then let go of the log
writer.call(|c| c.execute_batch(Checkpoint::Truncate.as_sql())).await?;
```

The readers moved above the checkpoint for the same reason: `TRUNCATE` waits for every other
connection to be done with the log, and an idle reader still holding its snapshot is one it would
wait out. Verified by the manual check (`rustak.sqlite-wal` is now **0 bytes** after `SIGTERM`)
and by both bootstrap tests. `rustak-server/src/db/connection.rs` was untouched by any other agent
at the time. **M0-07's owner should review the reorder.**

One cosmetic residue remains and is *not* worth chasing: `rustak.sqlite-shm` survives, because
`Arc::try_unwrap(writer)` fails while actix's `web::Data<AppContext>` clones are still being torn
down, so the connection is dropped rather than closed. The log is empty, which is what design §9
asked for; SQLite recreates the shared-memory file on the next open.

### `UNCONFIGURED_ISSUER`

`JwtIssuer` bakes the `iss` claim in at start-up and validates against it, but an installation
configured entirely through the browser has no host name until the wizard finishes — and the keys
are loaded before then. Rather than issue tokens with an empty issuer, which a later start-up
would go on accepting, they claim `urn:rustak:unconfigured`, which cannot collide with any base
URL. The consequence is deliberate: the session the wizard mints stops being accepted the moment
the server learns what it is called (the next restart), which is the right way round.

The issuer comes from `identity::settings::base_url(config, db)` rather than `config.issuer()`, so
that after a restart it is the host the **wizard** stored, not only the one the file names.

### `--check` still does not bring telemetry up

Design §3.4 orders telemetry before the configuration load; M0-06 requires `--check` not to
bootstrap telemetry or touch the data directory. Both hold: `--check` is answered before the
telemetry call, out of its own `check()` function, and the run path keeps the design's order so
that a configuration that will not parse is still reported through the session.

### Debug builds log again (the addendum)

`rustak_core::telemetry::bootstrap` now calls `Metadata::with_debug_builds()` — the library's own
API for this — rather than storing into `Session::enable()` after the fact. It changes nothing in
a release build, attaches no battery a debug build did not already ask for (`from_env` adds Sentry
only when a DSN is configured, and analytics never), and it is the single place M0-15 asked for:
`rustak-client::sidecar::run_with`'s copy of the workaround is deleted, and the sidecar now gets
the behaviour from `bootstrap` like everything else. `telemetry::testing_session` is left alone —
it is for suites that read back what the `Testing` battery collected, and that is its own decision.

## `tests/bootstrap.rs`

| Test | What it proves |
|---|---|
| `a_first_start_serves_its_own_tls_and_walks_an_operator_all_the_way_in` | temp data dir → `run()` in a task → `/robots.txt` 200 **over TLS**, with `reqwest` trusting `<data_dir>/pki/ca.crt` → `/api/v1/health` → `setup/status` → the token read from `[auth] setup_token_file` → `POST /setup/admin` → passkey register `start`/`finish` against `SoftAuthenticator` → the `TokenResponse` that comes back → `GET /me` says `ada`, `is_admin` → cancel → `Ok(())` inside ten seconds → the `-wal` file is 0 bytes |
| `the_insecure_development_listener_serves_plaintext_and_still_stops_cleanly` | the other `[web.public]` shape binds a real socket, and the same shutdown and truncation hold |
| `a_listener_that_cannot_bind_reports_it_rather_than_running_without_one` | a port somebody else holds is a `Kind::User` error naming the port, not a server that came up with no socket and would look healthy to an orchestrator |

Two things in the harness are worth knowing:

- **The server is reached at `localhost`, never `127.0.0.1`.** WebAuthn identifies a relying party
  by domain, so an address cannot register a passkey at all (M0-11 deviation 6). `reqwest`'s
  `.resolve("localhost", addr)` points the name at the socket the listener bound, which also makes
  the suite independent of `/etc/hosts` and of whether `localhost` resolves to `::1` first.
- **The client is dropped before the token is cancelled.** actix drains rather than cuts, and the
  TLS listener advertises `h2`, so an idle HTTP/2 keep-alive connection holds the drain open for
  the whole of `web::server`'s ten-second `shutdown_timeout`. Closing the client is what a client
  leaving does, and it is what makes the ten-second assertion about the shutdown rather than about
  that timeout. **Worth flagging for the orchestrator:** a real `SIGTERM` with a browser still
  connected costs those ten seconds, which is exactly Docker's default `--stop-timeout`, so a
  container would be `SIGKILL`ed at the moment the drain gave up and the `TRUNCATE` checkpoint
  would not run. `web::server::SHUTDOWN_TIMEOUT` belongs to M0-11; lowering it to ~5s, or
  documenting `--stop-timeout 30`, is a one-line follow-up nobody owns yet.

`Stopped::assert_clean()` is where the `-wal` assertion lives, and it takes the `TempDir` by value so
that the directory is still there when the file is measured and is deleted immediately afterwards.
That shape exists because the first version of this assertion was vacuous for exactly that reason.

The port is taken by binding `127.0.0.1:0`, reading the number and closing the socket. That is a
race in principle; the alternative — `run()` reporting the addresses it bound — is public API that
would exist only for this file, and the design's signature is `run(config, session, shutdown)`.

## Exit checks

Run at the end of this brief, against the final tree.

**A second agent was writing inside `rustak-server`, `rustak-client`, `rustak-cot` and `rustak-api`
throughout**, so several of these were run repeatedly before they could be made to mean anything;
the results below are the final ones, and the single remaining red is named at the bottom.

`cargo fmt` was never run workspace-wide, and never as a **write** after that agent's files
appeared — reformatting their unfinished files would have collided with their work.

```
$ cargo test -p rustak-server --features testing
     Running unittests src/lib.rs (target/debug/deps/rustak_server-…)
running 790 tests
test result: ok. 788 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 14.39s
     Running unittests src/main.rs (target/debug/deps/rustak-…)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/bootstrap.rs (target/debug/deps/bootstrap-…)
running 3 tests
test a_listener_that_cannot_bind_reports_it_rather_than_running_without_one ... ok
test the_insecure_development_listener_serves_plaintext_and_still_stops_cleanly ... ok
test a_first_start_serves_its_own_tls_and_walks_an_operator_all_the_way_in ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.85s
   Doc-tests rustak_server
running 5 tests
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

```
$ cargo test -p rustak-core
test result: ok. 136 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out   (doc-tests)

$ cargo test -p rustak-client -- sidecar::
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 44 filtered out
```

```
$ cargo clippy -p rustak-server -p rustak-core --all-targets --all-features -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.74s

$ cargo clippy -p rustak-client --all-features -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.00s
```

One finding here was this brief's and was fixed: `wrong_self_convention` on the bootstrap suite's
`Stopped::is_clean`, renamed to `assert_clean`.

```
$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server --no-deps
 Documenting rustak-server v0.1.0 (…/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.92s
   Generated target/doc/rustak_server/index.html and 1 other file
```

Two intra-doc link errors in `lib.rs` and `runtime.rs` were fixed to get there.

**The one check still red, and it is not this brief's:**

```
$ cargo clippy -p rustak-client --all-targets --all-features -- -D warnings
error: unused import: `Keepalive`
  --> rustak-client/tests/stream_client.rs:17:29
```

`rustak-client/tests/stream_client.rs` is the other agent's, added minutes before this file was
written. `--all-targets` on `rustak-server` and `rustak-core` — which is where everything this
brief wrote lives, including `tests/bootstrap.rs` — is clean.

### `cargo fmt --check` and `./scripts/check-file-length.sh`

```
$ cargo fmt -p rustak-server --check
(no output)

$ cargo fmt -p rustak-core --check
(no output)

$ cargo fmt -p rustak-client --check
# 21 diffs, all in the other agent's `src/stream/{mod,connection,connect_string,
# error,negotiation,testing,tls}.rs` and `tests/stream_client.rs`.
# `src/sidecar/run.rs`, the only file this brief touched there, is not among them:
$ rustfmt --edition 2024 --check <the seven files this brief touched>
(no output)
```

```
$ ./scripts/check-file-length.sh
(no output, exit 0)

# and the files this brief added, which git does not track yet, by hand with the
# same awk:
rustak-server/src/lib.rs               76
rustak-server/src/main.rs              74
rustak-server/src/runtime.rs          117
rustak-server/src/db/connection.rs    234
rustak-core/src/telemetry.rs           72
rustak-client/src/sidecar/run.rs       99
rustak-server/tests/bootstrap.rs      279   (tests/ is exempt)
```

### Manual: `--check` (M0-06's behaviour, unchanged)

```
$ rustak --config <copy>/config.toml --check
<copy>/config.toml is valid: rustak would listen on 127.0.0.1:18446, with data in <copy>/data.
exit=0

$ rustak --config /nope/missing.toml --check
error(usr):    We could not read your config file '/nope/missing.toml'.
│
╰────── cause: No such file or directory (os error 2)
╭─ Advice ─────────────────────────────────────────────────────────────────────╮
│  • Check that you have the necessary permissions to read the file.           │
│  • Ensure the file exists and is readable.                                   │
╰──────────────────────────────────────────────────────────────────────────────╯
exit=1
```

### Manual: `cargo run -p rustak-server -- --config <copy of config.example.toml>` then `SIGTERM`

`config.example.toml` copied with `data_dir` pointed at a temporary directory and
`[web.public] listen = ["127.0.0.1:18446"]`. **Debug build**, which is the point of the addendum.

```
INFO server.context:db.open{readers=2}:db.migrate: Applied a database migration. migration="0001_kv_queues_audit.sql"
… (0002 … 0007)
WARN server.context: No encryption key was configured, so one has been generated. Back this file up … key_id=90edb2ff
INFO server.context:auth.jwt.load: Created a token signing key. kid=59b9d11c9937c58c
INFO server.context: Storage is open. database=…/rustak.sqlite readers=2
INFO server.run:pki.ca.load: Created this installation's root certificate authority. subject=CN=rustak CA,O=rustak key_type=Rsa2048 not_after=2036-09-15…
INFO server.run:pki.ca.load: Exported the CA certificate for operators. path=…/pki/ca.crt
INFO server.run:pki.ca.load: Loaded the root certificate authority. fingerprint=250448e6… not_after=2036-09-15…
WARN server.run: Setup required: open https://tak.example.com/setup and enter the token from …/setup-token. token_file=…/setup-token
INFO server.run:web.tls.resolve:pki.server_cert.load: Issued this server's own TLS certificate from the internal authority. names=["tak.example.com"] addresses=[] not_after=2027-10-20…
INFO server.run:web.tls.resolve: The public listener presents a certificate from this installation's own authority; browsers will warn until the authority is installed, and enrolled devices will not.
INFO server.run:web.server.build: The public listener is bound. address=127.0.0.1:18446 tls=true
INFO server.run: rustak is running. version="0.1.0" name=rustak
INFO server.run:job.host.run{otel.kind=Consumer}: The job host has started with 2 registered handler(s). jobs=2

--- kill -TERM ---

INFO rustak_core::runtime: Received a shutdown signal; draining connections.
INFO rustak_server::runtime: Draining the public listener.
INFO server.run:job.host.run{otel.kind=Consumer}: The job host is stopping; in-flight jobs will be retried.
INFO actix_server::accept: accept thread stopped
INFO actix_server::worker: shutting down idle worker     (×10)
INFO server.run: The public listener has stopped.

exit status: 0  (took 0s)
No "could not reclaim sole ownership of the telemetry session" warning.

$ ls -la <data>/ | grep -E 'wal|sqlite$'
-rw-r--r--  499712  rustak.sqlite
-rw-r--r--       0  rustak.sqlite-wal      ← truncated
```

## Notes for the orchestrator and the briefs that follow

- **`build_context` is public** and returns a context with both `Late` slots filled, for the
  integration suites M0-14 and M1 will write. It does *not* load the CA or bind anything.
- **`runtime::run_all` is where a new listener goes.** The Marti listener (M2) and the stream
  listener (M1) are two more entries in the `join`, each wrapped in `stopping_on_exit` so that a
  failure in one stops the server rather than leaving a half-serving process. Each needs its own
  `web::tls::resolve`-equivalent; `pki::server_cert::load_or_issue` already reissues on a name or
  CA change.
- **`housekeeping` is where a sweep goes** when it is not durable work; `jobs/` is where it goes
  when it is. The dividing line used here: does a missed run cost anything?
- **The ten-second drain** described above is the one real risk this brief surfaced. It is
  `web::server`'s constant, not this brief's.
- **`UNCONFIGURED_ISSUER`** means tokens minted before the wizard finishes do not survive a
  restart. If M2 wants them to, the fix is to rebuild the `JwtIssuer` when
  `identity::settings::save` first records a host name, not to weaken the validation.
- **`Database::close`'s reorder needs M0-07's sign-off** — see the section above. Without it the
  `-wal` file survives every shutdown, and the test that was meant to catch that passed vacuously.
- **One check is still red and is not this brief's**: `cargo clippy -p rustak-client
  --all-targets` reports an unused `Keepalive` import in the other agent's
  `rustak-client/tests/stream_client.rs`, and `cargo fmt -p rustak-client --check` reports their
  `src/stream/**`. Re-run the workspace-wide `fmt`/`clippy` gates once that brief lands; nothing in
  this change set contributes to either.
