# M9-01 — `rustak-plugin-ais`: ships on the map from open AIS sources

**Status: complete, with two documented deviations** (Digitraffic is not implemented, by the
brief's own escape hatch; the heartbeat is reported through a workaround that **M9-04
supersedes**). Every exit check is green.

The plugin now has three sources — a receiver of your own over UDP, the AISStream.io
WebSocket feed, and the M9-00 replay file — an AIS-specific mapping with a per-MMSI static
cache, two staleness horizons, and a status report for the Services page.

## What landed

| Area | Files |
|---|---|
| Mapping | `rustak-plugin-ais/src/mapping.rs` (ship types, nav statuses, sentinels, remarks, the `Track` builder) |
| Static cache | `rustak-plugin-ais/src/vessels.rs` (`Observation`, `Vessels`) |
| Status | `rustak-plugin-ais/src/status.rs` (`Connection`, `FeedStatus`, `report`) |
| Sources | `rustak-plugin-ais/src/sources/{mod,udp,aisstream}.rs`, `sources/aisstream/wire.rs` |
| Plugin | `rustak-plugin-ais/src/lib.rs` (settings, two staleness horizons, the server-side `area` override) |
| Config + docs | `rustak-plugin-ais/{config.example.toml,README.md,Cargo.toml}`, `docs/plugins.md` (one AIS paragraph), `README.md` (one sentence) |
| Workspace | `Cargo.toml` — two additive lines at the end of `[workspace.dependencies]` (`nmea-parser`, `tokio-tungstenite`) |
| End-to-end test | `rustak-server/tests/feed_sidecars.rs` — one new case, appended (see *Boundaries*) |

`Cargo.lock` is updated as a build artefact.

## The sources

### `udp` — NMEA 0183 `!AIVDM` from a receiver of your own

`nmea-parser` 0.11 (Apache-2.0), one `NmeaParser` for the life of the socket so that the
two-sentence type 5 report reassembles. The port is bound in `open`, so a port already taken
is a start-up failure naming the address — the one thing this source refuses to start over.
A read that fails rebinds behind the capped backoff. The socket task decodes and buffers;
`poll` drains. **No key, no terms, no internet**: the source a deployment without one should
use, and the one the end-to-end test drives.

One decoder detail worth recording: `nmea-parser` splits the combined ship-and-cargo byte
into a `ShipType` and a `CargoType`, and the mapping wants the code AIS actually sent.
`ship_type_code` puts it back together — exact for 30–39 and 50–59, tens-plus-units elsewhere
— and is asserted round-trip over every representative code.

### `aisstream` — the AISStream.io WebSocket feed

`tokio-tungstenite` 0.30 with `rustls-tls-native-roots`. The subscription is built from the
configured area (latitude first; an area crossing the anti-meridian becomes two boxes) and
held in a `Secret`, so neither a `Debug` of the settings nor a log line nor a heartbeat can
carry the key. Reconnection is a capped exponential backoff (1 s → 60 s), a 15-second connect
timeout and a 15-minute idle timeout.

Three things this source does **not** do, each deliberate:

- **`permessage-deflate` is not negotiated.** `tungstenite` does not implement the extension
  and no pure-Rust WebSocket client in the tree does. The README says so and suggests a
  smaller area or a local receiver for a metered link. This is the brief's "enable
  `permessage-deflate`" left undone, for want of a crate.
- **The `rustls` crypto provider is installed by the plugin.** `tokio-tungstenite` builds its
  own `ClientConfig` when given no connector, and `ClientConfig::builder()` **panics** in a
  graph where more than one `rustls` backend feature is enabled — which rustak's is (`ring`
  and `aws-lc-rs`, transitively, the same trap `rustak-client/src/stream/tls.rs` documents).
  `AisStream::open` therefore calls `aws_lc_rs::default_provider().install_default()`, and a
  unit test asserts that doing so is what makes `ClientConfig::builder()` succeed — because
  the panic would otherwise only ever appear against the live service. This is why `rustls`
  is a direct dependency of the crate; it needed no new workspace line.
- **No test reaches the network.** The one test that calls `AisStream::open` cancels the
  shutdown first, so the socket task exits at the top of its loop.

### `digitraffic` — not implemented, by the brief's escape hatch

