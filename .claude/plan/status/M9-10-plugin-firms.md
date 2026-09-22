# M9-10 — `rustak-plugin-firms`: active fires on the map from NASA FIRMS

**Status: complete against a mock; not yet run against FIRMS itself or a
device.** The sandbox this was built in could not reach
`firms.modaps.eosdis.nasa.gov`, so everything about the wire is from FIRMS'
published API documentation and proven against hand-written fixtures.

## What landed

| Area | Files |
|---|---|
| Wire | `rustak-plugin-firms/src/wire.rs` (CSV by header name; VIIRS, MODIS, Landsat) |
| Mapping | `src/mapping.rs` (uid, marker, footprint polygon, colour, remarks) |
| Publisher | `src/hotspots.rs` (filter, dedupe, republish, per-tick cap, expiry) |
| Sources | `src/sources/{mod,firms,replay,state}.rs` |
| Settings, heartbeat, plugin | `src/{settings,health,lib,main}.rs` |
| Tests | in-file units, `tests/end_to_end.rs` over `wiremock`, `tests/fixtures/viirs.csv` |
| Config + docs | `config.example.toml`, `hotspots.example.csv`, `README.md`, `Dockerfile`; `docs/{plugins,ci,deployment}.md`, `README.md`, three matrix lines in `rust.yml` |

No workspace dependency was added, and nothing in `rustak-client/**` changed.

## Decisions

- **Markers and/or polygons, not tiles.** `[settings.display] shape = "marker" |
  "footprint" | "both"`. FIRMS' WMS/WMTS imagery is a device-side map source
  whose URL embeds the MAP_KEY; a sidecar has no CoT to publish for it.
- **Not `Track`/`FeedPublisher`.** A detection never moves and is aged from its
  acquisition, not its last report; `TrackKind` has no branch for it. The crate
  takes `Area` from `rustak_client::feed` and carries its own publisher.
- **Deterministic uid** (`FIRMS-<sat>-<yyyymmddHHMM>-<lat>-<lon>`), so re-reads
  and restarts overwrite rather than stack.
- **Republish every 10 m, at most 500 detections per tick.** The hub replays one
  latest event per peer connection to a late joiner (`stream/hub.rs`
  `latest_sa_for`), so a feed of many objects has to repeat itself.
- **A simplified `SourceState`**, without the ADS-B/AIS `notice` reminders: a
  ten-minute poll cannot flood a log.
- **The MAP_KEY is in FIRMS' URL.** It is a `Secret`, validated as alphanumeric
  before it becomes a path segment, never logged, stripped from `reqwest` errors
  with `without_url`, and redacted from any body FIRMS echoes it in (tested).
- **`days` defaults to 2**: FIRMS' days are UTC calendar days, so 1 is nearly
  empty after midnight while `max_age` is 24 h.

## Not verified

- The live API: the `Invalid MAP_KEY.` refusal text and its status, the `days`
  ceiling of 5, and `429` behaviour are from documentation.
- On-device rendering of `b-m-p-s-m` + `<color argb>` and of the `u-d-f`
  footprint (`<link relation="c" point=…>`, `strokeColor`, `fillColor`) in
  ATAK, iTAK, WinTAK and CloudTAK.
- The footprint is axis-aligned; the true scan line is tilted about ten degrees.
