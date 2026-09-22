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
| One-time enrolment token | Getting the client certificate above, once, on the first start | `RUSTAK_ENROLLMENT_TOKEN`, or `[service] enrollment_token` |
| Workload identity | **Both of the two above**, for a sidecar under Nomad or Kubernetes | `[service] workload_identity`, or nothing at all |

The last one is not a rustak credential: it is the JWT the orchestrator already
gave the task. A deployment that has one holds **no rustak secret at all** —
nothing to mint, nothing to hand over, nothing to rotate. See
[Three credentials](#three-credentials) below and "Workload identity" in
[`docs/deployment.md`](deployment.md).

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
| `health` | After every `tick` | The heartbeat to report, or `None` for "healthy" — see *Saying more than "healthy"* |
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
# enrollment_token = "${{ env.RUSTAK_ENROLLMENT_TOKEN }}"   # the first start only
certificate = "/etc/rustak/adsb.pem"    # required by an ssl:// stream
key = "/etc/rustak/adsb.key"            # written by enrolment if it is not there
truststore = "/etc/rustak/truststore.pem"
# control_truststore = "/etc/rustak/public-ca.pem"   # only to pin the public listener; see below
# pki_dir = "/data"                     # where enrolment writes what the three above do not name
# account = "svc.adsb"                  # the account it enrols as; default: the name above

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
rustak-plugin-adsb --config plugin.toml [--env .env] [--check] [--enroll]
```

- `--config` (or `RUSTAK_SIDECAR_CONFIG`) is the TOML file above.
- `--env` (or `RUSTAK_ENV_FILE`) is loaded **over** the process environment
  before the configuration is read, so `${{ env.X }}` can see it. Absence is not
  an error.
- `--check` loads and validates the configuration and exits, so a deployment
  pipeline can test a candidate file against the binary that will read it. It
  touches nothing: no network, no files, no enrolment. A file whose certificate
  and key are paths nothing has written yet still validates — it reports where
  the identity *will* be enrolled to instead of refusing to read it — as long as
  the file says it means to enrol, by `[service] pki_dir` or an enrolment token.
  An `${{ env.… }}` token whose variable is not set is refused by name, which is
  what makes this a real check of a pre-enrolment deployment.
- `--enroll` does what a first start would do about a missing certificate — it
  enrols, writes the three PEMs, says where they went — and exits 0 without
  starting the plugin. For an init container or a one-off task. A sidecar that
  already has a certificate exits 0 having done nothing, so it is safe to run
  before every start.
- `--help` and `--version` report the plugin's own name and version.

The start-up order is the one every rustak binary uses: environment file →
telemetry → shutdown signal → configuration → **enrolment** → CoT stream →
`start`. Telemetry comes up before the configuration is read because the most
common start-up failure *is* the configuration file, and the stream is opened
before `start` because the next most common one is the certificate paths.

`SIGINT`/`SIGTERM` stops the sidecar: the tick loop ends, `stop` is given its
grace period, telemetry is flushed, and the process exits 0. A second signal
exits immediately with status 130, so an impatient operator never has to reach
for `kill -9`.

### The first start

A sidecar whose `[service] certificate` and `key` are missing — unset, or naming
files that are not there — enrols for itself before it opens the stream, with
the first credential it can find:

| | | |
|---|---|---|
| 1 | its orchestrator's **workload identity** | a Nomad or Kubernetes JWT the task already holds; nothing to configure, nothing to mint |
| 2 | a one-time **enrolment token** | the credential a deployment with no orchestrator identity starts with |
| 3 | nothing | a fatal start-up error naming both of the ways out |

Workload identity first, deliberately: a deployment that has both is one being
migrated, and the credential that does not have to be minted, handed over and
spent is the one to prefer. For the rest of that story see
[Under an orchestrator](#under-an-orchestrator-none-of-the-three). With an
enrolment token:

```sh
RUSTAK_ENROLLMENT_TOKEN=<one-time token> rustak-plugin-adsb --config plugin.toml
```

It generates a key, sends a signing request, and writes three files: the
certificate, the key (mode `0600`) and the CA chain the server answered with, as
the truststore. Where they go is `[service] certificate`/`key`/`truststore` when
those name paths, `[service] pki_dir` for what they do not, and the directory
the configuration file is in for what *that* does not — which is the volume a
container image already mounts. The certificate's subject and expiry are logged
at `info`, and start-up carries on with them.

**The private key is generated inside the sidecar's own process and never leaves
it.** What crosses the wire is a signing request carrying the public half. There
is no "download my certificate" call to re-run, which is why a lost key is a
re-enrolment rather than a recovery — and why nothing has to ship a key into a
deployment.

On the next start the files are there, so nothing is enrolled and no token is
needed. A token that is still set is **ignored with an `info` line** rather than
spent again, so a leftover environment variable never re-enrols a running
deployment. An enrolment that fails — a spent or mistyped token, a server that
cannot be reached, a signing request the server refuses — is a fatal start-up
error naming the cause: a sidecar must not run half-identified.

| It enrols as | Which is |
|---|---|
| `username` | `[service] account`, or the service's own name — for a workload identity, whatever the server's binding rule says, and the certificate's common name is the answer |
| `clientUid` | `SERVICE-<name>`, the same uid it connects with |
| against | `[server] marti`, or `[server] control` — rustak's public listener serves `/Marti/api/tls/*` beside the control API |

### Which roots verify which endpoint

A sidecar talks to three things, and they do **not** all present the same kind
of certificate — see the listener table in `plan.md`. So `[service] truststore`
means something slightly different for each, and the sidecar decides per
endpoint rather than once:

| Endpoint | What the server presents | What the sidecar verifies it against |
|---|---|---|
| the CoT stream (`ssl://…:8089`) | rustak's internal CA, always | `[service] truststore`, **replacing** the platform's roots |
| `[server] marti` (`:8443`) | rustak's internal CA, always | `[service] truststore`, **replacing** the platform's roots |
| `[server] control` (`:8446`) | ACME, `files`, **or** the internal CA | the platform's roots **and** `[service] truststore` — or `[service] control_truststore` alone, when one is set |

The public listener is the odd one out because it is the one an installation is
most likely to have put a publicly trusted certificate on, while enrolment
writes rustak's own CA into `[service] truststore` on the first start. Joining
the two is what lets a sidecar enrol against an internal CA and then call a
control API behind Let's Encrypt — or the other way round — without being told
which shape its server has.

**`[service] control_truststore` replaces that set.** Set it only when you want
the public listener *pinned* to a PKI of your own and the platform's roots out
of the picture: an installation whose `:8446` certificate comes from a corporate
CA, say. It says nothing about the stream or Marti, nothing writes it, and a
deployment whose public listener holds an ACME or otherwise publicly trusted
certificate needs no such setting at all.

The enrolment call follows the same rule, because it is the first call a
deployment makes: against `[server] marti` it verifies with `[service]
truststore` alone, and against `[server] control` with the platform's roots plus
that truststore — or with `[service] control_truststore` when one is named. An
installation running rustak's `internal` CA on a listener the platform's roots
do not cover has to hand the sidecar that CA out of band first; a truststore
that is already there is kept rather than replaced by the chain the server
sends.

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
| After every `tick` | Asks `Sidecar::health`, and `POST /api/v1/services/<name>/heartbeat` with what it answered — a healthy state when it answered `None` |
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

`Heartbeat::healthy()` is the floor, and it is all the harness has to say on its
own. A plugin that knows more implements **`Sidecar::health`**, which the harness
asks after every `tick` and reports instead of the floor:

```rust
use rustak_api::{Heartbeat, ServiceState};

async fn health(&mut self) -> Option<Heartbeat> {
    let upstream = self.upstream.as_ref()?;

    Some(Heartbeat {
        state: match upstream.is_answering() {
            true => ServiceState::Healthy,
            false => ServiceState::Degraded,
        },
        message: Some(upstream.describe()),
        metrics: serde_json::json!({ "events_published": 1204, "queue_depth": 3 }),
    })
}
```

Three things make the hook the way to report, rather than one of two ways:

* **It cannot be overwritten.** The server stores the last heartbeat it was
  given — state, message and metrics, wholesale — so only one of them can be the
  row an administrator sees. What `health` answers *is* the heartbeat for that
  tick; the harness does not send one of its own as well.
* **It runs after the tick's work**, so it reports how the sidecar is *now*
  rather than how it was before it polled.
* **It is a value, not a request.** Nothing to await, nothing to swallow, and
  nothing to remember to call on the tick where it matters.

Answering is meant to be cheap — read what the tick already worked out, never
poll an upstream here, because the harness is waiting on it before it publishes.
`None` is "nothing to add", which the harness reports as healthy: that is what a
plugin answers before `start` has opened anything.

`metrics` is whatever your plugin says it is. The admin console renders it as a
key/value table without interpreting any of it (see *Monitoring a sidecar*
below), so nothing secret belongs in it.

#### The escape hatch

A plugin that must report *between* ticks — something that cannot wait for the
next one — still calls the control client itself:

```rust
if let Some(control) = self.context.as_ref().and_then(|ctx| ctx.control()) {
    let _ = control.heartbeat(&self.status()).await;
}
```

Swallow the failure, as above: a heartbeat that did not go through is not a
reason to stop publishing CoT. The harness notices that the plugin reported and
stays quiet for that tick rather than talking over it, so the two never race —
but the last word is then whichever of them spoke last, which is why `health` is
the one to reach for. Do not implement both for the same report.

### Monitoring a sidecar

**Settings → Services** in the admin console is the page this control API
exists for. It lists every registration — display name and `name`, version,
state, the message the last heartbeat carried, how long ago that was, when the
service registered, and its capabilities — with anything needing attention
sorted to the top, and re-reads itself every ten seconds while the tab is in
front. Selecting a row opens the detail beneath it: the endpoints the sidecar
reported, its `metrics`, its configuration, and a **Remove** button that takes
the registration away without touching the account, the certificate or the
channels behind it.

**What the page draws is the last heartbeat the sidecar sent**, which for a
plugin that implements `Sidecar::health` is exactly what that hook answered on
its last tick: the state beside the row, the `message` under it, the `metrics`
in the detail pane. A plugin that does not implement it shows as *healthy* with
nothing beside it, which is the harness saying the process is still ticking and
nothing more.

`metrics` is drawn as a key/value table:

| What you report | What the page shows |
|---|---|
| A flat object of numbers and short strings | One row per key, in key order |
| A nested object | Its key as a heading, with its own fields indented under it |
| An array | Its elements, comma-joined, on one row |
| Anything that is not an object | The value as a line of text |

So a heartbeat reports best as a flat object of counters, with at most one
level of nesting for something that genuinely groups:

```json
{
  "offered": 18422,
  "published": 4106,
  "suppressed": 14291,
  "expired": 25,
  "tracked": 612,
  "source": { "kind": "aisstream", "state": "connected" }
}
```

Keys are shown exactly as you spell them, and every value reaches the page as
text — a string that looks like HTML is rendered as that string, never as
markup — so a metric is never a way to put something into an administrator's
browser. Keep the values short: this is a table, not a log, and a paragraph in
a metric is a paragraph in a table cell. Anything that needs a sentence belongs
in the heartbeat's `message`, which the row shows in full.

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
without anybody restarting it — and it is what the console's Configuration
panel says when it saves: the change is stored, and the service picks it up on
its next tick rather than immediately. An administrator may read the
configuration as well as write it; a service reads only its own.

### Three credentials

A service token authenticates `/api/v1/services/*` and `GET /api/v1/events`, and
nothing else; the client certificate authenticates everything (and the control
API too, so a sidecar that has enrolled needs no token at all). Both are bounded
by the same things every other credential is: the account must not be disabled,
`[auth] user_acl` must allow the request, and a registration an administrator
has switched off stops authenticating. The token exists for the case where the
plugin has no certificate **yet**.

There is a third, and it is the one a deployment starts with: a **one-time
enrolment token**, which buys the certificate. The harness spends it for you on
the first start — see [The first start](#the-first-start) for the whole story,
the environment variable and the `--enroll` flag:

```sh
RUSTAK_ENROLLMENT_TOKEN=<one-time token> rustak-plugin-adsb --config plugin.toml
```

Mint one in the admin UI against the service's account, or with
`POST /api/v1/credentials` and kind `enrollment_token`. It is spent by the
enrolment it pays for and needed once, not on every start.

The same thing is a function for a plugin that wants to do it itself —
`rustak_client::enroll`:

```rust
use rustak_client::enroll::{Enrolment, Presentation, enroll};
use rustak_client::http::Trust;

let enrolled = enroll(&Enrolment {
    marti: "https://tak.example.com:8443",
    username: "svc.adsb",
    secret: &Secret::new(std::env::var("RUSTAK_ENROLLMENT_TOKEN")?),
    client_uid: "SERVICE-adsb",
    truststore: None,
    control_truststore: None,
    credential: Presentation::Basic,
    // The mTLS listener, so the truststore replaces the platform's roots.
    // `Trust::Public` for an enrolment against [server] control.
    trust: Trust::Internal,
})
.await?;

let paths = enrolled.write_to("/etc/rustak", "adsb")?;
```

**The private key is generated in the sidecar's own process and never leaves
it** — not on enrolment, not afterwards. What crosses the wire is a signing
request carrying the public half, and the key is written with mode `0600`. An
enrolment token is one-time and is spent only once the certificate has been
issued, so a failed enrolment leaves it usable — and a sidecar enrols when it
has no certificate rather than on every start.

#### Under an orchestrator, none of the three

A sidecar running under Nomad or Kubernetes already holds a signed statement of
what it is, and rustak accepts it in place of **both** minted credentials: it
buys the certificate, and it buys the access token the control API is reached
with. The deployment holds no rustak secret at all.

Nothing has to be configured on the sidecar. With `[service] workload_identity`
unset, three places are tried in order — `NOMAD_TOKEN_rustak`,
`${NOMAD_SECRETS_DIR}/nomad_rustak.jwt`, `/var/run/secrets/tokens/rustak` — and
a source that is found beats a leftover enrolment token, because the credential
that does not have to be minted and spent is the one to prefer. Name one
outright if you would rather be specific:

```toml
[service]
workload_identity = { env = "NOMAD_TOKEN_rustak" }
# or
workload_identity = { file = "/var/run/secrets/tokens/rustak" }
```

The token is re-read from its source **every time it is used**, because both
orchestrators rotate it; the access token bought with it is held until a minute
before it expires and exchanged again when the server refuses it. The server has
to be told which issuer to trust and which account a job maps to — that is
`[auth.workload]`, and it is the whole of the setup; see "Workload identity" in
[`docs/deployment.md`](deployment.md) for the jobspec, the pod spec and the
rules.

Whichever of the credentials a start used, one line at `info` says so:

```text
INFO Identity: this sidecar is 'ais', from the workload identity from NOMAD_TOKEN_rustak
INFO Identity: this sidecar is 'svc.adsb', from an enrolment token
INFO Identity: this sidecar is 'svc.adsb', from the certificate at '/data/adsb.pem'
```

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

## Feed sidecars

An *information feed* is the same plugin twice: subscribe to an open data
source over an area, turn each observation into a CoT track, publish it at a
rate a phone can carry, and let it go stale when the source stops reporting it.
`rustak_client::feed` is everything in that sentence except "subscribe to a
source", so a feed plugin is its own parsing and nothing else.

Two ship with rustak — [`rustak-plugin-ais`](../rustak-plugin-ais) (vessels) and
[`rustak-plugin-adsb`](../rustak-plugin-adsb) (aircraft) — and both are the
`Sidecar` above with a `feed` in the middle.

**AIS.** [`rustak-plugin-ais`](../rustak-plugin-ais) reads vessels from a
receiver of your own — `!AIVDM` sentences over UDP from AIS-catcher, `rtl_ais`
or a dAISy hat, with no key and no internet — or from the
[AISStream.io](https://aisstream.io) WebSocket feed, which wants a free API key
and the terms published on that site. It maps the AIS ship-type code to a
`VesselClass`, turns AIS's "not available" numbers (a heading of 511, a course
of 360, a speed of 102.3 knots) into nothing at all, and joins each position
report to the static report that carries the vessel's name a few minutes later.
Its README has the mapping table, the two staleness horizons it publishes with,
and what each source's data terms are.

The ADS-B sidecar reads a local `readsb`/`dump1090` receiver's own
`aircraft.json` (by path or over HTTP), a public aggregator's point endpoint
(adsb.lol, adsb.fi, airplanes.live) or the OpenSky Network's state vectors
behind OAuth2 client credentials, and maps the ADS-B emitter categories onto
`TrackKind::Aircraft`. Each of those has its own terms, its own rate limits and
its own attribution, and a couple need budgeting rather than just configuring:
[`rustak-plugin-adsb/README.md`](../rustak-plugin-adsb/README.md) is where they
are written down, and is worth reading before a deployment points at one.

**Not every feed is a track.** [`rustak-plugin-esb`](../rustak-plugin-esb) puts
Irish power outages on the map from ESB Networks' PowerCheck. An outage does not
move and has no allegiance, so it uses `Area` and `FeedCounters` from
`rustak_client::feed` but not `Track` or `FeedPublisher`: it builds spot-map
markers (`b-m-p-s-m`) coloured by status, republishes each one when it changes
and once per `refresh`, and keeps the last known outages on the map while its
upstream is down. It is the one to copy for anything that sits still — road
closures, weather warnings, river gauges.

### The `Track` contract

A source's whole job is to produce these:

| Field | What it is |
|---|---|
| `id: String` | The uid on the map, **already prefixed by its source**: `AIS-244660000`, `ADSB-3c6444`. The prefix is what stops two feeds watching the same airport from overwriting each other |
| `kind: TrackKind` | `Vessel(VesselClass)`, `Aircraft(AircraftClass)` or `GroundVehicle` — which decides the CoT type |
| `position: (f64, f64)` | Latitude and longitude, decimal degrees, WGS-84 |
| `altitude_hae_m: Option<f64>` | Height above the ellipsoid, metres. `None` becomes CoT's `9999999.0` |
| `speed_mps`, `course_deg`, `heading_deg` | Metres per second and degrees true. The heading is used as the course when no course was reported, because CoT's `<track>` has nowhere else to put it |
| `callsign: Option<String>` | What the map shows: a ship's name, a flight number |
| `remarks: Vec<(String, String)>` | Ordered lines rendered into `<remarks>` as `key: value` |
| `observed_at: DateTime<Utc>` | When the object was there, as its source said — not when we heard |
| `on_ground: bool` | For the plugin's own decisions; CoT says what a thing is through its type and has no separate flag |

`Track::to_event(affiliation, stale)` turns one into the event a client reads:
`how="m-g"`, the point with its sentinels, `time`/`start` from `observed_at`,
`stale` from the policy, a `<contact>` with the callsign (and **no endpoint** —
a ship is a thing on the map, not a chat peer), a `<track>` when a speed or a
bearing is known, and `<remarks>`.

### The CoT types

Written from the public MIL-STD-2525 hierarchy that CoT types follow, and the
affiliation (`Affiliation::{Unknown, Friend, Neutral, Hostile, Pending}` →
`u f n h p`) is a setting, defaulting to `unknown`: open data says nothing about
whose side a hull or an airframe is on.

| Kind | Type (unknown affiliation) |
|---|---|
| `Vessel(Merchant)` | `a-u-S-X-M` |
| `Vessel(Fishing)` | `a-u-S-X-F` |
| `Vessel(Leisure)` | `a-u-S-X-R` |
| `Vessel(LawEnforcement)` | `a-u-S-X-L` |
| `Vessel(Military)` | `a-u-S-C` |
| `Vessel(Other)` | `a-u-S-X` |
| `Aircraft(CivilFixedWing)` | `a-u-A-C-F` |
| `Aircraft(CivilRotary)` | `a-u-A-C-H` |
| `Aircraft(LighterThanAir)` | `a-u-A-C-L` |
| `Aircraft(MilitaryFixedWing)` | `a-u-A-M-F` |
| `Aircraft(Uav)` | `a-u-A-M-F-Q` |
| `Aircraft(Unknown)` | `a-u-A` |
| `GroundVehicle` | `a-u-G-E-V-C` |

### The area of interest

`Area` is a `Bbox { south, west, north, east }` (anti-meridian aware) or a
`Circle { lat, lon, radius_km }`, and it is used **twice**: the source
subscribes with it — `bbox()` for an upstream that only takes a box,
`centre()`/`radius_nm()` for one that takes a point — and the publisher checks
`contains()` again before anything goes out, because an upstream that widens its
box is not a reason for a channel to fill up with the Atlantic.

### The policy knobs

`PublishPolicy` is what turns a thousand vessels reporting every two seconds
into a rate an operator's device can carry:

| Key | Default | What it does |
|---|---|---|
| `stale` | `"2m"` (`"90s"` for ADS-B) | How long a published track lives on a map without another report |
| `min_interval` | `"5s"` | The floor between two publications of the same track, however far it moved |
| `max_interval` | `"60s"` | The ceiling, which is what keeps a moored vessel on the map |
| `min_move_m` | `25` | How far a track moves before it is worth saying so |
| `max_tracks` | `5000` | How many tracks are held before the least recently seen is dropped |

A track is published when it is new, when it has moved at least `min_move_m`,
when it has turned by ten degrees or changed speed by five knots, or when
`max_interval` has passed — and never more often than `min_interval`.
`tick()` forgets what has not been reported for `stale` and evicts beyond
`max_tracks`; `counters()` answers what was offered, published, suppressed and
expired, and the publisher logs that at `info` every five minutes.

**A feed never sends a delete.** TAK clients expire a track by the `stale` on
the last event they were given, so a sidecar that is stopped or killed leaves a
map that empties itself over the next two minutes rather than one full of
ghosts.

### Publishing is still a return value

`FeedPublisher` buffers rather than sends, because `tick` and `on_event` return
what the harness writes:

```rust
async fn tick(&mut self) -> Result<Vec<Event>, Error> {
    match self.feed.poll().await {
        Ok(tracks) => tracks.into_iter().for_each(|track| { self.publisher.offer(track); }),
        Err(err) => warn!("The feed did not answer: {err}"),
    }

    self.publisher.tick();

    Ok(self.publisher.drain())
}
```

An upstream that is down is a log line, never a stopped sidecar: reconnection
and backoff belong to the `Feed` implementation, which is the only thing that
knows whether its upstream wants a new socket, a new token or another minute.
On `SidecarEvent::Connected`, call `refresh_all()` — a reopened connection is a
new subscription, and the server has none of what went down the old one.

### The replay fixture format

`Replay` is a `Feed` over a file, which is what the demonstrations and
`rustak-server/tests/feed_sidecars.rs` run on: one JSON `Track` per line
(newline-delimited JSON), blank lines and `#` comments ignored.

```json
{"id":"AIS-244660000","kind":{"vessel":"merchant"},"position":[51.9512,4.1338],"speed_mps":6.2,"course_deg":271.5,"callsign":"ZEEBRUGGE","remarks":[["MMSI","244660000"]],"observed_at":"2026-09-20T12:00:00Z"}
```

Every poll answers the whole file, stamped with the moment it was offered — a
fixture written last week would otherwise publish tracks that every client
expires on arrival. Offering the same observations repeatedly is the point: it
is what makes a replay a fair test of the policy.

## See also

- `rustak-plugin-example/` — the template this document describes.
- `rustak-plugin-ais/`, `rustak-plugin-adsb/` — the two track feed sidecars.
- `rustak-plugin-esb/` — a feed of things that sit still: power outages as markers.
- `docs/ci.md` — how a plugin crate is built, published and released.
- `.claude/plan/plan.md` → Architecture → "Plugin (sidecar) contract".
