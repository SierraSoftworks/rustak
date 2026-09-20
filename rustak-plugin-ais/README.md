# rustak-plugin-ais

Ships on the map, from open AIS data.

AIS is what vessels broadcast about themselves over VHF — an MMSI, a name, a
position, a speed and a course — every few seconds under way and every few
minutes at anchor. This sidecar reads it, filters it to an area of interest, and
publishes each vessel as a CoT track on a rustak channel, at a rate an
operator's phone can carry.

It is an ordinary rustak plugin: its own process, its own certificate, its own
channel scope, and nothing it can do that a well-behaved TAK client could not.
See [`docs/plugins.md`](../docs/plugins.md) for the contract and the "Feed
sidecars" section there for the `rustak_client::feed` pieces this is built from.

## Status — M9-00 skeleton

What runs today is the plugin, its settings and its wiring, over a **replay**
source: a file of tracks, offered on every tick, which is what the integration
suite and the demonstration below use.

**The live sources arrive in M9-01** as further values of `[settings.source]`
`kind`:

| Source | What it is |
|---|---|
| AISStream.io | A WebSocket JSON stream, subscribed with a bounding box |
| NMEA 0183 over UDP | `!AIVDM` sentences from a receiver on your own roof |
| JSON over HTTP | The same observations, polled |

## Running it

```sh
cargo run -p rustak-plugin-ais -- --config rustak-plugin-ais/config.example.toml --check
cd rustak-plugin-ais && cargo run -p rustak-plugin-ais -- --config config.example.toml
```

The first validates the configuration and exits; the second runs the sidecar
against the five vessels in `tracks.example.ndjson`. Neither needs a server:
a configuration with no `[server] stream` opens no socket at all.

Every setting is documented with its default in
[`config.example.toml`](config.example.toml), which this crate's test suite
loads so that it cannot drift from the code.

## Data sources and licensing

**To come with M9-01.** Each source this plugin grows will be listed here with
its terms, because "open" covers everything from public domain to
attribution-required to "free for non-commercial use", and an operator pointing
a feed at a channel needs to know which they have agreed to. Nothing is
hard-coded: a deployment names the source it is entitled to use.

rustak itself is MIT; see [`LICENSE`](../LICENSE).
