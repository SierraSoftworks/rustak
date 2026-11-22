# rustak-plugin-remoteid

Drones and their operators on the map from Remote ID (ASTM F3411,
ASD-STAN EN 4709-002). A sidecar that tails the capture file a Remote ID
receiver is writing, tracks every drone it hears, and publishes an aircraft
marker and an operator marker for each, with a trust verdict on the aircraft,
because Remote ID is unauthenticated and easy to fake.

```sh
rustak-plugin-remoteid --config plugin.toml [--env .env] [--check] [--enroll]
```

`config.example.toml` documents every setting with its default.
[`docs/plugins.md`](../docs/plugins.md) covers identity, enrolment and the
sidecar contract. Decoding, tracking, spoof detection and the CoT mapping come
from the [`uas-cot`](https://crates.io/crates/uas-cot) crate.

## The source

The plugin does not drive a radio itself. Something else captures and the
plugin reads what it writes to `capture`, which is required (the sidecar
refuses to start without it):

- **Wi-Fi.** A card in monitor mode, for example
  `tcpdump -i wlan0mon -w /data/rid.pcap`, picks up Wi-Fi Beacon and NAN
  Remote ID. Any pcap or pcapng file works.
- **Bluetooth.** A BLE sniffer (the nRF Sniffer in Wireshark, for example)
  writing pcap or pcapng picks up Bluetooth 4 legacy and Bluetooth 5 long
  range advertising.
- **RIDF.** A RIDF recording, the compact record format `uas-cot` reads and
  writes (`capture::to_ridf`).

The file is read while it is being written:

- **Incremental.** Each `tick` (default `2s`) reads only the bytes appended
  since the last one and decodes only complete records. A record the receiver
  has only half flushed waits for the next tick, so a buffered `tcpdump -w`
  file is fine.
- **Rotation.** A file that is truncated, or on Unix replaced by a new file
  (logrotate, `tcpdump -C` or `-G`), is read again from the top, and the
  tracker starts over with it.
- **Restarts.** Every frame goes to the tracker, which needs the history for
  its trust verdict, but only drones heard within `stale_secs` of now are
  published. Restarting against an old capture does not put drones from hours
  ago back on the map as live.

## The CoT mapping

| Marker | uid | CoT type |
|---|---|---|
| Aircraft | `rid-<id>` | `a-u-A-M-F-Q` (drone), `a-u-A-C-L` (balloon, airship) or `a-u-A` |
| Operator (or takeoff point) | `rid-op-<id>` | `b-m-p-s-m` |

- **Id.** `<id>` is the drone's first Basic ID (serial number, registration
  and so on), or its radio address until a Basic ID has been heard.
- **One drone, one marker.** Many drones broadcast on more than one radio
  (BLE 4 and BLE 5, or BLE and Wi-Fi) from different addresses. Those are
  grouped by Basic ID and drawn once, from the worst verdict among them and
  then the freshest report, so the marker does not flip between suspect and
  unknown. A spoofer copying a real drone's serial therefore makes that serial
  suspect, which is the honest answer when the ID is contested.
- **Remarks.** UA type, status, height, vertical speed, operator ID and self
  ID as far as the drone has sent them, then the trust verdict and its
  reasons, for example `Trust: spoofed (teleport, known_spoofer).`
- **Verdict.** `spoofed`, `suspicious`, `plausible` or `authenticated`. With
  `spoofed_as_suspect` (default true) a spoofed aircraft is drawn as a suspect
  (`a-s-...`) rather than an unknown.
- **Lifetime.** A marker is published each tick the drone is heard and lives
  for `stale_secs` (default 30). No delete is sent: a drone that goes quiet
  expires by itself.

## Docker

```sh
docker run -d --name rustak-plugin-remoteid \
  -v /srv/rustak-remoteid:/data \
  ghcr.io/sierrasoftworks/rustak-plugin-remoteid:latest
```

`/data/plugin.toml` is the configuration; the certificate, key and truststore
are written beside it on first enrolment. Point `capture` at a file under
`/data` and have the receiver write it there. See
[`docs/deployment.md`](../docs/deployment.md) → "The sidecar images".

## Licensing

MIT, like the rest of rustak. `uas-cot` is MIT or Apache-2.0.
