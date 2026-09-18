# M2-11 — Stream and harness tidy-ups from the backlog — complete

Brief: `.claude/plan/briefs/M2-11-stream-harness-tidy.md`.
Read first: `conventions.md`; status files `M2-06` (the account-level selection and its one gap),
`M1-05` (segments, retention, `stream_segments`), `M2-09` (the negotiation knob and its 3-line patch),
`M0-19` (the second-signal sequence).

Five independent backlog items, each with its own test. All five are done.

---

## 1. Account-level active channels now live in a table

`PUT /Marti/api/groups/active` with no `clientUid` is the account's selection — CloudTAK's browser
half, the admin UI and a script all send it that way, because none of them has a device. M2-06 kept
that answer in `kv`, where the Marti reads could see it and **the routing path could not**: a device
enrolled *after* an account-level change has no `device_group_state` rows, so `members.effective`
read "nothing switched off" and routed on the channel until that device called the endpoint itself.

**`user_group_state` (migration `0012`)** closes it, because the same SQL that intersects grants with
the device's preference can now fall back to the account's:

```sql
LEFT JOIN device_group_state d ON … AND d.device_id = ?2
LEFT JOIN user_group_state   u ON … AND u.user_id   = m.user_id
WHERE … AND COALESCE(d.active, u.active, 1) = 1
```

Three layers, most specific first — device, account, on — and the same order is now read by the SQL,
by the `__ANON__` fallback and by `marti::channels::Selection`. They must not drift apart: the
endpoint telling a client one thing while the router does another is the bug the table exists to
close. Nothing is migrated out of `kv`; an account that had a selection there re-reads as
everything-on until it sets one again, which is the permissive direction and corrects itself the
first time a client calls the endpoint (ATAK does, on connect).

`group_set` is deliberately **unchanged**. It answers *entitlement* — what the admin API and the
membership listings ask about — and a channel somebody has switched off is still a channel they
hold. The new `user_state().effective()` is the no-device counterpart of `members().effective()`,
and it is what `stream/resolver.rs` and `members::reauth` now call for a connection with no device.

### Where the code went

`identity/members.rs` was at 230 functional lines and the account layer added ~75, so the file was
split by responsibility rather than trimmed:

- **`identity/members.rs`** — the *rights* half: grants, `replace_manual`, `channels_changed`.
- **`identity/active.rs`** (new) — the *preference* half: `effective_for_device`,
  `effective_for_account`, `set_active`, `set_active_for_user`, `active_for_device`,
  `active_for_user`, and the `__ANON__` fallback.

`members` re-exports `active`, so **no existing caller changed** — `web/api/devices.rs`,
`auth/cert.rs` and `marti/channels.rs` still say `members::effective_for_device`, and the two files
that belong to other agents were not touched. The device-state unit tests stayed in `members.rs`,
where they drive `active` through those re-exports, which is how every caller reaches it.

## 2. `[retention] cot_history_max_rows` is enforced

`config.example.toml` has always claimed both horizons are enforced; only the age one was. The cap
is **per stream key** — per device uid, for CoT — so one talkative source cannot evict everybody
else's history, and it is decided by a window function over the index rather than by reading every
segment row into memory:

```sql
SELECT … FROM (
  SELECT s.*, SUM(s.record_count) OVER (
    PARTITION BY s.stream_key ORDER BY s.last_time DESC, s.id DESC
    ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS newer
  FROM stream_segments s WHERE s.stream_kind = ?1
) WHERE sealed = 1 AND COALESCE(newer, 0) >= ?2
```

A segment goes when the segments *newer* than it already reach the cap, so it is entirely surplus.
The floor is therefore `max_rows` plus the tail of the straddling segment — the same approximation
the age horizon makes, for the same reason. Unsealed segments count towards the cap (they are the
newest thing there is) and are never the thing deleted.