The brief said "implement it only if an MQTT-over-WebSocket client is cheap … otherwise leave
a documented stub and say so". It is not cheap. `rumqttc` 0.25's `websocket` feature pulls
`async-tungstenite` **and** `ws_stream_tungstenite` — a second WebSocket stack beside
`tokio-tungstenite` — and its `use-rustls` default pulls `tokio-rustls/default` and its own
`rustls-native-certs`, i.e. a second TLS root store, for one regional feed whose water
`aisstream` already covers and whose operators are better served by `udp`.

There is **no `Digitraffic` variant**: a config naming one gets `unknown variant
"digitraffic", expected one of "replay", "aisstream", "udp"`, which is a better error than a
variant that opens and immediately fails. It is documented instead, with the endpoint, the
topics and the CC BY 4.0 licence, in `rustak-plugin-ais/README.md` → "Not implemented:
Fintraffic Digitraffic".

## Mapping decisions

- **Ship type → class** is exactly the brief's table (30 Fishing; 35 Military; 36/37 Leisure;
  55 LawEnforcement; 60–89 Merchant; everything else Other), with a unit test per arm and per
  range boundary.
- **Sentinels** (heading 511, course 360, speed 102.3 kn, position 91/181) become `None` or a
  dropped observation. A heading with no course still reaches `<track course>` through the
  model's own fallback.
- **Static data arriving after a position** re-offers the last position under the vessel's
  real name. `FeedPublisher` then usually *suppresses* it — the hull has not moved, so
  nothing the policy cares about changed — and the name reaches the map on the next
  publication. That is the publisher's decision to make and is left with it; the end-to-end
  test asserts the observable consequence (the rename lands with the next movement).
- **Two staleness horizons.** `Track::to_event` takes one `stale` from the policy, but a
  vessel at anchor (a report every three minutes) and one under way (every few seconds) want
  different ones. `AisSidecar::drain` rewrites `event.stale` for vessels that are *not*
  anchored, moored or aground, using `[settings] under_way_stale`. The `Track` model is not
  extended: `is_stationary` reads the navigational status back out of the `Status` remark
  that the mapping writes, and a unit test asserts that round trip over all sixteen codes. A
  navigational status is AIS's vocabulary and does not belong on a shared model that also
  describes aircraft — **so nothing under `rustak-client/` was changed by this brief.**
- **`under_way_stale` is raised to `max_interval + min_interval`** when the configured value
  is shorter, and the raise is logged at `info` once at start-up. The brief's own defaults
  collide here — `stale` "2m" for a vessel under way against `max_interval` "3m" — and a
  track whose staleness is shorter than the gap between two refreshes drops off the map and
  comes back. With the shipped example this makes an under-way vessel's wire staleness 3 m
  05 s, which the example file and the README both state.

## Observability (deliverable 5), and what M9-04 replaces

`FeedStatus::heartbeat` builds the `Heartbeat`: `unhealthy` when the source has never
connected, `degraded` when it has been reconnecting for longer than two `[sidecar] tick`
intervals, `healthy` otherwise, with a sentence and a flat metrics object in the shape
`docs/plugins.md` → "Monitoring a sidecar" documents:

```json
{"offered": 18422, "published": 4106, "suppressed": 14291, "expired": 25, "tracked": 612,
 "source": {"kind": "aisstream.io", "state": "connected", "since": "2026-09-20T12:04:11Z"}}
```

A `reconnecting` source adds `last_error`. Nothing in it is ever built from the API key.

**The harness overwrites it, and M9-04 is the fix.** `rustak-client/src/sidecar/run.rs` posts
`Heartbeat::healthy()` unconditionally after every `Sidecar::tick`, and the server stores the
last heartbeat wholesale — so a richer one posted *during* the tick is overwritten a moment
later. This brief may not edit `rustak-client/src/sidecar/**`, so `status::report` runs on a
task of its own and waits 500 ms after each tick before posting, which puts this plugin's
status last. It works, and it is a workaround.

**For M9-04:** the builder is `FeedStatus::heartbeat(poll, now)` in
`rustak-plugin-ais/src/status.rs` (there is no `health.rs` in this crate). The hook's body is
`Some(self.snapshot().heartbeat(poll, Utc::now()))` from `rustak-plugin-ais/src/lib.rs`.
Adopting the hook means deleting `status::report`, `REPORT_AFTER`, the `watch::Sender<FeedStatus>`
field on `AisSidecar` and the `tokio::spawn` in `start`, and the "Why the report is a task"
section of that module's documentation.

### The server-side `area` override

Honoured, as the brief's cheaper branch: `start` reads `control.config()` once and takes an
`area` key over the file's, logging the substitution at `info`. An unreachable control API, a
missing key and an unparseable value are each a warning and the file's own area, never a
sidecar that will not start. Documented in `config.example.toml` and the README.

