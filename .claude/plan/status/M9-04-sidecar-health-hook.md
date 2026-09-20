# M9-04 — A sidecar's own heartbeat reaches the Services page

**Status: complete.** Every exit check is green. One deviation from the brief's
literal text (the hook's signature) and one file edited outside the "files you
own" list (`rustak-plugin-adsb/tests/end_to_end.rs`, which asserted the very
behaviour this brief removes) are both recorded below.

`Sidecar::health` now exists, the harness reports what it answers *instead of*
`Heartbeat::healthy()`, both feed plugins have moved onto it, and their two
workarounds — the ADS-B plugin's own `control.heartbeat` call with its 30-second
repeat timer, and the AIS plugin's `status::report` task that slept 500 ms after
every tick to get the last word — are gone.

## What landed

| Area | Files |
|---|---|
| The hook | `rustak-client/src/sidecar/mod.rs` (`Sidecar::health`) |
| The loop | `rustak-client/src/sidecar/run.rs` (`control.report(sidecar.health().await)`) |
| The rule | `rustak-client/src/sidecar/control_link.rs` (`ControlLink::report`) |
| Tracking a plugin's own beat | `rustak-client/src/control/{mod,register}.rs` |
| ADS-B | `rustak-plugin-adsb/src/{lib,health}.rs`, `rustak-plugin-adsb/tests/end_to_end.rs` |
| AIS | `rustak-plugin-ais/src/{lib,status}.rs` |
| Example | `rustak-plugin-example/src/main.rs` (a comment block, nothing else) |
| Tests | `rustak-server/tests/{services_flow,feed_sidecars}.rs` |
| Docs | `docs/plugins.md` |

`rustak-server/tests/feed_support/mod.rs` needed no change: `RunningFeed::start`
already takes the plugin's own `[settings]` block, so the `readsb` case is the
same three lines as the `replay` ones with a `wiremock` URL in place of a path.

## 1. The hook

```rust
async fn health(&mut self) -> Option<Heartbeat> {
    None
}
```

**The brief writes the signature as `health(&mut self, ctx: &SidecarContext<Self::Settings>)`
and then says "name and signature to match the trait's existing style". Those
two disagree, and the style won.** `Sidecar` has a documented section — *Why the
context is only given to `start`* — saying that `start` takes the context and a
plugin that needs it keeps it; `tick`, `on_event` and `stop` all take `&mut self`
and nothing else. Adding a context parameter to `health` alone would have
contradicted that section and forced the harness to hold a clone of a context it
otherwise moves into `start`. Both plugins already keep the context, so neither
needed it passed — AIS reads `self.context…sidecar.tick()` for its degraded
threshold in exactly two lines. **If the orchestrator wanted the parameter, it
is a one-line change to the trait and a one-line change to each of the four
implementations.**

Everything else is as the brief asked: `Option<Heartbeat>`, defaulting to `None`,
asked after every tick, and the harness sends `Heartbeat::healthy()` for `None`
exactly as it did before.

### Not clobbering a plugin that reports for itself

Tracked on the control client rather than on the link, because the client is the
thing the plugin calls and the link holds the same `Arc` of it:

* `ControlClient` gained `reported: Arc<AtomicBool>`. `ControlClient::heartbeat`
  — the public call a plugin makes — sets it *before* sending, so a plugin that
  has spoken is not talked over whether or not the server took what it said.
* The actual request moved to `pub(crate) ControlClient::post_heartbeat`, which
  does not set the flag. That is what the harness uses, so the harness's own beat
  never claims the tick and never suppresses the next one.
* `ControlLink::report(Option<Heartbeat>)` reads and clears the flag once per
  tick. `true` → send nothing at all and log at `debug`; otherwise send the
  hook's heartbeat, or `healthy()` when the hook answered `None`.

The rule is the brief's literal one: **the harness sends only when the plugin did
not report during that tick**, so a plugin that implements the hook *and* calls
`control.heartbeat` in the same tick gets its direct call kept and the hook's
answer dropped. `docs/plugins.md` says not to do both for the same report and
says why the hook is preferred.

## 2. The plugins