**`max_rows = 0` means no cap, not "keep nothing".** History is files on disk, and an installation
that wants none of it turns `[stream.limits] record_history` off rather than asking for every sealed
segment to be unlinked six hours after it was written. Documented in `config.example.toml`,
`cot_store/retention.rs` and the test name.

`Swept` gained an `over_cap` field, so a log line and a test can tell the two reasons apart.

## 3. The negotiation knob is threaded, not published

M2-09's patch, applied verbatim plus the tidy-up it named. `ConnLimits::negotiate` is a
`NegotiationMode` instead of a `bool`, `stream/mod.rs` fills it from `[stream] negotiation` (or
`Silent` when `negotiate_protobuf` is off, because an installation that does not offer protobuf
makes no offer whatever the switch says), and `config/stream.rs` lost `SELECTED`, `select`,
`selected` and `select_negotiation`. `Negotiation::new` is gone; `with_mode` is the only
constructor.

Two servers in one process can now have different modes, and `config::stream`'s and
`stream::negotiation`'s tests no longer have to be written around each other's global.

## 4. The stream surface probe no longer races the listener

The stream listener binds *after* the public one, so a probe pass that started when
`/api/v1/health` answered could reach a port nothing was on yet and skip every stream scenario for
the run — intermittently. `interop/shared/src/probe.ts` now retries the bare TCP connect for **5 s**
at 100 ms intervals; `waitForPort` in `launch.ts` already closed the same gap for a suite that
starts its own server, and this closes it for one probing a server somebody else started.

Both suites get it from the shared file, so `interop/node-tak/src/surfaces.ts` and
`interop/eud/src/surfaces.ts` needed no change (deviation 3). A genuinely unserved port costs 5 s
once per run, not once per surface: there is only ever one `on: "stream"` probe.

## 5. Exit status on a second signal, and the e2e shutdown budget

`main.rs` checks `shutdown.is_aborted()` after `run` returns and **exits 130** — after the telemetry
flush, so nothing is lost. `run` returns `Ok` on both paths because it checkpointed the database
either way, so a status of 0 made "stopped cleanly" and "cut off mid-drain" indistinguishable to
whatever was supervising. `docs/deployment.md` now says so, with the `SuccessExitStatus=130` note a
`Restart=on-failure` unit needs.

`e2e/playwright.config.ts`'s `gracefulShutdown` went from **5 s to 15 s**. The server's own budget is
8 s of draining plus 2 s for the WAL checkpoint, so the old value SIGKILLed it in the middle of the
checkpoint it had just been asked to make.

---

## Files

**New**

| File | Functional lines (limit 300) | Tests |
|---|---:|---:|
| `rustak-server/migrations/0012_user_group_state.sql` | — | — |
| `rustak-server/src/db/repos/user_state.rs` | 91 | 4 |
| `rustak-server/src/identity/active.rs` | 158 | 6 |
| `rustak-server/tests/stream_channel_state.rs` | exempt | 3 |

**Changed**

| File | Change |
|---|---|
| `rustak-server/src/db/repos/mod.rs` | `pub mod user_state`, the re-exports, `Database::user_state()` |
| `rustak-server/src/db/repos/members.rs` | `effective` falls back to `user_group_state`; `to_set` is `pub(super)` |
| `rustak-server/src/db/repos/stream_segments.rs` | `over_row_cap` |
| `rustak-server/src/identity/members.rs` | split: the active-state half moved to `active`, re-exported |
| `rustak-server/src/identity/mod.rs` | `pub mod active` and one sentence of the module map |
| `rustak-server/src/marti/channels.rs` | the account selection reads and writes the table, not `kv`; 2 new tests |
| `rustak-server/src/stream/resolver.rs` | the no-device path uses `effective_for_account` |
| `rustak-server/src/cot_store/retention.rs` | `sweep` takes `max_rows`; `Swept::over_cap`; 5 new tests |
| `rustak-server/src/store/append_log.rs` | `remove_indexed` factored out of `prune_before` and made public |
| `rustak-server/src/jobs/retention.rs` | passes `config.retention.cot_history_max_rows` |
| `rustak-server/src/config/stream.rs` | `SELECTED`/`select`/`selected`/`select_negotiation` deleted |
| `rustak-server/src/stream/negotiation.rs` | `Negotiation::new` deleted; `with_mode` documents where the mode comes from |
| `rustak-server/src/stream/connection.rs` | `ConnLimits::negotiate: NegotiationMode` |
| `rustak-server/src/stream/mod.rs` | fills it from `[stream] negotiation` |
| `rustak-server/src/main.rs` | `exit(130)` after the flush when the drain was aborted |
| `config.example.toml` | what `cot_history_max_rows = 0` means |
| `docs/deployment.md` | the exit status of a stop that was cut short |
| `e2e/playwright.config.ts` | `gracefulShutdown` 5 s → 15 s |
| `interop/shared/src/probe.ts` | the stream probe retries for 5 s |
| `.claude/plan/backlog.md` | the five lines this brief landed, removed |

