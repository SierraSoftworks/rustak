# M7-02 — A scheduled sweep for stale CloudTAK hand-over bundles

Brief: `.claude/plan/briefs/M7-02-cloudtak-handover-sweep.md`
Read first: `conventions.md`; `jobs/mod.rs` (the "Recurring work" shape); `jobs/audit_prune.rs` and
`jobs/content_orphans.rs`; `identity/cloudtak/{mod,bundle}.rs`; status `M5-03`;
`rustak-server/tests/cloudtak_onboarding.rs`.

**Status: complete**, with one documented deviation (a four-line deletion in `bundle.rs`, below)
and one deliberate no-op (`docs/deployment.md`, below).

## What landed

| File | Functional lines | Contents |
|---|---:|---|
| `rustak-server/src/jobs/cloudtak_sweep.rs` (new) | 49 | `CLOUDTAK_SWEEP_PARTITION`, `SWEEP_INTERVAL`, `CloudTakSweepTask`, `CloudTakSweepJob`, `register_job!`, four tests |
| `rustak-server/src/jobs/mod.rs` | +5 | `pub mod cloudtak_sweep;` and the re-export block |
| `rustak-server/src/identity/cloudtak/bundle.rs` | −4 | `sweep`'s `debug!` moved to the job; module and function docs corrected |
| `rustak-server/tests/cloudtak_onboarding.rs` | +43 | `the_scheduled_sweep_removes_a_bundle_nobody_ever_came_back_for`, one import, one doc line |

Nothing else was touched. `docs/deployment.md` was left alone (see below).

## The job

`housekeeping/cloudtak-sweep`, `SWEEP_INTERVAL = TimeDelta::minutes(15)`, calling
`identity::cloudtak::sweep(services.db(), Utc::now())`. It follows the `audit_prune` /
`content_orphans` shape exactly: `propagate_parent() = false`, armed in `setup`, re-armed at the
top of `handle` under the partition name as its idempotency key so the schedule can never be armed
twice, and the re-arm comes before the work so a failed sweep is one missed sweep.

Three choices worth recording:

* **Fifteen minutes.** Comfortably outside the bundle's own ten-minute window
  (`BUNDLE_TTL_MINUTES`) and comfortably inside the hour, so an abandoned keystore is gone within
  twenty-five minutes of being prepared. The sweep is one read of a partition this feature alone
  writes to, holding at most a handful of rows, so four runs an hour cost nothing.
* **No configuration knob**, matching every other housekeeping job: `PRUNE_INTERVAL`,
  `SWEEP_INTERVAL` (content and retention) and `EXPIRY_INTERVAL` are all constants, and what is
  configurable in those cases is the *limit*, which an operator has an opinion about. Here the
  limit is `BUNDLE_TTL_MINUTES`, which is not configurable either, so there was nothing to add.
* **`setup` arms it immediately**, like `audit_prune` and `mission_expiry` rather than
  `content_orphans`. A server that was down over the moment a bundle expired has been holding that
  private key ever since, and unlike the content sweep there is no directory walk here that a
  restart is a bad moment for.

### Item 2 of the brief: `sweep` needed no signature change

`identity::cloudtak::sweep` already takes `(&Database, DateTime<Utc>)` and is already re-exported
from `identity::cloudtak`, so `cloudtak::sweep(services.db(), Utc::now())` compiles from a job
handler as written. (`stash` and `take` take `&AppContext`; `sweep` does not, which is exactly what
made it callable.) Nothing additive was needed and nothing was added.

### The one deviation: `bundle::sweep`'s `debug!` moved rather than duplicated

The brief asks the job to "log the count removed at `debug` (nothing when zero)". `bundle::sweep`
already did precisely that, inside itself. Doing both would have produced two near-identical debug
lines per non-empty run, and this crate does not do that anywhere: `mission_expiry::sweep` logs and
its job does not; `store::orphans::sweep` is silent and `ContentOrphansJob` logs. The brief names
`content_orphans.rs` as the template, so I followed that half of the pattern — the primitive
returns the count, the schedule reports it — and deleted the four-line `if swept > 0 { debug!(…) }`
block from `bundle::sweep`.

What this costs: the sweep `stash` runs on its way in is now silent. That is a debug line only, the
count is still returned for any caller that wants it, and with the job in place that call is
belt-and-braces which will almost always find nothing. If the orchestrator would rather have the
line back in `bundle.rs`, restoring it is a four-line revert and the job's own line can go instead.

`bundle.rs`'s module documentation and `sweep`'s own doc comment were corrected in the same pass:
both said the sweep runs "on the next preparation" *rather than* on a timer, which this change
makes false.

### `docs/deployment.md`: deliberately untouched

The brief says to fix the sentence only if it claims the bundle is deleted on expiry. It does not.
The relevant sentence (§ "Onboarding CloudTAK") reads "the key is generated on the server for one
download, sealed at rest, **deleted as the file is handed over, and expired after ten minutes**
whether or not anybody collected it" — "deleted" is scoped to the hand-over and "expired" to the
TTL, both of which were already true and remain true. Per the brief's "otherwise leave docs alone",
nothing was changed.

## Tests

In `jobs/cloudtak_sweep.rs`:

