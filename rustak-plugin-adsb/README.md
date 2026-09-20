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

## Running it

```sh
cargo run -p rustak-plugin-adsb -- --config rustak-plugin-adsb/config.example.toml --check
cd rustak-plugin-adsb && cargo run -p rustak-plugin-adsb -- --config config.example.toml
```

The first validates the configuration and exits; the second runs the sidecar
against the five aircraft in `tracks.example.ndjson`. Neither needs a server or
a receiver: a configuration with no `[server] stream` opens no socket at all.

Every setting is documented with its default in
[`config.example.toml`](config.example.toml), which this crate's test suite
loads so that it cannot drift from the code.

## Data sources

Four, chosen with `[settings.source]` `kind`. **Read the terms below before
pointing a deployment at one**: "open" covers everything from your own hardware
to attribution-required to research-and-non-commercial, and an operator putting
a feed on a channel needs to know which they have agreed to.

Every request this plugin makes carries

```
User-Agent: rustak-plugin-adsb/<version> (+https://github.com/SierraSoftworks/rustak)
```

so that an upstream can tell who is calling, and every live source honours a
`429` for as long as the response asks. Nothing is hard-coded to a provider: a
deployment names the source it is entitled to use.

### 1. `readsb` — your own receiver

```toml
[settings.source]
kind = "readsb"
url_or_path = "/run/readsb/aircraft.json"   # or "http://receiver.lan/data/aircraft.json"
poll = "1s"
```

A `readsb` or `dump1090` decoder's own output, as a file on this machine or over
HTTP — `tar1090` and the `dump1090` web interfaces serve the same document at
`<base>/data/aircraft.json`.

