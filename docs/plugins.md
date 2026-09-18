# Plugins (sidecars)

A rustak plugin is not loaded into the server. It is **its own process**, with
its own identity, that connects to the server the way an ATAK device does: a
client certificate on the CoT stream (`:8089`), the Marti API for missions and
files, and a small control API for registration and health. Anything a plugin
can do, a well-behaved client could do; anything it cannot do, no plugin can.

That is a deliberate trade. There is no plugin ABI to keep stable, no in-process
crash to take the server down with it, no privilege a plugin holds that is not
visible in the admin UI — and a plugin can be written in any language that can
open a TLS socket. `rustak-client` exists to make the Rust version of that a
hundred lines rather than a thousand.

> **Status (M1).** The harness and the CoT stream are real and run today: a
> plugin with a `[server] stream` connects, reconnects, receives and publishes.
> The Marti client lands in M2 and the `/api/v1/services/*` control API in M6;
> the shape they arrive through — `Sidecar::on_event` and `SidecarEvent` — is
> already here, so a plugin written now keeps compiling when they land.

## The identity model

| | What it is | Where it lives |
|---|---|---|
| **Service identity** | What the plugin *is*: its name, its control-API token, and the paths to the certificate, key and truststore it connects with | `rustak_core::service::ServiceIdentity` — never leaves the process |
| **Service descriptor** | What the plugin *says about itself*: name, display name, version, capabilities, and the endpoints it reached us on | `rustak_api::service::ServiceDescriptor` — published to the server, and from there to the admin UI |

A service is a user of kind `service`. It has a client certificate, it belongs
to channels, and its traffic is routed, filtered and audited by exactly the same
rules as a person's — which is the point of the whole model: an ADS-B feed that
floods a channel is as visible, and as easy to remove, as an EUD that does.

Its `clientUid` is derived from its name (`SERVICE-<name>`), so a service always
reappears as the same device rather than accumulating a row per restart. The
name is what an operator writes in `[service] name`; it is **not** the crate
name, because one plugin binary may be deployed several times against the same
server (`adsb-heathrow`, `adsb-gatwick`).

Nothing secret crosses into the descriptor. `ServiceDescriptor::from(&identity)`
names the fields that may be published rather than copying the identity and
removing the ones that may not, so a field added to the identity stays private
until somebody decides otherwise.

### Credentials

| Credential | What it authenticates | Configured as |
|---|---|---|
| Client certificate + key | The CoT stream and the Marti API — a service's *primary* identity | `[service] certificate`, `[service] key` |
| Service token | `/api/v1/services/*`, which a plugin may need before it has a certificate | `[service] token` |

Write the token as `"${{ env.RUSTAK_SERVICE_TOKEN }}"` and supply it from the
environment: the configuration file is the part of a deployment that gets
committed, copied and attached to support tickets. An expression whose variable
is not set is **refused at start-up, by name** — never sent as the literal
`${{ env.… }}` text, and never quietly turned into an empty credential.

Tokens are held in `rustak_core::identity::Secret`, which redacts itself in
`Debug` output and zeroises on drop, so logging a whole `SidecarContext` at
start-up stays a safe thing to do.

## Writing a plugin

### 1. Copy the crate

```bash
cp -r rustak-plugin-example rustak-plugin-adsb
```

Then, in the copy:

1. rename the package and the `[[bin]]` in `Cargo.toml` (the workspace picks up
   `rustak-*` by glob, so there is nothing to add to the root manifest);
2. update the `Dockerfile`'s `ADD`/`ENTRYPOINT` paths and its image description;
3. add the crate to the `build` matrix in `.github/workflows/rust.yml` (see
   `docs/ci.md`) so it is cross-compiled and published like the others;
4. write your plugin.

Every dependency comes from the workspace (`dep = { workspace = true }`), and
`rustak-client` re-exports what you need from `rustak-core`, so the manifest
rarely grows.

### 2. Implement `Sidecar`

