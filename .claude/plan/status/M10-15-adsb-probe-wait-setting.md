# M10-15 — ADS-B: the longest wait before re-probing a refused rung is a setting

**Status: complete.** Exit checks: see the end of this note.

## What changed and why

M10-05 capped the wait before a refused rung is probed again at a constant
three hours. The maintainer's decision (2026-09-29): three hours stays the
default, and the operator can change it from the file **and** the Services page.

- **The cap is a field, not a constant.** `state/probe.rs`: `MAX_WAIT` became
  `DEFAULT_MAX_WAIT` (3 h, unchanged) plus `LONGEST_MAX_WAIT` (24 h),
  `shortest_max_wait(configured)` (= `CLEAN_RUN` × the configured interval) and
  `out_of_range(key, wait, configured)`, which words a refusal naming the key
  and giving each bound in both units (`"1d (1440 minutes)"`, `"10m (10
  minutes)"`). `Probes` holds `max_wait`; `set_max_wait` changes it without
  touching the probe in progress or the refused rung, so the next
  `required()` is measured against the new cap. `Cadence` and the plugin's
  `SourceState` pass it through (`probe_max_wait`, `set_probe_max_wait`), and
  `AdsbFeed` gained `state_mut()` (all four feeds) so it can be changed on an
  open source.
- **In the file:** `[settings.source] probe_max_wait` (humane duration, like
  `poll`) for `aggregator` and `opensky`, the two rate-limited upstreams.
  `Source::open` applies it to the feed it opens. `Settings.source` is read
  with `deserialize_with = "checked"`, which refuses a value below one clean
  run at the interval the source will open at (resolved as the source resolves
  it: provider default, 2 s floor, OpenSky tier) or above 24 h — so `--check`
  (which only loads the file) refuses it too, with the toml span and
  "In [settings.source]: `probe_max_wait` is 25h …".
- **On the Services page:** new `src/remote.rs`, `AdsbConfig` = the shared
  `FeedConfig` (`#[serde(flatten)]`) + `probe_max_wait_minutes: Option<u32>`
  with `title`, description (doc comment), `minimum: 1`, `maximum: 1440`,
  `default: 180`. `config_schema()` is now `schema_for::<AdsbConfig>()`, so the
  server's schema check holds the static bounds. `validate_config` runs
  `FeedConfig::check` and then the relation to the open source's configured
  interval (only the static bounds before a source is open), reporting at
  `/probe_max_wait_minutes`.
- **Applied while running,** like `area`/`symbology`: `AdsbSidecar` keeps the
  wait in effect and where it came from (`FROM_SERVER`, `FROM_FILE`, new
  `FROM_DEFAULT`). At start the file's value (or the default) goes in first and
  the server's wins; the start line carries `probe_max_wait_s` and
  `probe_max_wait_from`. On each new document a tick applies the server value,
  or gives the choice back to the file when the document has none (as for
  `symbology`); it is set on the open source in place, logged once at `info`
  with `probe_max_wait_s`/`probe_max_wait_from` only when value or source
  changed. A value the open source cannot use (below one clean run at its
  `poll` — the schema cannot know that) is one `warn` and the current value
  stays. A source reopened for a new area gets the wait in effect (`watch`).

## Decisions

- **Minutes, not `"3h"`, on the Services page.** Read from the form as it is
  on `main` (`rustak-ui/src/pages/service_config/{schema,form,inputs}.rs`): an
  `integer` draws `NumberInput` with the schema's `minimum`/`maximum`, which
  shows the range as the placeholder and marks a value outside it before
  saving; a `string` draws a plain `TextInput` that shows no pattern or range
  and only learns the bounds when the save is refused. So the document key is
  `probe_max_wait_minutes` (unit in the key, as `radius_km` does) while the
  file keeps a duration. Nothing under `rustak-ui/` was edited.
- **Static schema minimum is 1 minute** (one clean run at 1 s, the fastest any
  source here polls); the real floor depends on the source's `poll` and is
  checked by `validate_config` / `--check`.
- **Not on `readsb`/`replay` in the file.** A receiver of one's own does not
  rate-limit; the server value still applies to whatever source is open.
- **A changed cap keeps what was learnt.** Only the cap moves; `times` of the
  refused rung is kept, so a longer cap later resumes the doubling where it was.

## Measured (simulated clock, provider budget 30 s, `poll` 10 s)