No new dependency. One migration, one `Swept` field, no new configuration key.

---

## Exit checks

```
$ ./scripts/check-file-length.sh
rustak-server/src/config/validate.rs: 313 functional lines (limit 300)
  ← another agent's file (+235 lines this session); every file this brief touched is under.
    Longest of mine: db/repos/members.rs 263, store/append_log.rs 269, identity/active.rs 158.

$ cargo fmt --all --check
Diff in rustak-client/src/sidecar/mod.rs, rustak-server/src/config/validate.rs,
        rustak-server/src/web/api/settings.rs, rustak-server/tests/acme_directory.rs
  ← four other agents' files; none of this brief's.

$ cargo test -p rustak-server --features testing --test stream_channel_state
running 3 tests
test a_device_that_has_switched_the_channel_on_overrules_the_account ... ok
test an_account_that_has_switched_nothing_off_still_receives_everything ... ok
test a_device_enrolled_after_an_account_level_change_routes_on_it ... ok
test result: ok. 3 passed; 0 failed

$ cargo test -p rustak-server --features testing --test marti_channels --test stream_routing \
                                                  --test stream_session --test stream_store
marti_channels: ok. 9 passed    stream_routing: ok. 12 passed
stream_session: ok. 10 passed   stream_store:   ok.  8 passed

$ cargo test -p rustak-server --lib --features testing
test result: FAILED. 1552 passed; 8 failed; 2 ignored
    pki::acme::renew::*            (2)   ← another agent's in-flight files
    pki::acme::challenge::*        (2)
    plugins::auth::*               (2)
    web::api::{services,}::*       (2)
  Every test in this brief's modules passed:
    identity::active            6/6      db::repos::user_state       4/4
    identity::members          11/11     db::repos::members         11/11
    marti::channels             8/8      db::repos::stream_segments  6/6
    cot_store::retention        8/8      store::append_log          14/14
    config::stream              9/9      stream::negotiation        10/10

$ cargo doc -p rustak-server --no-deps
(unresolved-link warnings in six other agents' files; none in this brief's)

$ cargo clippy -p rustak-server --lib -- -D warnings
error: this boolean expression can be simplified   --> src/plugins/auth.rs:205, :206
  ← another agent's file; nothing in this brief's

$ cd interop/node-tak && npm run typecheck && npm test
ℹ tests 25   ℹ pass 25   ℹ fail 0   ℹ skipped 0
(the three TODO(M4) mission scenarios now pass: M4-01/02 landed)

$ cd interop/eud && npm run typecheck && npm test
ℹ tests 41   ℹ pass 41   ℹ fail 0
[eud] surfaces served:  enrollment, stream, clientEndPoints, channels, certificateRevocation, missionPackages
[eud] surfaces missing: (none)
[eud] 0 passed, 9 skipped, 0 failed        (no container runtime on this machine)
```

---

## Deviations