```rust
use rustak_client::sidecar::{Sidecar, SidecarContext, SidecarEvent, async_trait, run};
use rustak_core::prelude::*;
use rustak_cot::Event;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    feed: Option<String>,
}

#[derive(Default)]
struct Adsb {
    context: Option<SidecarContext<Settings>>,
}

#[async_trait]
impl Sidecar for Adsb {
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    type Settings = Settings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        info!(uid = %ctx.identity().uid(), "Starting.");
        self.context = Some(ctx);
        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        // Poll the feed, and return the CoT the harness should publish.
        Ok(Vec::new())
    }

    async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
        if let SidecarEvent::Cot(event) = event {
            info!(uid = %event.uid, r#type = %event.r#type, "Something arrived.");
        }
        Ok(Vec::new())
    }
}

#[tokio::main]
async fn main() {
    run::<Adsb>().await;
}
```

| Method | Called | Returns |
|---|---|---|
| `start` | Once, after the configuration loads and the stream is opened | The only method given the `SidecarContext`; keep it if you need it |
| `tick` | On `[sidecar] tick`, starting immediately | The events to publish |
| `on_event` | For every `SidecarEvent` | The events to publish in reply |
| `stop` | Once, after the shutdown signal | Bounded by `[sidecar] shutdown_grace` |

**Returning an error from any of them stops the process** — the harness prints
it, records it if it is ours rather than the operator's, and exits 1. A failure
you expect to recover from (an upstream that is down, a heartbeat that did not
go through) is one to log and swallow in the plugin, not one to return. A
connection that drops is *not* one of these: it arrives as
`SidecarEvent::Disconnected` and the harness reopens it.

`SidecarEvent` is `#[non_exhaustive]`: match it with a `_` arm, and the variants
M2 and M6 add are an additive change rather than a broken build.

### The CoT stream

Set `[server] stream` and the harness opens it before `start`, reconnects with
backoff when it drops, answers the keepalive and the `t-x-takp-*` protocol
negotiation on your behalf, and hands you everything else:

| Event | When | What a plugin usually does |
|---|---|---|
| `Connected { endpoint }` | Every successful connect, including reconnects | **Return its SA event.** A reopened connection is a new subscription: until you send one, the server has a uid with no callsign, no group and no position |
| `Negotiated { protobuf }` | Once per connection, after `Connected` | Nothing. `false` is a server that does not offer TAK Protocol v1, refuses it, or never answers — all normal |
| `Cot(event)` | Every event that arrives | The plugin's actual work |
| `Disconnected { reason }` | Every drop | Count the outage, pause its own work |

Control traffic never reaches a plugin: pings, pongs and the negotiation
exchange are answered inside the client.

**Publishing is a return value, not a socket.** `tick` and `on_event` return the
`rustak_cot::Event`s to write, and the harness writes them in order. Nothing is
published while the connection is down — the harness logs what it dropped rather
than delivering a position report that is minutes stale — so a plugin that must
not lose an event holds it and returns it again from the next `Connected`.

A plugin with no `[server] stream` runs its ticks and opens no socket at all,
which is what makes `--check` and an offline unit test work without a server.

### 3. Describe it in `config.example.toml`

```toml
[service]
name = "adsb"                       # → uid SERVICE-adsb, and the callsign
capabilities = ["cot.publish"]
# token = "${{ env.RUSTAK_SERVICE_TOKEN }}"
certificate = "/etc/rustak/adsb.pem"    # required by an ssl:// stream
key = "/etc/rustak/adsb.key"
truststore = "/etc/rustak/truststore.pem"

[server]
stream = "ssl://tak.example.com:8089"

[sidecar]
tick = "30s"                        # the first tick happens at start-up
shutdown_grace = "10s"

[settings]                          # your own Sidecar::Settings
# feed = "https://example.com/adsb"
```

