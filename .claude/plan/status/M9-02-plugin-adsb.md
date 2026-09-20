# M9-02 — `rustak-plugin-adsb`: aircraft on the map from open ADS-B sources

**Status: complete, with two things recorded below rather than built** — the
end-to-end case lives in this crate's own `tests/` rather than in
`rustak-server/tests/feed_sidecars.rs` (a file this brief does not own and
another agent was editing in parallel), and the richer heartbeat reaches the
server but is overwritten by the harness a moment later, which is a one-line
gap in `rustak-client/src/sidecar/run.rs` that this brief does not own either.

The plugin now has three live sources besides `replay`: a `readsb`/`dump1090`
receiver, the public aggregators, and the OpenSky Network. Nothing in
`rustak-client/**` was changed — the `feed` API M9-00 shipped expressed every
one of them, which is the result that brief was hoping for.

## What landed

| Area | Files |
|---|---|
| Wire types | `rustak-plugin-adsb/src/wire.rs` |
| Mapping | `rustak-plugin-adsb/src/mapping.rs` |
| Sources | `rustak-plugin-adsb/src/sources/{mod,state,readsb,aggregator,opensky,replay}.rs` |
| Settings | `rustak-plugin-adsb/src/settings.rs` |
| Heartbeat | `rustak-plugin-adsb/src/health.rs` |
| Plugin | `rustak-plugin-adsb/src/lib.rs` |
| Tests | `rustak-plugin-adsb/tests/end_to_end.rs`, `rustak-plugin-adsb/tests/fixtures/{readsb,opensky}.json` |
| Config + docs | `rustak-plugin-adsb/{Cargo.toml,config.example.toml,README.md}`, `docs/plugins.md` (the ADS-B paragraph), `README.md` (one sentence) |

`rustak-plugin-adsb/src/main.rs`, `Dockerfile` and `tracks.example.ndjson` are
unchanged from M9-00. **The workspace `Cargo.toml` was not touched at all**:
`reqwest` (rustls), `serde_json`, `chrono`, `url` and `wiremock` were already in
`[workspace.dependencies]`, and `rustak-api` (for `Heartbeat`/`ServiceState`)
was too — so M9-01 had that block to itself.

### The `Source` enum

```toml
[settings.source]
kind = "readsb"        # url_or_path = "/run/readsb/aircraft.json" | "http://…/data/aircraft.json", poll = "1s"
kind = "aggregator"    # provider = "adsb_lol" | "adsb_fi" | "airplanes_live", poll = "5s"
kind = "opensky"       # client_id?, client_secret?, poll? ("10s" anonymous, "5s" authenticated)
kind = "replay"        # path = "tracks.ndjson"
```

`Source::open` takes the `Area` now (`open(&self, area) -> Box<dyn AdsbFeed>`),
because two of the three live sources subscribe with it. `url_or_path` is the
brief's name kept verbatim: it is one setting that is honestly two things, and
`url` would have been a lie for the common local-file case.

Four decisions worth writing down:

- **One parser, both aggregator array keys.** The brief says the provider picks
  "the URL template and the array key"; `Snapshot` accepts `aircraft` *and* `ac`
  and answers whichever carried data, so `Provider` picks only the URL. Two
  keys, one type, and a test that proves both. (Verified by probing both
  endpoints once on 2026-09-20: adsb.lol answered 37 aircraft under `ac`,
  adsb.fi 38 under `aircraft`, same objects.)
- **`AdsbFeed: Feed` is this crate's trait, not an addition to the shared one.**
  Reporting "connected / reconnecting since / last error" needs one method the
  shared `Feed` does not have. Adding it to `rustak_client::feed` would have
  made every feed plugin implement an HTTP-shaped concept, so it is a local
  supertrait with a `ReplayFeed` wrapper around M9-00's `Replay`. The brief's
  licence to change `rustak-client/src/feed/**` was therefore not used.
