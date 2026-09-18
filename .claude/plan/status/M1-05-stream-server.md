# M1-05 — `rustak-server::stream` + `cot_store`: TLS listener, hub, router, negotiation, replay — complete

Brief: `.claude/plan/briefs/M1-05-stream-server.md`
Read first: `conventions.md`; `compat/streaming.md`; `design/02-protocol-streaming.md` §2 (all), §4,
§5 steps 9–15, §6; `plan.md` → Appendix A.1, Storage, Listeners; status files `M1-01`, `M1-02`,
`M1-03` (rustak-cot API), `M1-06` (the `Eud` helper), `M2-01` (Pki facade, `stream_server_config`,
`RustakClientVerifier`, `PeerCertificate`, revocation hooks, `TestAuthority`), `M2-02`
(`members::effective_for_device`, `devices::upsert_seen`), `M0-09` (AppContext), `M0-10`
(`store::append_log`), `M0-12` (runtime wiring).

## What was built

| File | Functional lines | Unit tests | Contents |
|---|---:|---:|---|
| `stream/mod.rs` | 166 | 0 | module map, `StreamRuntime` (bind → run), `serve`, re-exports |
| `stream/listener_tls.rs` | 135 | 2 | `Bound`, `bind`, `run` (accept loop, handshake timeout, connection semaphore), `serve_one` |
| `stream/resolver.rs` | 166 | 0 | `StreamPrincipal`, `CertPrincipalResolver`, `DbPrincipalResolver` |
| `stream/connection.rs` | 209 | 0 | `ConnLimits`, `ConnDeps`, `run`, `read_loop`, `answer`, `decode`, codec-counter sync |
| `stream/writer.rs` | 132 | 6 | `WriterContext`, `run` (greedy drain), the `SwitchToProto` step, the `b-f-t-r` substitution |
| `stream/negotiation.rs` | 84 | 8 | `NegState`, `Intercepted`, `Negotiation` |
| `stream/subscription.rs` | 247 | 9 | `ConnId`, `Outbound`, `SendResult`, `ConnStats`, `ConnHandle`, `Subscription`, `SaUpdate`, `ClientEndpoint` |
| `stream/hub.rs` | 240 | 14 | `Hub` — the lock, and every routing question asked under it |
| `stream/registry.rs` | 60 | 3 | `Registry` and the two string indexes over it |
| `stream/router.rs` | 146 | 11 | `Disposition`, `Router::handle_inbound`, fan-out, recording |
| `stream/dest.rs` | 183 | 6 | `DropReason`, `Selection`, `select_recipients`, `<dest>` partitioning |
| `stream/control.rs` | 40 | 5 | `ControlAction`, `handle` (pong, incognito, explicit no-ops) |
| `stream/replay.rs` | 19 | 2 | `replay_latest_sa` |
| `stream/notify.rs` | 92 | 6 | `Notifier` + `impl for Hub`, `on_disconnect`, `on_groups_changed` |
| `stream/mission_hook.rs` | 46 | 2 | `MissionRef`, `MissionIngest`, `NoMissions`, `no_missions()` |
| `stream/live.rs` | 76 | 4 | `LiveState` — the handle the rest of the server holds |
| `stream/metrics.rs` | 35 | 3 | `StreamMetrics` |
| `cot_store/mod.rs` | 115 | 4 | `STREAM_KIND`, `CotRecord`, `CotStoreHandle` |
| `cot_store/writer.rs` | 115 | 4 | `CotStoreOptions`, `start`, the batching writer task |
| `cot_store/latest.rs` | 155 | 7 | `LatestRow`, `upsert_batch`, `latest_xml`, `latest_event`, `latest_events`, `prune_stale` |
| `cot_store/history.rs` | 93 | 4 | `HistoryWriter` — one `AppendLog` per uid, LRU-capped |
| `cot_store/retention.rs` | 33 | 3 | `Swept`, `sweep` |
| `config/stream.rs` | 90 | 6 | **+** `StreamLimits` under `[stream.limits]` |
| `jobs/retention.rs` | 49 | 2 | `CotRetentionJob` (+ registered in `jobs/mod.rs`) |
| `runtime.rs` | 164 | 4 | **+** the stream listener in the join, **+** `mutual_tls_authority` |

