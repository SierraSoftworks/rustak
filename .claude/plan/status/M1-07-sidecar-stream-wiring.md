# M1-07 — Wire the sidecar harness to the real stream client — complete

Brief: `.claude/plan/briefs/M1-07-sidecar-stream-wiring.md`
Depends on M0-15 (`rustak-client::sidecar`) and M1-06 (`rustak-client::stream`).

> The brief points at `.claude/plan/status/{M1-04-client-stream,M1-04b-client-stream-fixups}.md`.
> Those names do not exist; the stream client's status file is
> `M1-06-client-stream.md` (`M1-04*` are the EUD interop image and its OpenSSL
> build fix). Read that one instead.

## What was built

| File | Functional lines (limit 300) | Change |
|---|---:|---|
| `rustak-client/src/sidecar/link.rs` | 138 | **New.** `Link`: config → `StreamConfig`, `Reconnecting` behind it, connection state → `SidecarEvent`, `publish` |
| `rustak-client/src/sidecar/mod.rs` | 111 | `SidecarEvent::Cot(Box<rustak_cot::Event>)`, new `Negotiated { protobuf }`, `tick`/`on_event` return `Vec<Event>` |
| `rustak-client/src/sidecar/run.rs` | 116 | The loop now `select!`s over shutdown, tick **and** the stream, and writes what the plugin returns |
| `rustak-client/src/stream/reconnect.rs` | 201 | One additive accessor, `Reconnecting::last_error()` — see "The one edit outside the brief's files" |
| `rustak-client/src/lib.rs` | 6 | Crate doc: the harness drives the stream |
| `rustak-plugin-example/src/main.rs` | 127 | Publishes a real `a-f-G-U-C` SA with `<contact>` + `<__group>`; logs inbound `uid`/`type` |
| `rustak-plugin-example/config.example.toml` | — | `[settings] lat/lon/team/role`; `[server] stream` and the certificate keys documented against what now uses them |
| `docs/plugins.md` | — | New "The CoT stream" section, the M1 status note, publishing-is-a-return-value, testing with `Eud` |

87 tests (77 `rustak-client` unit, 5 `rustak-plugin-example` unit, plus the
crate's existing integration and doc tests), all green.

## Decisions

### `SidecarEvent::Cot` carries `rustak_cot::Event`

As the brief asks. The protobuf `TakMessage` was the M0 placeholder; a plugin
wants the decoded model, and `TakStream` already produces one whichever encoding
the connection settled on.

### `Negotiated { protobuf: bool }` is in

The brief left it optional ("if the harness can observe it cheaply"). It is
cheap: `Reconnecting::stream()` hands back the live `TakStream`, which already
exposes `negotiation()` and `mode()`, so `Link` reports it the first time
`Negotiation::is_settled()` is true on a connection. It is worth having — the
difference between "connected on XML" and "connected on protobuf" is the first
thing anybody asks when interop looks wrong, and a plugin would otherwise have
to reach for the connection to find out.

### **The trait's public surface changed: `tick` and `on_event` return `Vec<Event>`**

The brief says to send "whatever `on_event`/`tick` returns (the existing
`Vec<Event>`/publish shape)". There was no such shape — M0 shipped both methods
returning `Result<(), Error>` — so this is a change, documented here and in
`docs/plugins.md` as the brief requires:

```rust
async fn tick(&mut self) -> Result<Vec<Event>, Error>;
async fn on_event(&mut self, event: SidecarEvent) -> Result<Vec<Event>, Error>;
```

Returning rather than sending is what keeps the plugin away from the socket.
The alternative — a `Publisher` handle on the context feeding an mpsc the
harness drains — buys publishing from a plugin's own spawned task, and costs a
queue whose depth, ordering and drop policy become part of the plugin contract.
Nothing in M1 needs that, and `Connected` → return your SA covers the one case
that made it look necessary.

`start` and `stop` are unchanged, so the M0 doc example and the
"implement `start` and nothing else" plugin still compile.

### Publishing while disconnected drops, and says so

`Link::publish` writes only when the connection is up. A position report held
through a thirty-second backoff and then delivered is worse than one that was
never sent, and the alternative is an unbounded queue nobody asked for. A plugin
that disagrees holds the event itself and returns it again from the next
`Connected`, which is exactly what the example plugin does with its own SA.

