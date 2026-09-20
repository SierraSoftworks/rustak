# rustak-plugin-adsb

Aircraft on the map, from open ADS-B data.

ADS-B is what aircraft broadcast about themselves on 1090 MHz — a 24-bit ICAO
address, a flight number, a position, an altitude, a ground speed and a track —
about once a second. A receiver on a windowsill decodes it for nothing, and
public aggregators pool what thousands of those receivers hear. This sidecar
reads it, filters it to an area of interest, and publishes each aircraft as a
CoT track on a rustak channel, at a rate an operator's phone can carry.

It is an ordinary rustak plugin: its own process, its own certificate, its own
channel scope, and nothing it can do that a well-behaved TAK client could not.
See [`docs/plugins.md`](../docs/plugins.md) for the contract and the "Feed
sidecars" section there for the `rustak_client::feed` pieces this is built from.

## Status — M9-00 skeleton

What runs today is the plugin, its settings and its wiring, over a **replay**
source: a file of tracks, offered on every tick, which is what the integration
suite and the demonstration below use.

**The live sources arrive in M9-02** as further values of `[settings.source]`
`kind`:

| Source | What it is |
|---|---|
| `aircraft.json` | A local `readsb`/`dump1090` decoder's own output, as a file or over HTTP |
| Public aggregators | The same shape from `adsb.lol`, `adsb.fi`, `airplanes.live` |
| OpenSky | `states/all` over a bounding box, behind OAuth2 client credentials |

## Running it

```sh
cargo run -p rustak-plugin-adsb -- --config rustak-plugin-adsb/config.example.toml --check
cd rustak-plugin-adsb && cargo run -p rustak-plugin-adsb -- --config config.example.toml
```

The first validates the configuration and exits; the second runs the sidecar
against the five aircraft in `tracks.example.ndjson`. Neither needs a server:
a configuration with no `[server] stream` opens no socket at all.

Every setting is documented with its default in
[`config.example.toml`](config.example.toml), which this crate's test suite
loads so that it cannot drift from the code.

## Data sources and licensing

**To come with M9-02.** Each source this plugin grows will be listed here with
its terms, because "open" covers everything from public domain to
attribution-required to "free for non-commercial use", and an operator pointing
a feed at a channel needs to know which they have agreed to. Some aggregators
also ask for a feeder relationship or a contact address in the user agent;
nothing is hard-coded, and a deployment names the source it is entitled to use.

rustak itself is MIT; see [`LICENSE`](../LICENSE).
