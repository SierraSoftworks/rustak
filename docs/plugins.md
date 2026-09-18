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

> **Status (M6).** All of it runs today: the CoT stream, the typed Marti client,
> the `/api/v1/services/*` control API and the server-event feed. A plugin with a
> `[server] stream` connects, reconnects, receives and publishes; one with a
> `[server] control` registers itself, reports a heartbeat on every tick, and
> receives `SidecarEvent::Server`; one with a `[server] marti` reaches missions,
> files, channels and contacts through `ctx.marti()`.

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
sed -i '' 's/rustak-plugin-example/rustak-plugin-adsb/g' \
  rustak-plugin-adsb/Cargo.toml rustak-plugin-adsb/Dockerfile
cargo run -p rustak-plugin-adsb -- --config rustak-plugin-adsb/config.example.toml --check
```

Three occurrences in `Cargo.toml` and `Dockerfile` carry the name — the package,
the `[[bin]]`, and the `ADD`/`ENTRYPOINT` paths — and `--check` is what proves
the copy is a crate the workspace builds and a binary that reads its own example
file. There is nothing to add to the root manifest: the workspace picks up
`rustak-*` by glob.

Then, in the copy:

1. reword the `description` in `Cargo.toml` and the image description in the
   `Dockerfile`, which the `sed` above leaves saying "example";
2. add the crate to the `build` matrix in `.github/workflows/rust.yml` (see
   `docs/ci.md`) so it is cross-compiled and published like the others;
3. write your plugin.

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

`SidecarEvent` is `#[non_exhaustive]`: match it with a `_` arm, and a variant a
later release adds is an additive change rather than a broken build. `Server` is
the one M6 added, and a plugin written against M1 kept compiling.

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

`SidecarEvent::Server` arrives on the same `on_event` and comes from the
server-event feed rather than the stream — see "Reacting to server events"
below.

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

## Registering with the server

Set `[server] control` and the harness does three things for you:

| When | What it does |
|---|---|
| Before `start` | `POST /api/v1/services/register` with the `ServiceDescriptor` built from `[service]` and `[server]` |
| After every `tick` | `POST /api/v1/services/<name>/heartbeat` with a healthy state |
| Continuously | Holds `GET /api/v1/events` open and delivers what arrives as `SidecarEvent::Server` |

None of them can stop a plugin. A control API that refuses a registration, loses
a heartbeat or drops the feed is logged and retried, because the CoT a plugin
publishes is the part that matters and a server that cannot take a heartbeat
right now has not stopped taking events. What the server does about the silence
is its own business: a registration that has not reported for ninety seconds is
moved back to *not reporting*, which the admin UI draws as needing attention.

Registration is an **upsert keyed on the service's name**, so a sidecar that
restarts registers again rather than failing, and its configuration and its last
known health survive. The one rule the server enforces is that a name belongs to
the account that claimed it: registering under a name another account holds is a
`409`, not a takeover.

If an administrator removes the registration while the plugin is running, the
next heartbeat is a `404` — and the harness registers again rather than exiting.

### Saying more than "healthy"

The harness's heartbeat is the floor. A plugin with something to report reaches
the client itself:

```rust
use rustak_api::{Heartbeat, ServiceState};

if let Some(control) = self.context.as_ref().and_then(|ctx| ctx.control()) {
    let _ = control
        .heartbeat(&Heartbeat {
            state: ServiceState::Degraded,
            message: Some("The upstream feed has not answered for 4 minutes.".into()),
            metrics: serde_json::json!({ "events_published": 1204, "queue_depth": 3 }),
        })
        .await;
}
```

`metrics` is whatever your plugin says it is; it is rendered in the admin UI
as-is, so nothing secret belongs in it. Swallow the failure, as above — a
heartbeat that did not go through is not a reason to stop.

### Per-service configuration

`GET/PUT /api/v1/services/<name>/config` is a JSON object an administrator sets
and the service reads. **The service may read only its own, and only an
administrator may write one** — a plugin that could rewrite its own
configuration would make the admin UI's copy a suggestion rather than a setting.