The two "nothing was written" cases log differently on purpose: a sidecar with
no `[server] stream` at all gets `debug!` (its operator has a line to add, not a
fault to chase, and a `warn!` would repeat on every tick), while a connection
that is reconnecting gets `warn!`.

### The connection is opened before `Sidecar::start`

A connect string or a certificate path an operator got wrong is then reported as
itself, rather than behind whatever the plugin's own start-up does first. The
socket is not dialled there — `Reconnecting` dials lazily on its first poll — so
a server that is down still starts the plugin and retries.

### `Connected`/`Disconnected` are derived from the connection, not emitted by it

`Reconnecting` is deliberately opaque: it logs an outage, backs off, reopens,
and yields nothing but successfully decoded events. That is right for the loop,
and leaves nobody to tell a plugin it has a new subscription to re-announce
itself on. `Link::poll_next` therefore polls the connection and then reads the
transitions back out of the state that poll left behind (`is_connected()`,
`last_error()`, `stream().negotiation()`), emitting one per poll, ordered so
that `Connected` always precedes the first event of that connection (an event
that arrives in the same poll is held in `Link::held`, not dropped).

### Control traffic still never reaches a plugin

`StreamConfig::pass_control` stays false, so pings, pongs and the `t-x-takp-*`
exchange are answered inside `TakStream`. Asserted in
`a_connection_identifies_itself_as_the_service_the_file_names`.

### The example plugin is a contact, not a log line

It publishes `a-f-G-U-C` with its own `SERVICE-<name>` uid, a `<contact>`
carrying the descriptor's display name and the `*:-1:stcp` streaming-endpoint
sentinel, and a `<__group>` — the four things `compat/streaming.md` §10 says a
server reads a subscription's identity out of. It re-publishes on every
`Connected`, and its SA goes stale after three ticks (floor 60 s) so one missed
heartbeat does not make it vanish from every map at once. `[settings]` gained
`lat`/`lon` (default 0,0 — in the Gulf of Guinea, so an unconfigured sidecar is
obviously unconfigured rather than plausible), `team` and `role`.

`config.example.toml` still leaves `[server] stream` commented out, so
`cargo run -p rustak-plugin-example -- --config …` remains an offline demo.

## The one edit outside the brief's files

`rustak-client/src/stream/reconnect.rs` gained **three lines**: a
`last_error: Option<String>` field, the assignment in the existing
`drop_connection(&mut self, reason: &str)`, and a `pub fn last_error(&self) ->
Option<&str>` accessor.

The brief keeps `SidecarEvent::Disconnected { reason }` and requires the harness
to be driven by `Reconnecting`. `Reconnecting` is the only thing that knows why
a connection ended — it formats the reason into a `tracing::warn!` and discards
it — so without the accessor the variant can only ever carry a constant string.
Nothing else in `stream/**` was touched, and the change is purely additive.

## Testing

`the_harness_connects_receives_publishes_and_reconnects` (in `sidecar/run.rs`)
drives a real `drive()` against a `tokio::net::TcpListener` wrapped in
`stream::testing::Eud`, and asserts the whole cycle:

connect → `Connected` → server offers protobuf and refuses the request →
`Negotiated { protobuf: false }` → the server's SA arrives as
`SidecarEvent::Cot` with the right uid and callsign → the tick's event is read
off the wire by the server → the server drops the socket → `Disconnected` →
`Connected` again on a second `accept()`.

**Time is not paused, contrary to the brief's note.** Tokio's auto-advance races
real socket readiness: with a pending `accept()` and a live tick interval it
advances the clock past timers the test has not reached yet, which makes the run
non-deterministic in the other direction. Determinism comes from the assertions
instead — every wait is for something to arrive on a channel or a socket,
bounded by a timeout that only fires when the test has genuinely failed, and
there is no `sleep` anywhere in it. The only real wait is the reconnect's
`MIN_BACKOFF` second. Twelve consecutive runs: 1.00–1.01 s each, no failures.

A loopback listener rather than a `tokio::io::duplex`, because *redialling* is
the thing being proved and a duplex cannot be dialled twice. `Eud` is still what
sits on the server end of it.

The remaining coverage is in `sidecar/link.rs`: the uid/callsign/`pass_control`
derivation, a TLS endpoint refused at start-up for missing certificate material,
an unreadable connect string refused by name, an idle link that is never ready,
and publishing into a link that has no connection.

## Exit checks

### `cargo test -p rustak-client --all-features`