* `the_partition_is_the_one_the_registry_dispatches_on` — the constant matches `Job::partition`,
  **and** the job is in the link-time registry. `JobHost`'s own registry test
  (`every_registered_job_owns_a_partition_of_its_own`) names only `AUDIT_PRUNE_PARTITION` and
  `WAL_CHECKPOINT_PARTITION` explicitly and `host.rs` is not a file this brief owns, so the
  registration assertion is made here instead, by walking `inventory::iter::<JobRegistration>` for
  `CLOUDTAK_SWEEP_PARTITION` — the same iterator `JobHost::registry` builds from.
* `setting_up_arms_the_schedule_to_run_at_once` — `try_dequeue_any` finds it due immediately.
* `a_run_removes_an_expired_bundle_leaves_a_live_one_and_re_arms` — two bundles stashed, one
  backdated, run through `JobRunnable::handle` with a raw `{}` payload; only the live one survives
  and the re-arm is queued more than fourteen minutes out.
* `a_stash_with_nothing_expired_in_it_is_left_alone`.

No test waits a real window out: staleness is the stored `expires_at`, rewritten in place, exactly
as the integration suite's own `backdate` helper does. Both bundles are stashed before either is
backdated, because `stash` sweeps on its way in: closing the first one's window any earlier would
have let the second stash remove it, and the test would have proved nothing about the job.

In `tests/cloudtak_onboarding.rs`:

* `the_scheduled_sweep_removes_a_bundle_nobody_ever_came_back_for` — a real hand-over through
  `POST /api/v1/users/cloudtak/cloudtak-onboarding`, backdated, then the job run through
  `JobRunnable::handle` (the path the host dispatches on, which also proves the queued payload is
  one this handler can read). Asserts the `kv` row is **gone** — not merely refused, which the
  existing `410` assertions cannot distinguish because `take` deletes before it checks the window —
  and that the download is then `410`. This is the assertion the brief asked for: the row
  disappears after the job runs, with no second hand-over involved.

The suite's module documentation gained one line so its list of properties still matches what the
code does.

## Exit checks

Run at 2026-09-19, at the end of the task, against a tree four other agents were still editing.
Every finding below is in a file another brief owns; nothing in my four files is flagged by any
check.

**`cargo fmt --check`** — fails, in M7-01's files only:

```
Diff in /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/runtime.rs:196:
Diff in /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/web/plain.rs:346:
Diff in /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/web/plain.rs:472:
Diff in /Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/web/plain.rs:492:
```

My four files, checked directly so that `cargo fmt` could not rewrite anyone else's:

```
$ rustfmt --check --edition 2024 rustak-server/src/jobs/cloudtak_sweep.rs \
    rustak-server/src/jobs/mod.rs rustak-server/src/identity/cloudtak/bundle.rs \
    rustak-server/tests/cloudtak_onboarding.rs
(no output; exit 0)
```

**`cargo clippy --workspace --all-targets -- -D warnings`** — clean:

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.43s
```

**`cargo doc --workspace --no-deps`** — succeeds; the two warnings are in M7-03's and M7-01's
files (`identity/users.rs:64` links to a private item, `pki/acme/renew.rs:210` has a redundant
explicit link target), and both become errors under CI's `RUSTDOCFLAGS: -D warnings`:

```
warning: `rustak-server` (lib doc) generated 2 warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.06s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 6 other files
```

**`./scripts/check-file-length.sh`** — fails on one of M7-01's files:

```
rustak-server/src/config/validate.rs: 303 functional lines (limit 300)
(exit 1)
```

It passed cleanly earlier in this task, before that file grew. Mine: `jobs/cloudtak_sweep.rs` 49,
`jobs/mod.rs` 38, `identity/cloudtak/bundle.rs` 120 functional lines.

**`cargo test -p rustak-server --features testing --test cloudtak_onboarding`** — all 20, including
the new one (the suite's own module doc gives the `--features testing` spelling; without it the
file is `#![cfg]`'d out and runs nothing):

```
test the_scheduled_sweep_removes_a_bundle_nobody_ever_came_back_for ... ok
...
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.23s
```

**`cargo test -p rustak-server --lib jobs::`**:

```
test result: ok. 58 passed; 0 failed; 0 ignored; 0 measured; 1724 filtered out; finished in 1.34s
```

and the four new ones on their own:

```
running 4 tests
test jobs::cloudtak_sweep::tests::the_partition_is_the_one_the_registry_dispatches_on ... ok
test jobs::cloudtak_sweep::tests::setting_up_arms_the_schedule_to_run_at_once ... ok
test jobs::cloudtak_sweep::tests::a_stash_with_nothing_expired_in_it_is_left_alone ... ok
test jobs::cloudtak_sweep::tests::a_run_removes_an_expired_bundle_leaves_a_live_one_and_re_arms ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 1778 filtered out; finished in 0.06s
```

## Notes for the orchestrator

* The working tree was shared with four other agents throughout. `cargo fmt --check` over the whole
  workspace reports diffs in `rustak-server/src/identity/groups.rs` and
  `rustak-server/src/web/api/me.rs`, which belong to M7-03 and M7-01; my four files are clean under
  `rustfmt --check --edition 2024`, run directly so that `cargo fmt` could not rewrite theirs.
* For long stretches the crate did not build at all because of in-flight work in `pki/acme/**`,
  `web/tls.rs`, `runtime.rs`, `identity/users.rs` and `rustak-core/src/telemetry.rs`. The exit
  checks below were run once the tree compiled again; any error lines they still carry from those
  files are not mine.
* No `git`/`but` command of any kind was run.
