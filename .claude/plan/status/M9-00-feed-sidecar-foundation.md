# M9-00 — Foundation for information-feed sidecars

**Status: complete.** Every deliverable in the brief is implemented and every exit check is
green. `rustak_client::feed` is the shared half; `rustak-plugin-ais` and `rustak-plugin-adsb`
are runnable skeletons over a replay source, in all three CI matrices and in the Docker
publish path; `rustak-server/tests/feed_sidecars.rs` drives both plugins as libraries against
a real server with a fake EUD on the other side of the channel.

M9-01 and M9-02 add **source variants only**. Nothing in the model, the CoT mapping, the area
filter, the publishing policy or the plugin scaffolding needs to change to add a live feed:
write a `Feed` implementation, add a variant to that crate's `Source` enum, and point the
existing test harness at it.

## What landed

| Area | Files |
|---|---|
| Feed module | `rustak-client/src/feed/{mod,track,kind,area,policy,publish,source,replay}.rs` (+ two lines in `rustak-client/src/lib.rs`, a `toml` dev-dependency in `rustak-client/Cargo.toml`) |
| AIS skeleton | `rustak-plugin-ais/{Cargo.toml,Dockerfile,README.md,config.example.toml,tracks.example.ndjson,src/lib.rs,src/main.rs}` |
| ADS-B skeleton | `rustak-plugin-adsb/{Cargo.toml,Dockerfile,README.md,config.example.toml,tracks.example.ndjson,src/lib.rs,src/main.rs}` |
| CI + Docker | the three `crate:` matrix blocks in `.github/workflows/rust.yml` (`build`, `docker-build`, `docker-publish`) |
| Test harness | `rustak-server/tests/feed_sidecars.rs`, `rustak-server/tests/feed_support/mod.rs` (+ two dev-dependencies in `rustak-server/Cargo.toml`) |
| Docs | `docs/plugins.md` ("Feed sidecars"), `docs/ci.md`, `docs/deployment.md`, `README.md` |

`Cargo.lock` is updated as a build artefact of the two new crates.

## The `feed` public API — what M9-01 and M9-02 code against

`use rustak_client::feed::{…};` — everything below is re-exported from `feed`'s root; the
sub-modules are private.

### `Track` — the only thing a source has to produce

```rust
pub struct Track {
    pub id: String,                          // already prefixed: "AIS-244660000", "ADSB-3c6444"
    pub kind: TrackKind,
    pub position: (f64, f64),                // (lat, lon), decimal degrees, WGS-84
    pub altitude_hae_m: Option<f64>,         // metres above the ellipsoid
    pub speed_mps: Option<f64>,              // metres per second (convert knots once, here)
    pub course_deg: Option<f64>,             // degrees true, direction of travel
    pub heading_deg: Option<f64>,            // degrees true, direction pointing
    pub callsign: Option<String>,
    pub remarks: Vec<(String, String)>,      // rendered as "key: value" lines
    pub observed_at: DateTime<Utc>,
    pub on_ground: bool,
}

impl Track {
    pub fn new(id: impl Into<String>, kind: TrackKind, position: (f64, f64),
               observed_at: DateTime<Utc>) -> Self;
    pub fn with_callsign(self, callsign: impl Into<String>) -> Self;
    pub fn with_altitude_hae_m(self, altitude: f64) -> Self;
    pub fn with_velocity(self, speed_mps: f64, course_deg: f64) -> Self;
    pub fn with_heading_deg(self, heading: f64) -> Self;
    pub fn with_on_ground(self, on_ground: bool) -> Self;
    pub fn with_remark(self, key: impl Into<String>, value: impl Into<String>) -> Self;
    pub fn bearing_deg(&self) -> Option<f64>;                 // course, else heading
    pub fn to_event(&self, affiliation: Affiliation, stale: Duration) -> rustak_cot::Event;
}
```

`Serialize + Deserialize + Default + Clone + Debug + PartialEq`, `deny_unknown_fields`.
Optional fields are `skip_serializing_if`, so a fixture line writes only what it knows.

### `TrackKind` and the CoT types

