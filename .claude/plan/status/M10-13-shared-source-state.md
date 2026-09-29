# M10-13 — One `SourceState` for the feed plugins

**Done.** A new `rustak_client::feed::upstream` module holds what the polled
feed plugins shared by copy; ADS-B, ESB and FIRMS run on its `SourceState`,
AIS on its `Repeated`. Log wording, levels and cadence, metric names and timing
rules are as on main, with the two exceptions listed under "Differences"
below. All exit checks pass.

## The decision, written down before moving anything

Read from the code (and M10-04's duplication list):

| Thing | Verdict |
|---|---|
| `notice.rs` (`Repeated`, `Report`, `humanised`, `REMIND_EVERY`) | **Shared as is.** ADS-B/ESB/FIRMS identical; AIS's older copy is `Repeated::new(ZERO)`, now `Repeated::default()` |
| Outage run + rate-limit run, reminder cadence, recovery line | **Shared** in `SourceState` |
| Doubling backoff, `MAX_DOUBLINGS = 6`, `since`, `last_error`, `ever_connected`, `rate_limited`, `ready_at` | **Shared** |
| "`poll` is a floor": stated delay above the interval, unstated = 2× interval and said to be ours, never below the interval | **Shared** |
| Tolerant `retry_after()` (M9-12) | **Shared**; ESB and FIRMS now use it too |
| Counters-line cadence (300 s, due at once) | **Shared** as `Every` / `REPORT_EVERY`; `FeedPublisher`, ESB's `OutagePublisher` and FIRMS' `Hotspots` use it. The line itself stays each plugin's |
| `http_client` builder | **Shared** (`user_agent`, `timeout`, default headers); each plugin keeps its `USER_AGENT` and timeout |
| Backoff ceiling, `Retry-After` ceiling, 429-is-an-answer, refusal-run settle, every sentence | **Parameters** (`Rules` trait) — the plugins disagree |
| ADS-B adaptive cadence (AIMD, refused-rung memory) | **Not shared.** No second plugin would use it as is. Stays in ADS-B, which keeps a `Cadence` beside the shared state and moves the shared interval through `SourceState::set_interval` before every outcome |
| AIS `status.rs` `SourceState` (watch-channel connection enum) | **Not shared**: streaming, a different shape. AIS shares `Repeated` only |
| `area` / `area_from` / `ServiceSettings` block in `lib.rs` | **Not shared**: settings, not source state, and it sits on `sidecar::ServiceSettings` which is not this brief's. Each plugin keeps its own, so the disagreement below is kept as it was |

The decision is also the module doc of `rustak-client/src/feed/upstream/mod.rs`.

## Shape

`upstream::SourceState<R: Rules>`; each plugin has a unit type implementing
`Rules` and `pub type SourceState = upstream::SourceState<Firms>` (ESB: `Esb`),
so every call site, `health.rs` and the heartbeat code are unchanged. ADS-B's
`SourceState` is a small struct: the shared state plus its `Cadence`,
delegating everything else.

`Rules` has two required items (`MAX_BACKOFF`, `subject(name)`) and defaults
for the rest: `MAX_RETRY_AFTER = MAX_BACKOFF`, `REFUSAL_IS_AN_ANSWER = false`,
`REFUSALS_SETTLE_OVER_POLLS = 0` (settle = `max(REMIND_EVERY, n × interval)`),
and ten sentence functions. The default sentences are ADS-B's (the M9-12
"says who chose the number" wording). The level and fields of each line belong
to `SourceState`, so they are the same for every plugin.

Per plugin:

| | ADS-B | ESB | FIRMS |
|---|---|---|---|
| `MAX_BACKOFF` | 300 s | 900 s | 3600 s |
| `MAX_RETRY_AFTER` | 300 s | 3600 s | 3600 s |
| `REFUSAL_IS_AN_ANSWER` | no | no | **yes** |
| `REFUSALS_SETTLE_OVER_POLLS` | 0 (5 min) | 3 | 3 |
| Words | defaults, "The ADS-B source" | own, by name ("ESB PowerCheck") | own for refusals, "The FIRMS source" |

## Differences from main, stated rather than silent

1. **ADS-B's refusal reminder line now carries `seconds` and `stated` fields**,
   as ESB's and FIRMS' always did (the three shared one log call). Wording,
   level and cadence unchanged.
2. **ESB and FIRMS read `Retry-After` tolerantly** (decimal seconds rounded up,
   the RFC 850 and asctime date forms, a date's delay rounded up to whole
   seconds). The brief names M9-12's tolerant reading as the shared one; before,
   those forms read as "no delay stated".