**How to get it:** an RTL-SDR dongle, an antenna and
[readsb](https://github.com/wiedehopf/readsb) or
[tar1090](https://github.com/wiedehopf/tar1090), which most feeder images ship
with. **Terms:** none. It is your hardware hearing aircraft directly; nobody
else is involved and there is nothing to attribute. This is the source to
prefer.

### 2. `aggregator` — a public pool of receivers

```toml
[settings.source]
kind = "aggregator"
provider = "adsb_lol"    # adsb_lol | adsb_fi | airplanes_live
poll = "5s"
```

The `[settings.area]` circle becomes the circle the endpoint is asked for (a box
asks for the circle that encloses it, and the publisher filters the corners back
out). The radius is capped at the 250 nautical miles these endpoints accept, and
polling never goes faster than one request a second whatever `poll` says. Three
consecutive `403`s stop the source, with a log line saying so, rather than
hammering a service that has said no.

| `provider` | Endpoint | Terms and attribution |
|---|---|---|
| `adsb_lol` | `api.adsb.lol/v2/point/{lat}/{lon}/{nm}` | Open data, no account. Rate limits are described as dynamic with API keys planned — see <https://adsb.lol>. |
| `adsb_fi` | `opendata.adsb.fi/api/v2/lat/{lat}/lon/{lon}/dist/{nm}` | **Non-commercial use**, one request a second, and asks to be **cited and linked**: credit "adsb.fi" with a link to <https://adsb.fi> wherever the data is shown. |
| `airplanes_live` | `api.airplanes.live/v2/point/{lat}/{lon}/{nm}` | One request a second; see <https://airplanes.live/api-guide>. **Experimental** — our probe of the public endpoint was refused (`403`), so this path is implemented against the documented shape and is unverified. |

**How to get access:** none of the three needs an account today. All three are
free services run by volunteers and paid for by people who feed data into them;
if you have a receiver, feeding it back is the way to pay for what you take.

### 3. `opensky` — the OpenSky Network

```toml
[settings.source]
kind = "opensky"
client_id = "${{ env.OPENSKY_CLIENT_ID }}"
client_secret = "${{ env.OPENSKY_CLIENT_SECRET }}"
poll = "10s"
```

`GET /api/states/all` over the `[settings.area]` bounding box, with
`extended=1` so that the emitter category comes back. A box that crosses the
anti-meridian cannot be expressed in OpenSky's four parameters, so the plugin
asks for every longitude and filters locally, and says so at start-up.

**How to get access:** anonymous works. For the faster tier, create an API
client under your account at <https://opensky-network.org>; the plugin exchanges
the client id and secret for a bearer token at OpenSky's OAuth2 endpoint,
renews it before it expires and again on a `401`, and never writes either to a
log. Write them as `"${{ env.NAME }}"` so they stay out of the file.

**Terms:** research and non-commercial use. Cite the OpenSky Network as the
source. See <https://opensky-network.org/about/terms-of-use>.

#### Budgeting credits

OpenSky charges a daily budget rather than a rate:

| | Anonymous | Authenticated |
|---|---|---|
| Daily credits | 400 | 4000 |
| Resolution | 10 s | 5 s |

One request costs **1 credit** for a box of up to 25 square degrees, 2 up to
100, 3 up to 400 and 4 above that. So:

- a 5°×5° box (25 square degrees) polled every 10 s is 8640 credits a day —
  twenty times the anonymous budget;
- the same box polled every 5 minutes is 288 credits, which fits;
- authenticated at 5 s, a 1-credit box is 17 280 credits a day, which does not.

The sidecar logs what your configuration costs per request and per day at
start-up, and warns when the box costs more than one credit or when the daily
total exceeds the budget. **A smaller area, a longer `poll`, or a `readsb`
receiver are the three ways to fix it** — and the third is free.

### 4. `replay` — a file

```toml
[settings.source]
kind = "replay"
path = "tracks.example.ndjson"
```

One JSON track per line, offered on every tick. What the demonstration and the
test suites run on, and what proves the rest of the plugin works without an
upstream to be down. The format is in
[`docs/plugins.md`](../docs/plugins.md) → "Feed sidecars".

## The CoT mapping

A track's uid is `ADSB-<hex>`, lower-cased, keeping the `~` that marks a
non-ICAO address. The CoT type comes from the emitter category the aircraft sets
for itself:

| `readsb` category | OpenSky category | CoT type (`affiliation = "unknown"`) |
|---|---|---|
| `A1`–`A6`, `B1`, `B4` | 2–7, 9, 12 | `a-u-A-C-F` — civil fixed wing |
| `A7` | 8 | `a-u-A-C-H` — civil rotary |
| `B2` | 10 | `a-u-A-C-L` — lighter than air |
| `B6` | 14 | `a-u-A-M-F-Q` — UAV |
| `C1`–`C3` | 16, 17 | `a-u-G-E-V-C` — ground vehicle |
| anything else | anything else | `a-u-A` — aircraft, unknown |

An `A*` category on an airframe the upstream's database marks military
(`dbFlags` bit 1) becomes `a-u-A-M-F` instead. OpenSky publishes no military
flag, so nothing from it is ever flipped. `affiliation` changes the `u` to
`f`/`n`/`h`/`p`; it defaults to `unknown`, because open ADS-B says nothing about
whose side an airframe is on.

The rest of the event:

| CoT | From |
|---|---|
| `callsign` | The trimmed `flight`, else the registration, else the hex address |
| `hae` | `alt_geom` (feet → m), else `alt_baro` when it is a number; OpenSky's are metres already |
| `<track speed>` | `gs` in knots → m/s; OpenSky's `velocity` is m/s already |
| `<track course>` | `track` / OpenSky's `true_track` |
| `stale` | `[settings.publish] stale`, 90 seconds by default |
| `<remarks>` | `ICAO`, `Registration`, `Type`, `Squawk`, `Altitude`, `Vertical rate`, `Category`, `Emergency` (only when it is not `none`), `Source` (ADS-B / MLAT / TIS-B / Mode S), `Seen` |

Two things are dropped rather than drawn: an aircraft with no position (a Mode S
return with an altitude and nothing else is most of a busy receiver's list) and
one whose position is more than 60 seconds old — an airliner covers fifteen
kilometres in a minute, so a stale position is an aeroplane drawn where it
demonstrably is not. `alt_baro: "ground"` is not an altitude: it becomes an
on-ground track with no `hae`.

## Watching it from the admin UI

The sidecar reports its own heartbeat on the Services page: the source's kind
and name, whether it is connected, reconnecting or has never connected, what
went wrong last, how many aircraft are tracked, and the feed's counters
(offered, published, suppressed, expired). A source that has been reconnecting
for more than two poll intervals reports `degraded`; one that has never
connected reports `unhealthy`, because that is a setting to look at rather than
an outage to wait out. Nothing secret goes into it.

An administrator may also set an `area` for the service through
`PUT /api/v1/services/adsb/config`; the sidecar reads it at start-up and it wins
over the one in the file, logged at `info` when it does.

## Docker

The image is published beside rustak's own:

```sh
docker run --rm \
  -v /etc/rustak:/etc/rustak:ro \
  -v $PWD/adsb.toml:/etc/rustak/adsb.toml:ro \
  -e RUSTAK_SERVICE_TOKEN \
  ghcr.io/sierrasoftworks/rustak-plugin-adsb:latest \
  --config /etc/rustak/adsb.toml
```

A sidecar dials out and listens on nothing, so there is no port to publish. For
a local `readsb` source, bind-mount the receiver's file (`-v
/run/readsb:/run/readsb:ro`) or point `url_or_path` at the receiver's HTTP
interface. See [`docs/deployment.md`](../docs/deployment.md) for running it
beside the server with Compose.

## Licensing

rustak itself is MIT; see [`LICENSE`](../LICENSE). The data is not ours: each
source above carries its own terms, and an operator agrees to the terms of the
source they configure. Nothing in this crate is derived from a GPL project.
