# rustak-plugin-firms

Fires on the map, from [NASA FIRMS](https://firms.modaps.eosdis.nasa.gov). A
sidecar that reads the active-fire detections of the VIIRS, MODIS and Landsat
instruments over an area of interest and publishes each as CoT. It follows the
sidecar contract in [`docs/plugins.md`](../docs/plugins.md); every setting is in
[`config.example.toml`](config.example.toml) with its default.

```sh
# runs offline, replaying hotspots.example.csv
cd rustak-plugin-firms && cargo run -p rustak-plugin-firms -- --config config.example.toml
```

## Sources

`[settings.source] kind` chooses one.

### `firms`: the area API

- **Key.** A free [MAP_KEY](https://firms.modaps.eosdis.nasa.gov/api/map_key/), written as `map_key = "${{ env.FIRMS_MAP_KEY }}"`. FIRMS puts it in the request URL; this plugin never logs that URL and redacts the key from anything FIRMS sends back.
- **Requests.** One per sensor per poll (two for a box across the anti-meridian): `viirs_noaa20`, `viirs_noaa21`, `viirs_snpp` (375 m, the default), `modis` (1 km), `landsat` (30 m, US and Canada only).
- **Cadence.** `poll = "10m"`, never faster than `"1m"`; a `429` is waited out. `days = 2`, because FIRMS' days are UTC calendar days and `1` is nearly empty after midnight.

### `replay`: a CSV file

- **Format.** The document FIRMS serves, saved or hand-written; blank lines and `#` comments allowed.
- **Times.** Shifted once at start-up so the newest row is "now". A replay lasts one `max_age`.
- **Use.** Demonstrations, and this crate's tests.

## What is drawn

`[settings.display] shape` chooses one.

### `marker` (default)

- **Type.** `b-m-p-s-m`, the spot-map dot; `marker_type` overrides it.
- **Colour.** `colour_by = "confidence"` (yellow, orange, red) or `"intensity"` (by fire radiative power).
- **Details.** Callsign `Fire 13:42Z 47MW`; remarks carry satellite, confidence, FRP, brightness, pixel size and attribution. `ce` is half the pixel.

### `footprint`

- **Shape.** A closed `u-d-f` polygon of the ground the pixel covered, uid `<marker uid>-FP`.
- **Approximation.** `scan` is laid east-west and `track` north-south; the true scan line is tilted about ten degrees. It is a sensor pixel, not a fire perimeter.
- **Fallback.** A detection with no pixel size is a marker.

### `both`

Two objects per detection.

### Not tiles

FIRMS also serves WMS/WMTS imagery. A TAK client loads that as a map source on the device, not from the CoT stream, and the URL embeds the MAP_KEY, so it is not something a sidecar can publish.

## Lifetime

### Identity

The uid is `FIRMS-<satellite>-<yyyymmddHHMM>-<lat>-<lon>`: the same detection read twice, or by a restarted sidecar, overwrites itself.

### Ageing

`time` is the overpass and `stale` is `max_age` (24 h) after it. No delete is ever sent; a stopped sidecar leaves a map that empties itself.

### Rate

Each detection is published once, then every `republish` (10 m) for devices that joined since. At most `max_per_tick` (500) leave per tick, newest first.

## Terms

### The data

Detections are **thermal anomalies, not confirmed fires**: low-confidence ones are often sun glint, flares or hot roofs, and cloud hides real fires. Global latency is about three hours after an overpass; minutes over the US and Canada. Do not use it as the only source for a life-safety decision.

### The service

A MAP_KEY allows 5000 transactions per ten minutes; a large area or several days costs more than one. Every request carries `User-Agent: rustak-plugin-firms/<version> (+https://github.com/SierraSoftworks/rustak)`.

### Attribution

NASA data is free to use, and FIRMS [asks to be acknowledged](https://www.earthdata.nasa.gov/data/tools/firms). Every event's remarks end `Source: NASA FIRMS`.