An `ssl://` endpoint needs all three of `certificate`, `key` and `truststore`,
and the harness says which one is missing **at start-up** rather than letting it
surface as a TLS alert on the first connection attempt.

Put `#[serde(deny_unknown_fields)]` on your settings type — that is what turns a
misspelled key into a start-up failure naming the key, rather than a setting
that silently does nothing — and **load the example file in a unit test**, as
`rustak-plugin-example` does. An example file that the test suite reads cannot
drift from the code it documents.

The harness never logs `[settings]`, because they are whatever your plugin says
they are and may hold an upstream credential. If yours does, hold it in a
`rustak_core::identity::Secret` (redacted in `Debug`, zeroised on drop) and read
it from `${{ env.… }}`, exactly as `[service] token` is.

## Running one

```text
rustak-plugin-adsb --config plugin.toml [--env .env] [--check]
```

- `--config` (or `RUSTAK_SIDECAR_CONFIG`) is the TOML file above.
- `--env` (or `RUSTAK_ENV_FILE`) is loaded **over** the process environment
  before the configuration is read, so `${{ env.X }}` can see it. Absence is not
  an error.
- `--check` loads and validates the configuration and exits, so a deployment
  pipeline can test a candidate file against the binary that will read it.
- `--help` and `--version` report the plugin's own name and version.

The start-up order is the one every rustak binary uses: environment file →
telemetry → shutdown signal → configuration → CoT stream → `start`. Telemetry
comes up before the configuration is read because the most common start-up
failure *is* the configuration file, and the stream is opened before `start`
because the next most common one is the certificate paths.

`SIGINT`/`SIGTERM` stops the sidecar: the tick loop ends, `stop` is given its
grace period, telemetry is flushed, and the process exits 0. A second signal
exits immediately with status 130, so an impatient operator never has to reach
for `kill -9`.

## Testing one

`rustak_client::sidecar::drive` is the loop `run` ends in, and it is public for
exactly this: build a context, drive your sidecar, cancel the shutdown to end
it.

```rust
let config = rustak_core::config::load_str(include_str!("../config.example.toml"))?;
let context = SidecarContext::from_config(config, Adsb::VERSION, Shutdown::new())?;
let mut sidecar = Adsb::default();

drive(&mut sidecar, context).await?;
```

A sidecar that cancels `ctx.shutdown()` from inside its own `tick` after *n*
ticks gives a test that is about the sequence rather than about a wall clock.
A configuration with no `[server] stream` drives the plugin without opening a
socket, which is what keeps that test offline; most of what a plugin decides is
in `on_event`, and calling it directly with a `SidecarEvent` you built is the
cheapest test of all.

For a test that wants the wire, `rustak_client::stream::testing::Eud` (behind
the crate's `testing` feature) is a client that stands in for a device — or for
a server, if you point the sidecar's `[server] stream` at a listener of your
own. That is how `rustak-client`'s own harness test proves connect → receive →
publish → drop → reconnect.

## The control API (M6)

Registration and health are not wired up yet. When they are, they will be these
routes, authenticated with the service token:

| Route | What it is for |
|---|---|
| `POST /api/v1/services` | Register a `ServiceDescriptor`; list what is registered |
| `POST /api/v1/services/<name>/heartbeat` | `ServiceState` plus whatever metrics the plugin wants recorded |
| `GET`/`PUT /api/v1/services/<name>/config` | Per-service configuration key/value store |
| `GET /api/v1/events` | Server-event SSE feed: client connect/disconnect, mission changes, group changes, package uploads |

The harness will call the first two for you — registration in `start`, a
heartbeat on each `tick` — and deliver the feed to `on_event` as further
`SidecarEvent` variants. The DTOs already exist in `rustak_api::service`, so a
plugin can be written against them today.

## See also

- `rustak-plugin-example/` — the template this document describes.
- `docs/ci.md` — how a plugin crate is built, published and released.
- `.claude/plan/plan.md` → Architecture → "Plugin (sidecar) contract".