```rust
pub enum TrackKind { Vessel(VesselClass), Aircraft(AircraftClass), GroundVehicle }
pub enum VesselClass { Merchant, Fishing, Leisure, LawEnforcement, Military, Other }   // Other = Default
pub enum AircraftClass { CivilFixedWing, CivilRotary, LighterThanAir,
                         MilitaryFixedWing, Uav, Unknown }                             // Unknown = Default
pub enum Affiliation { Unknown, Friend, Neutral, Hostile, Pending }                    // Unknown = Default

impl TrackKind { pub fn cot_type(self, affiliation: Affiliation) -> String; }
impl Affiliation { pub const fn letter(self) -> char; }                                // u f n h p
```

| Kind | `cot_type(Unknown)` |
|---|---|
| `Vessel(Merchant)` | `a-u-S-X-M` |
| `Vessel(Fishing)` | `a-u-S-X-F` |
| `Vessel(Leisure)` | `a-u-S-X-R` |
| `Vessel(LawEnforcement)` | `a-u-S-X-L` |
| `Vessel(Military)` | `a-u-S-C` |
| `Vessel(Other)` | `a-u-S-X` |
| `Aircraft(CivilFixedWing)` | `a-u-A-C-F` |
| `Aircraft(CivilRotary)` | `a-u-A-C-H` |
| `Aircraft(LighterThanAir)` | `a-u-A-C-L` |
| `Aircraft(MilitaryFixedWing)` | `a-u-A-M-F` |
| `Aircraft(Uav)` | `a-u-A-M-F-Q` |
| `Aircraft(Unknown)` | `a-u-A` |
| `GroundVehicle` | `a-u-G-E-V-C` |

All three enums are `Serialize + Deserialize`, `snake_case`: `{"vessel":"merchant"}`,
`"ground_vehicle"`, `"unknown"`. **Mapping an AIS ship-type code or an ADS-B emitter category
to one of these is the source plugin's job** — those codes are properties of their own wire
format and do not belong in the shared model.

### `Area`

```rust
pub enum Area {
    Bbox { south: f64, west: f64, north: f64, east: f64 },   // anti-meridian aware
    Circle { lat: f64, lon: f64, radius_km: f64 },
}

impl Area {
    pub fn contains(&self, lat: f64, lon: f64) -> bool;
    pub fn bbox(&self) -> Area;          // a circle's enclosing box, for box-only upstreams
    pub fn centre(&self) -> (f64, f64);
    pub fn radius_nm(&self) -> f64;      // a box's reaches its furthest corner
}
impl Default for Area { /* the whole world */ }

pub fn distance_m(from: (f64, f64), to: (f64, f64)) -> f64;   // haversine, public
```

Serde: internally tagged, `#[serde(tag = "kind", rename_all = "snake_case",
deny_unknown_fields)]` — `kind = "bbox"` / `kind = "circle"` in a `[settings.area]` table.

### `PublishPolicy` and `FeedPublisher`

```rust
pub struct PublishPolicy {
    pub stale: chrono::Duration,          // serde: `with = "duration::humane"`, "2m"
    pub min_interval: chrono::Duration,   // "5s"
    pub max_interval: chrono::Duration,   // "60s"
    pub min_move_m: f64,                  // 25.0
    pub max_tracks: usize,                // 5000
}

impl PublishPolicy {
    pub fn with_stale(self, stale: Duration) -> Self;   // ADS-B uses 90s
    pub fn stale(&self) -> Duration;                    // std durations for the code paths
    pub fn min_interval(&self) -> Duration;
    pub fn max_interval(&self) -> Duration;
}

pub struct FeedPublisher { /* … */ }

impl FeedPublisher {
    pub fn new(policy: PublishPolicy, affiliation: Affiliation) -> Self;
    pub fn with_area(self, area: Area) -> Self;
    pub fn offer(&mut self, track: Track) -> bool;                                  // wall clock
    pub fn offer_at(&mut self, track: Track, now: DateTime<Utc>) -> bool;           // test clock
    pub fn tick(&mut self) -> usize;                                                // expired + evicted
    pub fn tick_at(&mut self, now: DateTime<Utc>) -> usize;
    pub fn drain(&mut self) -> Vec<rustak_cot::Event>;    // what `tick()` returns to the harness
    pub fn refresh_all(&mut self);                        // call on SidecarEvent::Connected
    pub fn counters(&self) -> FeedCounters;
    pub fn tracked(&self) -> usize;
    pub fn policy(&self) -> &PublishPolicy;
}

pub struct FeedCounters { pub offered: u64, pub published: u64,
                          pub suppressed: u64, pub expired: u64 }   // Copy + Serialize
```

