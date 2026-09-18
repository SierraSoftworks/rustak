# M0-09 — `services/` (AppContext) and `jobs/` (JobHost) — complete

Brief: `.claude/plan/briefs/M0-09-services-jobs.md`
Design: `design/01-foundations-storage-ci.md` §3.1 (file list), §3.2 (`AppContext`/`Services`),
§3.4 (start-up and shutdown ordering), §8 step 9.
Reference lifted from: `../automate/agent/src/{services/mod.rs,job.rs,jobs/cron.rs}`.

## What was built

| File | Functional lines | Contents |
|---|---:|---|
| `prelude.rs` | 5 | `rustak_core::prelude::*` + `Config`, the four storage traits, `Job`/`JobContext`, `AppContext`/`Services` |
| `services/mod.rs` | 166 | `HTTP_USER_AGENT`, `AppContext` (+ manual `Debug`), the `Services` trait, `impl Services for AppContext`, `impl<S: Services + ?Sized> Services for &S` |
| `services/late.rs` | 70 | `Late<T>` (shared once-cell for handles installed during start-up) and the uninhabited `Pending` placeholder |
| `services/wiring.rs` | 30 | `Database::from_config(&StorageConfig, &Path)` and `SecretStore::from_config(&AuthConfig, &Path)` |
| `services/mock.rs` | 21 | `AppContext::new_mock`, behind `cfg(any(test, feature = "testing"))` |
| `jobs/mod.rs` | 10 | module docs (the shape of a job; how recurring work re-arms itself) + re-exports |
| `jobs/job.rs` | 127 | `JobContext<S>`, the `Job` trait, `DEFAULT_JOB_TIMEOUT` |
| `jobs/runnable.rs` | 64 | `JobRunnable` (object-safe, `async_trait`), the blanket impl, `JobRegistration`, `inventory::collect!`, `register_job!` |
| `jobs/host.rs` | 220 | `JobHost::run` (shutdown-aware loop), `registry`, `process`, `hold`, `adopt_trace`, `backoff`, `record_failure` |
| `jobs/audit_prune.rs` | 60 | `AuditPruneJob` — daily, `[retention].audit` + `audit_max_entries` |
| `jobs/wal_checkpoint.rs` | 51 | `WalCheckpointJob` — `[storage].checkpoint_interval`, `wal_checkpoint(PASSIVE)` |

39 unit tests and 2 doctests added (`services/mod.rs`, `jobs/mod.rs`); every file has its single
trailing column-0 `#[cfg(test)] mod tests`.

`src/lib.rs` gained exactly one line, `pub mod prelude;`. **No other file outside
`src/{services,jobs}/` and `src/prelude.rs` was touched, and `rustak-server/Cargo.toml` was not
edited at all** — see "Dependencies" below.

## Deviations from the design, and why

1. **`Services` has no supertraits.** Design 01 §3.2 wrote `pub trait Services: Send + Sync +
   'static`. All three bounds moved to the use sites, which is also where automate has them:
   - `'static` had to move, because `&'a S` is never `'static` and the brief requires
     `impl<S: Services> Services for &S`.
   - `Send`/`Sync` followed, because with them as supertraits clippy's `implied_bounds_in_impls`
     rejects the `impl Services + Send + Sync + 'static` that every lifted handler signature
     writes. Keeping the supertraits would have meant rewriting every job and handler signature to
     `impl Services + 'static` and having later briefs discover that by hitting the lint.

   Nothing is lost: everything that spawns still spells out `impl Services + Send + Sync +
   'static`, and `JobContext<S>` requires it.

2. **`content()` and `jwt()` return `Result<Arc<…>, Error>`, not `&ContentStore`/`Arc<JwtKeys>`.**
   Both handles are built by M0-10 (`store::ContentStore`, `auth::JwtKeys`), which is landing
   concurrently, and the signing keys are in any case *read out of the database using the secret
   store the context already holds* — so they cannot be constructor arguments without building the
   context twice. They live in `Late<T>` slots (`services/late.rs`): a shared `OnceLock` that every
   clone of the context sees, installed once by `runtime::run_all` through
   `AppContext::install_content` / `install_jwt`, and reporting a `Kind::System` error naming the
   handle if read before then.

   **Wiring this up in M0-10/M0-12 is two one-line alias changes** in `services/mod.rs`:

   ```rust
   pub type ContentStore = Pending;   // → pub type ContentStore = crate::store::ContentStore;
   pub type JwtKeys = Pending;        // → pub type JwtKeys = crate::auth::JwtKeys;
   ```

   `Pending` is an uninhabited enum, so until those aliases move the slots are provably empty and
   the accessors are honest rather than panicking. The slot, its accessor, its documentation and
   its tests are already in place.