```rust
#[derive(Deserialize)]
struct Tuning {
    interval_seconds: u64,
}

let tuning: Tuning = control.config_as().await?;
```

Reading it on a tick is what lets a setting changed in the UI reach the sidecar
without anybody restarting it.

### Two credentials

A service token authenticates `/api/v1/services/*` and nothing else; the client
certificate authenticates everything (and the control API too, so a sidecar that
has enrolled needs no token at all). The token exists for the case where the
plugin has no certificate **yet** — which is also what `rustak_client::enroll`
is for:

```rust
use rustak_client::enroll::{Enrolment, enroll};

let enrolled = enroll(&Enrolment {
    marti: "https://tak.example.com:8443",
    username: "svc.adsb",
    secret: &Secret::new(std::env::var("RUSTAK_ENROLLMENT_TOKEN")?),
    client_uid: "SERVICE-adsb",
    truststore: None,
})
.await?;

let paths = enrolled.write_to("/etc/rustak", "adsb")?;
```

The private key is generated in the plugin's own process and never sent: what
crosses the wire is a signing request carrying the public half. An enrolment
token is one-time and is spent only once the certificate has been issued, so a
failed enrolment leaves it usable — and a sidecar enrols when it has no
certificate rather than on every start.

## Reacting to server events

`GET /api/v1/events` is a Server-Sent Events feed of what happened on the
server. The harness holds it open, reopens it when it drops, resumes from the
last event it saw, and delivers each one as `SidecarEvent::Server`:

| Event | When |
|---|---|
| `client.connected` / `client.disconnected` | A device joined or left the CoT stream |
| `mission.changed` | A mission was created, changed, shared or deleted |
| `channel.changed` | An account's channel membership or selection changed |
| `package.uploaded` | A file or mission package arrived in enterprise sync |
| `service.status` | A registered service reported its health, or stopped reporting it |

```rust
use rustak_client::control::ServerEventPayload;

async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error> {
    if let SidecarEvent::Server(event) = &event
        && let ServerEventPayload::ClientConnected(client) = &event.payload
    {
        info!(username = %client.username, "A client joined the stream.");
    }

    Ok(Vec::new())
}
```

An event **says that something changed, not what it now is**: read the new state
back through the API that owns it. That is what keeps an event small enough that
a slow consumer falls behind by kilobytes, and what keeps the feed free of
anything secret — no tokens, no certificate material, no file contents, no peer
addresses.

Each event carries an `id` that increases by one. A plugin that keeps state
watches for a **gap**: it means the feed was down for longer than the server's
ring of recent events, and whatever the plugin was tracking should be re-read.
The ids restart at 1 when the server does, which reads as an id lower than the
last one seen.

The feed is for administrators and services. An ordinary account is refused,
because it says which devices are on the stream and which packages arrived
across every channel.

## The Marti API

`[server] marti` gives `ctx.marti()`, a typed client over the same certificate:

```rust
let missions = marti.missions().list(None).await?;
let subscription = marti.missions().subscribe("OPS", ctx.identity().uid().as_str(), None).await?;

// Later calls about that mission present the token it handed back.
let changes = marti
    .missions()
    .with_token(subscription.token.unwrap_or_default())
    .changes("OPS", 60)
    .await?;

let uploaded = marti
    .files()
    .upload(&Upload::named("track.zip"), bytes)
    .await?;

let online = marti.contacts().connected().await?;
let channels = marti.groups().receiving().await?;
```

Every call answers a `human_errors::Error` carrying the server's own words, so a
`403` from a channel a service is not in reads as that rather than as a status
code. The fields a plugin acts on are typed; the parts TAK itself treats as
opaque stay `serde_json::Value`, and nothing uses `deny_unknown_fields` — the
wire shape is TAK's and grows.

## See also

- `rustak-plugin-example/` — the template this document describes.
- `docs/ci.md` — how a plugin crate is built, published and released.
- `.claude/plan/plan.md` → Architecture → "Plugin (sidecar) contract".
