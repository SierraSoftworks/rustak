# M10-05 — ADS-B: remember the rung that was refused

**Status: complete.** All exit checks green in the worktree (fmt, clippy
`--workspace --all-targets -D warnings`, rustdoc `-D warnings`, file length,
`cargo test -p rustak-plugin-adsb`: 168 unit + 10 integration).

## What changed and why

A step down after a clean run is now a **probe**. New `state/probe.rs`
(`Probes`) remembers the last rung whose probe was refused and how many
consecutive probes of it were. `Cadence` consults it:

- a probe is refused when the cadence *raises* (two refusals inside the window,
  the M9-12 rule, unchanged) while the step down is still on trial;
- the clean run required to step down to that rung again is `60 << times`
  (60, 120, 240 ...), capped at `MAX_WAIT` = 3 h divided by the resting interval
  (308 polls at 35 s), never below 60;
- a probe that survives 60 clean polls "holds" and clears memory at or above
  that rung; a stated delay adopted (`Change::Stated`) clears everything;
- a changed `poll` or provider builds a new `SourceState`/`Cadence`, so memory
  never outlives it (nothing is persisted; a restart also starts afresh).
- Refusals while resting, the first probe after start, the 120 s ceiling, the
  `max(configured, stated)` floor and `Retry-After` handling are untouched.

Logging (change only, no new periodic lines): `Change::Eased` now carries
`over` and says `after 60 clean polls (10m00s)`; new `Change::ProbeRefused`
says `Polling adsb.lol every 35s again: 23s was refused (429). It will be tried
again after 120 clean polls (about 1h10m).` It replaces the `Raised` line for
that event (the `Raised` line's "eases back after 60 clean polls" would be
wrong there).

**Metrics: nothing added.** `source.poll_effective_s` and `poll_configured_s`
already tell an operator the cadence in use; the probe wait is internal
bookkeeping that only changes the frequency of a rare experiment and is
announced in the log when it changes. A metric would be one more number to
explain.

## Tests

- `cadence.rs`: exact doubling 60 -> 120 -> 240 and first probe at exactly 60;
  cap at 3 h of 35 s polls; hold forgets; refusal while resting is an ordinary
  `Raised`; stated delay during a probe is `Stated`, and a stated floor is never
  decayed below; ceiling unchanged.
- `state.rs` (existing `Grudging` provider on the hand-moved clock): a 30 s
  provider settles at 35 s and is refused 22 times in a simulated day (assert
  < 24; today's behaviour is about 58); a provider that relaxes to 5 s is
  followed to 10 s within cap + three ladder rungs and refuses nothing after.
- `wording.rs`: both new sentences.

All are pure state-machine steps or simulated clocks: no sleeping, no wall
clock, so a host ten times slower runs the same steps in the same order.

## Files

Changed: `rustak-plugin-adsb/src/sources/state.rs` (module doc, `mod probe`,
tests), `.../state/cadence.rs`, `.../state/wording.rs`,
`rustak-plugin-adsb/README.md`.
Added: `rustak-plugin-adsb/src/sources/state/probe.rs`, this note.

## Open

- 22 refusals/day (0.9/h) is under one an hour but not dramatically so, because
  each refused probe costs two refusals and the cap is 3 h. A longer cap (6 h)
  would roughly halve it at the price of following a relaxed limit more slowly.