3. **`Database::from_config` takes `(&StorageConfig, &Path)`**, not `&StorageConfig` alone: the
   paths in `[storage]` resolve against `[server] data_dir`, which that section does not carry.
   `SecretStore::from_config(&AuthConfig, &Path)` is exactly as the brief specified. Both are
   inherent constructors written in `services/wiring.rs` — Rust allows an inherent `impl` anywhere
   in the defining crate — so `db/` and `crypto/` stayed untouched and still take plain parameters.

4. **`AppContext::new` returns `Result`.** automate `expect()`s on the `reqwest::Client` builder;
   the conventions forbid that outside tests, and a failure there means the TLS backend did not
   initialise, which is worth saying out loud.

5. **`Job::job_hash` was not lifted.** Nothing in rustak calls it, and it would have pulled in the
   `sha256` crate (rustak has `sha2`) for an unused default method.

6. **`dispatch`/`dispatch_delayed`/`dispatch_in` return `impl Future + Send` rather than being
   `async fn`.** An `async fn` in a trait promises nothing about `Send`, so a recurring job re-arming
   itself from inside `setup` — whose future *is* required to be `Send` — could not call it. The
   span is attached with `.instrument(…)` instead of `#[instrument]` for the same reason.

7. **The enqueue inside `dispatch_in` uses `Queue::enqueue` directly rather than a `Partition`
   handle.** `Partition<D, T>` carries a `PhantomData<T>` and so is `Sync` only when `T: Sync`,
   which would have meant widening `Job::JobType` to demand `Sync` of every payload for the sake of
   a partition name the function already holds.

## The shutdown-aware job host

automate's host sits inside a blocking `dequeue_any` that cannot be cancelled, and is torn down by
the process exiting. rustak stops cleanly — there is a write-ahead log to fold back in, and the
host holds the connection that has to do it — so the loop is built around
`Queue::try_dequeue_any` (the polling form M0-07 added for exactly this) and every wait is a
`select!` against the shutdown token:

- the loop condition checks `is_cancelled()`;
- the poll itself is `select!`ed against `cancelled()`, `biased` so cancellation wins a tie;
- the idle wait (`POLL_INTERVAL`, 1 s) and the error backoff (5 s) are both `select!`ed the same way;
- on exit, `JoinSet::shutdown()` aborts in-flight jobs. Their messages were never `complete`d, so
  they are retried; a shutdown that waits out a five-minute job is a shutdown an operator kills.

`the_host_returns_within_a_second_of_cancellation` spawns the host, lets it settle into the poll,
cancels, and asserts both that `tokio::time::timeout(1 s, …)` succeeds and that the measured
elapsed time is under a second. `a_host_whose_context_is_already_cancelled_returns_at_once` covers
the other end. An implementation that ignored the token would hang rather than merely be slow, so
the test fails loudly either way.

Two trade-offs are recorded in the code:

- Losing the `select!` can abandon a reservation the dequeue transaction had already committed —
  one message waits out its window before anybody sees it again. That is a few minutes' delay on one
  message, once, weighed against a shutdown that could otherwise be held up by a database busy
  timeout.
- `Job::setup` runs before the first poll and is **not** interruptible. Documented on
  `JobHost::run`: a job's wiring belongs in the queue, not in a network call.

## Recurring work without a scheduler

There is no cron in M0. A periodic job arms itself in `setup` and re-arms itself at the *top* of
`handle`, enqueueing under a fixed idempotency key. This works because of how the queue rows behave
(M0-07): re-enqueueing under the same key resets `reserved_by = NULL`, so the `complete` the host
runs after a successful handler — which requires `reserved_by = <reservation>` — removes nothing
and the rescheduled message survives. Re-arming *first* means a run that fails is one missed run
rather than the end of the schedule. This is automate's `CronJob` pattern with the workflow lookup
removed, and it is documented in `jobs/mod.rs`.

| Job | Partition | Armed at start-up | Cadence | Work |
|---|---|---|---|---|
| `AuditPruneJob` | `housekeeping/audit-prune` | immediately | 24 h (constant) | `AuditStore::prune_audit_log([retention].audit, [retention].audit_max_entries)` |
| `WalCheckpointJob` | `housekeeping/wal-checkpoint` | one interval out | `[storage].checkpoint_interval` | `Database::checkpoint(Checkpoint::Passive)` |

The prune runs immediately because an installation upgrading into it should not wait a day to come
back inside its limits; the checkpoint does not, because nothing has been written yet. Both re-read
their configuration on every run, so changing `checkpoint_interval` takes effect on the next run
rather than needing a restart (`re_arming_follows_the_configuration_rather_than_the_message`).