Integration suites (`tests/`, exempt from the line rule): `stream_support/mod.rs` (the harness),
`stream_routing.rs` (11), `stream_session.rs` (10), `stream_store.rs` (8).

**111 new unit tests** and **29 integration tests**. The server crate is 1038 lib tests, green
(it was 788 when M2-01 landed; the rest are the concurrent Marti and identity briefs').

Manifests: `parking_lot = "0.12.5"` added to `[workspace.dependencies]` (already in the lock file
through `rusqlite`) and to `rustak-server`; the `rustak-client` dev-dependency gained
`features = ["testing"]` for `stream::testing::Eud`. `config.example.toml` gained the
`[stream.limits]` block, because `config::tests::the_example_file_documents_every_key` fails
otherwise — that file is not in the brief's list but the new keys make it a consequence of it.

## The API the rest of the server holds

```rust
// rustak_server::stream

pub struct StreamRuntime;
impl StreamRuntime {
    async fn bind(&AppContext, &Arc<Pki>, Arc<dyn MissionIngest>) -> Result<Self, Error>;
    fn local_addr(&self) -> SocketAddr;      // the port, for a test that asked for zero
    fn live(&self) -> &LiveState;
    fn with_resolver(self, Arc<dyn CertPrincipalResolver>) -> Self;
    async fn run(self, Shutdown) -> Result<(), Error>;
}
/// `runtime::run_all`'s entry point. `None` when the listener is off.
pub async fn serve(AppContext, Option<Arc<Pki>>) -> Result<(), Error>;
```

### `Hub` — the registry, and every routing question

`parking_lot::RwLock<Registry>`; every method takes the lock for a bounded map scan and returns
**owned** data. There is no `.await` anywhere inside one.

```rust
pub struct ConnId(pub u64);

impl Hub {
    fn new() -> Self;                     fn next_id(&self) -> ConnId;
    fn register(&self, Subscription) -> ConnId;
    fn unregister(&self, ConnId) -> Option<Subscription>;
    fn len(&self) -> usize;               fn is_empty(&self) -> bool;

    fn apply_event(&self, ConnId, &Event, Option<&Arc<EncodedEvent>>) -> Option<SaUpdate>;
    fn set_incognito(&self, ConnId, bool) -> bool;   fn is_incognito(&self, ConnId) -> bool;
    fn set_mode(&self, ConnId, Mode);

    fn handle(&self, ConnId) -> Option<ConnHandle>;
    fn principal(&self, ConnId) -> Option<Arc<Principal>>;
    fn device_id(&self, ConnId) -> Option<DeviceId>;
    fn client_uid(&self, ConnId) -> Option<String>;

    fn reachable_from(&self, sender: ConnId, exclude_self: bool) -> Vec<ConnHandle>;
    fn handles_reachable_by(&self, &Principal, exclude: Option<ConnId>) -> Vec<ConnHandle>;
    fn resolve_callsigns(&self, sender: ConnId, &[String]) -> Vec<ConnHandle>;
    fn resolve_uids(&self, sender: ConnId, &[String]) -> Vec<ConnHandle>;
    fn reachable_in_group(&self, sender: ConnId, bitpos: u32) -> Vec<ConnHandle>;
    fn latest_sa_for(&self, receiver: ConnId) -> Vec<Arc<EncodedEvent>>;

    fn handles_for_uid(&self, &str) -> Vec<ConnHandle>;
    fn handles_for_user(&self, &Username) -> Vec<ConnHandle>;
    fn handles_for_fingerprint(&self, &str) -> Vec<ConnHandle>;

    fn snapshot(&self) -> Vec<ClientEndpoint>;              // /Marti/api/clientEndPoints
    fn snapshot_for(&self, &Principal) -> Vec<ClientEndpoint>;  // /Marti/api/contacts/all
}

pub struct ClientEndpoint {
    pub uid: String, pub callsign: String, pub username: String,
    pub team: String, pub role: String, pub takv: String,
    pub groups: Vec<GroupName>,
    pub last_status: DateTime<Utc>, pub connected_at: DateTime<Utc>,
    pub incognito: bool, pub mode: Mode, pub peer: SocketAddr,
}
```

`ConnHandle::send(Outbound) -> SendResult { Sent, Dropped, Closed }` is `try_send` and never waits;
`close_after_drops` consecutive drops escalate to `Closed` **and** cancel the connection's own
`Shutdown` child token, which is what tears down a connection whose queue is full.

### `Notifier` — pushing at connected clients

```rust
pub trait Notifier: Send + Sync {
    fn send_to_uid(&self, client_uid: &str, Event) -> usize;
    fn send_to_user(&self, &Username, Event) -> usize;
    fn send_to_conn(&self, ConnId, Event) -> bool;
    fn send_reachable_from(&self, from: ConnId, exclude_self: bool, Event) -> usize;
    fn disconnect_by_fingerprint(&self, fingerprint: &str) -> usize;
}
impl Notifier for Hub {}

pub fn on_disconnect(&Hub, &Subscription, uid: String, CotTime) -> usize;   // t-x-d-d
pub fn on_groups_changed(&Hub, &Username, originating_uid: Option<&str>, uid: String, CotTime) -> usize;  // t-x-g-c
```

Each returns how many connections it *reached*; a connection whose queue is full is one of the ones
it did not, and is about to be disconnected anyway.

### `MissionIngest` — the M4 seam

```rust
pub struct MissionRef<'a> { pub name: Option<&'a str>, pub guid: Option<&'a str>,
                            pub path: Option<&'a str>, pub after: Option<&'a str> }
impl MissionRef<'_> { pub fn label(&self) -> &str }

#[async_trait]
pub trait MissionIngest: Send + Sync + Debug {
    async fn publish(&self, MissionRef<'_>, sender: &Principal, &EncodedEvent)
        -> Result<Vec<String> /* clientUids to also push to */, Error>;
}
pub struct NoMissions;                 // M1 stub: Ok(vec![])
pub fn no_missions() -> Arc<dyn MissionIngest>;
```

The router already calls it and already resolves the uids it returns, so M4 changes only which
implementation is installed — not the routing path M1 tests.

### `LiveState` — what everything else takes

```rust
#[derive(Clone)]
pub struct LiveState;
impl LiveState {
    fn new(Arc<Hub>, Arc<Router>, CotStoreHandle, Arc<StreamMetrics>) -> Self;
    fn hub(&self) -> &Arc<Hub>;        fn router(&self) -> &Arc<Router>;
    fn store(&self) -> &CotStoreHandle; fn metrics(&self) -> &Arc<StreamMetrics>;
    fn connected(&self) -> usize;
    fn snapshot(&self) -> Vec<ClientEndpoint>;
    fn snapshot_for(&self, &Principal) -> Vec<ClientEndpoint>;
    fn notifier(&self) -> Arc<dyn Notifier>;
    fn groups_changed(&self, &Username, originating_uid: Option<&str>) -> usize;   // t-x-g-c
    fn disconnect_by_fingerprint(&self, &str) -> usize;
    fn send_to_uid(&self, &str, Event) -> usize;
    fn send_to_user(&self, &Username, Event) -> usize;
    fn send_to_conn(&self, ConnId, Event) -> bool;
}
```

It is **passed, not reached for**: a process-wide singleton would make two test servers in one
process share a registry. `StreamRuntime::live()` hands it over. See "Not done here" for the one
piece of wiring that still needs an owner.

### `cot_store`

```rust
pub const STREAM_KIND: &str = "cot";
pub struct CotRecord { /* uid, kind, callsign, user_id, device_id, group_bits, times, point, xml, proto, received_at */ }
impl CotRecord { fn new(&EncodedEvent, &Principal, Option<DeviceId>) -> Self; fn is_historic(&self) -> bool }

#[derive(Clone)] pub struct CotStoreHandle;
impl CotStoreHandle { fn disabled() -> Self; fn record(&self, CotRecord); fn recorded(&self) -> u64;
                      fn dropped(&self) -> u64; fn is_enabled(&self) -> bool }

pub fn start(Database, streams_dir, CotStoreOptions, Shutdown) -> (CotStoreHandle, JoinHandle<()>);

pub async fn latest_xml(&Database, uid) -> Result<Option<String>, Error>;      // /Marti/api/cot/xml/{uid}
pub async fn latest_event(&Database, uid) -> Result<Option<LatestRow>, Error>;
pub async fn latest_events(&Database, since, prefixes: &[String], limit) -> Result<Vec<LatestRow>, Error>;
impl LatestRow { fn visible_to(&self, &GroupSet) -> bool }                     // sender.IN ∩ reader.OUT, from the stored bits
pub async fn retention::sweep(&Database, streams_dir, before) -> Result<Swept, Error>;
```

## Decisions worth recording

### The listener is built in two steps

`StreamRuntime::bind` binds the socket, builds the TLS configuration, starts the store task and
registers the revocation hook; `run` accepts. An address that will not bind therefore fails
*start-up* rather than being discovered by the first device that cannot reach it, and a test learns
the port before anything connects.

### Loading the authority is gated on a listener that needs it

`runtime::listen` loads `Pki` — and installs it on the context — only when
`[stream.tls] enabled` **or** `[web.marti] enabled`. `Pki::load` also issues this server's own
certificate, which needs a host name, and the e2e launcher's configuration has no stream listener,
no Marti listener, no TLS and no `[server] domains`. An unconditional load made that configuration
fail at start-up; this was reported by the e2e agent mid-brief and is fixed and verified by
launching the binary against a copy of `e2e/scripts/start-server.mjs`'s own configuration.

`stream::serve` therefore takes `Option<Arc<Pki>>` and waits on the shutdown token when it is
`None` — returning early would cancel the token and stop the whole server, because `run_all` joins
its components and `stopping_on_exit` cancels on *any* exit.

### `AppContext::install_pki` is called from `runtime.rs`

The `Late<PkiAuthority>` slot landed in `services/mod.rs` from a concurrent brief with nothing
installing it. `runtime::listen` is the one place that loads the authority, so it installs it there.
Consequence, and worth knowing: on an installation with **both** mutual-TLS listeners switched off,
`context.pki()` is unavailable and `context.has_pki()` is false — which is what that accessor exists
for.

### `groups.rs` was not written; `rustak_core::identity::GroupSet` is the whole of it

The brief offered either. `GroupSet` (bitvec, `GROUP_BITS = 256`), `can_reach` and `GroupIndex`
already exist in `rustak-core` and are what `Principal` carries, so a second copy under `stream/`
would have been a second answer to the one question routing asks.

### `MissionPublisher` is called `MissionIngest`

Following the brief over `design/02` §2.1. Same shape.

### The replay copy is the *relayed* form

`Router::handle_inbound` strips `<marti>`, adds the flow tag, encodes, and only then calls
`hub.apply_event(from, encoded.event(), Some(&encoded))`. What a newcomer is replayed is therefore
byte for byte what that message's own recipients were sent — asserted by
`router::tests::the_cached_replay_copy_is_the_relayed_form`. It is also what `cot_latest` stores.

### `<dest publish>` is an address; `All Streaming` is not

A message carrying only `<dest publish>` reaches **nobody** rather than falling back to a broadcast
(TAK Server's own unimplemented state). A message carrying `All Streaming` has its whole callsign
list discarded and, if nothing else addressed it, *does* fall back to the broadcast the operator
asked for. Both are asserted in `dest::tests` and over a real socket in `stream_routing.rs`.

### The reader's codec mode is switched on the reader's task, the writer's on the writer's

`Outbound::SwitchToProto` carries the `t-x-takp-r`, so the answer is written, flushed and the encoder
flipped as one step with nothing able to interleave. The *decoder* is switched in `connection::answer`
the moment the answer is queued, which is safe because the client sends nothing between its request
and our reply (`compat/streaming.md` §4 step 4).

### A connection is torn down by a token, not a queue message

`ConnHandle::close()` cancels a `Shutdown` child **and** queues `Outbound::Close`. The queue message
alone would not do: the two reasons a connection is closed from outside are a revoked certificate
(the device may have stopped reading entirely) and a queue that is already full.

### The store never holds the router up

`CotStoreHandle::record` is `try_send` into a 4096-deep queue with a counter for what did not fit,
warned about once per power of two. A busy database must never be why one client's position report
takes longer to reach another. The writer task batches on whichever comes first — 256 records or
50 ms — and what is queued when the shutdown signal arrives is still written, because it has already
been relayed and is therefore history that happened.

### History is one `AppendLog` per uid, capped at 256 open

A thousand devices would otherwise mean a thousand open file descriptors. The least recently used
log is *sealed* when the cap is reached, which is also exactly what makes it available to retention.
An evicted stream is reopened and appended to, not lost (tested).

### `cot_latest` refuses to go backwards

The upsert carries `WHERE excluded.time >= cot_latest.time`. A relay or a reconnect can deliver an
older report after a newer one, and letting it win would move a contact backwards on every map.

### The stored channel bits are the sender's, at send time

`cot_latest.group_bits` holds `Principal::groups.to_bytes()`, and `LatestRow::visible_to` answers the
reachability question from *that* rather than from today's memberships — otherwise a message read
back tomorrow would leak to somebody who has since joined a channel, or hide from somebody who has
since left one. A row whose BLOB will not parse is **not** shown: a visibility check can only fail
closed.

### The resolver refuses a CN that disagrees with its row

The `certificates` row is the authority for who a connection is (it is what revocation, device
binding and enrolment all write); the CN is read so the logs are legible. They are written by the
same act, so a disagreement means tampering or a database restored over a live installation — and
picking one would be picking which of two contradictory facts to trust. Every refusal is one shape,
logged and never sent to the device, because a stream has no error channel.

### A connection with no channels is accepted

It can reach nobody and be reached by nobody, which looks like a routing bug from the client's side.
Refusing would leave an operator debugging a TLS failure when the problem is a membership list, so
it connects and says so at `info!`.

## Behaviour the wire contract fixes, asserted end to end

| `compat/streaming.md` | Where |
|---|---|
| §4 replay is XML and precedes the single `t-x-takp-v` | `stream_session::a_new_client_is_given_the_map_before_the_protocol_offer` |
| §4 `t-x-takp-q` v1 → `t-x-takp-r true` → protobuf both ways | `…::a_client_that_answers_the_offer_switches_to_protobuf` |
| §5 CloudTAK never negotiates and is still served | `…::a_client_that_never_answers_stays_on_xml_and_is_still_served`, `…::a_protobuf_peer_and_an_xml_peer_still_see_each_other` |
| §6 pong `uid = takPong`, no `<detail>`, only to the pinger | `…::a_ping_is_answered_only_to_the_client_that_sent_it` |
| §7 incognito reaches only explicit callsigns | `stream_routing::an_incognito_client_reaches_only_the_people_it_names` |
| §8 `sender.IN ∩ receiver.OUT`, asymmetric | `stream_routing::{two_members…, a_client_in_another_channel_hears_nothing, reachability_is_not_symmetric}` |
| §8 implicit broadcast never echoes | `stream_routing::a_broadcast_never_comes_back_to_its_sender` |
| §8 `All Streaming` degrades to broadcast | `stream_routing::all_streaming_discards_the_callsign_list_and_broadcasts` |
| §8 `<marti>` stripped from every relay | `stream_routing::a_message_addressed_to_a_callsign_reaches_only_that_callsign` |
| §8 explicit addressing still obeys the channels | `stream_routing::addressing_a_callsign_in_another_channel_reaches_nobody` |
| §8 flow tag added; our own tag drops the message | `stream_routing::{a_relayed_message_carries_this_servers_flow_tag, a_message_that_has_already_been_here_is_dropped}` |
| §8 `<dest group>` needs the sender's `IN` | `stream_store::{a_channel_addressed_message…, a_channel_the_sender_may_not_publish_into…}` |
| §9 `t-x-d-d` with `<link uid type>`, reachable only | `stream_session::a_departing_client_is_taken_off_its_peers_maps` |
| §3 >64 KiB protobuf → `b-f-t-r` pointer at `/Marti/api/cot/xml/{uid}` | `stream_store::an_oversize_message_reaches_a_protobuf_peer_as_a_pointer` |
| §1 client certificate required; revocation ends live sessions | `stream_session::{a_certificate_from_another_authority…, revoking_a_certificate_ends_the_session_it_bought}` |

The integration harness (`tests/stream_support/mod.rs`) goes through the real thing throughout: a
real `Pki`, a real `pki.enroll` per device, a real mutually authenticated handshake and
`rustak_client::stream::testing::Eud` on the other end. There is no test-only branch in `src/`.
`[auth] anon_group_default` is turned **off** in the harness, because the default channel would
otherwise connect every test's clients to each other and make the reachability suite vacuous.

Two notes for anyone writing more of these, both learned the hard way here:

- A `Stream` + `Sink` over one socket only progresses while it is polled, and `Eud::send` polls the
  *write* half. Negotiation is driven by reading, so a test that asserts on the mode has to pump the
  read half first — `stream_support::settle(&mut eud)`.
- `expect_none` fails on *any* event, including a peer's setup traffic. Drain every client that was
  sent something before asserting that one of them is sent nothing.

## Exit checks

```
$ cargo test -p rustak-server --features testing
     Running unittests src/lib.rs
test result: ok. 1038 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 17.26s
     Running unittests src/main.rs
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/bootstrap.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.01s
     Running tests/enroll_flows.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.43s
     Running tests/enroll_oauth.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.55s
     Running tests/marti_contract.rs
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.71s
     Running tests/stream_routing.rs
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.56s
     Running tests/stream_session.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.34s
     Running tests/stream_store.rs
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.13s
   Doc-tests rustak_server
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo test -p rustak-server --lib --features testing -- stream:: cot_store:: jobs::retention
test result: ok. 111 passed; 0 failed; 0 ignored; 0 measured; 927 filtered out; finished in 0.32s

$ cargo clippy -p rustak-server --all-targets --all-features -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.91s

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.48s
   Generated target/doc/rustak_server/index.html and 1 other file

$ cargo fmt -p rustak-server --check
(no output; exit 0 — the whole crate, not only this brief's files)

$ ./scripts/check-file-length.sh
(no output; exit 0)
```

`check-file-length.sh` walks `git ls-files` and this brief makes no `git`/`but` writes, so the new
files were measured with the script's own `awk`; the table at the top is that measurement. The
largest is `stream/subscription.rs` at 247, against a limit of 300. `stream/hub.rs` reached 298 on
the first cut and its registry and indexes were split into `stream/registry.rs` — by responsibility
(the hub answers questions and owns the lock; the registry is the bookkeeping that makes them cheap),
not by line count.

### The e2e start-up regression, verified fixed

```
$ ./target/debug/rustak --config <copy of e2e/scripts/start-server.mjs's config>
… INFO server.run: rustak is running. version="0.1.0" name=rustak-e2e
… INFO server.run:web.server.build_marti: The Marti listener is switched off in the configuration.
… INFO server.run: The CoT stream listener is turned off.
… INFO server.run:job.host.run: The job host has started with 3 registered handler(s). jobs=3
$ curl -fsS http://127.0.0.1:18999/robots.txt   → 200

$ cd e2e && npm run typecheck
(clean)

$ cd e2e && npx playwright test
[WebServer] [e2e] listening on:  http://localhost:18446 (bound 127.0.0.1:18446)
Running 13 tests using 1 worker
  ✘ 2 failed — browserType.launch: Executable doesn't exist at
    …/ms-playwright/chromium_headless_shell-1243/…
```

The **server** starts and the suite reaches the browser launch, which is what this brief broke and
fixed. The two failures are this machine's Playwright cache holding `chromium*-1234` while the
installed Playwright wants `-1243`; `npx playwright install` is a download this session cannot make,
and there is no system Chrome to point `RUSTAK_E2E_CHROMIUM` at. Nothing in these failures mentions
the server.

## Deviations from `design/02` §2, and why

1. **No `listener_tcp.rs`, no `auth_tcp.rs`, no `groups.rs`.** The first two go with the plan's
   removal of the plaintext listener and the `<auth>` handshake; `groups.rs` is
   `rustak_core::identity::GroupSet`, which `Principal` already carries.
2. **`config/stream.rs`, not `config/listeners.rs`.** The section already existed from M0-06 and the
   brief says to extend it. `StreamLimits` gained `negotiate_protobuf` and `record_history` (the
   design puts `negotiate_protobuf` per listener, and there is only one) and dropped
   `oversize_substitution`: the substitution is a protobuf-frame ceiling, so "always" would corrupt
   an XML client's perfectly deliverable message and "off" would corrupt a protobuf one's. It is
   unconditional on protobuf connections and never applied to XML ones — both tested.
3. **`Router::handle_inbound` returns `Disposition` by value with a `DropReason` that names the
   channel** (`GroupNotMember(String)`, `NoSuchGroup(String)`), plus `Unreadable`, so a log line can
   say which channel. `DropReason::as_str` is the metric label.
4. **`select_recipients` returns `Selection { handles, explicit }`** rather than a bare `Vec`, so the
   router can report whether the sender named its recipients without re-deriving it.
5. **`StreamPrincipal` rather than a bare `Principal`** out of the resolver: the stream also needs the
   fingerprint (for revocation), the device row id (for `cot_latest`'s foreign key), the channel
   *names* (for the contact listings) and the device's stored incognito preference. Resolving the
   names once at registration keeps `Hub::snapshot` free of database access.
6. **`ConnStats` has no `last_rx`**; the wall clock lives on the `Subscription` (for
   `/clientEndPoints`' `lastStatus`) and the idle timer is a `tokio::time::timeout` around the read.
   Two clocks for one fact is one clock too many.
7. **`cot_store` has a `history.rs`** the design does not list, because `writer.rs` with the per-uid
   `AppendLog` map inside it was one responsibility too many for one file.
8. **The history retention job is `jobs/retention.rs` on a six-hour schedule**, not daily: a segment
   is eight megabytes, and an installation that has just shortened its horizon should see the space
   back the same day.

## Not done here

- **`LiveState` is not on `AppContext`.** `/Marti/api/clientEndPoints` and `/Marti/api/contacts/all`
  need `snapshot()`/`snapshot_for()`, and the channels admin API needs `groups_changed()` for the
  `t-x-g-c` that M2-02 left a `TODO(M2-08)` for. Both want a `Late<LiveState>` slot beside
  `Late<PkiAuthority>` in `services/mod.rs` — which is another brief's file — installed from
  `runtime::listen` out of `StreamRuntime::live()`. Everything on this side is ready; it is one
  field and one `install_` call.
- **`[retention] cot_history_max_rows` is not enforced.** Only the age horizon is. A per-stream row
  cap needs a `stream_segments` query that lists a stream's segments newest-first, and
  `db/repos/**` is outside this brief.
- **`Hub::set_mode` is never called from the connection.** The `ClientEndpoint.mode` a snapshot
  reports is therefore always `Xml`. The negotiation state is per connection and the writer owns the
  encoder; plumbing it back into the registry is one line in `connection::answer` once something
  actually reads the field.
- **Metrics are counters in memory with no exporter.** `StreamMetrics` is on `LiveState`; wiring it
  to OpenTelemetry belongs with whoever owns the telemetry surface.
- **No interop run.** `design/02` §4's manual checklist (ATAK-CIV on TLS 8089, CloudTAK pointed at
  `ssl://host:8089`) is the M1 exit gate and needs the images from M1-04.

## Concurrency note

Three other agents were writing inside `rustak-server/src/{web,auth,marti}/**`, `rustak-api/**` and
`rustak-client/**` throughout. Several intermediate builds failed on *their* half-landed files — a
`CaSummary` that gained a field, `Resolved::claims` becoming an `Option`, a `rustak-client`
`sidecar/link` module that had not landed — and each resolved on its own. Every number above was
taken after the tree settled, and `cargo fmt -p rustak-server --check` is clean across the whole
crate rather than only this brief's files.