```
running 77 tests  (lib)
test result: ok. 77 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
running 12 tests  (tests/stream_client.rs)
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
running 2 tests   (tests/stream_reconnect.rs)
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
running 2 tests   (tests/stream_tls.rs)
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
running 6 tests   (doc-tests)
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
```

`cargo test -p rustak-client` (default features) also passes: 77 + 11 + 0 + 2 + 5.

### `cargo test -p rustak-plugin-example`

```
running 5 tests
test tests::an_absent_settings_table_gives_the_written_out_default ... ok
test tests::the_example_configuration_file_is_one_this_plugin_can_load ... ok
test tests::the_heartbeat_is_an_sa_message_a_server_can_read_an_identity_out_of ... ok
test tests::every_reconnection_republishes_the_contact ... ok
test tests::the_plugin_heartbeats_between_starting_and_stopping ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### `cargo clippy -p rustak-client -p rustak-plugin-example --all-targets --all-features -- -D warnings`

```
    Checking rustak-client v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-client)
    Checking rustak-plugin-example v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-example)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.34s
```

### `RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-client --no-deps`

```
 Documenting rustak-client v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-client)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.25s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_client/index.html
```

### `cargo fmt --all --check`

Clean for every file this brief owns. The workspace-wide run reports diffs in
`rustak-server/src/**` and `rustak-server/tests/**` only — another agent's
in-flight work, not touched here:

```
$ cargo fmt --all -- --check | grep '^Diff in' | grep -v rustak-server
(no output)
```

### `./scripts/check-file-length.sh`

```
(no output)
```

`link.rs` is untracked, so `git ls-files` does not yet reach it; counted by hand
with the script's own `awk` it is 138 functional lines.

### `cargo run -p rustak-plugin-example -- --config rustak-plugin-example/config.example.toml --check`

```
INFO rustak_client::sidecar::run: Loaded the configuration for rustak-plugin-example 0.1.0. descriptor=ServiceDescriptor { name: ServiceName(example), ... } file=rustak-plugin-example/config.example.toml
INFO rustak_client::sidecar::run: The configuration is valid; --check does not start the sidecar.
exit: 0
```

### A live run, then `kill -INT` (ANSI stripped)

```
INFO rustak_client::sidecar::run: Loaded the configuration for rustak-plugin-example 0.1.0. ...
INFO sidecar{service=example uid=SERVICE-example}: rustak_client::sidecar::link: This sidecar has no [server] stream, so it will not open a CoT connection.
INFO sidecar{service=example uid=SERVICE-example}: rustak_plugin_example: The example sidecar is starting as example. uid=SERVICE-example stream=None
INFO sidecar{service=example uid=SERVICE-example}: rustak_client::sidecar::run: The sidecar has started. interval=5s stream=None
INFO sidecar{service=example uid=SERVICE-example}: rustak_plugin_example: The example sidecar is alive. heartbeats=1
INFO sidecar{service=example uid=SERVICE-example}: rustak_plugin_example: The example sidecar is alive. heartbeats=2
INFO sidecar{service=example uid=SERVICE-example}: rustak_plugin_example: The example sidecar is alive. heartbeats=3
INFO sidecar{service=example uid=SERVICE-example}: rustak_plugin_example: The example sidecar is alive. heartbeats=4
INFO rustak_core::runtime: Received a shutdown signal; draining connections.
INFO sidecar{service=example uid=SERVICE-example}: rustak_client::sidecar::run: The sidecar is stopping.
INFO sidecar{service=example uid=SERVICE-example}: rustak_plugin_example: The example sidecar is stopping. heartbeats=4
```

Four ticks published nothing and said nothing about it, which is the point of
the `debug!`/`warn!` split above.

## For the orchestrator

- `rustak-client/src/sidecar/link.rs` is **new and untracked**.
- `rustak-client/src/stream/reconnect.rs` is touched — three additive lines,
  justified above. It is M1-06's file, so flag it if that brief is still open.
- The `Sidecar` trait is a **breaking change for any plugin outside this
  repository**: `tick` and `on_event` now return `Result<Vec<Event>, Error>`.
  The only in-tree implementors (`rustak-plugin-example`, the harness's own test
  sidecar) are updated.
- No `git`/`but` writes were made.
- `cargo fmt --all --check`, `cargo clippy --workspace` and `cargo doc
  --workspace` cannot pass while `rustak-server/**` is mid-flight; every check
  above is scoped to the crates this brief owns.