`PRUNE_INTERVAL` is a constant rather than a setting: the limits are configurable, and they are
what an operator actually has an opinion about.

## Retry backoff (new, using `queues.attempts`)

M0-07 added an `attempts` column and noted it is "what a backoff should read". `JobHost::backoff`
does: on failure the message is re-held for `timeout × 2^(attempts − 1)`, capped at 15 minutes.
A job that wants to be retried at once says so with a timeout in the past, which stays in the past
however often it is doubled — so the lifted `FailingJob` test still asserts immediate retriability.
`JobContext::attempts()` exposes the same number to handlers that want to give up rather than retry
for ever.

## Dependencies

**None added.** `inventory`, `async-trait`, `reqwest`, `opentelemetry`, `tracing-batteries`,
`chrono`, `tokio` and `serde_json` were all already in `rustak-server/Cargo.toml`, and
`rustak-server/Cargo.toml` is unmodified.

One consequence worth flagging: `AppContext::new_mock` builds its telemetry session inline as
`Session::new("rustak", "0.0.0-test").with_battery(tracing_batteries::Testing)` rather than calling
`rustak_core::telemetry::testing_session`, which is behind **rustak-core's** `testing` feature.
Reaching it would have meant adding `rustak-core = { workspace = true, features = ["testing"] }`
to `[dev-dependencies]` and `testing = ["rustak-core/testing"]` to `[features]` — an edit to a
manifest three other briefs were in flight against, for three lines. M0-12 may want to make that
change when it wires up `tests/bootstrap.rs`; nothing here depends on it either way.

## Notes for later briefs

1. **`AppContext::new(config, db, secrets, session, shutdown) -> Result<Self, Error>`.** M0-12's
   `run()` should be: `Database::from_config` → `SecretStore::from_config` → `AppContext::new` →
   `ContentStore::open` + `install_content` → `JwtKeys::load_or_create` + `install_jwt` →
   `runtime::run_all`. The two installs must happen before anything is spawned.
2. **`JobHost::run(context: AppContext) -> Result<(), Error>`** is one of the three futures
   `run_all` joins. It returns `Ok(())` on a clean cancellation, and `Err` only for a duplicate
   partition or a failing `setup` — both of which mean housekeeping would silently never run, so
   they are worth failing start-up over.
3. **`crate::prelude`** is `rustak_core::prelude::*` plus `Config`, `AuditStore`, `Cache`,
   `KeyValueStore`, `Queue`, `Job`, `JobContext`, `AppContext`, `Services`. The storage traits are
   in it because their methods are only in scope where the trait is; `db::Database`, the repos and
   the crypto types are deliberately not.
4. **`Database` carries three `partition()` methods** (M0-07 note 5), so a bare
   `services.kv().partition(…)` is ambiguous where more than one trait is in scope. Name it:
   `KeyValueStore::partition::<T>(&services.kv(), "name")`.
5. **Writing a job** is: a unit struct, `impl Job for it`, `register_job!(TheJob);`. Registration
   happens at link time, so there is no list to keep in step — but also no compile error if two
   jobs claim one partition; `JobHost::registry` refuses that at start-up and
   `every_registered_job_owns_a_partition_of_its_own` refuses it in CI.
6. **`HTTP_USER_AGENT`** is `SierraSoftworks/rustak/<CARGO_PKG_VERSION>`, applied by the shared
   client. Always clone `services.http_client()` rather than building another — it shares the
   connection pool.
7. `services::Late<T>` is available for any other handle a later brief finds itself wanting to
   install after the context exists; it is not specific to these two.

## Exit checks

### `cargo test -p rustak-server -- services:: jobs::`

```
running 46 tests
test result: ok. 46 passed; 0 failed; 0 ignored; 0 measured; 335 filtered out; finished in 0.43s

     Running unittests src/main.rs
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rustak_server
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s
```

exit status 0. Seven of the 46 are the pre-existing `db::repos::services::` tests the `services::`
filter also matches; the 39 written here are:

