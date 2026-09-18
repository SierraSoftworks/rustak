# M6-01 — Services control API, server-event feed, and the sidecar SDK

**Status: complete.** Every deliverable in the brief is implemented and every exit check is
green. `docs/plugins.md` no longer says "M6 is not wired up yet"; the copy-and-rename recipe
was run end to end and the doc now records the command that does it.

## What landed

| Area | Files |
|---|---|
| Server-event DTOs | `rustak-api/src/event.rs` (+ module and re-export lines in `rustak-api/src/lib.rs`) |
| Plugins domain | `rustak-server/src/plugins/{mod,auth,registry,health,events}.rs` (+ `pub mod plugins;` in `lib.rs`) |
| Control API + feed | `rustak-server/src/web/api/{services,events}.rs` (+ module lines, two `.configure(…)` lines and seven route-table entries in `web/api/mod.rs`) |
| Heartbeat metrics | `rustak-server/migrations/0014_service_metrics.sql`, `rustak-server/src/db/repos/services.rs` |
| Sweep job | `rustak-server/src/jobs/service_health.rs` (+ 2 lines in `jobs/mod.rs`) |
| Hooks | `stream/hub.rs` + `stream/live.rs` (connection watchers), `services/mod.rs` (the bus, `Services::events`, one line in `install_live`), one line each in `missions/notify.rs`, `identity/members.rs`, `files/upload.rs` |
| SDK — HTTP | `rustak-client/src/http.rs`, `rustak-client/src/enroll.rs` |
| SDK — Marti | `rustak-client/src/marti/{mod,client,missions,mission_model,files,groups,contacts}.rs` |
| SDK — control | `rustak-client/src/control/{mod,register,events}.rs` |
| SDK — harness | `rustak-client/src/sidecar/{mod,run,control_link}.rs`, `rustak-client/src/lib.rs`, `rustak-client/Cargo.toml` |
| Example + docs | `rustak-plugin-example/src/main.rs`, `rustak-plugin-example/config.example.toml`, `docs/plugins.md` |
| Tests | `rustak-server/tests/services_flow.rs` |

## The control API

All six routes are mounted in the `/api/v1` **public** scope — outside
`middleware::api_auth`, which takes one of our own RS256 access tokens and nothing else.
A sidecar's credential is a *service token* or its client certificate, and widening that gate
would have widened it for every route under it. Each handler therefore resolves its own
caller through `plugins::auth::caller`, and `web/api/mod.rs`'s existing "nothing behind the
gate answers without a session" table now lists all seven paths so that an unauthenticated
`401` is asserted rather than assumed.

| Route | Who | Answer |
|---|---|---|
| `POST /api/v1/services/register` | the service itself | `200` `ServiceSummary`; `403` for a person, `409` for a name another account holds |
| `GET /api/v1/services` | administrator | `200 ServiceSummary[]` |
| `POST /api/v1/services/{name}/heartbeat` | the service or an administrator | `200 ServiceStatus`; **`404` when the registration has gone**, which is what tells a running sidecar to register again |
| `DELETE /api/v1/services/{name}` | the service or an administrator | `204`, leaving the account (and therefore the certificate and the channels) alone |
| `GET /api/v1/services/{name}/config` | the service or an administrator | `200`, the JSON object |
| `PUT /api/v1/services/{name}/config` | **administrator only** | `200`, the stored object |
| `GET /api/v1/events` | administrators and services | `text/event-stream` |

Two authorization rules are worth naming because they are the ones that would otherwise be
easy to get wrong:

- **A service name belongs to the account that first claimed it.** Registration is an upsert
  keyed on the name, and the repository's `ON CONFLICT (name) DO UPDATE SET user_id = …`
  would otherwise let any service token take over any other sidecar's registration by naming
  it. `registry::register` reads the existing row first and answers `RegistryError::Taken`
  (`409`, code `service_name_taken`).
- **A service reads its own configuration; only an administrator writes one.** A plugin that
  could rewrite its own configuration would make the admin UI's copy a suggestion.

Bodies are parsed **after** authentication (`web::Bytes` plus `serde_json::from_slice`, not
`web::Json<T>`): an extractor runs before the handler, so a malformed body from a caller with
no credential would have answered `400` and confirmed the route exists. The route-table test
catches this — it sends `{}` everywhere and requires `401`.