- **`SourceState` is the only thing that logs a state change.** A source that
  logged every failed poll would write the same line every five seconds. It also
  owns the poll floor (independent of the sidecar's tick) and the capped
  exponential backoff (interval → ×2 per consecutive failure → 5 minutes).
- **`Aircraft`/`StateVector` do not use `deny_unknown_fields`**, unlike every
  other type in rustak. These are not files an operator wrote; a `readsb`
  release that adds a field must not take the plugin down. The conventions'
  rule is about configuration, and this is somebody else's document.

### Mapping

`src/mapping.rs`, every arm unit-tested: the readsb categories and the OpenSky
numbers onto `TrackKind`, `dbFlags` bit 1 flipping `A*` to `MilitaryFixedWing`
(including `A7` — the shared model has no military-rotary class, and a military
helicopter drawn as a civil one is the worse mistake, which the module
documents), feet→metres and knots→m/s, the `"ground"` sentinel, the `~` prefix
kept, and the ten ordered remark lines. Aircraft with no position or
`seen_pos > 60 s` are skipped; the seven-aircraft fixture yields five tracks for
exactly that reason.

`observed_at` is computed against **our** clock (`now - seen_pos`) rather than
the receiver's, because a receiver with no NTP is common and the publisher
measures staleness against wall-clock now. OpenSky's is against the server's
`time`, which is the clock that stamped `time_position`.

### Observability (deliverable 5)

`health::heartbeat` builds a `rustak_api::Heartbeat` carrying the source kind
and name, `connected`/`reconnecting`/`never_connected`, when that began, the
last error, the tracked count and the four `FeedCounters`; `unhealthy` when the
source has never connected, `degraded` when it has been reconnecting for more
than two poll intervals, `healthy` otherwise. `AdsbSidecar::report` posts it
through `ctx.control()` — the surface `docs/plugins.md` documents — on a state
change or every 30 s, swallowing failures. A test asserts the exact JSON that
reaches a mock control API.

The per-service config KV **is** read cheaply at start-up: `configured_area`
reads `GET /api/v1/services/<name>/config`, honours an `area` key, logs at
`info` that the server's setting won, and treats every failure as "use the
file". Tested against a mock.

## What is not verified, and why

1. **The harness overwrites this sidecar's heartbeat.**
   `rustak-client/src/sidecar/run.rs` sends `Heartbeat::healthy()`
   unconditionally *after* `Sidecar::tick` returns, so whatever the plugin
   posted during its tick is replaced in `services.record_heartbeat` (which
   writes state, message and metrics wholesale) a few milliseconds later. The
   Services page will therefore show `healthy` with no metrics until that
   changes. Everything on the plugin's side is built and tested; the missing
   piece is one of:
   - a `Sidecar::health(&self) -> Heartbeat` hook the harness calls instead of
     hard-coding `Heartbeat::healthy()`, or
   - the harness skipping its own beat when the plugin reported during the tick.

   Both are inside `rustak-client/src/sidecar/**`, which this brief explicitly
   does not own (its licence covers `feed/**` only, and only when a source
   cannot be expressed). Recommend M9-03 or a follow-up brief take the first
   option — it also gives `rustak-plugin-ais` the same thing for free.

2. **The end-to-end case is in `rustak-plugin-adsb/tests/end_to_end.rs`, not in
   `rustak-server/tests/feed_sidecars.rs`.** The brief asks for the live-source
   case to run "through the M9-00 harness … on a fake EUD", but that harness
   lives in `rustak-server`, which "Files you own" does not list and which the
   orchestrator told both parallel agents not to touch — and M9-01 was appending
   its own case to the same file while this ran. The substitute drives the same
   plugin through `Sidecar::start`/`tick` against a `wiremock` receiver and
   asserts on `rustak_cot::xml::write(&event)` — the exact bytes
   `rustak_client::stream` puts on the wire — for the CoT type, callsign, `hae`,
   `<track course/speed>`, the remarks and the absence of an `endpoint`. What it
   does **not** cover is the hop from `tick`'s return value to a device, which
   `feed_sidecars.rs`'s existing ADS-B replay case already covers and which is
   unchanged. Adding a `Readsb`-over-wiremock case there is a three-line job for
   whoever integrates this.

3. **No live source was reached from this session.** Outbound TCP from a spawned
   binary is blocked in this sandbox: the plugin's request to
   `api.adsb.lol/v2/point/51.47750/-0.46140/22` times out after ten seconds
   while `curl` to the same URL answers `200` in 0.2 s, and a bare Python
   `socket.create_connection(("api.adsb.lol", 443))` times out too. The two
   documentation probes (one GET each to adsb.lol and adsb.fi, with the
   descriptive user agent) went through the allowed tool and confirmed the
   response shapes the fixtures are built from. What the bounded live run *did*
   prove is the failure path: warn, back off, keep ticking, stop cleanly, exit 0.
4. **`airplanes.live` remains unverified**, as the brief said to expect. It is
   implemented behind the same code path as the other two providers and is
   marked experimental in the README, in `config.example.toml` and in the
   `Provider` variant's own documentation.
5. **OpenSky was not exercised against the real service** for the same reason as
   (3). The token exchange, the `401 → refresh → retry`, the `429` with
   `X-Rate-Limit-Retry-After-Seconds`, the anonymous fall-back when the token
   endpoint is down, and the poll floor are all covered by `wiremock` tests
   against a mock token endpoint and a mock states endpoint.

## Exit checks

```
$ cargo fmt -p rustak-plugin-adsb --check
(no output; exit 0)
```

(Workspace-wide `cargo fmt --check` reported one trailing-blank-line diff in
`rustak-plugin-ais/src/sources/udp.rs`, which is M9-01's file and was left
alone.)

```
$ cargo clippy --workspace --all-targets -- -D warnings
    Checking rustak-plugin-adsb v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-adsb)
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.23s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
 Documenting rustak-plugin-adsb v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-adsb)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.37s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 8 other files

$ ./scripts/check-file-length.sh
(no output; exit 0)   # the new files are untracked, so `git ls-files` does not
                      # yet see them; checked by hand with the same awk — the
                      # largest is sources/opensky.rs at 281, then mapping.rs at
                      # 259 and sources/aggregator.rs at 199.

$ cargo test -p rustak-plugin-adsb
test result: ok. 95 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

$ cargo test -p rustak-server --test feed_sidecars
test the_adsb_sidecar_publishes_its_replayed_aircraft_with_their_altitudes ... ok
test the_ais_sidecar_publishes_its_replayed_vessels_to_a_device_on_the_channel ... ok
test one_vessel_reported_ten_times_in_a_second_is_published_once ... ok
test a_feed_publishes_nothing_for_a_track_outside_its_area ... ok
test the_ais_sidecar_publishes_what_a_receiver_sends_it_over_udp ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.17s

$ cargo build --release -p rustak-plugin-adsb
    Finished `release` profile [optimized] target(s) in 19.96s

$ cargo run -p rustak-plugin-adsb -- --config rustak-plugin-adsb/config.example.toml --check
INFO The configuration is valid; --check does not start the sidecar.
(exit 0)
```

Also run by hand, because "it runs, publishes and stops cleanly" is not
something a unit test asserts:

```
$ cd rustak-plugin-adsb && ../target/debug/rustak-plugin-adsb --config config.example.toml
INFO The ADS-B sidecar is watching. source="replay" upstream="replay" area=Circle { lat: 51.4775, lon: -0.4614, radius_km: 120.0 } affiliation=Unknown
INFO The feed is publishing. tracked=5 offered=5 published=5 suppressed=0 expired=0
^C
INFO The ADS-B sidecar is stopping. source="replay" offered=10 published=5 suppressed=5 expired=0
(exit 0)
```

and the same against the live `adsb.lol` aggregator, which the sandbox's egress
policy turned into a demonstration of the failure path instead:

```
INFO Reading aircraft from a public aggregator. adsb.lol is open data; see https://adsb.lol for the current terms. provider="adsb.lol" lat=51.4775 lon=-0.4614 radius_nm=22
WARN The ADS-B source stopped answering; retrying with backoff. We could not reach adsb.lol. (User error)
 - error sending request for url (https://api.adsb.lol/v2/point/51.47750/-0.46140/22)
 - operation timed out
INFO The ADS-B sidecar is stopping. source="aggregator" offered=0 published=0 suppressed=0 expired=0
(exit 0)
```

## Conventions and boundaries

- No `git` or `but` command of any kind was run.
- Files touched outside `rustak-plugin-adsb/**`: the ADS-B paragraph in
  `docs/plugins.md` and one sentence in the root `README.md` (rewritten so that
  the AIS clause still says "in progress" and M9-01 can flip its own half
  without a conflict). Nothing else, and the workspace `Cargo.toml` not at all.
- No ATAK, TAK Server or OpenTAKServer source was read, and nothing was copied
  from a GPL project. The emitter-category tables are from the public ADS-B
  category definitions and the CoT types are M9-00's; both fixtures are written
  by hand from the field names in the public documentation rather than captured.
- No credential appears in a log, a heartbeat, a `Debug` rendering or a fixture.
  `client_secret` is a `rustak_core::Secret` (which prints `Secret(***)`), the
  bearer token is one too, a `readsb` URL's user information is stripped out of
  the name used in logs, and a test asserts each of those.