```
jobs::audit_prune::tests::a_log_inside_its_retention_is_left_alone
jobs::audit_prune::tests::a_run_trims_the_log_to_the_configured_count_and_re_arms
jobs::audit_prune::tests::setting_up_arms_the_schedule_to_run_at_once
jobs::host::tests::a_failed_message_stays_on_the_queue_under_the_job_s_own_window
jobs::host::tests::a_host_whose_context_is_already_cancelled_returns_at_once
jobs::host::tests::a_job_asking_to_be_retried_at_once_still_is
jobs::host::tests::a_message_nothing_is_registered_for_is_dropped
jobs::host::tests::a_processed_message_runs_its_handler_and_leaves_the_queue
jobs::host::tests::every_registered_job_owns_a_partition_of_its_own
jobs::host::tests::only_a_chosen_idempotency_key_is_surfaced_to_the_handler
jobs::host::tests::the_backoff_doubles_per_attempt_up_to_its_cap
jobs::host::tests::the_host_returns_within_a_second_of_cancellation
jobs::host::tests::the_host_sets_every_registered_job_up_before_it_consumes
jobs::host::tests::the_registry_is_keyed_by_partition
jobs::job::tests::a_context_carries_what_the_message_was_enqueued_with
jobs::job::tests::a_delayed_dispatch_is_not_due_yet
jobs::job::tests::dispatching_puts_a_message_on_the_job_s_own_partition
jobs::job::tests::the_default_timeout_is_the_documented_one
jobs::runnable::tests::a_payload_the_handler_cannot_read_is_a_user_error_naming_the_problem
jobs::runnable::tests::the_erased_view_deserialises_the_payload_and_dispatches
jobs::runnable::tests::the_erased_view_reports_what_the_job_declared
jobs::runnable::tests::the_erased_view_runs_the_job_s_own_setup
jobs::wal_checkpoint::tests::a_run_checkpoints_and_re_arms_itself
jobs::wal_checkpoint::tests::re_arming_follows_the_configuration_rather_than_the_message
jobs::wal_checkpoint::tests::setting_up_arms_the_schedule_one_interval_out
services::late::tests::a_pending_slot_can_never_be_filled
services::late::tests::a_second_install_is_refused
services::late::tests::a_slot_starts_empty
services::late::tests::every_clone_sees_what_any_clone_installs
services::tests::a_borrowed_handle_stands_in_for_an_owned_one
services::tests::every_clone_shares_one_shutdown_signal
services::tests::the_context_is_built_from_the_handles_start_up_opens
services::tests::the_late_handles_report_themselves_missing_rather_than_panicking
services::tests::the_stores_are_the_database_underneath
services::tests::the_user_agent_names_the_release
services::wiring::tests::a_relative_storage_path_is_taken_against_the_data_directory
services::wiring::tests::a_retired_key_that_cannot_be_read_is_refused_by_name
services::wiring::tests::the_database_is_opened_where_the_configuration_says
services::wiring::tests::the_secret_store_generates_a_key_beside_the_database_when_none_is_configured
```

`jobs::host::tests::the_host_returns_within_a_second_of_cancellation` was run three times in a row
to check for flakiness; stable at ~0.33 s for the eleven `jobs::host` tests each time.

The whole crate, so that nothing already landed regressed:

```
$ cargo test -p rustak-server
test result: ok. 380 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 2.61s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
```

(The one ignored test is M0-10's `#[ignore]`d append-log throughput check.)

### `cargo clippy --workspace --all-targets -- -D warnings`

```
   Compiling rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.55s
```

exit status 0.

### `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`

```
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
 Documenting rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.63s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 6 other files
```

exit status 0.

### `cargo fmt --all --check`

```
(no output)
```

exit status 0.

### `./scripts/check-file-length.sh`

```
(no output)
```

exit status 0. As M0-04 recorded, the script iterates `git ls-files` and so does not yet see this
brief's untracked files. The same counter (`awk`, functional = non-blank, non-comment, before the
column-0 `#[cfg(test)]`) run over them explicitly:

```
prelude.rs                  5
services/mod.rs           166
services/late.rs           70
services/wiring.rs         30
services/mock.rs           21
jobs/mod.rs                10
jobs/job.rs               127
jobs/runnable.rs           64
jobs/host.rs              220
jobs/audit_prune.rs        60
jobs/wal_checkpoint.rs     51
```

Largest is `jobs/host.rs` at 220 of the 300 allowed.

## Concurrency notes

M0-10 (`store/`, `pki/`, `auth/`), M0-13 (`rustak-ui`) and M0-15 (`rustak-client`) were all in
flight in the same working tree while this brief ran.

- Nothing under `src/{store,pki,auth}/` was read or written here, and `src/lib.rs` gained only
  `pub mod prelude;`.
- Mid-brief, workspace-wide `clippy`/`doc` runs reported findings in `store/append_log.rs`,
  `pki/ca.rs`, `store/content.rs` and `rustak-client/src/sidecar/mod.rs`; those were left alone and
  had been fixed by their own agents by the time the exit checks above were taken. The exit-check
  outputs are all from clean runs over the tree as it now stands.
- `cargo fmt -p rustak-server` formats the whole crate (M0-07 note 6). This brief's files were
  checked with `rustfmt --edition 2024 --check` on the specific paths to avoid reformatting
  anybody else's work; the final `cargo fmt --all --check` is a read-only verification.