## Authentication (`plugins::auth`)

Certificate → service token → access token, strongest first, the order
`auth::resolve::resolve_principal` already uses. The service-token arm looks the credential up
by `lookup_hint` rather than by account, because a service token names no account — it *is*
the account. There is deliberately no dummy-hash on the miss path: with no username in the
request there is nothing a timing difference could reveal, which is also why the arm is not
rate limited (30 bytes of entropy behind `rsk_`, the same argument our own bearer tokens
rest on).

A bearer header that is not a service token is **not** a refusal; it falls through to
`auth::resolve::bearer`, because the same header carries an administrator's session.

## The server-event feed

`plugins::events::ServerEvents` is a `tokio::sync::broadcast` channel (depth 256) plus a ring
of the same size, held on `AppContext` and reached through a new `Services::events()`. It is
on the trait rather than an inherent method because two of the four hooks are in domain
modules written against `&impl Services`.

The five hook call sites are one additive line each:

| Event | Hook |
|---|---|
| `client.connected` / `client.disconnected` | `Hub::register`/`unregister` announce to `ConnectionWatcher`s; `AppContext::install_live` registers the one watcher |
| `mission.changed` | `MissionService::notify` |
| `channel.changed` | `identity::members::channels_changed` |
| `package.uploaded` | `files::upload::audit` (the filter on `action == "uploaded"` lives in `events.rs`, so the call site stays unconditional) |
| `service.status` | `plugins::health::{record, sweep}` |

`stream::live` gained `ConnectionChange`, `ConnectionSummary`, `ConnectionWatcher` and
`LiveState::watch_connections`; `stream/hub.rs` gained `Hub::watch` and two `announce` calls.
The three types live in `live.rs` rather than `hub.rs` only because `hub.rs` would otherwise
be 319 functional lines. Watchers are called **outside** the registry lock, on the
connection's own task, and the only watcher we register does nothing but build a payload and
push it.

`GET /api/v1/events` is an `actix-web` streaming response built from `futures::stream::unfold`:

- `retry: 5000` preamble, then the backlog, then the live feed.
- `Last-Event-ID` **or** `?lastEventId=` (an `EventSource` cannot set a header on its first
  connection). The subscription is taken *before* the backlog is read, and ids are checked on
  the way out, so nothing published in between is lost or delivered twice.
- A consumer that lags is **refilled from the ring** rather than disconnected; the gap in the
  ids is what says what could not be delivered.
- `: keep-alive` every 20s of silence, plus `X-Accel-Buffering: no`, so a proxy neither
  buffers the stream nor decides it is dead.

Nothing secret crosses the bus: names, uids, hashes and sizes — no tokens, no certificate
material, no file contents, no peer addresses.

## Migration `0014`

`ALTER TABLE services ADD COLUMN metrics TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metrics))`.
`Heartbeat.metrics` has been part of the DTO contract since M0-03 and had nowhere to land, so
the admin UI could show that a feed was degraded but not by how much. `db/repos/services.rs`
reads it back and `record_heartbeat` gained a fourth argument (see deviations).

## The SDK

```
rustak-client/src/
├── http.rs              one reqwest client from a ServiceIdentity, shared by both APIs
├── enroll.rs            CSR → signClient/v2 → three files on disk
├── marti/               client, missions, mission_model, files, groups, contacts
├── control/             ControlClient, register/heartbeat/config, the SSE EventStream
└── sidecar/control_link.rs   the harness's end of the control API
```