Refusals on day 1 / day 2 (day 2 is steady state, the wait at its cap; each
refused probe is two refusals):

| cap | day 1 | day 2 |
|---|---|---|
| 1 h | 50 | 48 |
| 3 h (default) | 22 | 16 |
| 6 h | 16 | 8 |
| 24 h | 14 | 2 |

So six hours halves the steady-state rate, as M10-05 predicted, but only cuts
the first day from 22 to 16 because the wait starts at 60 polls and has to
double up. README and `config.example.toml` say this.

## Tests (all on the hand-moved clock or pure state steps)

- `state.rs`: `the_default_longest_wait_is_three_hours_as_m10_05_left_it`
  (default = 3 h; default run identical to an explicit 3 h; 22 on day 1, as
  M10-05); `a_longer_cap_costs_fewer_refusals_a_day_and_a_shorter_one_more`
  (exact counts in the table above);
  `a_changed_cap_applies_to_the_next_wait_and_keeps_the_refused_rung` (12 h at
  the default, next refused probe, cap set to 1 h: the next probe comes after
  exactly 3600/35 = 102 clean polls, more than the 60 a forgotten rung gets).
- `settings.rs`: file value reaches the opened feed (aggregator, OpenSky) and
  unset is the default; `25h` refused naming `probe_max_wait` and `1440
  minutes`; `9m` at adsb.lol's 10 s refused naming `10m`; OpenSky `poll = 60s`
  with `30m` refused naming `1h`; `10m` and `24h` accepted.
- `remote.rs`: schema carries title/description/min/max/default and still the
  shared `area`/`symbology`, key not required; documents read back;
  1441 refused at `/probe_max_wait_minutes`; 9 at 10 s refused, 10 accepted,
  9 accepted before a source is open; a string value and a zero radius refused.
- `lib.rs`: `apply_probe_max_wait` from the server (6 h, from the server), an
  unusable one (25 h) changes nothing, none gives the choice back (default);
  `validate_config` on a started sidecar accepts 360 and refuses 1441 at the
  pointer.
- `tests/end_to_end.rs`: a control API answering
  `{"probe_max_wait_minutes":360}` puts 6 h on the readsb source at start.

On a host ten times slower: nothing sleeps or reads elapsed host time; the
simulations step a `DateTime` by hand and assert counts, so they run the same
steps in the same order. The end-to-end test waits only on local wiremock
requests, with no upper time bound.

## Files

Changed: `rustak-plugin-adsb/Cargo.toml` (`schemars`, workspace, already in the
lockfile via `rustak-client`), `Cargo.lock` (one line: the dependency edge),
`rustak-plugin-adsb/README.md`, `rustak-plugin-adsb/config.example.toml`,
`rustak-plugin-adsb/src/lib.rs`, `src/settings.rs`, `src/sources/mod.rs`,
`src/sources/{aggregator,opensky,readsb,replay}.rs` (`state_mut`),
`src/sources/state.rs`, `src/sources/state/cadence.rs`,
`src/sources/state/probe.rs`, `tests/end_to_end.rs`.
Added: `rustak-plugin-adsb/src/remote.rs`, this note.

## Exit checks (run in this worktree)

1. `cargo fmt --check` — pass.
2. `cargo clippy --workspace --all-targets -- -D warnings` — pass (re-run for
   `-p rustak-plugin-adsb` after a last doc-comment reflow: pass).
3. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` — pass.
4. `./scripts/check-file-length.sh` — pass. It reads `git ls-files`, so the
   new untracked `remote.rs` was counted with the same awk: 43. `lib.rs` is
   now 277.
5. `cargo test -p rustak-plugin-adsb` — 152 unit + 11 end-to-end, all pass.

## Open

- **A better control in the form** (M10-14's `components/schema_form/`, not
  touched here): a duration input — e.g. `type: string` with `format:
  "duration"` or a `x-unit` hint, drawn as a number plus a unit picker (min /
  h) that writes the humane form — would let the Services page and the file
  share `probe_max_wait = "3h"` instead of minutes. Also, the form draws a
  field's `default` only when a value is added; showing it as the placeholder
  of an empty optional field ("180 if unset") would make "leave empty to use the
  file" clearer.
- The floor that depends on `poll` is enforced by the sidecar, so a document
  saved while the sidecar is **not** running is held only to 1–1440 by the
  server; the sidecar warns and ignores a too-short value when it reads it.
- `docs/plugins.md` does not list per-plugin server-side keys; left alone.