`offer` publishes when the track is new, when it has moved at least `min_move_m`, when it has
turned 10° or changed speed by 2.5 m/s, or when `max_interval` has elapsed — never more often
than `min_interval`, never when the track is outside the area, and never for an observation
older than the newest one already held for that id. `offered == published + suppressed`
always. `tick_at` logs the counters at `info` every five minutes.

### `Feed` and `Replay`

```rust
#[async_trait]
pub trait Feed: Send {
    fn name(&self) -> &str;                                    // the upstream's name, for logs
    async fn poll(&mut self) -> Result<Vec<Track>, human_errors::Error>;
}

pub struct Replay { /* … */ }
impl Replay {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error>;   // newline-delimited JSON
    pub fn over(tracks: Vec<Track>) -> Self;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn path(&self) -> &Path;
}
```

`poll` is called once per sidecar tick and answers **what has arrived since the last call**.
It may wait, but not for longer than a tick is worth: a streaming upstream buffers in its own
task and drains here. **Reconnection and backoff belong to the source.** An error from `poll`
is a log line in the plugin, never a returned error — returning one from `tick` would stop the
process.

## Two decisions the brief left to the implementation

- **`FeedPublisher` buffers; it does not hold a stream handle.** The brief says it takes "the
  sidecar's stream handle (whatever `SidecarContext` exposes for sending CoT)" — and
  `SidecarContext` exposes none. Publishing in this SDK is a *return value*: `tick` and
  `on_event` hand events back and the harness writes them, so that nothing is queued into a
  connection that is down (`sidecar/mod.rs`, "Publishing is a return value, not a socket").
  The publisher therefore ends in `drain() -> Vec<Event>`, which is what the plugin returns.
  The "fake sink and controllable clock" the brief asks for are `drain()` and the `*_at`
  methods; the unit tests move time by hand and never wait.
- **`Replay` lives in `rustak_client::feed`, not in each plugin.** Both skeletons need it and
  M9-01/M9-02 will keep it as their offline source; duplicating a file reader in two crates
  would have been the only alternative. The `Source` enum itself stays per-plugin, because
  that is where the live variants go.

## The skeleton plugins

Both are the same shape: `Settings { area, publish, affiliation, source }` with
`deny_unknown_fields`, `Source` as `#[serde(tag = "kind")]` with a single `Replay { path }`
variant, a `[lib]` target so the integration suite drives the real plugin, and a `main.rs`
that is four lines of `run::<S>()`.

| | AIS | ADS-B |
|---|---|---|
| Crate | `rustak-plugin-ais` | `rustak-plugin-adsb` |
| Type | `AisSidecar` | `AdsbSidecar` |
| Service name in the example | `ais` | `adsb` |
| `stale` default | `"2m"` (the module default) | `"90s"` (`default_publish()`) |
| Example area | Maas approach bbox | 120 km circle on Heathrow |
| Fixture | `tracks.example.ndjson`, five vessels | `tracks.example.ndjson`, four aircraft and a tug |

One sharp edge is documented in both the ADS-B settings type and its example file: serde fills
a **partial** `[settings.publish]` table key by key from `PublishPolicy`'s own defaults, so a
table that omits `stale` gets two minutes rather than ninety seconds. The example file writes
`stale` out for that reason.

`M9-01`/`M9-02`: add your variant to `Source`, match it in `Source::open`, document it in
`config.example.toml` and the README's "Data sources and licensing" section (which is a
placeholder today, by the brief), and add a case to `feed_sidecars.rs`.

## The test harness

`rustak-server/tests/feed_support/mod.rs` is the reusable half, generic over the plugin:

```rust
let mut feed = RunningFeed::start::<AisSidecar>("ais", &settings).await;   // settings = a [settings] block
let event = feed.eud.expect_uid("AIS-244660000", EXPECT).await.unwrap();
feed.stop().await;
```