1. **Four files outside the brief's list were touched, all of them unowned.** The brief named
   `marti/channels.rs`, `identity/members.rs`, `stream/resolver.rs`, `cot_store/retention.rs`,
   `config/stream.rs`, `stream/negotiation.rs`, `stream/{connection,mod}.rs`, the migration and
   `tests/stream_*.rs`. A migration with no repository is inert and a `sweep` signature change has a
   caller, so the work also needed `db/repos/{mod,members,stream_segments}.rs`,
   `db/repos/user_state.rs` (new), `identity/{mod,active}.rs` (one new), `jobs/retention.rs`,
   `store/append_log.rs`, `config.example.toml` and `docs/deployment.md`. None of these is in
   another agent's list, and the two files that *are* — `web/api/devices.rs` and `auth/cert.rs`,
   which call `members::effective_for_device` — were left untouched by re-exporting rather than
   moving.

2. **`identity/members.rs` was split rather than trimmed.** The account layer took it to 306
   functional lines. Splitting by responsibility (rights vs. preference) is what `conventions.md`
   asks for; the alternative was deleting documentation to get under a number.

3. **`interop/{node-tak,eud}/src/surfaces.ts` were not changed.** The brief listed them with item 4,
   but the race is entirely in the shared probe and both suites bind to it. Editing their surface
   maps would have been a change with no effect in files the CI steward may also be in.

4. **The e2e launcher still exits before the server has drained.** `e2e/scripts/start-server.mjs`
   handles `SIGTERM` by `child.kill("SIGTERM")`, removing the scratch directory, and calling
   `process.exit(130)` at once — it does not wait for the child. Playwright signals the whole
   process group, so the server does get its own `SIGTERM` and the raised budget is real; but the
   directory it is checkpointing into has already been removed by then. Out of scope here (the brief
   named `playwright.config.ts` and nothing else, and the launcher is an e2e harness file), and
   worth one backlog line: *the e2e launcher should await the child's exit before cleaning up*.

5. **`cot_history_max_rows = 0` disables the cap** rather than keeping nothing, which is the
   opposite of what `audit_max_entries` would do with the same value. Deliberate and documented:
   the audit log is rows in a database that an operator might genuinely want emptied, while this
   one would unlink every sealed history file on the next six-hourly sweep — a config typo with a
   destructive blast radius and an existing, clearer way to say it
   (`[stream.limits] record_history = false`).

6. **`Swept` gained a field rather than folding the two counts together.** A sweep that removed
   segments because they were old and one that removed them because a device is noisy are different
   operational facts, and an operator reading `over_cap = 4000` on every sweep should raise the cap
   rather than the age.

---

## Not done here

- **`kv` still holds whatever M2-06 wrote** under `marti-channels/active-<user id>`. Nothing reads
  it; a migration that copied the rows across would have had to parse a JSON blob written by a
  previous version, for a selection each client re-sends on its next connect.
- **`/Marti/api/groups/activeForce`** is still unbuilt (M2-06 deviation 5), so CloudTAK's
  force-activate flow still goes through the ordinary `active` write.
- **The row cap is not enforced on `cot_latest`**, which is one row per uid and swept by staleness.
  A per-device cap there would be a cap of one.
- **No exporter for the new counters.** `over_cap` is an `info!` field like `segments` beside it.

---

## Concurrency note

Six other agents were writing in this tree throughout. `cargo test --lib` could not be built for
roughly forty minutes because of in-progress edits in `pki/acme/{challenge,account,transport}.rs`,
`plugins/auth.rs`, `web/api/packages.rs` and `rustak-client/src/http.rs` — none of them this
brief's, and each cleared on its own. The integration tests, which link the library without its
`#[cfg(test)]` modules, were green throughout and are what proved items 1 and 2 end to end.
`rustak-cot/src/msgs.rs` (M1-08) carried unused-import warnings for the whole session; likewise not
this brief's.

The lib suite did eventually build: **1552 passed, 8 failed**, and all eight failures are in
`pki/acme/**`, `plugins/auth.rs` and `web/api/**`, which are three other agents' files mid-edit.
Every test in the ten modules this brief touched passed.