**ADS-B.** `health::heartbeat` is unchanged; what went is `REPEAT_AFTER`,
`AdsbSidecar::report`, the `reported: Option<(Instant, ServiceState)>` field and
the `self.report().await` in `tick`. `AdsbSidecar::heartbeat()` is the public
builder (`Option<Heartbeat>`, `None` before `start` has opened a source) and
`health` is one line around it. The `context` field went too: nothing read it any
more, and `-D warnings` will not tolerate a field that is only written.

**AIS.** `FeedStatus::heartbeat(poll, now)` is unchanged; what went is
`REPORT_AFTER`, `status::report`, the `status: watch::Sender<FeedStatus>` field,
the `tokio::spawn` in `start`, the `send_replace` in `tick`, and the *Why the
report is a task* section of that module's documentation. `health` is
`Some(self.snapshot().heartbeat(poll, Utc::now()))` with `poll` read from the
kept context, exactly as the M9-01 status note predicted.

The heartbeat now goes out on **every** tick for both plugins rather than on a
change or a 30-second timer. That is not more load than before: the harness was
already posting one per tick, and this replaces it rather than adding to it.

**Example.** A comment block after `tick` showing the override and saying that a
plugin without one is reported `healthy`. No behaviour, as the brief asked.

## 3. Tests

**`rustak-server/tests/services_flow.rs`** — two new cases, both control-only
sidecars (no stream, no enrolment: everything under test happens between `tick`
and the control API):

* `a_plugins_own_health_report_is_what_the_services_listing_shows` — a `Reporter`
  plugin whose `health` returns `degraded`, a message and metrics including a
  tick counter. `GET /api/v1/services`, read with a real administrator session
  over HTTP because that listing is administrative, shows exactly that state,
  message and metrics after the first tick, and **still** shows them once the
  tick counter has moved on. A harness beat landing after the hook's would show
  up here as a healthy row with no metrics.
* `a_sidecar_with_the_default_health_hook_is_listed_healthy` — a `Quiet` plugin
  with no `health` at all: `healthy`, no message, no metrics.

Two helpers were added for them: `administrator` (creates an `is_admin` account
and mints a session through `rustak_server::testing::context::session_for`) and
`listed` (the `GET /api/v1/services` row for a name). Nothing existing in that
file was changed.

**`rustak-server/tests/feed_sidecars.rs`** — `the_adsb_sidecar_publishes_what_a_readsb_receiver_serves`,
the case M9-02 could not add: a `wiremock` receiver serving
`rustak-plugin-adsb/tests/fixtures/readsb.json` (`include_str!` across the crate
boundary rather than a second copy), the plugin's `Readsb` source, and the fake
EUD asserting the CoT type, callsign, `hae`, `<track course/speed>` and the
remarks. It also asserts the health hook end to end through the real control API:
`metrics.source.kind == "readsb"`, `connection == "connected"`, `tracked == 5`.
Nothing existing in that file was changed.

**`rustak-client`** — `control_link.rs` gained three cases (healthy for `None`,
the hook's heartbeat verbatim, and a plugin's own call not being talked over,
with the next tick reported normally), and `run.rs` one that drives the whole
loop and asserts three ticks produce three heartbeats and all three are the
plugin's words.

**`rustak-plugin-{adsb,ais}`** — each gained a `health()` case and a
"not started answers `None`" case; the AIS case that used to call
`snapshot().heartbeat(…)` now goes through the hook.

## The one file edited outside "files you own"

`rustak-plugin-adsb/tests/end_to_end.rs` →
`the_sidecar_reports_its_feed_and_its_upstream_to_the_control_api` asserted that
`tick` posts a heartbeat to a mock control API, and that a second unchanged tick
posts nothing (the `REPEAT_AFTER` timer). Both of those are the workaround this
brief deletes, so the test could not be left alone and `cargo test -p
rustak-plugin-adsb` is an exit check. It is now
`the_sidecar_reports_its_feed_and_its_upstream_through_the_health_hook`: same
fixtures, same assertions on the heartbeat's contents, read from `health()`
rather than from the mock's request log — plus a new assertion that the plugin
posts **nothing** of its own, which is the regression guard for the workaround
coming back. No other test in that file was touched.

## What is not verified here

1. **No live upstream and no real admin UI.** The Services page rendering
   `metrics` is M9-03's and is unchanged by this; what is proved here is that the
   row the page reads carries the plugin's state, message and metrics. The AIS
   `aisstream` and the ADS-B aggregator/OpenSky sources still have only the
   coverage M9-01 and M9-02 gave them — this brief changed neither.
