# rustak-plugin-esb

Irish power outages on the map. A sidecar that reads ESB Networks'
[PowerCheck](https://powercheck.esbnetworks.ie) and publishes every outage as a
coloured CoT spot marker, for tracking supply disruption after storms.

```sh
rustak-plugin-esb --config plugin.toml [--env .env] [--check] [--enroll]
```

`config.example.toml` documents every setting with its default and runs as it
stands, replaying `outages.example.ndjson`. [`docs/plugins.md`](../docs/plugins.md)
covers identity, enrolment and the sidecar contract.

## The source

PowerCheck's API is **unofficial and undocumented**. It may change or go away,
and it is busiest exactly when it matters most, so the plugin is a careful guest.

- **Key.** Requests need the `API-Subscription-Key` header the PowerCheck site
  itself sends to `api.esb.ie`; your browser's network inspector shows it. Set
  it as `api_key`, from the environment. ESB can rotate it: three refusals in a
  row stop the source, and the Services page says why.
- **Cadence.** The list of outages is asked for every `poll` (default `5m`,
  floor `1m`). Details cost one request per outage, so they are fetched
  `details_per_tick` at a time (default 10 per `10s` tick), refreshed every 30
  minutes, and never for an outage outside `[settings.area]` or one that is
  restored and final. A `429` to either request holds both back for as long
  as it asks, up to an hour, and a detail that fails holds the rest back for a
  minute.
- **Outages of the API.** A failed poll keeps the last known outages on the map
  for up to two hours rather than clearing it.

The data is ESB Networks'. Attribute it, do not resell it, and treat a pin as
where a fault *is*, not as who is off supply.

## The CoT mapping

Each outage is a spot-map marker (`b-m-p-s-m`, `how="m-r"`) with uid
`ESB-<outageId>`, so it renders in ATAK, WinTAK, iTAK and CloudTAK without an
icon set.

| `include` kind | ESB `outageType` | Colour | `<color argb>` |
|---|---|---|---|
| `fault` | `Fault` | red | `-65536` |
| `planned` | `Planned` | orange | `-35072` |
| `restored` | `Restored` | green | `-16711936` |
| `other` | anything else | white | `-1` |

- **Label.** `<contact callsign>` is `Fault: Carrigaline (412)`: kind, ESB's
  place name, customers affected.
- **Remarks.** Type, location, customers, start, estimated restore, restore
  time, status message, planned reason and depot, as far as ESB has said.
  Times are UTC; ESB's own are Irish wall-clock and are converted.
- **Lifetime.** A marker is republished when anything about it changes and
  every `refresh`; it lives for `stale` (default `15m`). No delete is sent: an
  outage ESB stops listing expires by itself, as does everything when the
  sidecar stops.

## Watching it from the admin UI

The heartbeat reports `healthy`, `degraded` (failing for two poll intervals) or
`unhealthy` (never answered: check the key), with a one-line message.

| `metrics` key | What it is |
|---|---|
| `source` | `kind`, `connection`, `since`, `last_success`, `last_error`, `poll_s` |
| `outages` | Markers on the map by kind, and `customers` off supply across faults |
| `feed` | `offered`, `published`, `suppressed`, `expired` |

An `area` in the service's configuration document (admin UI) overrides
`[settings.area]` at start-up.

## Docker

```sh
docker run -d --name rustak-plugin-esb \
  -v /srv/rustak-esb:/data \
  -e ESB_API_KEY=... \
  ghcr.io/sierrasoftworks/rustak-plugin-esb:latest
```

`/data/plugin.toml` is the configuration; the certificate, key and truststore
are written beside it on first enrolment. See
[`docs/deployment.md`](../docs/deployment.md) → "The sidecar images".

## Licensing

MIT, like the rest of rustak. The wire shapes were written from the API's
observable behaviour; fixtures are hand-written, never captured.