## Exit checks

```
$ cargo fmt --check
(no output; exit 0)

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.13s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 8 other files

$ ./scripts/check-file-length.sh
(no output; exit 0)   # the new files are untracked, so `git ls-files` does not see them;
                      # checked by hand with the same awk — the largest is lib.rs at 219

$ cargo test -p rustak-plugin-ais
test result: ok. 63 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s

$ cargo test -p rustak-server --test feed_sidecars
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.26s

$ cargo build --release -p rustak-plugin-ais
    Finished `release` profile [optimized] target(s) in 18.88s

$ cargo run -p rustak-plugin-ais -- --config rustak-plugin-ais/config.example.toml --check
INFO The configuration is valid; --check does not start the sidecar.
```

Run by hand as well, since "it runs, publishes and stops cleanly" is not something a unit test
asserts:

```
$ cd rustak-plugin-ais && ../target/debug/rustak-plugin-ais --config config.example.toml
INFO A vessel under way is refreshed less often than it would expire; its staleness has been
     raised to match. stale=185s
INFO The AIS sidecar is watching. source="replay" area=Bbox { … } affiliation=Unknown
INFO The feed is publishing. tracked=5 offered=5 published=5 suppressed=0 expired=0
^C
INFO The AIS sidecar is stopping. offered=10 published=5 suppressed=5 expired=0
(exit 0)

$ ./target/debug/rustak-plugin-ais --config <an aisstream config with a bogus key>
WARN The AIS stream is unavailable: wss://stream.aisstream.io/v0/stream did not answer
     within 15s source="aisstream.io" retry_in=1s
^C
INFO The AIS sidecar is stopping. offered=0 published=0 suppressed=0 expired=0
(exit 0)
```

## What is not verified here

- **No live AISStream.io connection was made.** This session's sandbox blocks outbound TCP to
  `stream.aisstream.io:443` (a plain `socket.create_connection` times out), and there is no
  API key. What *is* verified: the subscription JSON's shape and box handling (unit tests),
  that the process's crypto provider makes `ClientConfig::builder()` succeed (unit test), and
  that a hung connect is timed out and backed off (the manual run above). **The handshake,
  the server's acceptance of the subscription and the live message shapes are unverified** —
  every AISStream wire test runs against JSON written by hand from the documented shape.
  That hang is also how the missing connect timeout was found, so the sandbox earned its keep.
- **The UDP decoder is verified against our own encoded sentences**, never a capture. The
  encoder lives in `sources/udp.rs`'s test module and builds each payload field by field from
  the public ITU-R M.1371 layout; the four sentences the integration test sends are its
  output, pasted in with the assertions that keep them honest.
- **The Dockerfile was not built** (unchanged from M9-00; the `docker-build` job exercises it).
- **Nothing was run on GitHub Actions** from this session.

## Boundaries, and one thing the orchestrator should look at

- **`docs/plugins.md` was swept into another agent's commit.** The AIS paragraph this brief
  asked for was written into the working tree and then committed by M9-03 as part of
  `0a89549 feat(ui): Replace the Services stub with a live page…` — `git diff` now shows
  `docs/plugins.md` as unmodified because of it. Nothing was lost and the paragraph is
  correct where it is, but it belongs in this brief's commit; **moving that hunk needs a
  `but` write, which this brief forbids.** No `git`/`but` command was run from this session.
- **`rustak-server/tests/feed_sidecars.rs` is not on this brief's "files you own" list**, but
  deliverable 4 asks for an end-to-end case through the M9-00 harness and the exit checks name
  that suite. The case is **appended at the end of the file** to minimise the conflict with
  M9-02, which was told to add one too. Nothing existing in that file was changed except two
  `f64::from` calls clippy rejected in the new case.
- **Workspace `Cargo.toml`: two additive lines only**, at the end of `[workspace.dependencies]`,
  immediately before `[workspace.lints.rust]`. Nothing else in that file was touched.
- **`rustak-client/**` was not edited at all** — see the two-staleness note above for why the
  shared model did not need a new field.
- `README.md`: only the `rustak-plugin-ais` clause of the one sentence; the ADS-B half was
  left exactly as M9-02 will want it.
- No ATAK, TAK Server or OpenTAKServer source was read. `nmea-parser` is Apache-2.0 and was
  read for its public API only. Every fixture in this crate is our own.