2. **The "both mechanisms in one tick" rule is proved at unit level only**
   (`control_link.rs`), not through a server. A plugin doing both is documented
   as a thing not to do, and no plugin in the tree does it.
3. **`ServiceState` transitions over a long run** — a feed going degraded and
   back while the server's sweep is running — are not exercised. The sweep is
   untouched and still moves a silent service to `unknown` after 90 s.
4. **Nothing was run on GitHub Actions** from this session.

## Exit checks

```
$ cargo fmt --check
(no output; exit 0)

$ cargo clippy --workspace --all-targets -- -D warnings
    Checking rustak-plugin-ais v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-ais)
    Checking rustak-plugin-example v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-example)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 15.89s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
 Documenting rustak-plugin-example v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-example)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 16.42s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 8 other files

$ ./scripts/check-file-length.sh
(no output; exit 0)
```

Every file this brief touched was checked by hand with the same `awk` as well,
because `git ls-files` does not see an untracked path: the largest is
`rustak-plugin-ais/src/lib.rs` at 217 functional lines, then
`rustak-client/src/sidecar/mod.rs` at 151 and `rustak-plugin-adsb/src/lib.rs` at
144. `tests/` trees are exempt by the script's own rule.

```
$ cargo test -p rustak-client -p rustak-plugin-ais -p rustak-plugin-adsb -p rustak-plugin-example
     Running unittests src/lib.rs (rustak_client)
test result: ok. 168 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.02s
     Running tests/stream_client.rs
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
     Running tests/stream_tls.rs
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
     Running unittests src/lib.rs (rustak_plugin_adsb)
test result: ok. 97 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
     Running tests/end_to_end.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
     Running unittests src/lib.rs (rustak_plugin_ais)
test result: ok. 64 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
     Running unittests src/main.rs (rustak_plugin_example)
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo test -p rustak-server --test services_flow --test feed_sidecars
test the_adsb_sidecar_publishes_what_a_readsb_receiver_serves ... ok
test the_adsb_sidecar_publishes_its_replayed_aircraft_with_their_altitudes ... ok
test the_ais_sidecar_publishes_its_replayed_vessels_to_a_device_on_the_channel ... ok
test a_feed_publishes_nothing_for_a_track_outside_its_area ... ok
test one_vessel_reported_ten_times_in_a_second_is_published_once ... ok
test the_ais_sidecar_publishes_what_a_receiver_sends_it_over_udp ... ok
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.24s
test a_sidecar_enrols_connects_registers_reports_and_hears_what_the_server_saw ... ok
test a_sidecar_with_the_default_health_hook_is_listed_healthy ... ok
test a_plugins_own_health_report_is_what_the_services_listing_shows ... ok
test a_sidecar_whose_registration_is_removed_registers_again ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.99s
```

Run beyond the brief's list, because a change to the harness reaches every suite
that drives a sidecar:

```
$ cargo test --workspace
(52 suites, every one `test result: ok`; 3150 passed, 0 failed, exit 0)
```

## Conventions and boundaries

- No `git` or `but` command of any kind was run.
- Files touched: the eleven the brief names, plus
  `rustak-plugin-adsb/tests/end_to_end.rs` for the reason above. The workspace
  `Cargo.toml` and `Cargo.lock` were not touched at all, and no dependency was
  added: `AtomicBool` and `Arc` are `std`, and `wiremock` was already a
  dev-dependency of both `rustak-client` and `rustak-server`.
- `.claude/plan/{plan,backlog}.md` and every CI file are unchanged.
- The M9-01 and M9-02 status notes still describe their workarounds as shipped;
  both already say M9-04 supersedes them, and neither is this brief's to rewrite.
- Nothing secret can reach a heartbeat that could not before: the hook moves
  *where* the same `Heartbeat` is built from, not what goes in it, and both
  plugins' "nothing in a heartbeat is a credential" tests still run.
- No ATAK, TAK Server or OpenTAKServer source was read, and nothing was copied
  from a GPL project. The `readsb` fixture the new `feed_sidecars` case serves is
  `rustak-plugin-adsb`'s own, written by hand by M9-02.