Everything else was checked line by line: each sentence is byte-identical to
main (asserted in each plugin's wording test); the order of log lines within
one call is unchanged; FIRMS' "`since` resets when not connected" and ADS-B/ESB's
"on recovery or first answer" are the same condition (proved from when
`connected` can be false) and are one rule now.

## Questions for the orchestrator (disagreements kept by parameter)

1. **Retry-After floor after the wait.** ESB/FIRMS return to the operator's
   `poll` after a stated delay; ADS-B keeps a floor raised after two stated
   delays inside ten polls (its `Cadence`). Kept: the shared state never raises
   its own floor; ADS-B does it through `set_interval`. Should ESB/FIRMS adopt
   a stated floor too?
2. **Is a `429` an answer?** FIRMS: yes (ends an outage, "connected"); ADS-B and
   ESB: no (schedule only). Kept as `REFUSAL_IS_AN_ANSWER`. One rule for all?
3. **Refusal-run settle.** ADS-B 5 min; ESB/FIRMS `max(5 min, 3 × poll)`. Kept
   as `REFUSALS_SETTLE_OVER_POLLS`. The ESB/FIRMS rule is the more general one
   (with ADS-B's 5–120 s polls it would give the same 5 min).
4. **Retry-After ceiling.** ADS-B caps at its 5-minute backoff, ESB at an hour
   above a 15-minute backoff, FIRMS at its 1-hour backoff. Kept as
   `MAX_RETRY_AFTER`.
5. **A configuration document without `area`.** ESB/FIRMS give the choice back
   to the file; ADS-B keeps the current area. Not touched (the settings block
   was not lifted); still a question.

## Tests: moved, merged, stayed

Nothing asserted before is unasserted now. Counts: rustak-client lib 393
(+new module), ADS-B 168 → 139 unit, ESB 101 → 87, FIRMS 68 → 58 unit (+7
end-to-end unchanged), AIS 77; the difference is the moved tests, now in
`rustak-client`.

**Moved to `rustak-client/src/feed/upstream/`:**
- `notice.rs`: all nine of ADS-B's `notice` tests (ESB's and FIRMS' were the
  same nine: merged). AIS's eight merged into them; AIS's differently-numbered
  reminder case and "ends on the first clear" became
  `the_default_run_is_an_outage_that_ends_on_the_first_clear`.
- `retry.rs`: ADS-B's eight `retry_after` tests (header name now
  `RETRY_AFTER`); ESB's `a_retry_after_is_a_delay_or_nothing` (2 cases) and
  FIRMS' `a_retry_after_is_read_in_either_form_or_not_at_all` merged into
  `a_retry_after_in_seconds_…` / `…_we_cannot_read_…`; ESB's
  `a_retry_after_as_a_date_is_the_delay_until_then` became
  `a_retry_after_as_a_date_on_the_wall_clock_is_the_delay_until_then`.
- `rules.rs`: ADS-B `wording` tests for `refused`, `refused_again`,
  `still_refusing`, `stopped_refusing` (four), plus a new one for the shared
  outage sentences.