It starts the in-process `:8089` listener (`stream_support::Harness`), binds the API, mints an
enrolment token and a service token, enrols the sidecar over the real
`POST /Marti/api/tls/signClient/v2`, connects a fake EUD **before** the plugin starts (the
first tick is immediate, and a device that joined afterwards would miss it), and drives the
plugin with `rustak_client::sidecar::drive`. `replay_settings(&dir, &tracks)` writes a fixture
and answers the `[settings.source]` block naming it. A suite using it declares
`mod stream_support;` alongside `mod feed_support;`.

Four cases in `feed_sidecars.rs`: the five vessels arrive with the right uids, types,
callsigns, `<track>`, `<remarks>` and no `endpoint` (and the sidecar registered); the five
aircraft arrive with their altitudes and the 90-second staleness; one vessel reported ten
times in a second is published once and nothing follows for the settle window; a vessel
outside the configured box is never published.

## Exit checks

```
$ cargo fmt --check
(no output; exit 0)

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 22.51s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 8 other files

$ ./scripts/check-file-length.sh
(no output; exit 0)   # the new files are untracked, so `git ls-files` does not yet see them;
                      # checked by hand with the same awk — the largest is feed/publish.rs at 203

$ cargo test -p rustak-client
test result: ok. 164 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.03s
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
(45 of the 164 are the feed module's own)

$ cargo test -p rustak-plugin-ais -p rustak-plugin-adsb
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo test -p rustak-server --test feed_sidecars
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.45s

$ cargo build --release -p rustak-plugin-ais -p rustak-plugin-adsb
    Finished `release` profile [optimized] target(s) in 41.65s
```

Also run by hand, because "each compiles, `--check`s its example config, runs, registers,
publishes the replay file, and stops cleanly" is not something a unit test asserts:

```
$ cargo run -p rustak-plugin-ais -- --config rustak-plugin-ais/config.example.toml --check
INFO The configuration is valid; --check does not start the sidecar.

$ cd rustak-plugin-ais && cargo run -p rustak-plugin-ais -- --config config.example.toml
INFO The feed is publishing. tracked=5 offered=5 published=5 suppressed=0 expired=0
^C
INFO The AIS sidecar is stopping. offered=20 published=5 suppressed=15 expired=0
(exit 0)
```

The same two for `rustak-plugin-adsb` (`tracked=5 offered=5 published=5`, then
`offered=10 published=5 suppressed=5` on `^C`, exit 0); registration is covered by the
integration suite rather than by hand, since it needs a server.

## What is not verified here

- **The CI matrices are structural only.** The three blocks were edited and the file still
  parses (`ruby -ryaml`), with all three matrices listing the four crates; nothing was run on
  GitHub Actions from this session, so `ghcr.io/sierrasoftworks/rustak-plugin-{ais,adsb}` do
  not exist yet. The `build` matrix is now 20 jobs (4 crates × 5 targets); `docs/ci.md`'s
  counts were updated to match.
- **The Dockerfiles were not built.** They are the example's with the name and description
  changed and the `EXPOSE` line dropped (a sidecar dials out and listens on nothing); the
  `docker-build` job is what exercises them.
- **No live data source was contacted**, by design: M9-00 delivers no upstream.
- **Licensing sections in both READMEs are placeholders**, as the brief specifies — M9-01 and
  M9-02 fill them in with the terms of the sources they add.

## Conventions and boundaries

- No `git`/`but` command was run. The working tree also holds another session's changes to
  `.claude/plan/plan.md` and `.claude/plan/briefs/*`, which were not touched.
- Only the three `crate:` matrix blocks in `.github/workflows/rust.yml` were edited; nothing
  else under `.github/`.
- The two plugin crates are dev-dependencies of `rustak-server` by **path**, not through
  `[workspace.dependencies]`: the dependency direction is plugins → client, never the reverse,
  and a dev-only path dependency keeps the root manifest out of it (and this brief's file
  ownership intact).
- No ATAK, TAK Server or OpenTAKServer source was read. The CoT type arms are the brief's,
  from the public MIL-STD-2525 hierarchy; every fixture is our own.
