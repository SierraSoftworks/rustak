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

## Sources

One is chosen per deployment, in `[settings.source]`.

| `kind` | What it is | What it needs | Terms |
|---|---|---|---|
| `udp` | `!AIVDM` sentences from a receiver of your own | An antenna and a UDP port | None — it is your receiver |
| `aisstream` | The AISStream.io WebSocket feed | A free API key | The terms published on aisstream.io |
| `replay` | A file of tracks, offered on every tick | Nothing | None |

### `udp` — a receiver of your own

Point the UDP output of **AIS-catcher**, **`rtl_ais`**, a **dAISy** hat or a
commercial transponder at the address in `listen`, and this plugin decodes what
it hears: class A positions (types 1, 2, 3), class B positions (types 18, 19)
and the two-fragment static and voyage report (type 5) that carries the name.

```toml
[settings.source]
kind = "udp"
listen = "127.0.0.1:10110"
```

`127.0.0.1` takes datagrams from this host only, which is what a receiver in the
same container or on the same machine should send to. `0.0.0.0` takes them from
anywhere on the network, which means anyone on that network can put a vessel on
your map — prefer a specific interface address.

Decoding is [`nmea-parser`](https://crates.io/crates/nmea-parser) (Apache-2.0).
No key, no account, no internet: this is the source a deployment without one
should use.

### `aisstream` — worldwide AIS over a WebSocket

[AISStream.io](https://aisstream.io) aggregates receivers worldwide and streams
JSON over `wss://stream.aisstream.io/v0/stream`. Get a **free API key** from
[aisstream.io/account](https://aisstream.io/account) — it is shown once — and
keep it in the environment rather than in a file:

```toml
[settings.source]
kind = "aisstream"
api_key = "${{ env.AISSTREAM_API_KEY }}"
```

```sh
AISSTREAM_API_KEY=… rustak-plugin-ais --config plugin.toml
```

The subscription is built from `[settings.area]`, latitude first, and an area
crossing the anti-meridian is sent as two boxes. The connection is reopened with
a capped exponential backoff, and the key never reaches a log line, an error
message or a heartbeat.

Messages arrive in either text or binary WebSocket frames — the service sends
binary, whose payload is the same UTF-8 JSON — and both are read; anything the
service refuses the subscription for, and a connection that is open but
decoding nothing, are reported on the Services page (with `frames_undecoded`
and `messages_ignored` beside them) rather than left to a log.

Two limits are worth knowing: **three connections per account and per IP**, and
a server that drops a client which stops reading. `permessage-deflate` is *not*
negotiated — `tungstenite` does not implement the extension — so a large box
over a metered link costs more bandwidth than it needs to; prefer a receiver of
your own or a smaller area.

**Terms:** the site publishes no licence text for the data. What you agree to is
the terms on aisstream.io, and a free API key is what they are attached to.

### `replay` — a file, for demonstrations and tests

```toml
[settings.source]
kind = "replay"
path = "tracks.example.ndjson"
```

Newline-delimited JSON, one `Track` per line, offered on every tick. This is
what the integration suite and the demonstration below use, and what lets the
plugin be worked on with no upstream at all.

### Not implemented: Fintraffic Digitraffic

Finland's [Digitraffic](https://www.digitraffic.fi) publishes AIS for the Baltic
over MQTT-on-WebSocket (`wss://meri.digitraffic.fi:443/mqtt`, topics
`vessels-v2/<mmsi>/locations` and `.../metadata`), with no key and a **CC BY
4.0** licence. It is deliberately absent: the only current MQTT client with a
WebSocket transport, `rumqttc`, brings a second WebSocket stack and a second TLS
root store into the dependency graph for one regional feed. The same water is
covered by `aisstream`, and a Baltic operator with a receiver is better served
by `udp`. See `.claude/plan/status/M9-01-plugin-ais.md`.

## The CoT mapping

The uid is `AIS-<mmsi>`, `how` is `m-g`, and no `<contact endpoint>` is
published — a ship is a thing on the map, not a chat peer.

| AIS ship type | Class | CoT type (unknown affiliation) |
|---|---|---|
| 30 | Fishing | `a-u-S-X-F` |
| 35 | Military | `a-u-S-C` |
| 36, 37 | Leisure | `a-u-S-X-R` |
| 55 | Law enforcement | `a-u-S-X-L` |
| 60–89 (passenger, cargo, tanker) | Merchant | `a-u-S-X-M` |
| anything else, or unknown | Other | `a-u-S-X` |

The affiliation letter is `[settings] affiliation`, defaulting to `unknown`:
open AIS says nothing about whose side a hull is on.

| Field | From |
|---|---|
| `<contact callsign>` | The vessel's name, or `MMSI <n>` until the static report arrives |
| `<track speed>` | Speed over ground, knots → metres per second |
| `<track course>` | Course over ground, falling back to the true heading |
| `<point hae>` | Never set: AIS has no altitude |
| `<remarks>` | `MMSI`, `Call sign`, `IMO`, `Type` (code and word), `Status`, `Destination`, `ETA`, `Length/Beam`, `Source` — each line only when the vessel reported it |

AIS spells "not available" as a number: a heading of **511**, a course of
**360**, a speed of **102.3** knots and a position of **91/181**. Each becomes
nothing at all rather than a ship doing a hundred knots due north.

A position and a name arrive in different messages minutes apart, so a vessel is
published as soon as it is heard and renamed when its static report lands. The
per-MMSI cache that makes that work is bounded by `max_tracks`.

### Two staleness horizons

A vessel at anchor reports every three minutes; one under way, every few
seconds. One staleness cannot suit both, so:

* **anchored, moored or aground** → `[settings.publish] stale`, default `10m`;
* **everything else** → `[settings] under_way_stale`, default `2m`, raised to
  `max_interval + min_interval` when that is longer (a track must not expire
  between two refreshes) and logged at `info` when it is.

No delete message is ever sent: TAK clients expire a track by its `stale`
attribute, so a sidecar that is stopped leaves a map that empties itself.

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

### Beside rustak, in Docker

```yaml
services:
  ais:
    image: ghcr.io/sierrasoftworks/rustak-plugin-ais:latest
    restart: unless-stopped
    environment:
      AISSTREAM_API_KEY: ${AISSTREAM_API_KEY}
      RUSTAK_SERVICE_TOKEN: ${RUSTAK_SERVICE_TOKEN}
    volumes:
      - ./ais.toml:/etc/rustak/plugin.toml:ro
      - ./pki:/etc/rustak/pki:ro
    command: ["--config", "/etc/rustak/plugin.toml"]
```

For the `udp` source, listen on `0.0.0.0:10110` inside the container, publish
that port (`ports: ["10110:10110/udp"]`) and point the receiver at the host —
or run the receiver in the same compose project and use its service name.

## Watching it from the admin UI

With `[server] control` set, the sidecar registers itself and reports on every
tick, so the **Services** page answers "is the feed working?" without anybody
reading a log:

| | |
|---|---|
| `healthy` | The source is connected, or has been reconnecting for less than two ticks |
| `degraded` | The source has been reconnecting for longer than two ticks |
| `unhealthy` | The source has never connected at all |

The metrics beside it are the publisher's `offered` / `published` /
`suppressed` / `expired` counters, how many vessels are tracked, and a `source`
group naming the upstream and how it is doing:

```json
{
  "offered": 18422, "published": 4106, "suppressed": 14291,
  "expired": 25, "tracked": 612,
  "source": {"kind": "aisstream.io", "state": "connected", "since": "2026-09-20T12:04:11Z"}
}
```

A `reconnecting` source carries `last_error` as well. No credential ever
reaches any of it.

An administrator may also set an `area` in the service's configuration, which
wins over the file:

```json
{"area": {"kind": "circle", "lat": 51.95, "lon": 4.13, "radius_km": 60}}
```

## Data sources and licensing

| Source | Data licence | What you agree to |
|---|---|---|
| `udp` | None — your receiver, your data | Nothing |
| `aisstream` | Not published | The terms on [aisstream.io](https://aisstream.io), attached to the free API key |
| `replay` | Ours; the fixture is written by hand | Nothing |

Nothing is hard-coded: a deployment names the source it is entitled to use, and
no source is contacted that the configuration did not name.

rustak itself is MIT; see [`LICENSE`](../LICENSE). `nmea-parser` is Apache-2.0
and `tokio-tungstenite` is MIT.