- `state_tests.rs`: from ADS-B `a_successful_poll_holds_…`,
  `the_backoff_doubles_…` (now through `failed_at`, not by setting the field),
  `a_failure_remembers_…`, `recovering_clears_…`,
  `a_rate_limit_moves_the_schedule_…`, `a_rate_limit_never_asks_…_cap`,
  `a_short_rate_limit_…`, `a_provider_that_refuses_every_other_request_…`
  (now `a_run_of_refusals_settles_after_five_minutes_…`); from ESB
  `a_new_source_…` (merged with ADS-B's generic half), `an_answer_holds_…`
  (merged with FIRMS' `a_source_is_asked_once_an_interval_…`),
  `a_failure_is_remembered_…` (now `a_failure_after_an_answer_…`); ESB's and
  FIRMS' `an_outage_is_said_once_…` merged into one. New: reminder with a
  count, ceiling below the interval, refusal-not-an-answer, refusal-is-an-answer,
  stated delay believed past the backoff, unstated 2×, `set_interval`,
  `due_at`, and the shared versions of the explicit-poll-floor and
  refusal-run tests.
- `every.rs`: the "due at 0, 300, 600" cadence, once; a late tick; a clock
  stepping backwards.

**Stayed (what is each plugin's own):**
- ADS-B `state.rs`: every cadence test (stated floors, windows, AIMD ladder,
  `Grudging` provider simulations, probes, `mostly_refused`) and
  `a_new_source_is_ready_and_has_never_connected` (configured/mostly_refused);
  `wording.rs`: `each_kind_of_interval_change_…`; `mod.rs`: user agent, client
  builds.
- ESB `state.rs`: its ceiling (`a_stated_delay_is_believed_only_so_far`),
  `being_asked_to_wait_is_not_a_failure`, the explicit-poll floor and
  refusal-run tests with its numbers, plus new
  `being_asked_to_wait_before_the_first_answer_is_not_an_answer` and a wording
  test pinning every ESB sentence. `publish.rs`'s counters-cadence test stays.
- FIRMS `state.rs`: `a_source_is_asked_once_…`, `failures_back_off_to_a_ceiling_…`,
  `a_rate_limit_is_an_answer_…`, `being_asked_to_wait_is_not_an_outage`, the
  floor and refusal-run tests, plus a wording test pinning every FIRMS
  sentence. `hotspots.rs`'s counters-cadence test stays.
- ESB `powercheck_tests.rs`: `due_now()` (a `cfg(test)` helper) is now the
  shared `due_at(Utc::now())`; `an_upstream_that_has_been_gone_for_hours_…`
  used a test-only `answered_at`; it now records the last answer three hours
  ago with `succeeded_at(now − 3h)` before the failing poll. Same assertion.
- `rustak-server/tests/feed_sidecars.rs`: unchanged, 8 passed.

**On a host ten times slower:** every new or moved test is a pure state step
on a hand-moved `DateTime` (or a header value); none sleeps, none reads elapsed
host time, none asserts an upper time bound. Two `retry_after` tests read the
wall clock for an HTTP date two minutes ahead and assert the delay is in
(100 s, 120 s]; they moved unchanged from ESB/ADS-B and would need a > 20 s
stall between two adjacent statements to fail.

## Files

Added:
- `rustak-client/src/feed/upstream/mod.rs` (module doc = the decision, `http_client`)
- `rustak-client/src/feed/upstream/state.rs`, `state_tests.rs`
- `rustak-client/src/feed/upstream/rules.rs`
- `rustak-client/src/feed/upstream/notice.rs` (ADS-B's `notice.rs`, moved)
- `rustak-client/src/feed/upstream/retry.rs`
- `rustak-client/src/feed/upstream/every.rs`
- `.claude/plan/status/M10-13-shared-source-state.md`

Changed:
- `rustak-client/src/feed/mod.rs` (`pub mod upstream`, table row)
- `rustak-client/src/feed/publish.rs` — **outside the brief's list**, smallest
  change: `reported_at` → `Every`, private `REPORT_EVERY` removed
- `rustak-plugin-esb/src/publish.rs`, `rustak-plugin-firms/src/hotspots.rs` —
  **outside the list**, same smallest change (their public `REPORT_EVERY`
  now aliases the shared one)
- `rustak-plugin-adsb/src/sources/{mod.rs, state.rs, state/wording.rs, opensky/token.rs}`
- `rustak-plugin-ais/src/sources/{mod.rs, aisstream.rs, udp.rs}`
- `rustak-plugin-esb/src/sources/{mod.rs, state.rs, powercheck.rs, powercheck_tests.rs}`
- `rustak-plugin-firms/src/sources/{mod.rs, state.rs, firms.rs}`
- `docs/plugins.md` — new section "Polling somebody else's service: `feed::upstream`"

Deleted: `rustak-plugin-{adsb,ais,esb,firms}/src/sources/notice.rs`.

No plugin `lib.rs` needed a change: the sources are wired as before.

## Exit checks

1. `cargo fmt --check` — pass.
2. `cargo clippy --workspace --all-targets -- -D warnings` — pass.
3. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` — pass.
4. `./scripts/check-file-length.sh` — pass. It reads `git ls-files`, so the
   new untracked files were counted with the same awk: `state.rs` 218,
   `rules.rs` 98, `notice.rs` 95, `retry.rs` 47, `every.rs` 28, `mod.rs` 28
   (`state_tests.rs` is exempt as `*_tests.rs`).
5. `cargo test -p rustak-client` (393 + 13 integration + 17 doc),
   `-p rustak-plugin-adsb` (139 + 10), `-p rustak-plugin-ais` (77),
   `-p rustak-plugin-esb` (87), `-p rustak-plugin-firms` (58 + 7) — all pass.
   Also `cargo test -p rustak-server --test feed_sidecars` — 8 passed.

## Open

- The five questions above.
- A process slip: while deleting FIRMS' `notice.rs` I ran `git rm --cached` on
  it, so that one deletion is **staged** in this worktree's index rather than
  only in the working tree. No other git write was run; I did not run another
  to undo it. The content change is the intended deletion either way.
- ESB and FIRMS could now drop their `last_success`/`ever_connected`
  differences in `health.rs` (both are available on the shared type); left
  alone, not this brief's files.