- **`http::client`** attaches the client certificate when the identity carries both halves and
  leaves it off when it does not — a sidecar with only a service token can still call the
  control API, which is the whole reason that token exists. A configured `[service] truststore`
  **replaces** the platform roots (`reqwest`'s `tls_certs_only`) rather than joining them, for
  the reason `stream::TlsIdentity` gives.
- **`enroll`** generates the key locally (rcgen, added to `rustak-client`'s dependencies),
  sends the CSR to `POST /Marti/api/tls/signClient/v2?clientUid=…&version=3` under HTTP Basic,
  re-armours the bare base64 the server answers with, and writes `<name>.pem`, `<name>.key`
  and `truststore.pem`. `Paths::attach` turns them into a `ServiceIdentity`.
- **`marti`** is lenient by design: every field defaults, nothing uses `deny_unknown_fields`,
  and the parts TAK treats as opaque stay `serde_json::Value`. `Envelope<T>` is the
  deserialising half of the server's `ApiResponse`, with `data: Option<T>` because its absence
  is meaningful. `/Marti/api/contacts/all` is read without the envelope, because it has none.
  Mission tokens travel in `MissionAuthorization`, and `API_VERSION: 3` is set on every
  request so a caller with no read role gets a stripped `200` rather than a `403`.
- **`control::events`** parses SSE by hand. An event whose `type` this build has never heard of
  is logged and skipped rather than ending the stream, which is what lets a plugin built today
  keep running against a server that learns to announce something new.

### The harness

`SidecarContext` gained `marti()` and `control()`, built from one shared `reqwest` client so a
plugin that uses both APIs shares a connection pool. `SidecarEvent` gained `Server(Box<ServerEvent>)`.
`drive` now registers before `Sidecar::start`, reports a heartbeat after every tick, and
selects on a fourth branch.

The feed is a **task plus an `mpsc` channel**, not a stream polled from the `select!`: opening
an HTTP response is not cancel-safe, and a sidecar with a short tick would never finish opening
one. `mpsc::Receiver::recv` is cancel-safe. The task reconnects with backoff (1s → 60s) and
resumes from the last id it delivered.

**Nothing in the control path can stop a sidecar.** A refused registration, a lost heartbeat
or a dropped feed is logged and retried: the CoT a plugin publishes is the part that matters,
and the server notices missing heartbeats on its own.

## Tests

- `plugins::{auth,registry,health,events}` unit tests: name theft, revoked tokens, one service
  acting on another, heartbeats for a registration that has gone, the bounded ring, resume.
- `web::api::{services,events}` route tests over `TestServer`: the full register → heartbeat →
  read-config path, "only an administrator writes", cross-service refusals, `Last-Event-ID`
  and `?lastEventId=` resume, and `401` for every route with no credential.
- `rustak-client`: `wiremock` unit tests for every Marti area, the control client, the SSE
  parser (including a skipped unknown event and a frame with no trailing blank line), and
  enrolment (including a spent token and where the files land).
- `rustak-server/tests/services_flow.rs`: two in-process suites over real sockets —
  **enrol → connect → register → heartbeat → server event received**, and *an administrator
  removes the registration while the sidecar is running and the sidecar registers again*.
  The plugin is written out in the test file because `rustak-plugin-example` is a binary crate
  with no library target; what is exercised is `rustak_client::sidecar::drive`, which is the
  loop every plugin's `main` ends in.

`services_flow` runs under `#[actix_web::test]` (an actix `System` is what `HttpServer::run`
needs) and binds the public route tree over the stream harness's own context on plain HTTP —
one listener serving both `/Marti/api/tls/*` and `/api/v1/services/*`. Teardown stops the
*harness* before the API: cancelling the server's shutdown is what ends the open server-event
response, and a graceful stop with one still open waits for its next keepalive write to fail
(that ordering took the suite from 32s to 1.9s).

## The copy-and-rename recipe, checked

```bash
cp -r rustak-plugin-example rustak-plugin-adsb
sed -i '' 's/rustak-plugin-example/rustak-plugin-adsb/g' \
  rustak-plugin-adsb/Cargo.toml rustak-plugin-adsb/Dockerfile
cargo run -p rustak-plugin-adsb -- --config rustak-plugin-adsb/config.example.toml --check
```

Run end to end against a scratch copy: the workspace `rustak-*` glob picks the crate up with
no root-manifest change, the three name occurrences are the package, the `[[bin]]` and the
Dockerfile's `ADD`/`ENTRYPOINT`, and `--check` loads the copied example file. The scratch crate
was removed afterwards. `docs/plugins.md` now carries this rather than the four-step prose it
had, and its old "M6 is not wired up yet" table is replaced by "Registering with the server",
"Reacting to server events" and "The Marti API".

## Deviations

1. **`db/repos/services.rs` was edited**, which the brief did not list. Migration `0014` is
   useless without a repository that reads the column: `COLUMNS` gained `metrics`, `ServiceRow`
   gained the field, and `record_heartbeat` gained a fourth argument. Its three existing tests
   were updated for the new signature. No other caller exists.
2. **`jobs/service_health.rs` was added** (+ `pub mod` and `pub use` in `jobs/mod.rs`).
   `health::sweep` would otherwise be a public function nothing calls, and "healthy, an hour
   ago" is the answer that misleads an operator. Self-arming on the shape `audit_prune`
   established, every 30s against a 90s grace period.
3. **`services/mod.rs` gained more than one line**: the `events` field, its construction, a
   `Services::events()` trait method with its two implementations, a `Debug` field, and the
   one-line hook in `install_live`. There was nowhere else to hold a bus that both the stream
   listener and three domain modules feed.
4. **`marti/mission_model.rs` and `sidecar/control_link.rs`** are files the brief did not name.
   Both are the file-length rule, not a design statement: `missions.rs` was 315 functional
   lines with the model in it, and the harness's control-API glue does not belong in `run.rs`.
5. **`rustak-client/Cargo.toml` adds `features = ["query"]` to the workspace `reqwest`** rather
   than changing the root manifest, which M2-10 owns. Features are additive, so this asks for
   one more rather than replacing the set. `rcgen` moved from dev-dependencies to dependencies
   (enrolment needs it) and `wiremock` was added as a dev-dependency.
6. **`rustak-plugin-example` does not call `ctx.control()` itself.** The harness registers and
   heartbeats for it, which is the point; what the example demonstrates is the *reacting* half
   (`SidecarEvent::Server` → `client.connected`). `docs/plugins.md` shows the richer heartbeat
   for a plugin that wants one.
7. **Enrolment takes its truststore from the sign response, not from `tls/config`.** The brief
   said "truststore from `tls/config`/CA download"; `GET /Marti/api/tls/config` carries no
   certificate at all (it is the `nameEntry` list), and `GET /api/v1/setup/ca` is administrator-
   only and `410 Gone` once the wizard is finished. The `ca0`/`ca1`/… keys of the
   `signClient/v2` answer are the chain, and are what ATAK's own quick-connect stores — so
   `Enrolled::truststore_pem` is built from those. `Enrolment::truststore` is the *inbound*
   half: the CA a private-CA installation has to distribute before a sidecar can enrol at all,
   exactly as it does for a device.
8. **The feed is for administrators and services**, not every authenticated account. It says
   which devices are on the stream and which packages arrived across every channel, which is
   not something an ordinary account is given. The brief did not specify; this is the
   conservative reading.

## Exit checks

```
$ cargo fmt --all --check
(clean)

$ cargo clippy --workspace --all-targets -- -D warnings
Finished `dev` profile — no warnings

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
Generated target/doc/rustak_api/index.html and 6 other files

$ ./scripts/check-file-length.sh
(clean)

$ cargo test -p rustak-server --features testing
23 suites, 1773 passed, 0 failed

$ cargo test --workspace
38 suites, 2542 passed, 0 failed

$ cargo test -p rustak-client --all-features
5 suites, 148 passed, 0 failed

$ cargo run -p rustak-plugin-example -- --config rustak-plugin-example/config.example.toml --check
INFO Loaded the configuration for rustak-plugin-example 0.1.0.
INFO The configuration is valid; --check does not start the sidecar.
```

## One thing for the orchestrator

`rustak-server/tests/enroll_flows.rs` (M2-03) is **flaky under load**. Its two mTLS tests —
`a_cloudtak_shaped_enrolment_produces_a_certificate_the_marti_listener_accepts` and
`a_revoked_certificate_cannot_complete_the_handshake` — failed together on three
`cargo test --workspace` runs while other agents were rebuilding, then passed on the next two,
and failed once under `--workspace --exclude rustak-plugin-example` after passing under it.
They always pass under `cargo test -p rustak-server`. The likely cause is `reserve_port()`
(`enroll_flows.rs:79`), which binds `:0`, reads the port and *drops the listener* before
`build_marti` binds it — the configuration has to name a port before the listener exists, and
under a saturated machine something else takes it in between. Nothing in M6-01 is mounted on
the Marti listener. Worth a fix in that suite (bind once and hand the `TcpListener` to
`build_marti`, as `services_flow` does) rather than a retry loop.
